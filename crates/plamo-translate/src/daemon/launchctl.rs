use std::process::{Command, Output};

pub trait Launchctl {
    fn run(&self, args: &[String]) -> std::io::Result<Output>;
}

pub struct SystemLaunchctl;

impl Launchctl for SystemLaunchctl {
    fn run(&self, args: &[String]) -> std::io::Result<Output> {
        // Hidden override so CLI tests can substitute a fake and never touch the real launchd.
        let program = std::env::var_os("PLAMO_TRANSLATE_LAUNCHCTL")
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/bin/launchctl".into());
        Command::new(program).args(args).output()
    }
}

pub fn gui_domain() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

#[derive(Debug, Default, PartialEq)]
pub struct PrintInfo {
    pub state: Option<String>,
    pub pid: Option<u32>,
    pub last_exit_code: Option<String>,
}

impl PrintInfo {
    /// `last exit code` is "(never exited)", "0", or e.g. "78: EX_CONFIG".
    pub fn exited_with_failure(&self) -> bool {
        self.last_exit_code
            .as_deref()
            .and_then(|c| c.split(':').next())
            .and_then(|n| n.trim().parse::<i64>().ok())
            .is_some_and(|n| n != 0)
    }
}

/// Reads only the service's own top-level fields: nested blocks (coalitions, endpoints)
/// repeat keys such as `state` with unrelated meanings.
pub fn parse_print(out: &str) -> PrintInfo {
    let mut info = PrintInfo::default();
    for line in out.lines() {
        let Some(field) = line.strip_prefix('\t') else {
            continue;
        };
        if field.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = field.split_once(" = ") else {
            continue;
        };
        let value = value.trim();
        match key {
            "state" => info.state = Some(value.to_string()),
            "pid" => info.pid = value.parse().ok(),
            "last exit code" => info.last_exit_code = Some(value.to_string()),
            _ => {}
        }
    }
    info
}

#[cfg(test)]
pub use fake::RecordingLaunchctl;

#[cfg(test)]
mod fake {
    use super::Launchctl;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    /// Responses are keyed by launchctl subcommand; unlisted subcommands succeed with no output.
    #[derive(Default)]
    pub struct RecordingLaunchctl {
        pub calls: RefCell<Vec<Vec<String>>>,
        pub responses: HashMap<String, (i32, String)>,
    }

    impl RecordingLaunchctl {
        pub fn respond(mut self, subcommand: &str, code: i32, stdout: &str) -> Self {
            self.responses
                .insert(subcommand.to_string(), (code, stdout.to_string()));
            self
        }

        pub fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl Launchctl for RecordingLaunchctl {
        fn run(&self, args: &[String]) -> std::io::Result<Output> {
            self.calls.borrow_mut().push(args.to_vec());
            let (code, stdout) = self.responses.get(&args[0]).cloned().unwrap_or_default();
            Ok(Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = include_str!("../../tests/fixtures/launchctl_print_running.txt");
    const EXITED: &str = include_str!("../../tests/fixtures/launchctl_print_exited.txt");

    #[test]
    fn gui_domain_uses_uid() {
        assert_eq!(gui_domain(), format!("gui/{}", unsafe { libc::getuid() }));
    }

    #[test]
    fn status_parses_launchctl_print() {
        assert_eq!(
            parse_print(RUNNING),
            PrintInfo {
                state: Some("running".into()),
                pid: Some(1197),
                last_exit_code: Some("(never exited)".into()),
            }
        );
        let exited = parse_print(EXITED);
        assert_eq!(
            exited,
            PrintInfo {
                state: Some("not running".into()),
                pid: None,
                last_exit_code: Some("0".into()),
            }
        );
        assert!(!exited.exited_with_failure());
        assert!(!parse_print(RUNNING).exited_with_failure());
    }

    #[test]
    fn nonzero_last_exit_code_is_a_failure() {
        let crashed = EXITED.replace("last exit code = 0", "last exit code = 78: EX_CONFIG");
        let info = parse_print(&crashed);
        assert_eq!(info.last_exit_code.as_deref(), Some("78: EX_CONFIG"));
        assert!(info.exited_with_failure());
    }
}
