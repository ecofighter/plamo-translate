use assert_cmd::Command;
use predicates::str::contains;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use tempfile::TempDir;

struct Fake {
    dir: TempDir,
    socket: PathBuf,
    requests: mpsc::Receiver<Value>,
}

fn fake_daemon(responses: &[&str]) -> Fake {
    let dir = tempfile::Builder::new()
        .prefix("pt")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, requests) = mpsc::channel();
    let responses: Vec<String> = responses.iter().map(|s| format!("{s}\n")).collect();
    thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(conn.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        tx.send(serde_json::from_str(&line).unwrap()).unwrap();
        for r in responses {
            conn.write_all(r.as_bytes()).unwrap();
        }
        conn.flush().unwrap();
        // the client never shuts down its write half; keep the connection until it exits
        let _ = BufReader::new(conn).read_line(&mut String::new());
    });
    Fake {
        dir,
        socket,
        requests,
    }
}

fn cmd(fake: &Fake) -> Command {
    let mut c = Command::cargo_bin("plamo-translate").unwrap();
    c.env("PLAMO_TRANSLATE_SOCKET", &fake.socket)
        .env("PLAMO_TRANSLATE_CONFIG", fake.dir.path().join("none.toml"))
        .env("PLAMO_TRANSLATE_CONNECT_TIMEOUT_MS", "300");
    c
}

const STREAM: [&str; 3] = [
    r#"{"type":"delta","text":"The "}"#,
    r#"{"type":"delta","text":"meeting"}"#,
    r#"{"type":"done","text":"The meeting","usage":{"input_tokens":3,"output_tokens":2},"model":"m","revision":"r"}"#,
];

#[test]
fn streams_deltas_to_stdout() {
    let f = fake_daemon(&STREAM);
    cmd(&f)
        .args(["-t", "en", "会議"])
        .assert()
        .success()
        .stdout("The meeting\n");
}

#[test]
fn reads_stdin_when_no_text() {
    let f = fake_daemon(&STREAM);
    cmd(&f).write_stdin("会議").assert().success();
    assert_eq!(f.requests.recv().unwrap()["text"], "会議");
}

#[test]
fn auto_target_from_japanese_text() {
    let f = fake_daemon(&STREAM);
    cmd(&f).arg("会議は月曜").assert().success();
    assert_eq!(f.requests.recv().unwrap()["target_lang"], "en");
}

#[test]
fn glossary_files_merge_config_then_args() {
    let f = fake_daemon(&STREAM);
    let ga = f.dir.path().join("cfg.tsv");
    let gb = f.dir.path().join("arg.tsv");
    std::fs::write(&ga, "a\t1\n").unwrap();
    std::fs::write(&gb, "a\t2\n").unwrap();
    let cfg = f.dir.path().join("config.toml");
    std::fs::write(&cfg, format!("glossary_file = \"{}\"\n", ga.display())).unwrap();
    cmd(&f)
        .env("PLAMO_TRANSLATE_CONFIG", &cfg)
        .args(["--glossary", gb.to_str().unwrap(), "-t", "ja", "x"])
        .assert()
        .success();
    let req = f.requests.recv().unwrap();
    assert_eq!(
        req["glossary"],
        serde_json::json!([{"source":"a","target":"2"}])
    );
}

#[test]
fn no_stream_flag_sets_stream_false() {
    let f =
        fake_daemon(&[r#"{"type":"done","text":"Whole","usage":null,"model":"m","revision":"r"}"#]);
    cmd(&f)
        .args(["--no-stream", "-t", "en", "x"])
        .assert()
        .success()
        .stdout("Whole\n");
    assert_eq!(f.requests.recv().unwrap()["stream"], false);
}

#[test]
fn json_flag_prints_done_event() {
    let f = fake_daemon(&STREAM);
    let out = cmd(&f).args(["--json", "-t", "en", "x"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["type"], "done");
    assert_eq!(v["text"], "The meeting");
}

#[test]
fn daemon_error_exits_1_with_message() {
    let f = fake_daemon(&[
        r#"{"type":"error","code":"invalid_request","message":"form must be polite or plain"}"#,
    ]);
    cmd(&f)
        .args(["-t", "ja", "--form", "rude", "x"])
        .assert()
        .code(1)
        .stderr(contains(
            "error: invalid_request: form must be polite or plain",
        ));
}

#[test]
fn unreachable_exits_3_with_install_hint() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("plamo-translate")
        .unwrap()
        .env("PLAMO_TRANSLATE_SOCKET", dir.path().join("nope.sock"))
        .env("PLAMO_TRANSLATE_CONFIG", dir.path().join("none.toml"))
        .env("PLAMO_TRANSLATE_CONNECT_TIMEOUT_MS", "100")
        .args(["-t", "en", "x"])
        .assert()
        .code(3)
        .stderr(contains("plamo-translate daemon install"));
}

#[test]
fn context_and_context_file_conflict_exits_2() {
    let f = fake_daemon(&STREAM);
    cmd(&f)
        .args(["--context", "a", "--context-file", "b", "x"])
        .assert()
        .code(2);
}

#[test]
fn empty_stdin_exits_2() {
    let f = fake_daemon(&STREAM);
    cmd(&f).write_stdin("").assert().code(2);
}

#[test]
fn status_prints_model_and_loaded() {
    let f = fake_daemon(&[
        r#"{"type":"status","model":"pfnet/m","revision":"abc","loaded":true,"uptime_secs":42,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1}"#,
    ]);
    cmd(&f)
        .arg("status")
        .assert()
        .success()
        .stdout(contains("model: pfnet/m\n"))
        .stdout(contains("loaded: true\n"));
}

#[test]
fn text_that_is_not_a_subcommand_is_translated() {
    let f = fake_daemon(&STREAM);
    cmd(&f).args(["-t", "en", "statuses"]).assert().success();
    assert_eq!(f.requests.recv().unwrap()["text"], "statuses");
}
