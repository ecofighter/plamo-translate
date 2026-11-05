use futures::StreamExt;
use plamo_client::{Client, Error, Event, HistoryEntry, TranslateRequest};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

const STATUS_WITH_HISTORY: &str = r#"{"type":"status","model":"m","revision":"r","loaded":true,"uptime_secs":1,"idle_timeout_secs":60,"queue_len":0,"version":"0","protocol":1,"features":["history"]}"#;
const STATUS_WITHOUT_FEATURES: &str = r#"{"type":"status","model":"m","revision":"r","loaded":true,"uptime_secs":1,"idle_timeout_secs":60,"queue_len":0,"version":"0","protocol":1}"#;
const DONE: &str = r#"{"type":"done","text":"ok","usage":{"input_tokens":1,"output_tokens":1},"model":"m","revision":"r"}"#;

struct Fake {
    socket: PathBuf,
    task: JoinHandle<Vec<String>>,
    _dir: tempfile::TempDir,
}

/// Accepts one connection per scripted reply, in order, and returns the request lines it saw.
/// After the script is exhausted it waits 300 ms for a further connection and panics if one arrives.
fn fake_daemon(replies: &'static [&'static str]) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            requests.push(line.trim_end().to_string());
            w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
            w.flush().await.unwrap();
        }
        let extra = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(extra.is_err(), "unexpected extra connection");
        requests
    });
    Fake {
        socket,
        task,
        _dir: dir,
    }
}

fn op(line: &str) -> String {
    let v: Value = serde_json::from_str(line).unwrap();
    v["op"].as_str().unwrap().to_string()
}

fn request(with_history: bool) -> TranslateRequest {
    TranslateRequest {
        text: "hello".into(),
        target_lang: "ja".into(),
        history: if with_history {
            vec![HistoryEntry {
                source: "a".into(),
                translation: "b".into(),
            }]
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}

async fn run(client: &Client, req: &TranslateRequest) {
    let mut events = client.translate(req).await.unwrap();
    let last = events.next().await.unwrap().unwrap();
    assert!(matches!(last, Event::Done { .. }));
}

#[tokio::test]
async fn history_request_fails_without_feature_and_sends_no_translate() {
    let fake = fake_daemon(&[STATUS_WITHOUT_FEATURES]);
    let client = Client::new(fake.socket.clone());
    let err = match client.translate(&request(true)).await {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    };
    assert!(matches!(err, Error::HistoryUnsupported));
    assert_eq!(
        err.to_string(),
        r#"daemon does not support translation history; run "plamo-translate daemon restart""#
    );
    let requests = fake.task.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(op(&requests[0]), "status");
}

#[tokio::test]
async fn history_request_with_feature_sends_status_then_translate() {
    let fake = fake_daemon(&[STATUS_WITH_HISTORY, DONE]);
    let client = Client::new(fake.socket.clone());
    run(&client, &request(true)).await;
    let requests = fake.task.await.unwrap();
    let ops: Vec<String> = requests.iter().map(|l| op(l)).collect();
    assert_eq!(ops, ["status", "translate"]);
    let sent: Value = serde_json::from_str(&requests[1]).unwrap();
    assert_eq!(sent["history"][0]["source"], "a");
}

#[tokio::test]
async fn every_history_request_preflights() {
    let fake = fake_daemon(&[STATUS_WITH_HISTORY, DONE, STATUS_WITH_HISTORY, DONE]);
    let client = Client::new(fake.socket.clone());
    run(&client, &request(true)).await;
    run(&client, &request(true)).await;
    let requests = fake.task.await.unwrap();
    let ops: Vec<String> = requests.iter().map(|l| op(l)).collect();
    assert_eq!(ops, ["status", "translate", "status", "translate"]);
}

#[tokio::test]
async fn no_preflight_without_history() {
    let fake = fake_daemon(&[DONE]);
    let client = Client::new(fake.socket.clone());
    run(&client, &request(false)).await;
    let requests = fake.task.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(op(&requests[0]), "translate");
}
