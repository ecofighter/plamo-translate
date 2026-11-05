use futures::StreamExt;
use plamo_client::{
    Client, Config, Error, ErrorCode, Event, GlossaryEntry, HistoryEntry, TranslateRequest,
    check_history_eligible, drop_oldest_half, merge_glossaries, parse_glossary,
};
use rmcp::{
    Peer, RoleServer, ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken},
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::time::Duration;
use tokio::time::Instant;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub const TIMEOUT_ENV: &str = "PLAMO_TRANSLATE_MCP_TIMEOUT_SECS";

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

const TOOL_DESCRIPTION: &str = "Translate text with the local PLaMo-3-translate model. \
target_lang examples: ja, en, zh-hans, zh-hant, ko, fr, de, es, ja(easy) (plain Japanese for learners). \
form (polite | plain) is only valid when target_lang=ja. \
glossary entries ({source, target}) are applied on top of the user's configured glossary_file. \
For a document translated in several calls, pass the earlier parts as history ({source, translation}, oldest first, translation exactly as this tool returned it) to keep terms and style consistent. \
If the prompt would exceed the model's context window, the oldest history pairs are dropped automatically. \
A second content block starting with \"note:\" is not part of the translation; follow it.";

pub struct PlamoServer {
    client: Client,
    config_glossary: Vec<GlossaryEntry>,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Form {
    Polite,
    Plain,
}

impl Form {
    fn as_str(self) -> &'static str {
        match self {
            Form::Polite => "polite",
            Form::Plain => "plain",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TranslateArgs {
    /// Text to translate.
    text: String,
    /// Target language code, e.g. ja, en, zh-hans, zh-hant, ko, fr, de, es, ja(easy).
    target_lang: String,
    /// Source language code. Detected by the model when omitted.
    source_lang: Option<String>,
    /// Surrounding text that helps disambiguate the translation; it is not translated.
    context: Option<String>,
    /// Term pairs to enforce. These override the configured glossary_file on conflicts.
    glossary: Option<Vec<GlossaryEntry>>,
    /// Japanese register: polite (desu/masu) or plain. Only valid when target_lang=ja.
    form: Option<Form>,
    /// Earlier parts of the same document and their translations, oldest first.
    /// Pass each translation exactly as this tool returned it.
    history: Option<Vec<HistoryEntry>>,
}

enum Attempt {
    Done {
        text: String,
        finish_reason: Option<String>,
    },
    ContextTooLong(String),
    Failed(CallToolResult),
}

impl PlamoServer {
    pub fn new(client: Client, config_glossary: Vec<GlossaryEntry>, timeout: Duration) -> Self {
        Self {
            client,
            config_glossary,
            timeout,
        }
    }

    /// Builds the server from the same config file and socket resolution as the CLI.
    pub fn from_env() -> Result<Self, String> {
        let config = Config::load().map_err(|e| e.to_string())?;
        let config_glossary = match &config.glossary_file {
            Some(path) if path.exists() => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read glossary {}: {e}", path.display()))?;
                parse_glossary(&text).map_err(|e| format!("{}: {e}", path.display()))?
            }
            _ => Vec::new(),
        };
        let timeout = parse_timeout(std::env::var(TIMEOUT_ENV).ok().as_deref())?;
        Ok(Self::new(
            Client::new(config.socket_path),
            config_glossary,
            timeout,
        ))
    }

    fn build_request(&self, args: TranslateArgs) -> TranslateRequest {
        let glossary = merge_glossaries([
            self.config_glossary.clone(),
            args.glossary.unwrap_or_default(),
        ]);
        TranslateRequest {
            text: args.text,
            target_lang: args.target_lang,
            source_lang: args.source_lang,
            context: args.context,
            history: args.history.unwrap_or_default(),
            glossary,
            form: args.form.map(|f| f.as_str().to_string()),
            stream: true,
            max_tokens: None,
        }
    }

    /// Retries `context_too_long` with the oldest half of the history dropped each time,
    /// keeping `max_tokens` as given, until the request fits or the history is empty.
    async fn run(
        &self,
        mut req: TranslateRequest,
        mut progress: Option<Progress>,
    ) -> CallToolResult {
        let supplied = req.history.len();
        loop {
            match self.attempt(&req, &mut progress).await {
                Attempt::Done {
                    text,
                    finish_reason,
                } => {
                    return success_with_notes(
                        text,
                        finish_reason.as_deref(),
                        supplied,
                        req.history.len(),
                    );
                }
                Attempt::ContextTooLong(m) if req.history.is_empty() => {
                    return error_result(context_too_long_text(&m, supplied));
                }
                Attempt::ContextTooLong(_) => {
                    drop_oldest_half(&mut req.history);
                }
                Attempt::Failed(r) => return r,
            }
        }
    }

    async fn attempt(&self, req: &TranslateRequest, progress: &mut Option<Progress>) -> Attempt {
        let failed = |message: String| Attempt::Failed(error_result(message));
        let mut events = match self.client.translate(req).await {
            Ok(events) => events,
            Err(e) => return failed(describe_client_error(&e)),
        };
        while let Some(event) = events.next().await {
            match event {
                Ok(Event::Done {
                    text,
                    finish_reason,
                    ..
                }) => {
                    return Attempt::Done {
                        text,
                        finish_reason,
                    };
                }
                Ok(Event::Error {
                    code: ErrorCode::ContextTooLong,
                    message,
                }) => return Attempt::ContextTooLong(message),
                Ok(Event::Error { code, message }) => {
                    return failed(format!("{}: {message}", code_name(code)));
                }
                Ok(Event::Progress {
                    stage,
                    done_bytes,
                    total_bytes,
                }) => {
                    if let Some(p) = progress.as_mut() {
                        p.stage(&stage, done_bytes, total_bytes).await;
                    }
                }
                Ok(Event::Delta { text }) => {
                    if let Some(p) = progress.as_mut() {
                        p.delta(&text).await;
                    }
                }
                Ok(Event::Status(_) | Event::Unknown) => {}
                Err(e) => return failed(describe_client_error(&e)),
            }
        }
        failed("daemon closed the connection without a result".to_string())
    }
}

#[tool_router]
impl PlamoServer {
    #[tool(description = TOOL_DESCRIPTION)]
    async fn translate(
        &self,
        Parameters(args): Parameters<TranslateArgs>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let progress = context
            .meta
            .get_progress_token()
            .map(|token| Progress::new(context.peer.clone(), token));
        let req = self.build_request(args);
        // Leaving this select drops the EventStream, which closes the socket so the
        // daemon stops generating.
        tokio::select! {
            outcome = tokio::time::timeout(self.timeout, self.run(req, progress)) => match outcome {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!(timeout = ?self.timeout, "translation timed out");
                    error_result(format!(
                        "translation timed out after {:?} (set {TIMEOUT_ENV} to change the limit)",
                        self.timeout
                    ))
                }
            },
            _ = context.ct.cancelled() => {
                tracing::info!("translation cancelled by the client");
                error_result("translation cancelled".to_string())
            }
        }
    }
}

#[tool_handler(
    name = "plamo-translate-mcp",
    instructions = "Translate text locally with PLaMo-3-translate via the plamo-translate daemon."
)]
impl ServerHandler for PlamoServer {}

fn error_result(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// `blocks[0]` is always the translation verbatim; any notes go in a second block so
/// the caller can pass `blocks[0]` back as history unchanged.
fn success_with_notes(
    text: String,
    finish_reason: Option<&str>,
    supplied: usize,
    kept: usize,
) -> CallToolResult {
    let mut notes = Vec::new();
    if kept < supplied {
        notes.push(format!(
            "note: the oldest {} of {supplied} history pairs were dropped to fit the model's context window; only the last {kept} were used.",
            supplied - kept
        ));
    }
    // A daemon that predates finish_reason gives no basis for judging eligibility.
    if finish_reason.is_some()
        && let Err(reason) = check_history_eligible(finish_reason, &text)
    {
        notes.push(format!(
            "note: do not pass this translation as history: {reason}."
        ));
    }
    let mut blocks = vec![ContentBlock::text(text)];
    if !notes.is_empty() {
        blocks.push(ContentBlock::text(notes.join("\n")));
    }
    CallToolResult::success(blocks)
}

fn context_too_long_text(message: &str, supplied: usize) -> String {
    if supplied == 0 {
        format!("context_too_long: {message}")
    } else {
        format!(
            "context_too_long: {message} (all {supplied} history pairs were dropped and the request still does not fit; shorten text, context or glossary)"
        )
    }
}

fn describe_client_error(err: &Error) -> String {
    match err {
        Error::DaemonUnreachable {
            socket_path,
            source,
        } => format!(
            "cannot reach the plamo-translate daemon at {}: {source}\n\
             Set it up with `services.plamo-translate.enable = true` in home-manager, \
             or run `plamo-translate daemon install`.",
            socket_path.display()
        ),
        other => other.to_string(),
    }
}

fn code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::UnsupportedVersion => "unsupported_version",
        ErrorCode::ContextTooLong => "context_too_long",
        ErrorCode::Busy => "busy",
        ErrorCode::ModelLoadFailed => "model_load_failed",
        ErrorCode::ShuttingDown => "shutting_down",
        ErrorCode::Internal => "internal",
        ErrorCode::Unknown => "unknown",
    }
}

pub fn parse_timeout(value: Option<&str>) -> Result<Duration, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(DEFAULT_TIMEOUT),
        Some(v) => match v.parse::<u64>() {
            Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
            _ => Err(format!(
                "{TIMEOUT_ENV} must be a positive integer number of seconds, got {v:?}"
            )),
        },
    }
}

/// Sends MCP progress notifications for one tool call.
///
/// MCP requires `progress` to strictly increase per token, so the generation phase
/// continues from the last value sent during download/load instead of restarting at 0.
struct Progress {
    peer: Peer<RoleServer>,
    token: ProgressToken,
    tracker: ProgressTracker,
}

impl Progress {
    fn new(peer: Peer<RoleServer>, token: ProgressToken) -> Self {
        Self {
            peer,
            token,
            tracker: ProgressTracker::default(),
        }
    }

    async fn stage(&mut self, stage: &str, done_bytes: Option<u64>, total_bytes: Option<u64>) {
        let update = self
            .tracker
            .stage(Instant::now(), stage, done_bytes, total_bytes);
        self.send(update).await;
    }

    async fn delta(&mut self, text: &str) {
        let update = self.tracker.delta(Instant::now(), text);
        self.send(update).await;
    }

    async fn send(&self, update: Option<Update>) {
        let Some(update) = update else { return };
        let mut param = ProgressNotificationParam::new(self.token.clone(), update.progress)
            .with_message(update.message);
        if let Some(total) = update.total {
            param = param.with_total(total);
        }
        if let Err(e) = self.peer.notify_progress(param).await {
            tracing::debug!(error = %e, "failed to send progress notification");
        }
    }
}

#[derive(Debug, PartialEq)]
struct Update {
    progress: f64,
    total: Option<f64>,
    message: String,
}

#[derive(Default)]
struct ProgressTracker {
    last_sent: Option<(Instant, f64)>,
    last_stage: Option<String>,
    generation_base: Option<f64>,
    chars: usize,
}

impl ProgressTracker {
    fn stage(
        &mut self,
        now: Instant,
        stage: &str,
        done_bytes: Option<u64>,
        total_bytes: Option<u64>,
    ) -> Option<Update> {
        let stage_changed = self.last_stage.as_deref() != Some(stage);
        self.last_stage = Some(stage.to_string());
        let last = self.last_value();
        let (progress, total, message) = match (done_bytes, total_bytes) {
            (Some(done), Some(total)) if total > 0 => (
                done as f64,
                Some(total as f64),
                format!(
                    "{}: {}%",
                    stage_label(stage),
                    (done.saturating_mul(100) / total).min(100)
                ),
            ),
            (Some(done), _) => (done as f64, None, stage_label(stage).to_string()),
            _ => (last + 1.0, None, stage_label(stage).to_string()),
        };
        self.emit(now, stage_changed, progress, total, message)
    }

    fn delta(&mut self, now: Instant, text: &str) -> Option<Update> {
        let first = self.generation_base.is_none();
        let base = *self.generation_base.get_or_insert(self.last_value());
        self.chars += text.chars().count();
        let message = format!("translating: {} characters generated", self.chars);
        self.emit(now, first, base + self.chars as f64, None, message)
    }

    fn last_value(&self) -> f64 {
        self.last_sent.map_or(0.0, |(_, v)| v)
    }

    fn emit(
        &mut self,
        now: Instant,
        force: bool,
        progress: f64,
        total: Option<f64>,
        message: String,
    ) -> Option<Update> {
        if let Some((at, value)) = self.last_sent {
            if progress <= value {
                return None;
            }
            if !force && now.duration_since(at) < PROGRESS_INTERVAL {
                return None;
            }
        }
        self.last_sent = Some((now, progress));
        Some(Update {
            progress,
            total,
            message,
        })
    }
}

fn stage_label(stage: &str) -> &str {
    match stage {
        "downloading" => "downloading model",
        "loading" => "loading model",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_defaults_and_parses() {
        assert_eq!(parse_timeout(None), Ok(DEFAULT_TIMEOUT));
        assert_eq!(parse_timeout(Some("")), Ok(DEFAULT_TIMEOUT));
        assert_eq!(parse_timeout(Some("12")), Ok(Duration::from_secs(12)));
        assert!(parse_timeout(Some("0")).is_err());
        assert!(parse_timeout(Some("abc")).is_err());
    }

    #[test]
    fn deltas_are_throttled_but_first_one_is_sent() {
        let t0 = Instant::now();
        let mut p = ProgressTracker::default();
        let first = p.delta(t0, "ab").unwrap();
        assert_eq!(first.progress, 2.0);
        assert!(p.delta(t0 + Duration::from_millis(10), "c").is_none());
        let later = p.delta(t0 + PROGRESS_INTERVAL, "d").unwrap();
        assert_eq!(later.progress, 4.0);
        assert_eq!(later.message, "translating: 4 characters generated");
    }

    #[test]
    fn progress_strictly_increases_across_phases() {
        let t0 = Instant::now();
        let mut p = ProgressTracker::default();
        let dl = p.stage(t0, "downloading", Some(50), Some(100)).unwrap();
        assert_eq!((dl.progress, dl.total), (50.0, Some(100.0)));
        assert_eq!(dl.message, "downloading model: 50%");
        let load = p.stage(t0, "loading", None, None).unwrap();
        assert_eq!(load.progress, 51.0);
        let gen_first = p.delta(t0, "x").unwrap();
        assert_eq!(gen_first.progress, 52.0);
    }

    #[test]
    fn stage_change_bypasses_throttle() {
        let t0 = Instant::now();
        let mut p = ProgressTracker::default();
        assert!(p.stage(t0, "downloading", Some(1), Some(10)).is_some());
        assert!(p.stage(t0, "downloading", Some(2), Some(10)).is_none());
        assert!(p.stage(t0, "loading", None, None).is_some());
    }
}
