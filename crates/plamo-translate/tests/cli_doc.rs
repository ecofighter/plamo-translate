use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use tempfile::TempDir;

struct Fake {
    dir: TempDir,
    socket: PathBuf,
    requests: mpsc::Receiver<Value>,
}

impl Fake {
    fn all_requests(&self) -> Vec<Value> {
        self.requests.try_iter().collect()
    }

    fn translates(&self) -> Vec<Value> {
        self.all_requests()
            .into_iter()
            .filter(|r| r["op"] == "translate")
            .collect()
    }
}

/// Accepts connections until the test ends. Each connection reads one request line, forwards
/// it to `requests`, writes the handler's lines, then holds the connection until the client
/// drops it.
fn fake_daemon_fn(handler: impl Fn(&Value) -> Vec<String> + Send + Sync + 'static) -> Fake {
    let dir = tempfile::Builder::new()
        .prefix("pt")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, requests) = mpsc::channel();
    let handler = std::sync::Arc::new(handler);
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let tx = tx.clone();
            let handler = handler.clone();
            thread::spawn(move || {
                let mut line = String::new();
                BufReader::new(conn.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let lines = handler(&request);
                let _ = tx.send(request);
                for l in lines {
                    conn.write_all(l.as_bytes()).unwrap();
                    conn.write_all(b"\n").unwrap();
                }
                conn.flush().unwrap();
                let _ = BufReader::new(conn).read_line(&mut String::new());
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

fn status_line(kind: &str) -> String {
    match kind {
        "history" => STATUS_HISTORY.to_owned(),
        _ => STATUS_OLD.to_owned(),
    }
}

fn done(text: &str, reason: &str) -> String {
    serde_json::json!({
        "type": "done",
        "text": text,
        "finish_reason": reason,
        "usage": null,
        "model": "m",
        "revision": "r",
    })
    .to_string()
}

fn error_line(code: &str, message: &str) -> String {
    serde_json::json!({"type": "error", "code": code, "message": message}).to_string()
}

/// Translate request number k (from 1) answers "X{k}".
fn echo(status: &'static str) -> impl Fn(&Value) -> Vec<String> {
    let count = AtomicUsize::new(0);
    move |req| match req["op"].as_str() {
        Some("status") => vec![status_line(status)],
        _ => {
            let k = count.fetch_add(1, Ordering::SeqCst) + 1;
            vec![done(&format!("X{k}"), "stop")]
        }
    }
}

/// Translate requests answer with their own text.
fn identity(status: &'static str) -> impl Fn(&Value) -> Vec<String> {
    move |req| match req["op"].as_str() {
        Some("status") => vec![status_line(status)],
        _ => vec![done(req["text"].as_str().unwrap(), "stop")],
    }
}

fn cmd(fake: &Fake) -> Command {
    let mut c = Command::cargo_bin("plamo-translate").unwrap();
    c.env("PLAMO_TRANSLATE_SOCKET", &fake.socket)
        .env("PLAMO_TRANSLATE_CONFIG", fake.dir.path().join("none.toml"))
        .env("PLAMO_TRANSLATE_CONNECT_TIMEOUT_MS", "300");
    c
}

fn para(k: usize) -> String {
    format!("Para{k} {}", "lorem ".repeat(350).trim_end())
}

fn three_paras() -> String {
    let paras: Vec<String> = (1..=3).map(para).collect();
    format!("{}\n", paras.join("\n\n"))
}

fn history_sources(req: &Value) -> Vec<String> {
    req["history"]
        .as_array()
        .map(|h| {
            h.iter()
                .map(|e| e["source"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn write_file(fake: &Fake, name: &str, content: &str) -> PathBuf {
    let path = fake.dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    path
}

const HISTORY_3: &str = concat!(
    r#"{"source":"一","translation":"One"}"#,
    "\n",
    r#"{"source":"二","translation":"Two"}"#,
    "\n",
    r#"{"source":"三","translation":"Three"}"#,
    "\n",
);

#[test]
fn doc_driver_translates_pieces_in_order_with_history() {
    let f = fake_daemon_fn(echo("history"));
    cmd(&f)
        .args(["doc", "-t", "en"])
        .write_stdin(three_paras())
        .assert()
        .success()
        .stdout("X1\n\nX2\n\nX3\n");
    let requests = f.all_requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0]["op"], "status");
    for r in &requests[1..] {
        assert_eq!(r["op"], "translate");
        assert_eq!(r["stream"], true);
        assert_eq!(r["target_lang"], "en");
        assert!(r["max_tokens"].is_u64(), "{r}");
    }
    assert_eq!(requests[1]["history"], Value::Null);
    assert_eq!(history_sources(&requests[3]).len(), 2);
}

#[test]
fn doc_driver_auto_target_is_decided_once_for_the_document() {
    let f = fake_daemon_fn(echo("history"));
    let text = format!("{}\n\n{}\n", "会議".repeat(400), "会議".repeat(400));
    cmd(&f).args(["doc"]).write_stdin(text).assert().success();
    let translates = f.translates();
    assert_eq!(translates.len(), 2);
    for r in &translates {
        assert_eq!(r["target_lang"], "en");
    }
}

#[test]
fn doc_driver_history_bytes_zero_skips_the_preflight_and_history() {
    let f = fake_daemon_fn(echo("history"));
    cmd(&f)
        .args(["doc", "-t", "en", "--history-bytes", "0"])
        .write_stdin(three_paras())
        .assert()
        .success()
        .stdout("X1\n\nX2\n\nX3\n");
    let requests = f.all_requests();
    assert_eq!(requests.len(), 3);
    for r in &requests {
        assert_eq!(r["op"], "translate");
        assert!(r.get("history").is_none(), "{r}");
    }
}

#[test]
fn doc_driver_history_last_trims_the_seed() {
    let f = fake_daemon_fn(echo("history"));
    let h = write_file(&f, "h.jsonl", HISTORY_3);
    cmd(&f)
        .args([
            "doc",
            "-t",
            "en",
            "--history",
            h.to_str().unwrap(),
            "--history-last",
            "1",
        ])
        .write_stdin(three_paras())
        .assert()
        .success();
    let translates = f.translates();
    assert_eq!(history_sources(&translates[0]), vec!["三"]);
}

#[test]
fn doc_driver_history_append_records_each_piece() {
    let f = fake_daemon_fn(echo("history"));
    let h = f.dir.path().join("new.jsonl");
    cmd(&f)
        .args([
            "doc",
            "-t",
            "en",
            "--history",
            h.to_str().unwrap(),
            "--history-append",
        ])
        .write_stdin(three_paras())
        .assert()
        .success();
    let content = std::fs::read_to_string(&h).unwrap();
    let entries: Vec<Value> = content
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(entries.len(), 3);
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(e["source"], para(i + 1).trim_end());
        assert_eq!(e["translation"], format!("X{}", i + 1));
    }
}

#[test]
fn doc_driver_verbatim_blocks_are_never_sent_and_output_is_byte_exact() {
    let f = fake_daemon_fn(identity("history"));
    let input = "Intro text here.\n\n```\nsecret code\n```\n\nOutro text here.\n";
    cmd(&f)
        .args(["doc", "-t", "ja", "--format", "markdown"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(input);
    let translates = f.translates();
    assert_eq!(translates.len(), 1);
    let text = translates[0]["text"].as_str().unwrap();
    assert!(text.contains("<!--pt:k1-->"), "{text}");
    assert!(!text.contains("secret code"), "{text}");
}

#[test]
fn doc_driver_daemon_error_stops_with_whole_pieces_on_stdout() {
    let count = AtomicUsize::new(0);
    let f = fake_daemon_fn(move |req| match req["op"].as_str() {
        Some("status") => vec![status_line("history")],
        _ => match count.fetch_add(1, Ordering::SeqCst) + 1 {
            2 => vec![error_line("internal", "boom")],
            k => vec![done(&format!("X{k}"), "stop")],
        },
    });
    cmd(&f)
        .args(["doc", "-t", "en"])
        .write_stdin(three_paras())
        .assert()
        .code(1)
        .stdout("X1\n\n")
        .stderr(contains("error: internal: boom").and(contains("stopped at piece 2/3")));
}

#[test]
fn doc_driver_stale_daemon_fails_before_any_output() {
    let f = fake_daemon_fn(echo("old"));
    cmd(&f)
        .args(["doc", "-t", "en"])
        .write_stdin(three_paras())
        .assert()
        .code(1)
        .stdout("")
        .stderr(contains("daemon does not support translation history"));
    let requests = f.all_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["op"], "status");
}

#[test]
fn doc_driver_unreachable_daemon_exits_3_with_install_hint() {
    let dir = TempDir::new().unwrap();
    Command::cargo_bin("plamo-translate")
        .unwrap()
        .env("PLAMO_TRANSLATE_SOCKET", dir.path().join("nope.sock"))
        .env("PLAMO_TRANSLATE_CONFIG", dir.path().join("none.toml"))
        .env("PLAMO_TRANSLATE_CONNECT_TIMEOUT_MS", "300")
        .args(["doc", "-t", "en"])
        .write_stdin(three_paras())
        .assert()
        .code(3)
        .stderr(contains("plamo-translate daemon install"));
}

#[test]
fn doc_driver_nothing_to_translate_echoes_input_without_the_daemon() {
    let dir = TempDir::new().unwrap();
    let input = "```\nlet x = 1;\n```\n";
    Command::cargo_bin("plamo-translate")
        .unwrap()
        .env("PLAMO_TRANSLATE_SOCKET", dir.path().join("nope.sock"))
        .env("PLAMO_TRANSLATE_CONFIG", dir.path().join("none.toml"))
        .env("PLAMO_TRANSLATE_CONNECT_TIMEOUT_MS", "300")
        .args(["doc", "-t", "en", "--format", "markdown"])
        .write_stdin(input)
        .assert()
        .success()
        .stdout(input);
}

#[test]
fn doc_driver_context_too_long_retries_with_half_the_history() {
    let count = AtomicUsize::new(0);
    let f = fake_daemon_fn(move |req| match req["op"].as_str() {
        Some("status") => vec![status_line("history")],
        _ => match count.fetch_add(1, Ordering::SeqCst) + 1 {
            2 => vec![error_line("context_too_long", "too long")],
            k => vec![done(&format!("X{k}"), "stop")],
        },
    });
    let h = write_file(
        &f,
        "h.jsonl",
        concat!(
            r#"{"source":"一","translation":"One"}"#,
            "\n",
            r#"{"source":"二","translation":"Two"}"#,
            "\n",
        ),
    );
    cmd(&f)
        .args(["doc", "-t", "en", "--history", h.to_str().unwrap()])
        .write_stdin(three_paras())
        .assert()
        .success()
        .stdout("X1\n\nX3\n\nX4\n");
    let translates = f.translates();
    let first = history_sources(&translates[1]);
    let retry = history_sources(&translates[2]);
    assert_eq!(first.len(), 3);
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0], first[2]);
    assert_eq!(translates[1]["text"], translates[2]["text"]);
}

#[test]
fn doc_driver_usage_errors_exit_2() {
    let f = fake_daemon_fn(echo("history"));
    cmd(&f)
        .args(["doc", "-t", "en"])
        .write_stdin("")
        .assert()
        .code(2)
        .stderr(contains(
            "error: no text given (pipe the document to stdin)",
        ));
    cmd(&f)
        .args(["doc", "--json"])
        .write_stdin("text")
        .assert()
        .code(2);
    cmd(&f)
        .args(["doc", "x"])
        .write_stdin("text")
        .assert()
        .code(2);
    cmd(&f)
        .args(["doc", "--history", "-"])
        .write_stdin("text")
        .assert()
        .code(2)
        .stderr(contains(
            "error: --history does not read stdin (stdin is reserved for the document); pass a file path",
        ));
    cmd(&f)
        .args(["doc", "--history-append"])
        .write_stdin("text")
        .assert()
        .code(2);
    assert!(f.all_requests().is_empty());
}

#[test]
fn doc_driver_length_truncation_splits_the_piece() {
    let count = AtomicUsize::new(0);
    let f = fake_daemon_fn(move |req| match req["op"].as_str() {
        Some("status") => vec![status_line("history")],
        _ => match count.fetch_add(1, Ordering::SeqCst) + 1 {
            2 => vec![done("cut off", "length")],
            k => vec![done(&format!("X{k}"), "stop")],
        },
    });
    let sentences = "Short sentence here. ".repeat(100);
    let input = format!("{}\n\n{}\n\n{}\n", para(1), sentences.trim_end(), para(3));
    cmd(&f)
        .args(["doc", "-t", "en"])
        .write_stdin(input)
        .assert()
        .success();
    assert!(f.translates().len() > 3);
}

#[test]
fn doc_driver_auto_format_prints_a_note() {
    let input = "# Title\n\nBody text.\n";
    let f = fake_daemon_fn(echo("history"));
    cmd(&f)
        .args(["doc", "-t", "en"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(contains(
            "note: input format: markdown (auto-detected; use --format to override)",
        ));
    cmd(&f)
        .args(["doc", "-t", "en", "--format", "markdown"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(contains("note:").not());
}
