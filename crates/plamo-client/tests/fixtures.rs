use futures::StreamExt;
use plamo_client::{Client, Error, Event, TranslateRequest};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

struct Fixture {
    name: String,
    request: Value,
    event_lines: Vec<String>,
}

fn fixtures() -> Vec<Fixture> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/fixtures");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("ndjson") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        out.push(Fixture {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            request: serde_json::from_str(lines[1]).unwrap(),
            event_lines: lines[2..].iter().map(|s| s.to_string()).collect(),
        });
    }
    assert!(!out.is_empty());
    out
}

fn concretize(v: &mut Value) {
    match v {
        Value::String(s) if s == "<any>" => *s = "x".into(),
        Value::Object(m) => m.values_mut().for_each(concretize),
        Value::Array(a) => a.iter_mut().for_each(concretize),
        _ => {}
    }
}

fn concrete_events(f: &Fixture) -> (Vec<String>, Vec<Event>) {
    // Numeric fields cannot take "x": replace those placeholders with 0 first.
    let numeric = [
        "done_bytes",
        "total_bytes",
        "uptime_secs",
        "idle_timeout_secs",
        "queue_len",
    ];
    let mut lines = Vec::new();
    let mut events = Vec::new();
    for l in &f.event_lines {
        let mut v: Value = serde_json::from_str(l).unwrap();
        if let Value::Object(m) = &mut v {
            for k in numeric {
                if m.get(k).is_some_and(|x| x == "<any>") {
                    m.insert(k.into(), 0.into());
                }
            }
            if m.get("loaded").is_some_and(|x| x == "<any>") {
                m.insert("loaded".into(), true.into());
            }
        }
        concretize(&mut v);
        let line = v.to_string();
        events.push(
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{}: {e}: {line}", f.name)),
        );
        lines.push(line);
    }
    (lines, events)
}

#[test]
fn every_fixture_event_line_deserializes_and_ends_terminal() {
    for f in fixtures() {
        let (_, events) = concrete_events(&f);
        assert!(!events.is_empty(), "{}", f.name);
        assert!(events.last().unwrap().is_terminal(), "{}", f.name);
    }
}

fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("pt")
        .tempdir_in("/tmp")
        .unwrap()
}

async fn serve_once(
    listener: UnixListener,
    reply: Vec<String>,
    close_after: bool,
) -> tokio::task::JoinHandle<String> {
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut request = String::new();
        BufReader::new(r).read_line(&mut request).await.unwrap();
        for l in reply {
            w.write_all(l.as_bytes()).await.unwrap();
            w.write_all(b"\n").await.unwrap();
        }
        w.flush().await.unwrap();
        if close_after {
            drop(w);
        }
        request
    })
}

#[tokio::test]
async fn client_yields_fixture_events() {
    let mut ran = 0;
    for f in fixtures() {
        let op = f.request.get("op").and_then(Value::as_str);
        if f.request.get("raw").is_some() || f.request.get("v") != Some(&Value::from(1)) {
            continue;
        }
        let dir = short_dir();
        let sock = dir.path().join("d.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let (lines, expected) = concrete_events(&f);
        let server = serve_once(listener, lines, true).await;
        let client = Client::new(sock);

        let got: Vec<Event> = match op {
            Some("status") => vec![Event::Status(client.status().await.unwrap())],
            other => {
                let stream = if other == Some("load") {
                    client.load().await.unwrap()
                } else {
                    let req = TranslateRequest {
                        text: f.request["text"].as_str().unwrap_or_default().into(),
                        target_lang: f.request["target_lang"].as_str().unwrap_or_default().into(),
                        stream: f
                            .request
                            .get("stream")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                        max_tokens: f
                            .request
                            .get("max_tokens")
                            .and_then(Value::as_u64)
                            .map(|n| n as u32),
                        ..Default::default()
                    };
                    client.translate(&req).await.unwrap()
                };
                stream.map(|e| e.unwrap()).collect().await
            }
        };
        assert_eq!(got, expected, "{}", f.name);
        let sent: Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert_eq!(sent["op"], f.request["op"], "{}", f.name);
        ran += 1;
    }
    assert!(ran >= 5);
}

#[tokio::test]
async fn stream_ends_with_none_after_terminal_even_if_server_keeps_open() {
    let dir = short_dir();
    let sock = dir.path().join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let server = serve_once(
        listener,
        vec![r#"{"type":"done","text":"a"}"#.into()],
        false,
    )
    .await;
    let mut s = Client::new(sock).load().await.unwrap();
    assert!(matches!(s.next().await, Some(Ok(Event::Done { .. }))));
    assert!(s.next().await.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn unreachable_socket_is_daemon_unreachable() {
    let dir = short_dir();
    let sock = dir.path().join("missing.sock");
    let client = Client::new(sock.clone()).with_connect_timeout(Duration::from_millis(200));
    match client.status().await {
        Err(Error::DaemonUnreachable {
            socket_path,
            source,
        }) => {
            assert_eq!(socket_path, sock);
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn connect_retries_until_socket_appears() {
    let dir = short_dir();
    let sock = dir.path().join("late.sock");
    let late = sock.clone();
    let server = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let listener = UnixListener::bind(&late).unwrap();
        serve_once(listener, vec![r#"{"type":"done","text":""}"#.into()], true)
            .await
            .await
            .unwrap();
    });
    let mut s = Client::new(sock).load().await.unwrap();
    assert!(matches!(s.next().await, Some(Ok(Event::Done { .. }))));
    server.await.unwrap();
}

#[tokio::test]
async fn eof_before_terminal_is_protocol_error() {
    let dir = short_dir();
    let sock = dir.path().join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let server = serve_once(
        listener,
        vec![r#"{"type":"delta","text":"a"}"#.into()],
        true,
    )
    .await;
    let mut s = Client::new(sock).load().await.unwrap();
    assert!(matches!(s.next().await, Some(Ok(Event::Delta { .. }))));
    match s.next().await {
        Some(Err(Error::Protocol(m))) => assert_eq!(m, "connection closed before terminal event"),
        other => panic!("unexpected: {other:?}"),
    }
    assert!(s.next().await.is_none());
    server.await.unwrap();
}
