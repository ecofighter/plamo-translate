use plamo_client::{Client, GlossaryEntry};
use plamo_translate_mcp::PlamoServer;
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{CallToolRequestParams, ClientRequest, ProgressNotificationParam, Request},
    service::{NotificationContext, PeerRequestOptions, RunningService},
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc as tmpsc;

use rmcp::model::Tool;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const WAIT: Duration = Duration::from_secs(5);

struct Fake {
    _dir: TempDir,
    socket: PathBuf,
    requests: mpsc::Receiver<Value>,
    closed: mpsc::Receiver<()>,
}

/// Answers one connection with `responses`, then reports when the client closes it.
fn fake_daemon(responses: &[&str]) -> Fake {
    let dir = tempfile::Builder::new()
        .prefix("pm")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (req_tx, requests) = mpsc::channel();
    let (closed_tx, closed) = mpsc::channel();
    let responses: Vec<String> = responses.iter().map(|s| format!("{s}\n")).collect();
    thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        req_tx.send(serde_json::from_str(&line).unwrap()).unwrap();
        for r in responses {
            conn.write_all(r.as_bytes()).unwrap();
        }
        conn.flush().unwrap();
        let _ = reader.read_to_end(&mut Vec::new());
        let _ = closed_tx.send(());
    });
    Fake {
        _dir: dir,
        socket,
        requests,
        closed,
    }
}

struct TestClient {
    progress: tmpsc::UnboundedSender<ProgressNotificationParam>,
}

impl ClientHandler for TestClient {
    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.progress.send(params);
    }
}

struct Session {
    client: RunningService<RoleClient, TestClient>,
    progress: tmpsc::UnboundedReceiver<ProgressNotificationParam>,
}

async fn connect(server: PlamoServer) -> Session {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.unwrap();
        let _ = running.waiting().await;
    });
    let (tx, progress) = tmpsc::unbounded_channel();
    let client = TestClient { progress: tx }.serve(client_io).await.unwrap();
    Session { client, progress }
}

fn server_for(socket: PathBuf) -> PlamoServer {
    server_with(socket, vec![], Duration::from_secs(30))
}

fn server_with(socket: PathBuf, glossary: Vec<GlossaryEntry>, timeout: Duration) -> PlamoServer {
    let client = Client::new(socket).with_connect_timeout(Duration::from_millis(300));
    PlamoServer::new(client, glossary, timeout, Duration::from_secs(30))
}

fn translate_params(args: Value) -> CallToolRequestParams {
    let Value::Object(map) = args else {
        panic!("arguments must be an object")
    };
    CallToolRequestParams::new("translate").with_arguments(map)
}

async fn call(session: &Session, args: Value) -> Value {
    let result = tokio::time::timeout(WAIT, session.client.call_tool(translate_params(args)))
        .await
        .expect("tool call timed out")
        .unwrap();
    serde_json::to_value(result).unwrap()
}

fn text_of(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    if let Some(r) = schema["$ref"].as_str() {
        let pointer = r.trim_start_matches('#');
        return root.pointer(pointer).unwrap();
    }
    if let Some(alts) = schema["anyOf"].as_array().or(schema["oneOf"].as_array()) {
        for alt in alts {
            let resolved = resolve(root, alt);
            if resolved.get("enum").is_some() {
                return resolved;
            }
        }
    }
    schema
}

const STREAM: [&str; 3] = [
    r#"{"type":"delta","text":"The "}"#,
    r#"{"type":"delta","text":"meeting"}"#,
    r#"{"type":"done","text":"The meeting","usage":{"input_tokens":3,"output_tokens":2},"model":"m","revision":"r"}"#,
];

#[tokio::test]
async fn lists_translate_tool_with_schema() {
    let session = connect(server_for(PathBuf::from("/tmp/pm-unused.sock"))).await;
    let tools = session.client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 2);
    let tool = tool_named(&tools, "translate");
    assert_eq!(tool["name"], "translate");
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("ja(easy)"), "{description}");
    assert!(description.contains("zh-hans"), "{description}");
    assert!(description.contains("translate_document"), "{description}");

    let schema = &tool["inputSchema"];
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"text"), "{required:?}");
    assert!(required.contains(&"target_lang"), "{required:?}");
    let form = resolve(schema, &schema["properties"]["form"]);
    assert_eq!(form["enum"], json!(["polite", "plain"]));
}

#[tokio::test]
async fn call_returns_done_text() {
    let fake = fake_daemon(&STREAM);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(
        &session,
        json!({"text": "会議", "target_lang": "en", "form": "polite"}),
    )
    .await;
    assert_eq!(text_of(&result), "The meeting");
    assert!(!is_error(&result));

    let req = fake.requests.recv_timeout(WAIT).unwrap();
    assert_eq!(req["op"], "translate");
    assert_eq!(req["text"], "会議");
    assert_eq!(req["target_lang"], "en");
    assert_eq!(req["form"], "polite");
    assert_eq!(req["stream"], true);
}

#[tokio::test]
async fn daemon_error_is_tool_error() {
    let fake = fake_daemon(&[r#"{"type":"error","code":"context_too_long","message":"too long"}"#]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(&session, json!({"text": "x", "target_lang": "en"})).await;
    assert!(is_error(&result));
    assert_eq!(text_of(&result), "context_too_long: too long");
}

#[tokio::test]
async fn unreachable_is_tool_error_with_hint() {
    let dir = tempfile::Builder::new()
        .prefix("pm")
        .tempdir_in("/tmp")
        .unwrap();
    let session = connect(server_for(dir.path().join("missing.sock"))).await;
    let result = call(&session, json!({"text": "x", "target_lang": "en"})).await;
    assert!(is_error(&result));
    let text = text_of(&result);
    assert!(text.contains("missing.sock"), "{text}");
    assert!(text.contains("services.plamo-translate.enable"), "{text}");
    assert!(text.contains("plamo-translate daemon install"), "{text}");
}

#[tokio::test]
async fn sends_progress_when_token_given() {
    let fake = fake_daemon(&[
        r#"{"type":"progress","stage":"loading"}"#,
        STREAM[0],
        STREAM[1],
        STREAM[2],
    ]);
    let mut session = connect(server_for(fake.socket.clone())).await;
    let handle = session
        .client
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(translate_params(
                json!({"text": "会議", "target_lang": "en"}),
            ))),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    let token = handle.progress_token.clone();
    tokio::time::timeout(WAIT, handle.await_response())
        .await
        .unwrap()
        .unwrap();
    let first = tokio::time::timeout(WAIT, session.progress.recv())
        .await
        .expect("no progress notification")
        .unwrap();
    assert_eq!(first.progress_token, token);
    assert!(first.message.is_some());
}

#[tokio::test]
async fn timeout_is_tool_error() {
    let fake = fake_daemon(&[]);
    let session = connect(server_with(
        fake.socket.clone(),
        vec![],
        Duration::from_millis(300),
    ))
    .await;
    let result = call(&session, json!({"text": "x", "target_lang": "en"})).await;
    assert!(is_error(&result));
    assert!(
        text_of(&result).contains("timed out"),
        "{}",
        text_of(&result)
    );
    fake.closed
        .recv_timeout(WAIT)
        .expect("daemon connection was not closed after the timeout");
}

// Multi-threaded because the test blocks on the fake daemon while the server must keep running.
#[tokio::test(flavor = "multi_thread")]
async fn cancellation_closes_daemon_connection() {
    let fake = fake_daemon(&[]);
    let session = connect(server_for(fake.socket.clone())).await;
    let handle = session
        .client
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(translate_params(
                json!({"text": "x", "target_lang": "en"}),
            ))),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    fake.requests.recv_timeout(WAIT).unwrap();
    handle.cancel(Some("test".into())).await.unwrap();
    fake.closed
        .recv_timeout(WAIT)
        .expect("daemon connection was not closed after cancellation");
}

#[tokio::test]
async fn config_glossary_merged_with_args() {
    let fake = fake_daemon(&STREAM);
    let entry = |s: &str, t: &str| GlossaryEntry {
        source: s.into(),
        target: t.into(),
    };
    let session = connect(server_with(
        fake.socket.clone(),
        vec![entry("推論", "inference"), entry("会議", "conference")],
        Duration::from_secs(30),
    ))
    .await;
    call(
        &session,
        json!({
            "text": "会議",
            "target_lang": "en",
            "glossary": [{"source": "会議", "target": "meeting"}, {"source": "月曜", "target": "Monday"}]
        }),
    )
    .await;
    let req = fake.requests.recv_timeout(WAIT).unwrap();
    assert_eq!(
        req["glossary"],
        json!([
            {"source": "推論", "target": "inference"},
            {"source": "会議", "target": "meeting"},
            {"source": "月曜", "target": "Monday"}
        ])
    );
}

struct SeqFake {
    _dir: TempDir,
    socket: PathBuf,
    requests: mpsc::Receiver<Value>,
}

/// Accepts connections in order and answers the n-th one with `conversations[n]`.
/// Every request line from every connection goes to the one `requests` channel.
fn fake_daemon_seq(conversations: &[&[&str]]) -> SeqFake {
    let dir = tempfile::Builder::new()
        .prefix("pm")
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
            // keep this connection open until the client drops it while serving the next one
            thread::spawn(move || {
                let _ = reader.read_to_end(&mut Vec::new());
                drop(conn);
            });
        }
    });
    SeqFake {
        _dir: dir,
        socket,
        requests,
    }
}

const STATUS_HISTORY: &str = r#"{"type":"status","model":"pfnet/m","revision":"abc","loaded":true,"uptime_secs":1,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1,"features":["history"],"template_verified":true}"#;
const STATUS_OLD: &str = r#"{"type":"status","model":"pfnet/m","revision":"abc","loaded":true,"uptime_secs":1,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1}"#;
const DONE_STOP: &str = r#"{"type":"done","text":"The meeting","finish_reason":"stop","usage":{"input_tokens":3,"output_tokens":2},"model":"m","revision":"r"}"#;
const DONE_LENGTH: &str = r#"{"type":"done","text":"The meeting","finish_reason":"length","usage":{"input_tokens":3,"output_tokens":2},"model":"m","revision":"r"}"#;
const TOO_LONG: &str = r#"{"type":"error","code":"context_too_long","message":"too long"}"#;

fn pairs(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| json!({"source": format!("s{i}"), "translation": format!("t{i}")}))
            .collect(),
    )
}

fn content_len(result: &Value) -> usize {
    result["content"].as_array().unwrap().len()
}

fn next_request(fake: &SeqFake) -> Value {
    fake.requests.recv_timeout(WAIT).unwrap()
}

#[tokio::test]
async fn schema_has_history_array_of_pairs() {
    let session = connect(server_for(PathBuf::from("/tmp/pm-unused.sock"))).await;
    let tools = session.client.list_all_tools().await.unwrap();
    let tool = serde_json::to_value(&tools[0]).unwrap();
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("history"), "{description}");
    assert!(description.contains("note:"), "{description}");

    let schema = &tool["inputSchema"];
    let history = &schema["properties"]["history"];
    let ty = &history["type"];
    let is_array = ty == "array"
        || ty
            .as_array()
            .is_some_and(|types| types.iter().any(|t| t == "array"));
    assert!(is_array, "{history}");
    let item = resolve(schema, &history["items"]);
    assert_eq!(item["type"], "object", "{item}");
    assert_eq!(item["required"], json!(["source", "translation"]), "{item}");
}

#[tokio::test]
async fn history_is_sent_after_preflight() {
    let fake = fake_daemon_seq(&[&[STATUS_HISTORY], &[DONE_STOP]]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(
        &session,
        json!({"text": "会議", "target_lang": "en", "history": pairs(2)}),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(text_of(&result), "The meeting");
    assert_eq!(content_len(&result), 1, "{result}");

    assert_eq!(next_request(&fake)["op"], "status");
    let req = next_request(&fake);
    assert_eq!(req["op"], "translate");
    assert_eq!(req["history"], pairs(2));
}

#[tokio::test]
async fn empty_history_skips_preflight() {
    let fake = fake_daemon_seq(&[&[DONE_STOP]]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(
        &session,
        json!({"text": "会議", "target_lang": "en", "history": []}),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    let req = next_request(&fake);
    assert_eq!(req["op"], "translate");
    assert!(req.get("history").is_none(), "{req}");
}

#[tokio::test]
async fn stale_daemon_is_tool_error() {
    let fake = fake_daemon_seq(&[&[STATUS_OLD], &[DONE_STOP]]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(
        &session,
        json!({"text": "会議", "target_lang": "en", "history": pairs(1)}),
    )
    .await;
    assert!(is_error(&result), "{result}");
    assert_eq!(
        text_of(&result),
        r#"daemon does not support translation history; run "plamo-translate daemon restart""#
    );
    assert_eq!(next_request(&fake)["op"], "status");
    assert!(
        fake.requests
            .recv_timeout(Duration::from_millis(300))
            .is_err()
    );
}

#[tokio::test]
async fn context_too_long_drops_oldest_half_and_retries() {
    let fake = fake_daemon_seq(&[
        &[STATUS_HISTORY],
        &[TOO_LONG],
        &[STATUS_HISTORY],
        &[TOO_LONG],
        &[DONE_STOP],
    ]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(
        &session,
        json!({"text": "会議", "target_lang": "en", "history": pairs(3)}),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(text_of(&result), "The meeting");
    assert_eq!(
        result["content"][1]["text"],
        "note: the oldest 3 of 3 history pairs were dropped to fit the model's context window; only the last 0 were used."
    );

    assert_eq!(next_request(&fake)["op"], "status");
    let first = next_request(&fake);
    assert_eq!(first["op"], "translate");
    assert_eq!(first["history"], pairs(3));
    assert_eq!(next_request(&fake)["op"], "status");
    let second = next_request(&fake);
    assert_eq!(second["op"], "translate");
    assert_eq!(
        second["history"],
        json!([{"source": "s2", "translation": "t2"}])
    );
    let third = next_request(&fake);
    assert_eq!(third["op"], "translate");
    assert!(third.get("history").is_none(), "{third}");
}

#[tokio::test]
async fn context_too_long_with_history_exhausted_is_error() {
    let fake = fake_daemon_seq(&[&[STATUS_HISTORY], &[TOO_LONG], &[TOO_LONG]]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(
        &session,
        json!({"text": "会議", "target_lang": "en", "history": pairs(1)}),
    )
    .await;
    assert!(is_error(&result), "{result}");
    assert_eq!(
        text_of(&result),
        "context_too_long: too long (all 1 history pairs were dropped and the request still does not fit; shorten text, context or glossary)"
    );
}

#[tokio::test]
async fn truncated_result_has_history_note() {
    let fake = fake_daemon_seq(&[&[DONE_LENGTH]]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(&session, json!({"text": "会議", "target_lang": "en"})).await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(text_of(&result), "The meeting");
    assert_eq!(
        result["content"][1]["text"],
        "note: do not pass this translation as history: the translation did not finish normally (finish_reason: length)."
    );
}

#[tokio::test]
async fn stop_result_has_no_note() {
    let fake = fake_daemon_seq(&[&[DONE_STOP]]);
    let session = connect(server_for(fake.socket.clone())).await;
    let result = call(&session, json!({"text": "会議", "target_lang": "en"})).await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(text_of(&result), "The meeting");
    assert_eq!(content_len(&result), 1, "{result}");
}

#[tokio::test]
async fn every_history_call_preflights() {
    let fake = fake_daemon_seq(&[
        &[STATUS_HISTORY],
        &[DONE_STOP],
        &[STATUS_HISTORY],
        &[DONE_STOP],
    ]);
    let session = connect(server_for(fake.socket.clone())).await;
    for _ in 0..2 {
        let result = call(
            &session,
            json!({"text": "会議", "target_lang": "en", "history": pairs(1)}),
        )
        .await;
        assert!(!is_error(&result), "{result}");
    }
    let ops: Value = (0..4).map(|_| next_request(&fake)["op"].clone()).collect();
    assert_eq!(ops, json!(["status", "translate", "status", "translate"]));
}

fn tool_named(tools: &[Tool], name: &str) -> Value {
    tools
        .iter()
        .map(|t| serde_json::to_value(t).unwrap())
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no tool named {name}"))
}

struct DocFake {
    dir: TempDir,
    socket: PathBuf,
    requests: mpsc::Receiver<Value>,
    closed: mpsc::Receiver<()>,
}

impl DocFake {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.try_iter().collect()
    }
}

/// Accepts connections until the test ends. Each connection reads one request line, forwards
/// it to `requests`, lets the handler write to the connection, then reports on `closed` once
/// the client drops it.
fn fake_daemon_fn(handler: impl Fn(&Value, &mut UnixStream) + Send + Sync + 'static) -> DocFake {
    let dir = tempfile::Builder::new()
        .prefix("pm")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = dir.path().join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, requests) = mpsc::channel();
    let (closed_tx, closed) = mpsc::channel();
    let handler = Arc::new(handler);
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { break };
            let tx = tx.clone();
            let closed_tx = closed_tx.clone();
            let handler = handler.clone();
            thread::spawn(move || {
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let _ = tx.send(request.clone());
                handler(&request, &mut conn);
                let _ = conn.flush();
                let _ = reader.read_to_end(&mut Vec::new());
                let _ = closed_tx.send(());
            });
        }
    });
    DocFake {
        dir,
        socket,
        requests,
        closed,
    }
}

fn send_line(conn: &mut UnixStream, line: &str) -> bool {
    conn.write_all(format!("{line}\n").as_bytes())
        .and_then(|()| conn.flush())
        .is_ok()
}

fn done_line(text: &str, reason: Option<&str>) -> String {
    json!({
        "type": "done",
        "text": text,
        "finish_reason": reason,
        "usage": null,
        "model": "m",
        "revision": "r",
    })
    .to_string()
}

fn delta_line(text: &str) -> String {
    json!({"type": "delta", "text": text}).to_string()
}

/// Translate request number k (from 1) answers "X{k}"; request number `fail_at` answers
/// with a daemon `internal` error instead.
fn echo_failing_at(
    fail_at: Option<usize>,
) -> impl Fn(&Value, &mut UnixStream) + Send + Sync + 'static {
    let count = AtomicUsize::new(0);
    move |req, conn| {
        let line = match req["op"].as_str() {
            Some("status") => STATUS_HISTORY.to_owned(),
            _ => {
                let k = count.fetch_add(1, Ordering::SeqCst) + 1;
                if Some(k) == fail_at {
                    json!({"type": "error", "code": "internal", "message": "boom"}).to_string()
                } else {
                    done_line(&format!("X{k}"), Some("stop"))
                }
            }
        };
        send_line(conn, &line);
    }
}

fn echo() -> impl Fn(&Value, &mut UnixStream) + Send + Sync + 'static {
    echo_failing_at(None)
}

fn para(k: usize) -> String {
    format!("Para{k} {}", "lorem ".repeat(350).trim_end())
}

fn three_paras() -> String {
    let paras: Vec<String> = (1..=3).map(para).collect();
    format!("{}\n", paras.join("\n\n"))
}

fn doc_server(socket: PathBuf, timeout: Duration, doc_timeout: Duration) -> PlamoServer {
    let client = Client::new(socket).with_connect_timeout(Duration::from_millis(300));
    PlamoServer::new(client, vec![], timeout, doc_timeout)
}

fn doc_server_for(fake: &DocFake) -> PlamoServer {
    doc_server(
        fake.socket.clone(),
        Duration::from_secs(30),
        Duration::from_secs(30),
    )
}

fn document_params(args: Value) -> CallToolRequestParams {
    let Value::Object(map) = args else {
        panic!("arguments must be an object")
    };
    CallToolRequestParams::new("translate_document").with_arguments(map)
}

async fn call_document(session: &Session, args: Value) -> Value {
    let result = tokio::time::timeout(WAIT, session.client.call_tool(document_params(args)))
        .await
        .expect("tool call timed out")
        .unwrap();
    serde_json::to_value(result).unwrap()
}

fn block(result: &Value, i: usize) -> &str {
    result["content"][i]["text"].as_str().unwrap()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[tokio::test]
async fn lists_translate_document_with_schema() {
    let session = connect(server_for(PathBuf::from("/tmp/pm-unused.sock"))).await;
    let tools = session.client.list_all_tools().await.unwrap();
    let tool = tool_named(&tools, "translate_document");
    let schema = &tool["inputSchema"];
    assert_eq!(schema["required"], json!(["target_lang"]), "{schema}");
    let format = resolve(schema, &schema["properties"]["format"]);
    assert_eq!(format["enum"], json!(["auto", "text", "markdown"]));
    for name in ["input_path", "output_path", "history_path", "history_bytes"] {
        assert!(schema["properties"].get(name).is_some(), "{name}: {schema}");
    }
}

#[tokio::test]
async fn document_text_mode_returns_translation_and_notes() {
    let fake = fake_daemon_fn(|req, conn| {
        let line = match req["op"].as_str() {
            Some("status") => STATUS_HISTORY.to_owned(),
            // No finish_reason: the piece is translated but cannot serve as history.
            _ => done_line(req["text"].as_str().unwrap(), None),
        };
        send_line(conn, &line);
    });
    let session = connect(doc_server_for(&fake)).await;
    let input = "# Title\n\nSome text.\n\n<div>\nraw html\n</div>\n";
    let result = call_document(&session, json!({"text": input, "target_lang": "en"})).await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(block(&result, 0), input);
    let notes: Vec<&str> = block(&result, 1).lines().collect();
    assert_eq!(notes[0], "note: input format: markdown (auto-detected)");
    assert!(
        notes.contains(&"note: line 5: HTML block left untranslated"),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|l| l.starts_with("warning: piece 1/1: not used as history: ")),
        "{notes:?}"
    );
}

#[tokio::test]
async fn document_file_mode_writes_part_then_renames() {
    let out_dir = tempfile::Builder::new()
        .prefix("pm")
        .tempdir_in("/tmp")
        .unwrap();
    let output = out_dir.path().join("out.md");
    let part = out_dir.path().join("out.md.part");
    let seen_part = Arc::new(AtomicUsize::new(0));
    let echo = echo();
    let (out_c, part_c, seen_c) = (output.clone(), part.clone(), seen_part.clone());
    let fake = fake_daemon_fn(move |req, conn| {
        if req["op"] == "translate" && part_c.exists() && !out_c.exists() {
            seen_c.fetch_add(1, Ordering::SeqCst);
        }
        echo(req, conn);
    });
    let input = fake.path("in.md");
    std::fs::write(&input, three_paras()).unwrap();
    let session = connect(doc_server_for(&fake)).await;
    let result = call_document(
        &session,
        json!({"input_path": path_str(&input), "output_path": path_str(&output), "target_lang": "en"}),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(
        block(&result, 0),
        format!("wrote {} (3 pieces, 11 bytes)", output.display())
    );
    assert_eq!(
        block(&result, 1),
        "note: input format: text (auto-detected)"
    );
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "X1\n\nX2\n\nX3\n"
    );
    assert!(!part.exists());
    assert_eq!(seen_part.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn document_file_mode_refuses_overwrite_without_flag() {
    let fake = fake_daemon_fn(echo());
    let output = fake.path("out.md");
    std::fs::write(&output, "old").unwrap();
    let session = connect(doc_server_for(&fake)).await;
    let result = call_document(
        &session,
        json!({"text": three_paras(), "output_path": path_str(&output), "target_lang": "en"}),
    )
    .await;
    assert!(is_error(&result), "{result}");
    assert!(
        text_of(&result).starts_with("output_path exists; pass overwrite=true to replace it"),
        "{result}"
    );
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "old");
    assert!(!fake.path("out.md.part").exists());
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn document_file_mode_overwrite_replaces() {
    let fake = fake_daemon_fn(echo());
    let output = fake.path("out.md");
    std::fs::write(&output, "old").unwrap();
    let session = connect(doc_server_for(&fake)).await;
    let result = call_document(
        &session,
        json!({
            "text": three_paras(),
            "output_path": path_str(&output),
            "overwrite": true,
            "target_lang": "en"
        }),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "X1\n\nX2\n\nX3\n"
    );
    assert!(!fake.path("out.md.part").exists());
}

#[tokio::test]
async fn document_stop_leaves_part_file() {
    let fake = fake_daemon_fn(echo_failing_at(Some(2)));
    let output = fake.path("out.md");
    let session = connect(doc_server_for(&fake)).await;
    let result = call_document(
        &session,
        json!({"text": three_paras(), "output_path": path_str(&output), "target_lang": "en"}),
    )
    .await;
    assert!(is_error(&result), "{result}");
    assert_eq!(
        text_of(&result),
        format!(
            "internal: boom; stopped at piece 2/3; the pieces before it are in {}.part",
            output.display()
        )
    );
    assert_eq!(
        std::fs::read_to_string(fake.path("out.md.part")).unwrap(),
        "X1\n\n"
    );
    assert!(!output.exists());
}

#[tokio::test]
async fn document_text_mode_stop_returns_partial_in_second_block() {
    let fake = fake_daemon_fn(echo_failing_at(Some(2)));
    let session = connect(doc_server_for(&fake)).await;
    let result = call_document(
        &session,
        json!({"text": three_paras(), "target_lang": "en"}),
    )
    .await;
    assert!(is_error(&result), "{result}");
    assert_eq!(
        text_of(&result),
        "internal: boom; stopped at piece 2/3; the 1 pieces before it follow in the next content block; to continue, call again with the untranslated remainder"
    );
    assert_eq!(block(&result, 1), "X1\n\n");
}

#[tokio::test]
async fn document_rejects_relative_paths() {
    let fake = fake_daemon_fn(echo());
    let session = connect(doc_server_for(&fake)).await;
    let cases = [
        (
            json!({"input_path": "in.md", "target_lang": "en"}),
            "invalid arguments: input_path must be absolute",
        ),
        (
            json!({"text": "Hi.", "output_path": "out.md", "target_lang": "en"}),
            "invalid arguments: output_path must be absolute",
        ),
        (
            json!({"text": "Hi.", "history_path": "h.jsonl", "target_lang": "en"}),
            "invalid arguments: history_path must be absolute",
        ),
        (
            json!({"text": "Hi.", "history_path": "-", "target_lang": "en"}),
            "invalid arguments: history_path must be absolute",
        ),
        (
            json!({"text": "Hi.", "history_append": true, "target_lang": "en"}),
            "invalid arguments: history_append requires history_path",
        ),
    ];
    for (args, expected) in cases {
        let result = call_document(&session, args.clone()).await;
        assert!(is_error(&result), "{args}: {result}");
        assert!(text_of(&result).starts_with(expected), "{args}: {result}");
    }
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn document_requires_text_xor_input_path() {
    let fake = fake_daemon_fn(echo());
    let input = fake.path("in.md");
    std::fs::write(&input, "Hi.\n").unwrap();
    let session = connect(doc_server_for(&fake)).await;
    for args in [
        json!({"target_lang": "en"}),
        json!({"text": "Hi.", "input_path": path_str(&input), "target_lang": "en"}),
    ] {
        let result = call_document(&session, args.clone()).await;
        assert!(is_error(&result), "{args}: {result}");
        assert!(
            text_of(&result)
                .starts_with("invalid arguments: pass exactly one of text or input_path"),
            "{args}: {result}"
        );
    }
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn document_rejects_same_input_and_output() {
    let fake = fake_daemon_fn(echo());
    std::fs::create_dir(fake.path("sub")).unwrap();
    let input = fake.path("in.md");
    std::fs::write(&input, "Hi.\n").unwrap();
    let session = connect(doc_server_for(&fake)).await;
    for output in [input.clone(), fake.path("sub/../in.md")] {
        let result = call_document(
            &session,
            json!({
                "input_path": path_str(&input),
                "output_path": path_str(&output),
                "overwrite": true,
                "target_lang": "en"
            }),
        )
        .await;
        assert!(is_error(&result), "{result}");
        assert!(
            text_of(&result)
                .starts_with("invalid arguments: output_path must differ from input_path"),
            "{result}"
        );
    }
    assert_eq!(std::fs::read_to_string(&input).unwrap(), "Hi.\n");
    assert!(!fake.path("in.md.part").exists());
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn document_progress_counts_pieces() {
    let fake = fake_daemon_fn(echo());
    let mut session = connect(doc_server_for(&fake)).await;
    let handle = session
        .client
        .send_cancellable_request(
            ClientRequest::CallToolRequest(Request::new(document_params(
                json!({"text": three_paras(), "target_lang": "en"}),
            ))),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    tokio::time::timeout(WAIT, handle.await_response())
        .await
        .unwrap()
        .unwrap();
    let mut seen = Vec::new();
    while let Ok(Some(p)) =
        tokio::time::timeout(Duration::from_millis(300), session.progress.recv()).await
    {
        seen.push(p);
    }
    assert!(!seen.is_empty());
    assert_eq!(seen[0].progress, 0.0);
    assert_eq!(seen[0].total, Some(3.0));
    assert_eq!(seen[0].message.as_deref(), Some("piece 1/3"));
    for pair in seen.windows(2) {
        assert!(pair[1].progress > pair[0].progress, "{seen:?}");
    }
    assert!(seen.iter().any(|p| p.progress >= 1.0), "{seen:?}");
}

#[tokio::test]
async fn document_event_timeout_fires() {
    let fake = fake_daemon_fn(|_, _| {});
    let session = connect(doc_server(
        fake.socket.clone(),
        Duration::from_millis(300),
        Duration::from_secs(30),
    ))
    .await;
    let result = call_document(
        &session,
        json!({"text": "Hello.", "target_lang": "ja", "history_bytes": 0}),
    )
    .await;
    assert!(is_error(&result), "{result}");
    assert!(
        text_of(&result).contains("no event from the daemon"),
        "{result}"
    );
    fake.closed
        .recv_timeout(WAIT)
        .expect("daemon connection was not closed after the timeout");
}

#[tokio::test]
async fn document_event_timeout_resets_on_activity() {
    let fake = fake_daemon_fn(|_, conn| {
        for _ in 0..7 {
            thread::sleep(Duration::from_millis(150));
            send_line(conn, &delta_line("x"));
        }
        send_line(conn, &done_line("こんにちは。", Some("stop")));
    });
    let session = connect(doc_server(
        fake.socket.clone(),
        Duration::from_millis(300),
        Duration::from_secs(30),
    ))
    .await;
    let result = call_document(
        &session,
        json!({"text": "Hello.", "target_lang": "ja", "history_bytes": 0}),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(text_of(&result), "こんにちは。");
}

#[tokio::test]
async fn document_total_timeout_fires() {
    let fake = fake_daemon_fn(|_, conn| {
        while send_line(conn, &delta_line("x")) {
            thread::sleep(Duration::from_millis(50));
        }
    });
    let session = connect(doc_server(
        fake.socket.clone(),
        Duration::from_secs(30),
        Duration::from_millis(300),
    ))
    .await;
    let result = call_document(
        &session,
        json!({"text": "Hello.", "target_lang": "ja", "history_bytes": 0}),
    )
    .await;
    assert!(is_error(&result), "{result}");
    assert!(text_of(&result).contains("timed out after"), "{result}");
    fake.closed
        .recv_timeout(WAIT)
        .expect("daemon connection was not closed after the timeout");
}

#[tokio::test]
async fn document_history_path_append_records_pieces() {
    let fake = fake_daemon_fn(echo());
    let history = fake.path("h.jsonl");
    let session = connect(doc_server_for(&fake)).await;
    let result = call_document(
        &session,
        json!({
            "text": three_paras(),
            "target_lang": "en",
            "history_path": path_str(&history),
            "history_append": true
        }),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(text_of(&result), "X1\n\nX2\n\nX3\n");
    let entries: Vec<Value> = std::fs::read_to_string(&history)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(entries.len(), 3);
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(e["source"], para(i + 1));
        assert_eq!(e["translation"], format!("X{}", i + 1));
    }
    let requests = fake.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0]["op"], "status");
}

#[tokio::test]
async fn document_history_bytes_zero_skips_preflight() {
    let fake = fake_daemon_fn(echo());
    let session = connect(doc_server_for(&fake)).await;
    let result = call_document(
        &session,
        json!({"text": three_paras(), "target_lang": "en", "history_bytes": 0}),
    )
    .await;
    assert!(!is_error(&result), "{result}");
    assert_eq!(text_of(&result), "X1\n\nX2\n\nX3\n");
    let requests = fake.requests();
    assert_eq!(requests.len(), 3);
    for r in &requests {
        assert_eq!(r["op"], "translate");
        assert!(r.get("history").is_none(), "{r}");
    }
}
