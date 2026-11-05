use assert_cmd::Command;
use predicates::str::contains;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const LABEL: &str = "local.plamo-translate.daemon";

struct Home {
    dir: TempDir,
    launchctl: PathBuf,
}

// HOME and launchctl are always fake so these tests never touch the real ~/Library or launchd.
fn home() -> Home {
    home_with_print("exit 113")
}

/// `print_action` is the shell run for `launchctl print`; other subcommands succeed silently.
fn home_with_print(print_action: &str) -> Home {
    let dir = tempfile::Builder::new()
        .prefix("pth")
        .tempdir_in("/tmp")
        .unwrap();
    let launchctl = dir.path().join("fake-launchctl");
    std::fs::write(
        &launchctl,
        format!(
            "#!/bin/sh\necho \"$@\" >> \"$0.calls\"\ncase \"$1\" in print) {print_action};; esac\nexit 0\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&launchctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    Home { dir, launchctl }
}

impl Home {
    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("plamo-translate").unwrap();
        c.env("HOME", self.path())
            .env_remove("PLAMO_TRANSLATE_SOCKET")
            .env_remove("PLAMO_TRANSLATED_BIN")
            .env("PLAMO_TRANSLATE_CONFIG", self.path().join("none.toml"))
            .env("XDG_CONFIG_HOME", self.path().join("xdg"))
            .env("PLAMO_TRANSLATE_LAUNCHCTL", &self.launchctl)
            .env("PLAMO_TRANSLATE_CONNECT_TIMEOUT_MS", "300");
        c
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.launchctl.with_extension("calls")).unwrap_or_default()
    }

    fn data_dir(&self) -> PathBuf {
        self.path()
            .join("Library/Application Support/plamo-translate")
    }

    fn log_dir(&self) -> PathBuf {
        self.path().join("Library/Logs/plamo-translate")
    }

    fn plist(&self) -> PathBuf {
        self.path()
            .join(format!("Library/LaunchAgents/{LABEL}.plist"))
    }
}

#[test]
fn model_path_prints_hub_dir() {
    let h = home();
    let expected = format!("{}\n", h.data_dir().join("huggingface/hub").display());
    h.cmd()
        .args(["model", "path"])
        .assert()
        .success()
        .stdout(expected);
}

#[test]
fn install_with_daemon_bin_writes_plist_and_bootstraps() {
    let h = home();
    let bin = h.path().join("bin/plamo-translated");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    h.cmd()
        .args(["daemon", "install", "--daemon-bin"])
        .arg(&bin)
        .assert()
        .success();

    let plist = std::fs::read_to_string(h.plist()).unwrap();
    assert!(plist.contains(bin.to_str().unwrap()));
    let calls = h.calls();
    assert!(calls.contains("print gui/"), "{calls}");
    assert!(
        calls.contains("bootstrap gui/") && calls.contains(h.plist().to_str().unwrap()),
        "{calls}"
    );
    assert!(!calls.contains("bootout"), "{calls}");
}

#[test]
fn install_without_daemon_fails_with_usage() {
    let h = home();
    h.cmd()
        .args(["daemon", "install"])
        .env("PATH", h.path().join("empty"))
        .assert()
        .code(2)
        .stderr(contains("plamo-translated"));
    assert_eq!(h.calls(), "");
}

#[test]
fn uninstall_refuses_nix_managed_plist() {
    let h = home();
    std::fs::create_dir_all(h.plist().parent().unwrap()).unwrap();
    std::fs::write(
        h.plist(),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>local.plamo-translate.daemon</string>
<key>EnvironmentVariables</key><dict><key>PLAMO_TRANSLATE_MANAGED_BY</key><string>nix</string></dict>
</dict></plist>"#,
    )
    .unwrap();
    h.cmd()
        .args(["daemon", "uninstall"])
        .assert()
        .code(2)
        .stderr(contains("home-manager"));
    assert!(h.plist().exists());
    assert_eq!(h.calls(), "");
}

#[test]
fn uninstall_purge_declined_changes_nothing() {
    let h = home();
    std::fs::create_dir_all(h.data_dir()).unwrap();
    std::fs::create_dir_all(h.log_dir()).unwrap();
    h.cmd()
        .args(["daemon", "uninstall", "--purge"])
        .write_stdin("n\n")
        .assert()
        .code(1)
        .stderr(contains("aborted"));
    assert!(h.data_dir().exists());
    assert!(h.log_dir().exists());
    assert_eq!(h.calls(), "");
}

#[test]
fn uninstall_purge_confirmed_deletes_data_and_logs() {
    let h = home();
    std::fs::create_dir_all(h.data_dir().join("huggingface/hub")).unwrap();
    std::fs::create_dir_all(h.log_dir()).unwrap();
    h.cmd()
        .args(["daemon", "uninstall", "--purge"])
        .write_stdin("y\n")
        .assert()
        .success();
    assert!(!h.data_dir().exists());
    assert!(!h.log_dir().exists());
}

#[test]
fn logs_prints_daemon_log() {
    let h = home();
    std::fs::create_dir_all(h.log_dir()).unwrap();
    std::fs::write(h.log_dir().join("daemon.log"), "line one\nline two\n").unwrap();
    h.cmd()
        .args(["daemon", "logs"])
        .assert()
        .success()
        .stdout("line one\nline two\n");
}

#[test]
fn logs_without_file_fails() {
    let h = home();
    h.cmd()
        .args(["daemon", "logs"])
        .assert()
        .code(1)
        .stderr(contains("daemon.log"));
}

#[test]
fn status_when_not_loaded_exits_unreachable() {
    let h = home();
    h.cmd()
        .args(["daemon", "status"])
        .assert()
        .code(3)
        .stdout(contains("not loaded"));
    assert!(h.calls().starts_with("print gui/"));
}

#[test]
fn model_pull_without_daemon_exits_unreachable() {
    let h = home();
    h.cmd()
        .args(["model", "pull"])
        .assert()
        .code(3)
        .stderr(contains("daemon install"));
}

#[test]
fn stop_when_loaded_but_idle_exits_ok_without_kill() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launchctl_print_exited.txt");
    let h = home_with_print(&format!("cat '{}'; exit 0", fixture.display()));
    h.cmd()
        .args(["daemon", "stop"])
        .assert()
        .success()
        .stdout(contains("not running"));
    let calls = h.calls();
    assert!(calls.starts_with("print gui/"), "{calls}");
    assert!(!calls.contains("kill"), "{calls}");
}

#[test]
fn stop_when_not_loaded_fails() {
    let h = home();
    h.cmd()
        .args(["daemon", "stop"])
        .assert()
        .code(1)
        .stderr(contains("is the daemon installed"));
    assert!(!h.calls().contains("kill"));
}
