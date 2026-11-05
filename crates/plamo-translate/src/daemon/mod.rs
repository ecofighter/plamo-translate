pub mod launchctl;
pub mod locate;
pub mod plist;

use crate::ExitCode;
use clap::{Args, Subcommand};
use launchctl::{Launchctl, gui_domain};
use plamo_client::Config;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const LABEL: &str = "net.ecofighter.plamo-translate.daemon";

#[derive(Subcommand)]
pub enum DaemonCmd {
    /// Write the LaunchAgent plist and load it with launchctl
    Install(InstallArgs),
    /// Unload the LaunchAgent and remove its plist
    Uninstall(UninstallArgs),
    /// Start the daemon process now (the socket is always listening)
    Start,
    /// Stop the daemon process (it restarts on the next connection)
    Stop,
    /// Restart the daemon process
    Restart,
    /// Show the launchd state and the daemon status
    Status,
    /// Show daemon.log
    Logs {
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Args, Default)]
pub struct InstallArgs {
    /// Path of the plamo-translated executable
    #[arg(long, value_name = "PATH")]
    pub daemon_bin: Option<PathBuf>,
}

#[derive(Args, Default)]
pub struct UninstallArgs {
    /// Also delete the downloaded model, data and logs
    #[arg(long)]
    pub purge: bool,
    /// Do not ask for confirmation before --purge deletes files
    #[arg(long)]
    pub yes: bool,
}

pub fn data_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support/plamo-translate")
}

pub fn hub_dir(home: &Path) -> PathBuf {
    data_dir(home).join("huggingface/hub")
}

pub fn log_dir(home: &Path) -> PathBuf {
    home.join("Library/Logs/plamo-translate")
}

pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

const SUN_PATH_MAX: usize = 104;

pub async fn run(
    cmd: DaemonCmd,
    cfg: &Config,
    lc: &dyn Launchctl,
    home: &Path,
) -> io::Result<ExitCode> {
    let target = service_target();
    match cmd {
        DaemonCmd::Install(args) => install(args, cfg, lc, home),
        DaemonCmd::Uninstall(args) => uninstall(args, lc, home),
        DaemonCmd::Start => simple(lc, &["kickstart", &target]),
        DaemonCmd::Stop => stop(lc, &target),
        DaemonCmd::Restart => simple(lc, &["kickstart", "-k", &target]),
        DaemonCmd::Status => status(cfg, lc, home).await,
        DaemonCmd::Logs { follow } => logs(home, follow),
    }
}

fn service_target() -> String {
    format!("{}/{LABEL}", gui_domain())
}

fn launchctl(lc: &dyn Launchctl, args: &[&str]) -> io::Result<Output> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    lc.run(&args)
}

fn report_failure(args: &[&str], out: &Output) -> ExitCode {
    eprintln!(
        "error: `launchctl {}` failed ({}): {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    eprintln!("hint: is the daemon installed? see `plamo-translate daemon status`");
    ExitCode::Translation
}

fn simple(lc: &dyn Launchctl, args: &[&str]) -> io::Result<ExitCode> {
    let out = launchctl(lc, args)?;
    if out.status.success() {
        Ok(ExitCode::Ok)
    } else {
        Ok(report_failure(args, &out))
    }
}

/// `launchctl kill` fails with ESRCH on a loaded but idle service, which is not an error for `stop`.
fn stop(lc: &dyn Launchctl, target: &str) -> io::Result<ExitCode> {
    let print = ["print", target];
    let out = launchctl(lc, &print)?;
    if !out.status.success() {
        return Ok(report_failure(&print, &out));
    }
    let info = launchctl::parse_print(&String::from_utf8_lossy(&out.stdout));
    if info.state.as_deref() != Some("running") {
        println!("daemon is not running");
        return Ok(ExitCode::Ok);
    }
    simple(lc, &["kill", "SIGTERM", target])
}

fn is_loaded(lc: &dyn Launchctl) -> io::Result<bool> {
    Ok(launchctl(lc, &["print", &service_target()])?
        .status
        .success())
}

fn bootout_if_loaded(lc: &dyn Launchctl) -> io::Result<Option<ExitCode>> {
    if !is_loaded(lc)? {
        return Ok(None);
    }
    let target = service_target();
    let args = ["bootout", target.as_str()];
    let out = launchctl(lc, &args)?;
    Ok((!out.status.success()).then(|| report_failure(&args, &out)))
}

/// home-manager owns plists it marks `nix`; overwriting them would be undone (or fight)
/// on the next `darwin-rebuild switch`.
fn refuse_if_nix_managed(home: &Path) -> Option<ExitCode> {
    let path = plist_path(home);
    let xml = std::fs::read(&path).ok()?;
    if plist::read_managed_by(&xml).as_deref() != Some("nix") {
        return None;
    }
    eprintln!(
        "error: {} is managed by home-manager ({}=nix); change it through your Nix configuration instead",
        path.display(),
        plist::MANAGED_BY_KEY
    );
    Some(ExitCode::Usage)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// A bare store path (e.g. from `nix run .#daemon-install`) has no GC root, so the plist
/// would point at a deleted file after the next `nix-collect-garbage`.
fn gc_root_warning(program: &Path) -> Option<String> {
    program.starts_with("/nix/store").then(|| {
        format!(
            "warning: {} is a bare /nix/store path that the garbage collector may delete; \
             run `nix profile install` and then `plamo-translate daemon install` from the profile, \
             or use the home-manager module",
            program.display()
        )
    })
}

fn install(
    args: InstallArgs,
    cfg: &Config,
    lc: &dyn Launchctl,
    home: &Path,
) -> io::Result<ExitCode> {
    let socket_len = cfg.socket_path.as_os_str().len();
    if socket_len >= SUN_PATH_MAX {
        eprintln!(
            "error: socket path {} is {socket_len} bytes; it must be shorter than {SUN_PATH_MAX} bytes (set socket_path in the config)",
            cfg.socket_path.display()
        );
        return Ok(ExitCode::Usage);
    }
    if let Some(code) = refuse_if_nix_managed(home) {
        return Ok(code);
    }
    let explicit = args.daemon_bin.map(std::path::absolute).transpose()?;
    let env_bin = std::env::var_os("PLAMO_TRANSLATED_BIN");
    let path_var = std::env::var_os("PATH");
    let Some(program) = locate::locate_daemon(
        explicit.as_deref(),
        env_bin.as_deref(),
        &std::env::current_exe()?,
        path_var.as_deref(),
    ) else {
        eprintln!(
            "error: cannot find an executable {}; pass --daemon-bin PATH or set PLAMO_TRANSLATED_BIN",
            locate::DAEMON_BIN_NAME
        );
        return Ok(ExitCode::Usage);
    };

    create_private_dir(&data_dir(home))?;
    if let Some(parent) = cfg.socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(log_dir(home))?;
    let plist_file = plist_path(home);
    if let Some(parent) = plist_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if let Some(code) = bootout_if_loaded(lc)? {
        return Ok(code);
    }
    let xml = plist::render_plist(&plist::AgentSpec {
        label: LABEL.into(),
        program: program.clone(),
        socket_path: cfg.socket_path.clone(),
        log_dir: log_dir(home),
        managed_by: "cli",
    });
    std::fs::write(&plist_file, xml)?;
    let domain = gui_domain();
    let plist_arg = plist_file.to_string_lossy();
    let bootstrap = ["bootstrap", domain.as_str(), &plist_arg];
    let out = launchctl(lc, &bootstrap)?;
    if !out.status.success() {
        return Ok(report_failure(&bootstrap, &out));
    }
    if let Some(warning) = gc_root_warning(&program) {
        eprintln!("{warning}");
    }
    println!(
        "installed {}\ndaemon: {}\nsocket: {}",
        plist_file.display(),
        program.display(),
        cfg.socket_path.display()
    );
    Ok(ExitCode::Ok)
}

fn confirm(prompt: &str) -> io::Result<bool> {
    eprint!("{prompt} [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

fn remove_dir_if_exists(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn uninstall(args: UninstallArgs, lc: &dyn Launchctl, home: &Path) -> io::Result<ExitCode> {
    if let Some(code) = refuse_if_nix_managed(home) {
        return Ok(code);
    }
    let (data, logs) = (data_dir(home), log_dir(home));
    if args.purge && !args.yes {
        let prompt = format!(
            "This permanently deletes {} (including downloaded models) and {}. Continue?",
            data.display(),
            logs.display()
        );
        if !confirm(&prompt)? {
            eprintln!("aborted; nothing was changed");
            return Ok(ExitCode::Translation);
        }
    }
    if let Some(code) = bootout_if_loaded(lc)? {
        return Ok(code);
    }
    let plist_file = plist_path(home);
    match std::fs::remove_file(&plist_file) {
        Ok(()) => println!("removed {}", plist_file.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if args.purge {
        remove_dir_if_exists(&data)?;
        remove_dir_if_exists(&logs)?;
        println!("removed {}\nremoved {}", data.display(), logs.display());
    }
    Ok(ExitCode::Ok)
}

const STDERR_TAIL_LINES: usize = 20;

async fn status(cfg: &Config, lc: &dyn Launchctl, home: &Path) -> io::Result<ExitCode> {
    let out = launchctl(lc, &["print", &service_target()])?;
    if !out.status.success() {
        println!("launchd: not loaded");
        eprintln!("hint: run `plamo-translate daemon install` to set it up");
        return Ok(ExitCode::Unreachable);
    }
    let info = launchctl::parse_print(&String::from_utf8_lossy(&out.stdout));
    let or_dash = |v: Option<String>| v.unwrap_or_else(|| "-".into());
    println!(
        "launchd: loaded\nstate: {}\npid: {}\nlast exit code: {}",
        or_dash(info.state.clone()),
        or_dash(info.pid.map(|p| p.to_string())),
        or_dash(info.last_exit_code.clone()),
    );
    // Only query a running daemon: connecting to an idle one would make launchd spawn it.
    if info.state.as_deref() == Some("running") {
        match crate::client_for(cfg).status().await {
            Ok(s) => print!("{}", crate::translate::format_status(&s)),
            Err(e) => eprintln!("warning: the daemon is running but did not answer: {e}"),
        }
    }
    if info.exited_with_failure() {
        print_stderr_tail(&log_dir(home).join("daemon.stderr.log"));
    }
    Ok(ExitCode::Ok)
}

fn print_stderr_tail(path: &Path) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
            println!("--- last lines of {} ---", path.display());
            for line in &lines[start..] {
                println!("{line}");
            }
        }
        Err(e) => eprintln!("warning: cannot read {}: {e}", path.display()),
    }
}

fn logs(home: &Path, follow: bool) -> io::Result<ExitCode> {
    let path = log_dir(home).join("daemon.log");
    if follow {
        // -F keeps following across RotatingFileHandler rollovers.
        let status = Command::new("/usr/bin/tail")
            .args(["-n", "50", "-F"])
            .arg(&path)
            .status()?;
        return Ok(if status.success() {
            ExitCode::Ok
        } else {
            ExitCode::Translation
        });
    }
    match std::fs::read(&path) {
        Ok(bytes) => {
            io::stdout().write_all(&bytes)?;
            Ok(ExitCode::Ok)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("error: no log yet at {}", path.display());
            Ok(ExitCode::Translation)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::launchctl::RecordingLaunchctl;
    use super::*;
    use tempfile::TempDir;

    struct Env {
        home: TempDir,
        cfg: Config,
        daemon_bin: PathBuf,
    }

    // /tmp keeps the socket path well under the 104-byte sun_path limit.
    fn env() -> Env {
        let home = tempfile::Builder::new()
            .prefix("pth")
            .tempdir_in("/tmp")
            .unwrap();
        let daemon_bin = home.path().join("bin/plamo-translated");
        std::fs::create_dir_all(daemon_bin.parent().unwrap()).unwrap();
        std::fs::write(&daemon_bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&daemon_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = Config::load_from(&|_| None, home.path()).unwrap();
        Env {
            home,
            cfg,
            daemon_bin,
        }
    }

    impl Env {
        fn home(&self) -> &Path {
            self.home.path()
        }

        fn install_cmd(&self) -> DaemonCmd {
            DaemonCmd::Install(InstallArgs {
                daemon_bin: Some(self.daemon_bin.clone()),
            })
        }

        fn write_plist(&self, managed_by: &'static str) {
            let p = plist_path(self.home());
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let xml = plist::render_plist(&plist::AgentSpec {
                label: LABEL.into(),
                program: self.daemon_bin.clone(),
                socket_path: self.cfg.socket_path.clone(),
                log_dir: log_dir(self.home()),
                managed_by,
            });
            std::fs::write(p, xml).unwrap();
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn target() -> String {
        format!("{}/{LABEL}", gui_domain())
    }

    #[tokio::test]
    async fn install_bootouts_if_loaded_then_bootstraps() {
        let e = env();
        let lc = RecordingLaunchctl::default();
        let code = run(e.install_cmd(), &e.cfg, &lc, e.home()).await.unwrap();

        assert_eq!(code, ExitCode::Ok);
        let plist = plist_path(e.home());
        assert_eq!(
            lc.calls(),
            vec![
                s(&["print", &target()]),
                s(&["bootout", &target()]),
                s(&["bootstrap", &gui_domain(), plist.to_str().unwrap()]),
            ]
        );
        let xml = std::fs::read(&plist).unwrap();
        assert_eq!(plist::read_managed_by(&xml).as_deref(), Some("cli"));
        assert!(
            String::from_utf8(xml)
                .unwrap()
                .contains(e.daemon_bin.to_str().unwrap())
        );
        let mode = std::fs::metadata(data_dir(e.home()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(log_dir(e.home()).is_dir());
    }

    #[test]
    fn bare_store_daemon_path_gets_gc_warning() {
        let store =
            Path::new("/nix/store/abc123-plamo-translate-0.1.0/libexec/bin/plamo-translated");
        let warning = gc_root_warning(store).unwrap();
        assert!(warning.contains("nix profile install"), "{warning}");
        assert!(warning.contains("home-manager"), "{warning}");
        for rooted in [
            "/Users/t/.nix-profile/libexec/bin/plamo-translated",
            "/etc/profiles/per-user/t/libexec/bin/plamo-translated",
            "/usr/local/bin/plamo-translated",
            "/nix/storefront/bin/plamo-translated",
        ] {
            assert_eq!(gc_root_warning(Path::new(rooted)), None, "{rooted}");
        }
    }

    #[tokio::test]
    async fn install_skips_bootout_when_not_loaded() {
        let e = env();
        let lc = RecordingLaunchctl::default().respond("print", 113, "");
        let code = run(e.install_cmd(), &e.cfg, &lc, e.home()).await.unwrap();

        assert_eq!(code, ExitCode::Ok);
        let calls = lc.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1][0], "bootstrap");
    }

    #[tokio::test]
    async fn install_overwrites_cli_managed_plist() {
        let e = env();
        e.write_plist("cli");
        let lc = RecordingLaunchctl::default();
        let code = run(e.install_cmd(), &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Ok);
    }

    #[tokio::test]
    async fn install_refuses_nix_managed_plist() {
        let e = env();
        e.write_plist("nix");
        let before = std::fs::read(plist_path(e.home())).unwrap();
        let lc = RecordingLaunchctl::default();
        let code = run(e.install_cmd(), &e.cfg, &lc, e.home()).await.unwrap();

        assert_eq!(code, ExitCode::Usage);
        assert!(lc.calls().is_empty());
        assert_eq!(std::fs::read(plist_path(e.home())).unwrap(), before);
    }

    #[tokio::test]
    async fn install_rejects_long_socket_path() {
        let mut e = env();
        e.cfg.socket_path = e.home().join("s".repeat(104));
        let lc = RecordingLaunchctl::default();
        let code = run(e.install_cmd(), &e.cfg, &lc, e.home()).await.unwrap();

        assert_eq!(code, ExitCode::Usage);
        assert!(lc.calls().is_empty());
        assert!(!plist_path(e.home()).exists());
    }

    #[tokio::test]
    async fn uninstall_bootouts_and_removes_plist() {
        let e = env();
        e.write_plist("cli");
        let lc = RecordingLaunchctl::default();
        let cmd = DaemonCmd::Uninstall(UninstallArgs::default());
        let code = run(cmd, &e.cfg, &lc, e.home()).await.unwrap();

        assert_eq!(code, ExitCode::Ok);
        assert_eq!(
            lc.calls(),
            vec![s(&["print", &target()]), s(&["bootout", &target()])]
        );
        assert!(!plist_path(e.home()).exists());
    }

    #[tokio::test]
    async fn uninstall_refuses_nix_managed_plist() {
        let e = env();
        e.write_plist("nix");
        let lc = RecordingLaunchctl::default();
        let cmd = DaemonCmd::Uninstall(UninstallArgs {
            purge: true,
            yes: true,
        });
        let code = run(cmd, &e.cfg, &lc, e.home()).await.unwrap();

        assert_eq!(code, ExitCode::Usage);
        assert!(lc.calls().is_empty());
        assert!(plist_path(e.home()).exists());
    }

    #[tokio::test]
    async fn uninstall_purge_yes_deletes_data_and_logs() {
        let e = env();
        e.write_plist("cli");
        std::fs::create_dir_all(hub_dir(e.home())).unwrap();
        std::fs::create_dir_all(log_dir(e.home())).unwrap();
        std::fs::write(log_dir(e.home()).join("daemon.log"), "x").unwrap();
        let lc = RecordingLaunchctl::default().respond("print", 113, "");
        let cmd = DaemonCmd::Uninstall(UninstallArgs {
            purge: true,
            yes: true,
        });
        let code = run(cmd, &e.cfg, &lc, e.home()).await.unwrap();

        assert_eq!(code, ExitCode::Ok);
        assert_eq!(lc.calls(), vec![s(&["print", &target()])]);
        assert!(!data_dir(e.home()).exists());
        assert!(!log_dir(e.home()).exists());
        assert!(!plist_path(e.home()).exists());
    }

    #[tokio::test]
    async fn start_kickstarts_only() {
        let e = env();
        let lc = RecordingLaunchctl::default();
        let code = run(DaemonCmd::Start, &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Ok);
        assert_eq!(lc.calls(), vec![s(&["kickstart", &target()])]);
    }

    #[tokio::test]
    async fn stop_sends_sigterm_kill_when_running() {
        let e = env();
        let printed = include_str!("../../tests/fixtures/launchctl_print_running.txt");
        let lc = RecordingLaunchctl::default().respond("print", 0, printed);
        let code = run(DaemonCmd::Stop, &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Ok);
        assert_eq!(
            lc.calls(),
            vec![s(&["print", &target()]), s(&["kill", "SIGTERM", &target()])]
        );
    }

    #[tokio::test]
    async fn stop_when_loaded_but_idle_is_ok_without_kill() {
        let e = env();
        let printed = include_str!("../../tests/fixtures/launchctl_print_exited.txt");
        let lc = RecordingLaunchctl::default().respond("print", 0, printed);
        let code = run(DaemonCmd::Stop, &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Ok);
        assert_eq!(lc.calls(), vec![s(&["print", &target()])]);
    }

    #[tokio::test]
    async fn stop_when_not_loaded_fails_without_kill() {
        let e = env();
        let lc = RecordingLaunchctl::default().respond("print", 113, "");
        let code = run(DaemonCmd::Stop, &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Translation);
        assert_eq!(lc.calls(), vec![s(&["print", &target()])]);
    }

    #[tokio::test]
    async fn restart_uses_kickstart_k() {
        let e = env();
        let lc = RecordingLaunchctl::default();
        let code = run(DaemonCmd::Restart, &e.cfg, &lc, e.home())
            .await
            .unwrap();
        assert_eq!(code, ExitCode::Ok);
        assert_eq!(lc.calls(), vec![s(&["kickstart", "-k", &target()])]);
    }

    #[tokio::test]
    async fn launchctl_failure_is_reported() {
        let e = env();
        let lc = RecordingLaunchctl::default().respond("kickstart", 113, "");
        let code = run(DaemonCmd::Start, &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Translation);
    }

    #[tokio::test]
    async fn status_when_not_loaded_is_unreachable() {
        let e = env();
        let lc = RecordingLaunchctl::default().respond("print", 113, "");
        let code = run(DaemonCmd::Status, &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Unreachable);
        assert_eq!(lc.calls(), vec![s(&["print", &target()])]);
    }

    #[tokio::test]
    async fn status_when_loaded_but_idle_is_ok() {
        let e = env();
        let printed = include_str!("../../tests/fixtures/launchctl_print_exited.txt");
        let lc = RecordingLaunchctl::default().respond("print", 0, printed);
        let code = run(DaemonCmd::Status, &e.cfg, &lc, e.home()).await.unwrap();
        assert_eq!(code, ExitCode::Ok);
    }
}
