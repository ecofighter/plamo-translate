use crate::server::{
    DOC_TIMEOUT_ENV, Form, PROGRESS_INTERVAL, PlamoServer, TIMEOUT_ENV, describe_client_error,
    error_result, stage_label,
};
use plamo_client::{
    Error, GlossaryEntry, HistoryEntry, TranslateRequest, merge_glossaries, parse_history,
};
use plamo_doc::{
    ClientBackend, Document, Format, Job, Limits, NoteKind, ProgressEvent, Report, Sink, Stop,
    StopCause, Warning, WarningKind, prepare, translate,
};
use rmcp::{
    Peer, RoleServer,
    model::{CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken},
    service::RequestContext,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time::Instant;

pub const DOCUMENT_DESCRIPTION: &str = "Translate a whole plain-text or Markdown document with the local PLaMo-3-translate model, piece by piece with rolling history so terms stay consistent. Code blocks and HTML are kept verbatim; the output keeps the source structure. Pass the document as text (it then appears in this conversation twice: as the argument and as the result), or as input_path (absolute) to avoid that; pass output_path (absolute) to write the result to a file and get a short summary instead of the text (an existing output_path needs overwrite=true; a stale output_path.part from an earlier failed call is replaced). Use history_path + history_append to keep terminology across several calls on the same document. target_lang, source_lang, context, glossary and form work as in translate. The result's first content block is the translation (or the write summary); a second block with note:/warning: lines describes pieces that could not be translated fully (their source text is kept in place) and is not part of the translation. If the call fails part-way, the error names the piece and the translated part is returned in the second block (text mode) or left in output_path.part (file mode). A document takes seconds per piece; the call fails if the daemon sends nothing for PLAMO_TRANSLATE_MCP_TIMEOUT_SECS (default 300) or runs longer than PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS (default 3600).";

const DEFAULT_HISTORY_BYTES: u32 = 8000;
/// Keeps the advisory block readable for a model; the rest is only counted.
const MAX_ADVISORY_LINES: usize = 50;

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DocFormat {
    #[default]
    Auto,
    Text,
    Markdown,
}

impl DocFormat {
    fn into_format(self) -> Option<Format> {
        match self {
            DocFormat::Auto => None,
            DocFormat::Text => Some(Format::Text),
            DocFormat::Markdown => Some(Format::Markdown),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TranslateDocumentArgs {
    /// The document itself. Pass exactly one of text or input_path.
    text: Option<String>,
    /// Absolute path of a UTF-8 file to translate. Pass exactly one of text or input_path.
    input_path: Option<PathBuf>,
    /// Absolute path to write the translation to; the result is then a short summary.
    /// Must differ from input_path.
    output_path: Option<PathBuf>,
    /// Replace an existing output_path. Default false.
    overwrite: Option<bool>,
    /// Input format. Default auto (detected from the content).
    format: Option<DocFormat>,
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
    /// Absolute path of a JSONL history file ({"source", "translation"} per line, oldest
    /// first); read first if it exists.
    history_path: Option<PathBuf>,
    /// Append each translated piece to history_path (created if missing). Default false.
    history_append: Option<bool>,
    /// History sent with each piece, in UTF-8 bytes. Default 8000; 0 disables history.
    history_bytes: Option<u32>,
}

/// A validated call: everything the run needs, with the output file already created.
struct Plan {
    input: String,
    format: Option<Format>,
    output: Option<FileOutput>,
    job: Job,
}

struct FileOutput {
    path: PathBuf,
    part: PathBuf,
    file: File,
}

pub(crate) async fn run(
    server: &PlamoServer,
    args: TranslateDocumentArgs,
    context: RequestContext<RoleServer>,
) -> CallToolResult {
    let plan = match validate(args, &server.config_glossary) {
        Ok(plan) => plan,
        Err(message) => return error_result(message),
    };
    let document = prepare(&plan.input, plan.format, &Limits::default());
    let advisories = prelude(&document);
    let progress = context
        .meta
        .get_progress_token()
        .map(|token| DocProgress::new(context.peer.clone(), token));
    let (out, output) = match plan.output {
        Some(FileOutput { path, part, file }) => (Out::File { file, bytes: 0 }, Some((path, part))),
        None => (Out::Text(String::new()), None),
    };
    let mut sink = DocSink { out, progress };
    let backend = ClientBackend::new(&server.client).with_event_timeout(server.timeout);
    // Leaving this select drops the run, which closes the daemon connection.
    let outcome = tokio::select! {
        r = tokio::time::timeout(
            server.doc_timeout,
            translate(&backend, document, plan.job, &mut sink),
        ) => Some(r),
        _ = context.ct.cancelled() => None,
    };
    match outcome {
        Some(Ok(report)) => finish(report, sink.out, output, advisories, server.timeout),
        Some(Err(_)) => {
            tracing::warn!(timeout = ?server.doc_timeout, "document translation timed out");
            error_result(format!(
                "translation timed out after {:?} (set {DOC_TIMEOUT_ENV} to change the limit)",
                server.doc_timeout
            ))
        }
        None => {
            tracing::info!("document translation cancelled by the client");
            error_result("translation cancelled".to_string())
        }
    }
}

fn validate(
    args: TranslateDocumentArgs,
    config_glossary: &[GlossaryEntry],
) -> Result<Plan, String> {
    let invalid = |m: &str| Err(format!("invalid arguments: {m}"));
    if args.text.is_some() == args.input_path.is_some() {
        return invalid("pass exactly one of text or input_path");
    }
    if args.input_path.as_ref().is_some_and(|p| !p.is_absolute()) {
        return invalid("input_path must be absolute");
    }
    if args.output_path.as_ref().is_some_and(|p| !p.is_absolute()) {
        return invalid("output_path must be absolute");
    }
    if args.history_path.as_ref().is_some_and(|p| !p.is_absolute()) {
        return invalid("history_path must be absolute");
    }
    let history_append = args.history_append.unwrap_or(false);
    if history_append && args.history_path.is_none() {
        return invalid("history_append requires history_path");
    }
    let input = match (&args.text, &args.input_path) {
        (Some(text), _) => text.clone(),
        (None, Some(path)) => std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read input_path {}: {e}", path.display()))?,
        (None, None) => unreachable!("checked above"),
    };
    check_distinct_paths(
        args.input_path.as_deref(),
        args.output_path.as_deref(),
        args.history_path.as_deref(),
    )?;
    let output = match &args.output_path {
        Some(path) => Some(open_output(path, args.overwrite.unwrap_or(false))?),
        None => None,
    };
    let seed = match &args.history_path {
        Some(path) => read_history(path)?,
        None => Vec::new(),
    };
    let glossary = merge_glossaries([config_glossary.to_vec(), args.glossary.unwrap_or_default()]);
    Ok(Plan {
        input,
        format: args.format.unwrap_or_default().into_format(),
        output,
        job: Job {
            request: TranslateRequest {
                text: String::new(),
                target_lang: args.target_lang,
                source_lang: args.source_lang,
                context: args.context,
                history: Vec::new(),
                glossary,
                form: args.form.map(|f| f.as_str().to_string()),
                stream: true,
                max_tokens: None,
            },
            seed,
            history_bytes: args.history_bytes.unwrap_or(DEFAULT_HISTORY_BYTES) as usize,
            record: args.history_path.filter(|_| history_append),
        },
    })
}

/// Checks the output path and creates (or truncates) `<path>.part`, so that a bad path
/// fails before any daemon time is spent.
fn open_output(path: &Path, overwrite: bool) -> Result<FileOutput, String> {
    if path.is_dir() {
        return Err("output_path is a directory".into());
    }
    if path.exists() && !overwrite {
        return Err("output_path exists; pass overwrite=true to replace it".into());
    }
    let part = part_path(path);
    let file = File::create(&part).map_err(|e| format!("cannot create {}: {e}", part.display()))?;
    Ok(FileOutput {
        path: path.to_owned(),
        part,
        file,
    })
}

/// Refuses paths that name the same file, which would truncate the source document
/// (output or its `.part`) or append history lines to a document file.
fn check_distinct_paths(
    input: Option<&Path>,
    output: Option<&Path>,
    history: Option<&Path>,
) -> Result<(), String> {
    let input = input.and_then(canonical_target);
    let output_part = output.and_then(|p| canonical_target(&part_path(p)));
    let output = output.and_then(canonical_target);
    let history = history.and_then(canonical_target);
    let same = |a: &Option<PathBuf>, b: &Option<PathBuf>| a.is_some() && a == b;
    if same(&output, &input) {
        return Err("invalid arguments: output_path must differ from input_path".into());
    }
    if same(&output_part, &input) {
        return Err("invalid arguments: output_path.part must differ from input_path".into());
    }
    if same(&history, &input) || same(&history, &output) || same(&history, &output_part) {
        return Err(
            "invalid arguments: history_path must differ from input_path and output_path".into(),
        );
    }
    Ok(())
}

fn part_path(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// The canonical path `path` names, whether or not the file exists yet.
fn canonical_target(path: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::fs::canonicalize(path) {
        return Some(p);
    }
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    Some(parent.join(path.file_name()?))
}

fn read_history(path: &Path) -> Result<Vec<HistoryEntry>, String> {
    let fail =
        |e: &dyn std::fmt::Display| format!("cannot read history_path {}: {e}", path.display());
    match std::fs::read_to_string(path) {
        Ok(s) => parse_history(&s).map_err(|e| fail(&e)),
        // The first history_append call on a document creates the file.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(fail(&e)),
    }
}

/// The advisory lines known before translating: the detected format and blocks kept as is.
fn prelude(document: &Document) -> Vec<String> {
    let detected = document.format_detected();
    let mut lines = Vec::new();
    if detected {
        lines.push(format!(
            "note: input format: {} (auto-detected)",
            document.format().as_str()
        ));
    }
    // With an explicit format, an indented block is code by the caller's choice.
    lines.extend(
        document
            .notes()
            .iter()
            .filter(|n| n.kind != NoteKind::IndentedCode || detected)
            .map(|n| format!("note: {n}")),
    );
    lines
}

fn finish(
    report: Report,
    out: Out,
    output: Option<(PathBuf, PathBuf)>,
    mut advisories: Vec<String>,
    event_timeout: Duration,
) -> CallToolResult {
    if let Some(stop) = &report.stop {
        return stopped(
            stop,
            out,
            output.as_ref().map(|(_, part)| part.as_path()),
            event_timeout,
        );
    }
    let mut history_failure = None;
    for w in &report.warnings {
        match w.kind {
            WarningKind::HistoryAppend => history_failure = Some(w),
            _ => advisories.push(format!("warning: {w}")),
        }
    }
    if report.degraded > 0 {
        advisories.push(format!(
            "note: {} of {} pieces were not fully translated; their source text was kept where marked above",
            report.degraded, report.pieces
        ));
    }
    if let Some(w) = history_failure {
        advisories.push(format!("note: {w}"));
    }
    let first = match (out, output) {
        (Out::Text(text), _) => text,
        (Out::File { bytes, .. }, Some((path, part))) => {
            if let Err(e) = std::fs::rename(&part, &path) {
                return error_result(format!(
                    "cannot rename {} to {}: {e}",
                    part.display(),
                    path.display()
                ));
            }
            format!(
                "wrote {} ({} pieces, {bytes} bytes)",
                path.display(),
                report.pieces
            )
        }
        (Out::File { .. }, None) => unreachable!("a file sink always has an output path"),
    };
    let mut blocks = vec![ContentBlock::text(first)];
    if !advisories.is_empty() {
        blocks.push(ContentBlock::text(cap(advisories).join("\n")));
    }
    CallToolResult::success(blocks)
}

fn cap(mut lines: Vec<String>) -> Vec<String> {
    if lines.len() > MAX_ADVISORY_LINES {
        let more = lines.len() - MAX_ADVISORY_LINES;
        lines.truncate(MAX_ADVISORY_LINES);
        lines.push(format!("note: ... and {more} more"));
    }
    lines
}

fn stopped(stop: &Stop, out: Out, part: Option<&Path>, event_timeout: Duration) -> CallToolResult {
    let mut message = stop_cause_text(&stop.cause, event_timeout);
    if let Some(label) = &stop.piece {
        message.push_str(&format!("; stopped at piece {label}"));
        let translated_before = piece_number(label).saturating_sub(1);
        match (part, &out) {
            (Some(part), _) => {
                message.push_str(&format!("; the pieces before it are in {}", part.display()))
            }
            // Nothing translated yet: there is no partial to point at or remainder to continue from.
            (None, Out::Text(partial)) if translated_before == 0 || partial.is_empty() => {}
            (None, _) => message.push_str(&format!(
                "; the {translated_before} pieces before it follow in the next content block; to continue, call again with the untranslated remainder"
            )),
        }
    }
    let mut blocks = vec![ContentBlock::text(message)];
    if let Out::Text(partial) = out
        && !partial.is_empty()
    {
        blocks.push(ContentBlock::text(partial));
    }
    CallToolResult::error(blocks)
}

/// K of a "K/M" or "K/M.s" label.
fn piece_number(label: &str) -> usize {
    label
        .split('/')
        .next()
        .and_then(|k| k.parse().ok())
        .unwrap_or(0)
}

fn stop_cause_text(cause: &StopCause, event_timeout: Duration) -> String {
    match cause {
        StopCause::Client(Error::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut => format!(
            "translation timed out: no event from the daemon for {event_timeout:?} (set {TIMEOUT_ENV} to change the limit)"
        ),
        StopCause::Client(e) => describe_client_error(e),
        other => other.to_string(),
    }
}

enum Out {
    Text(String),
    File { file: File, bytes: usize },
}

struct DocSink {
    out: Out,
    progress: Option<DocProgress>,
}

impl Sink for DocSink {
    async fn write(&mut self, text: &str) -> std::io::Result<()> {
        match &mut self.out {
            Out::Text(buffer) => buffer.push_str(text),
            // Blocking writes are acceptable: the target is a local file and the pieces are small.
            Out::File { file, bytes } => {
                file.write_all(text.as_bytes())?;
                file.flush()?;
                *bytes += text.len();
            }
        }
        Ok(())
    }

    async fn warn(&mut self, _warning: &Warning) {}

    async fn progress(&mut self, event: &ProgressEvent) {
        if let Some(p) = self.progress.as_mut() {
            p.event(event).await;
        }
    }
}

/// Sends MCP progress notifications for one document: `progress` counts finished pieces,
/// with the fraction of the current piece estimated from the characters generated.
struct DocProgress {
    peer: Peer<RoleServer>,
    token: ProgressToken,
    tracker: DocProgressTracker,
}

impl DocProgress {
    fn new(peer: Peer<RoleServer>, token: ProgressToken) -> Self {
        Self {
            peer,
            token,
            tracker: DocProgressTracker::default(),
        }
    }

    async fn event(&mut self, event: &ProgressEvent) {
        let Some(update) = self.tracker.event(Instant::now(), event) else {
            return;
        };
        let param = ProgressNotificationParam::new(self.token.clone(), update.progress)
            .with_total(update.total)
            .with_message(update.message);
        if let Err(e) = self.peer.notify_progress(param).await {
            tracing::debug!(error = %e, "failed to send progress notification");
        }
    }
}

#[derive(Debug, PartialEq)]
struct DocUpdate {
    progress: f64,
    total: f64,
    message: String,
}

#[derive(Default)]
struct DocProgressTracker {
    label: String,
    index: usize,
    total: usize,
    piece_chars: usize,
    last_sent: Option<(Instant, f64)>,
}

impl DocProgressTracker {
    fn event(&mut self, now: Instant, event: &ProgressEvent) -> Option<DocUpdate> {
        match event {
            ProgressEvent::Piece {
                label,
                index,
                total,
                chars,
            } => {
                self.label.clone_from(label);
                self.index = *index;
                self.total = *total;
                self.piece_chars = *chars;
                let message = format!("piece {label}");
                self.emit(now, true, self.base(), message)
            }
            ProgressEvent::Generated { chars } => {
                let expected = (1.5 * self.piece_chars as f64).max(1.0);
                let value = self.base() + (*chars as f64 / expected).min(0.99);
                let message = format!("piece {}: {chars} characters", self.label);
                self.emit(now, false, value, message)
            }
            ProgressEvent::Stage {
                stage,
                done_bytes,
                total_bytes,
            } => {
                let message = match (done_bytes, total_bytes) {
                    (Some(done), Some(total)) if *total > 0 => format!(
                        "{}: {}%",
                        stage_label(stage),
                        (done.saturating_mul(100) / total).min(100)
                    ),
                    _ => stage_label(stage).to_string(),
                };
                let value = self.last_value() + 0.001;
                self.emit(now, true, value, message)
            }
        }
    }

    fn base(&self) -> f64 {
        self.index.saturating_sub(1) as f64
    }

    fn last_value(&self) -> f64 {
        self.last_sent.map_or(0.0, |(_, v)| v)
    }

    /// MCP requires `progress` to strictly increase, and a value must not reach the next
    /// piece's boundary before that piece starts.
    fn emit(
        &mut self,
        now: Instant,
        force: bool,
        value: f64,
        message: String,
    ) -> Option<DocUpdate> {
        if value >= self.index as f64 {
            return None;
        }
        if let Some((at, last)) = self.last_sent {
            if value <= last {
                return None;
            }
            if !force && now.duration_since(at) < PROGRESS_INTERVAL {
                return None;
            }
        }
        self.last_sent = Some((now, value));
        Some(DocUpdate {
            progress: value,
            total: self.total as f64,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(index: usize, chars: usize) -> ProgressEvent {
        ProgressEvent::Piece {
            label: format!("{index}/3"),
            index,
            total: 3,
            chars,
        }
    }

    #[test]
    fn progress_counts_pieces_and_stays_below_the_next_boundary() {
        let t0 = Instant::now();
        let mut p = DocProgressTracker::default();
        let first = p.event(t0, &piece(1, 10)).unwrap();
        assert_eq!((first.progress, first.total), (0.0, 3.0));
        assert_eq!(first.message, "piece 1/3");
        let later = t0 + PROGRESS_INTERVAL;
        let generated = p
            .event(later, &ProgressEvent::Generated { chars: 1000 })
            .unwrap();
        assert_eq!(generated.progress, 0.99);
        assert_eq!(generated.message, "piece 1/3: 1000 characters");
        assert_eq!(p.event(later, &piece(2, 10)).unwrap().progress, 1.0);
    }

    #[test]
    fn generated_is_throttled_and_sub_pieces_never_go_back() {
        let t0 = Instant::now();
        let mut p = DocProgressTracker::default();
        p.event(t0, &piece(1, 100));
        let soon = t0 + Duration::from_millis(10);
        assert!(
            p.event(soon, &ProgressEvent::Generated { chars: 30 })
                .is_none()
        );
        let later = t0 + PROGRESS_INTERVAL;
        let g = p
            .event(later, &ProgressEvent::Generated { chars: 75 })
            .unwrap();
        assert_eq!(g.progress, 0.5);
        // A sub-piece restarts at the same index and is dropped until it passes the last value.
        assert!(p.event(later, &piece(1, 50)).is_none());
        let again = later + PROGRESS_INTERVAL;
        assert!(
            p.event(again, &ProgressEvent::Generated { chars: 30 })
                .is_none()
        );
        assert!(
            p.event(again, &ProgressEvent::Generated { chars: 60 })
                .is_some()
        );
    }

    #[test]
    fn stage_continues_from_the_last_value() {
        let t0 = Instant::now();
        let mut p = DocProgressTracker::default();
        p.event(t0, &piece(1, 10));
        let stage = ProgressEvent::Stage {
            stage: "downloading".into(),
            done_bytes: Some(50),
            total_bytes: Some(100),
        };
        let u = p.event(t0, &stage).unwrap();
        assert_eq!(u.progress, 0.001);
        assert_eq!(u.message, "downloading model: 50%");
    }

    #[test]
    fn advisories_are_capped() {
        let lines: Vec<String> = (0..53).map(|i| format!("warning: {i}")).collect();
        let capped = cap(lines);
        assert_eq!(capped.len(), 51);
        assert_eq!(capped[50], "note: ... and 3 more");
    }
}
