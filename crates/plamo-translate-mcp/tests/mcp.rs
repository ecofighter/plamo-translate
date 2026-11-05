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
    PlamoServer::new(client, glossary, timeout)
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
    assert_eq!(tools.len(), 1);
    let tool = serde_json::to_value(&tools[0]).unwrap();
    assert_eq!(tool["name"], "translate");
    let description = tool["description"].as_str().unwrap();
    assert!(description.contains("ja(easy)"), "{description}");
    assert!(description.contains("zh-hans"), "{description}");

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
