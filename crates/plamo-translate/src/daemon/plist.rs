use plist::{Dictionary, Value};
use std::io::Cursor;
use std::path::PathBuf;

pub const MANAGED_BY_KEY: &str = "PLAMO_TRANSLATE_MANAGED_BY";

pub struct AgentSpec {
    pub label: String,
    pub program: PathBuf,
    pub socket_path: PathBuf,
    pub log_dir: PathBuf,
    pub managed_by: &'static str,
}

pub fn render_plist(spec: &AgentSpec) -> String {
    let path = |p: &PathBuf| Value::String(p.to_string_lossy().into_owned());
    let stderr_log = path(&spec.log_dir.join("daemon.stderr.log"));

    let mut listener = Dictionary::new();
    listener.insert("SockPathName".into(), path(&spec.socket_path));
    listener.insert("SockPathMode".into(), Value::Integer(0o600.into()));
    let mut sockets = Dictionary::new();
    sockets.insert("Listeners".into(), Value::Dictionary(listener));
    let mut env = Dictionary::new();
    env.insert(MANAGED_BY_KEY.into(), Value::String(spec.managed_by.into()));

    // KeepAlive and RunAtLoad are deliberately absent: launchd starts the job on the first connection.
    let mut root = Dictionary::new();
    root.insert("Label".into(), Value::String(spec.label.clone()));
    root.insert(
        "ProgramArguments".into(),
        Value::Array(vec![path(&spec.program)]),
    );
    root.insert("Sockets".into(), Value::Dictionary(sockets));
    root.insert("StandardOutPath".into(), stderr_log.clone());
    root.insert("StandardErrorPath".into(), stderr_log);
    root.insert("ProcessType".into(), Value::String("Interactive".into()));
    root.insert("EnvironmentVariables".into(), Value::Dictionary(env));

    let mut buf = Vec::new();
    Value::Dictionary(root)
        .to_writer_xml(&mut buf)
        .expect("writing a plist to memory cannot fail");
    buf.push(b'\n');
    String::from_utf8(buf).expect("plist XML is UTF-8")
}

pub fn read_managed_by(xml: &[u8]) -> Option<String> {
    let root = Value::from_reader(Cursor::new(xml)).ok()?;
    root.as_dictionary()?
        .get("EnvironmentVariables")?
        .as_dictionary()?
        .get(MANAGED_BY_KEY)?
        .as_string()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(managed_by: &'static str) -> AgentSpec {
        AgentSpec {
            label: "local.plamo-translate.daemon".into(),
            program: "/nix/profile/libexec/bin/plamo-translated".into(),
            socket_path: "/Users/u/Library/Application Support/plamo-translate/daemon.sock".into(),
            log_dir: "/Users/u/Library/Logs/plamo-translate".into(),
            managed_by,
        }
    }

    #[test]
    fn plist_snapshot() {
        insta::assert_snapshot!(render_plist(&spec("cli")));
    }

    #[test]
    fn read_managed_by_roundtrip() {
        for who in ["cli", "nix"] {
            assert_eq!(
                read_managed_by(render_plist(&spec(who)).as_bytes()).as_deref(),
                Some(who)
            );
        }
    }

    #[test]
    fn read_managed_by_absent_or_invalid() {
        let no_env = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Label</key><string>x</string></dict></plist>"#;
        assert_eq!(read_managed_by(no_env), None);
        assert_eq!(read_managed_by(b"not a plist"), None);
    }
}
