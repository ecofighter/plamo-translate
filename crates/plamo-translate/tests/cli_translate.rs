use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
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
fn json_includes_finish_reason() {
    let f = fake_daemon(&[
        r#"{"type":"done","text":"The meeting","finish_reason":"stop","usage":{"input_tokens":3,"output_tokens":2},"model":"m","revision":"r"}"#,
    ]);
    let out = cmd(&f).args(["--json", "-t", "en", "x"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["finish_reason"], "stop");

    let f = fake_daemon(&STREAM);
    let out = cmd(&f).args(["--json", "-t", "en", "x"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v.as_object().unwrap().contains_key("finish_reason"));
    assert!(v["finish_reason"].is_null());
}

#[test]
fn status_prints_features_and_template_verified() {
    let f = fake_daemon(&[
        r#"{"type":"status","model":"pfnet/m","revision":"abc","loaded":true,"uptime_secs":42,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1,"features":["history"],"template_verified":true}"#,
    ]);
    cmd(&f).arg("status").assert().success().stdout(contains(
        "protocol: 1\nfeatures: history\ntemplate_verified: true\n",
    ));

    let f = fake_daemon(&[
        r#"{"type":"status","model":"pfnet/m","revision":"abc","loaded":true,"uptime_secs":42,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1}"#,
    ]);
    cmd(&f)
        .arg("status")
        .assert()
        .success()
        .stdout(contains("features: -\n"))
        .stdout(contains("template_verified: -\n"));
}

#[test]
fn text_that_is_not_a_subcommand_is_translated() {
    let f = fake_daemon(&STREAM);
    cmd(&f).args(["-t", "en", "statuses"]).assert().success();
    assert_eq!(f.requests.recv().unwrap()["text"], "statuses");
}

/// Accepts one connection per conversation, in order; each conversation's first request line
/// goes to `requests` and its lines are written back.
fn fake_daemon_seq(conversations: &[&[&str]]) -> Fake {
    let dir = tempfile::Builder::new()
        .prefix("pt")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, requests) = mpsc::channel();
    let conversations: Vec<Vec<String>> = conversations
        .iter()
        .map(|c| c.iter().map(|s| format!("{s}\n")).collect())
        .collect();
    thread::spawn(move || {
        for responses in conversations {
            let (mut conn, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            tx.send(serde_json::from_str::<Value>(&line).unwrap())
                .unwrap();
            for r in responses {
                conn.write_all(r.as_bytes()).unwrap();
            }
            conn.flush().unwrap();
            // serve the next connection while this one stays open until the client drops it
            thread::spawn(move || {
                let _ = reader.read_line(&mut String::new());
                drop(conn);
            });
        }
    });
    Fake {
        dir,
        socket,
        requests,
    }
}

const STATUS_HISTORY: &str = r#"{"type":"status","model":"pfnet/m","revision":"abc","loaded":true,"uptime_secs":1,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1,"features":["history"],"template_verified":true}"#;
const STATUS_OLD: &str = r#"{"type":"status","model":"pfnet/m","revision":"abc","loaded":true,"uptime_secs":1,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1}"#;
const DONE_STOP: &str = r#"{"type":"done","text":"The meeting","finish_reason":"stop","usage":{"input_tokens":3,"output_tokens":2},"model":"m","revision":"r"}"#;
const STREAM_STOP: [&str; 3] = [STREAM[0], STREAM[1], DONE_STOP];
const DONE_LENGTH: &str = r#"{"type":"done","text":"The meeting","finish_reason":"length","usage":{"input_tokens":3,"output_tokens":2},"model":"m","revision":"r"}"#;
const HISTORY_3: &str = concat!(
    r#"{"source":"一","translation":"One"}"#,
    "\n",
    r#"{"source":"二","translation":"Two"}"#,
    "\n",
    r#"{"source":"三","translation":"Three"}"#,
    "\n",
);

fn write_history(fake: &Fake, content: &str) -> PathBuf {
    let path = fake.dir.path().join("h.jsonl");
    std::fs::write(&path, content).unwrap();
    path
}

fn sources(req: &Value) -> Vec<String> {
    req["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["source"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn history_file_is_sent_after_status_preflight() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &STREAM_STOP]);
    let h = write_history(&f, HISTORY_3);
    cmd(&f)
        .args(["--history", h.to_str().unwrap(), "-t", "en", "会議"])
        .assert()
        .success()
        .stdout("The meeting\n");
    assert_eq!(f.requests.recv().unwrap()["op"], "status");
    let req = f.requests.recv().unwrap();
    assert_eq!(req["op"], "translate");
    assert_eq!(
        req["history"],
        serde_json::json!([
            {"source":"一","translation":"One"},
            {"source":"二","translation":"Two"},
            {"source":"三","translation":"Three"},
        ])
    );
    assert_eq!(req["text"], "会議");
}

#[test]
fn history_with_stdin_text() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &STREAM_STOP]);
    let h = write_history(&f, HISTORY_3);
    cmd(&f)
        .args(["--history", h.to_str().unwrap(), "-t", "en"])
        .write_stdin("会議")
        .assert()
        .success();
    assert_eq!(f.requests.recv().unwrap()["op"], "status");
    let req = f.requests.recv().unwrap();
    assert_eq!(req["text"], "会議");
    assert_eq!(sources(&req), ["一", "二", "三"]);
}

#[test]
fn history_last_sends_tail() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &STREAM_STOP]);
    let h = write_history(&f, HISTORY_3);
    cmd(&f)
        .args([
            "--history",
            h.to_str().unwrap(),
            "--history-last",
            "2",
            "-t",
            "en",
            "会議",
        ])
        .assert()
        .success();
    assert_eq!(f.requests.recv().unwrap()["op"], "status");
    assert_eq!(sources(&f.requests.recv().unwrap()), ["二", "三"]);
}

#[test]
fn history_last_zero_exits_2() {
    let f = fake_daemon_seq(&[]);
    let h = write_history(&f, HISTORY_3);
    cmd(&f)
        .args([
            "--history",
            h.to_str().unwrap(),
            "--history-last",
            "0",
            "-t",
            "en",
            "x",
        ])
        .assert()
        .code(2);
}

#[test]
fn history_flags_require_history_exit_2() {
    let f = fake_daemon_seq(&[]);
    cmd(&f)
        .args(["--history-append", "-t", "en", "x"])
        .assert()
        .code(2);
    cmd(&f)
        .args(["--history-last", "2", "-t", "en", "x"])
        .assert()
        .code(2);
}

#[test]
fn history_dash_exits_2() {
    let f = fake_daemon_seq(&[]);
    cmd(&f)
        .args(["--history", "-", "-t", "en", "x"])
        .assert()
        .code(2)
        .stderr(contains(
            "error: --history does not read stdin (stdin is reserved for TEXT); pass a file path",
        ));
}

#[test]
fn missing_history_file_exits_2() {
    // A queued conversation keeps the request channel open, so try_recv detects a stray connection.
    let f = fake_daemon_seq(&[&[STATUS_HISTORY]]);
    let h = f.dir.path().join("missing.jsonl");
    cmd(&f)
        .args(["--history", h.to_str().unwrap(), "-t", "en", "x"])
        .assert()
        .code(2)
        .stderr(contains(format!(
            "error: cannot read history {}: ",
            h.display()
        )));
    assert!(f.requests.try_recv().is_err());
}

#[test]
fn invalid_history_line_exits_2_without_connecting() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &STREAM_STOP]);
    let h = write_history(
        &f,
        "{\"source\":\"a\",\"translation\":\"b\"}\n{\"source\":\"a\"}\n",
    );
    cmd(&f)
        .args(["--history", h.to_str().unwrap(), "-t", "en", "x"])
        .assert()
        .code(2)
        .stderr(contains(
            "h.jsonl: history line 2: expected {\"source\": string, \"translation\": string}: ",
        ));
    assert!(f.requests.try_recv().is_err());
}

#[test]
fn empty_text_wins_over_bad_history() {
    let f = fake_daemon_seq(&[]);
    let h = write_history(&f, "not json\n");
    cmd(&f)
        .args(["--history", h.to_str().unwrap(), "-t", "en"])
        .write_stdin("")
        .assert()
        .code(2)
        .stderr(contains("error: no text given (pass TEXT or pipe stdin)"))
        .stderr(contains("history line").not());
}

const STALE_MESSAGE: &str =
    "error: daemon does not support translation history; run \"plamo-translate daemon restart\"";

#[test]
fn stale_daemon_with_history_exits_1() {
    let f = fake_daemon_seq(&[&[STATUS_OLD], &[DONE_STOP]]);
    let h = write_history(&f, HISTORY_3);
    cmd(&f)
        .args(["--history", h.to_str().unwrap(), "-t", "en", "x"])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains(STALE_MESSAGE));
    assert_eq!(f.requests.recv().unwrap()["op"], "status");
    assert!(
        f.requests
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err()
    );
}

#[test]
fn history_append_preflights_even_with_missing_file() {
    let f = fake_daemon_seq(&[&[STATUS_OLD], &[DONE_STOP]]);
    let h = f.dir.path().join("h.jsonl");
    cmd(&f)
        .args([
            "--history",
            h.to_str().unwrap(),
            "--history-append",
            "-t",
            "en",
            "会議",
        ])
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains(STALE_MESSAGE));
    assert!(!h.exists());
    assert_eq!(f.requests.recv().unwrap()["op"], "status");
    assert!(
        f.requests
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err()
    );
}

#[test]
fn history_append_writes_eligible_pair() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &STREAM_STOP]);
    let h = f.dir.path().join("h.jsonl");
    cmd(&f)
        .args([
            "--history",
            h.to_str().unwrap(),
            "--history-append",
            "-t",
            "en",
            "会議",
        ])
        .assert()
        .success()
        .stdout("The meeting\n");
    assert_eq!(
        std::fs::read_to_string(&h).unwrap(),
        "{\"source\":\"会議\",\"translation\":\"The meeting\"}\n"
    );
    assert_eq!(f.requests.recv().unwrap()["op"], "status");
    let req = f.requests.recv().unwrap();
    assert_eq!(req["op"], "translate");
    assert!(req.get("history").is_none());
}

#[test]
fn history_append_keeps_stdin_text_verbatim() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &STREAM_STOP]);
    let h = f.dir.path().join("h.jsonl");
    cmd(&f)
        .args([
            "--history",
            h.to_str().unwrap(),
            "--history-append",
            "-t",
            "en",
        ])
        .write_stdin("会議\n")
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(&h).unwrap(),
        "{\"source\":\"会議\\n\",\"translation\":\"The meeting\"}\n"
    );
}

#[test]
fn history_append_with_existing_history_preflights_once() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &STREAM_STOP, &[STATUS_HISTORY]]);
    let h = write_history(&f, HISTORY_3);
    cmd(&f)
        .args([
            "--history",
            h.to_str().unwrap(),
            "--history-append",
            "-t",
            "en",
            "会議",
        ])
        .assert()
        .success();
    assert_eq!(f.requests.recv().unwrap()["op"], "status");
    assert_eq!(f.requests.recv().unwrap()["op"], "translate");
    assert!(
        f.requests
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&h).unwrap(),
        format!("{HISTORY_3}{{\"source\":\"会議\",\"translation\":\"The meeting\"}}\n")
    );
}

#[test]
fn history_append_skips_truncated() {
    let f = fake_daemon_seq(&[&[STATUS_HISTORY], &[DONE_LENGTH]]);
    let h = f.dir.path().join("h.jsonl");
    cmd(&f)
        .args([
            "--history",
            h.to_str().unwrap(),
            "--history-append",
            "--no-stream",
            "-t",
            "en",
            "会議",
        ])
        .assert()
        .code(0)
        .stdout("The meeting\n")
        .stderr(contains(
            "warning: translation not added to history: the translation did not finish normally (finish_reason: length)",
        ));
    assert!(!h.exists());
}
