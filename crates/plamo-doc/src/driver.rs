//! Translates a sequence of units through the daemon: history, retries and splitting.
//! The core (`drive`) works only through `DocUnit` and `Item`, so its tests can script
//! units; only the adapter (`impl DocUnit for Unit`, `prepare`, `translate`) names the
//! segmenter's types.

use crate::history::select_history;
use crate::report::{ProgressEvent, Report, Sink, Stop, StopCause, Warning, WarningKind};
use crate::segment::{self, FinishError, Format, Issue, Limits, Note, Segment, Unit};
use futures::StreamExt;
use plamo_client::{
    Client, Error, ErrorCode, Event, HistoryEntry, TranslateRequest, append_history,
    check_history_eligible, drop_oldest_half,
};
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

/// A piece of the document that is sent to the daemon.
pub(crate) trait DocUnit: Sized {
    /// The exact input bytes this unit covers.
    fn source(&self) -> String;
    /// What is sent; also the base of `max_tokens`.
    fn text(&self) -> String;
    fn finish(&self, raw: &str) -> Result<Outcome, UnitError>;
    fn history_pair(&self, raw: &str) -> Option<(String, String)>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub text: String,
    pub issues: Vec<UnitIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnitIssue {
    pub structure: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitError {
    Empty,
    Markers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item<U> {
    Keep(String),
    Unit(U),
}

// `Sync` keeps the `translate` future `Send`.
pub(crate) type SplitFn<'a, U> = &'a (dyn Fn(&U) -> Option<Vec<Item<U>>> + Sync);

#[derive(Debug)]
pub enum Attempt {
    Done {
        text: String,
        finish_reason: Option<String>,
    },
    ContextTooLong(String),
    Daemon {
        code: ErrorCode,
        message: String,
    },
    Client(Error),
}

// Written as `impl Future + Send` rather than `async fn` so that `translate` is `Send`
// for any `Sync` backend.
pub trait Backend: Sync {
    fn require_history(&self) -> impl Future<Output = Result<(), Error>> + Send;
    /// Forwards `Stage`/`Generated` to `sink.progress` for every daemon progress/delta event.
    fn attempt<S: Sink>(
        &self,
        req: &TranslateRequest,
        sink: &mut S,
    ) -> impl Future<Output = Attempt> + Send;
}

pub struct ClientBackend<'a> {
    client: &'a Client,
    event_timeout: Option<Duration>,
}

impl<'a> ClientBackend<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self {
            client,
            event_timeout: None,
        }
    }

    /// Limits each connect, the preflight, and each wait for the next daemon event to `d`.
    /// A long document stays alive as long as the daemon keeps sending events.
    pub fn with_event_timeout(self, d: Duration) -> Self {
        Self {
            event_timeout: Some(d),
            ..self
        }
    }

    async fn limit<F: Future>(&self, f: F) -> Result<F::Output, Error> {
        match self.event_timeout {
            None => Ok(f.await),
            Some(d) => tokio::time::timeout(d, f).await.map_err(|_| {
                Error::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("no event from the daemon for {d:?}"),
                ))
            }),
        }
    }
}

impl Backend for ClientBackend<'_> {
    async fn require_history(&self) -> Result<(), Error> {
        self.limit(self.client.require_history()).await?
    }

    async fn attempt<S: Sink>(&self, req: &TranslateRequest, sink: &mut S) -> Attempt {
        let mut events = match self.limit(self.client.translate_unchecked(req)).await {
            Ok(Ok(events)) => events,
            Ok(Err(e)) | Err(e) => return Attempt::Client(e),
        };
        let mut received = 0usize;
        loop {
            let event = match self.limit(events.next()).await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(e) => return Attempt::Client(e),
            };
            match event {
                Ok(Event::Progress {
                    stage,
                    done_bytes,
                    total_bytes,
                }) => {
                    sink.progress(&ProgressEvent::Stage {
                        stage,
                        done_bytes,
                        total_bytes,
                    })
                    .await
                }
                Ok(Event::Delta { text }) => {
                    received += text.chars().count();
                    sink.progress(&ProgressEvent::Generated { chars: received })
                        .await;
                }
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
                Ok(Event::Error { code, message }) => return Attempt::Daemon { code, message },
                Ok(Event::Status(_) | Event::Unknown) => {}
                Err(e) => return Attempt::Client(e),
            }
        }
        Attempt::Client(Error::Protocol(
            "connection closed before terminal event".into(),
        ))
    }
}

/// A document split into verbatim text and translation units, ready for `translate`.
pub struct Document {
    format: Format,
    detected: bool,
    notes: Vec<Note>,
    segments: Vec<Segment>,
}

/// `format: None` detects the format from the input.
pub fn prepare(input: &str, format: Option<Format>, limits: &Limits) -> Document {
    let detected = format.is_none();
    let format = format.unwrap_or_else(|| segment::detect_format(input));
    let split = segment::split(input, format, limits);
    Document {
        format,
        detected,
        notes: split.notes,
        segments: split.segments,
    }
}

impl Document {
    pub fn format(&self) -> Format {
        self.format
    }

    pub fn format_detected(&self) -> bool {
        self.detected
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    pub fn pieces(&self) -> usize {
        self.units().count()
    }

    /// The text sent for every unit, joined by "\n"; input for picking the target language.
    pub fn translatable_text(&self) -> String {
        self.units().map(Unit::text).collect::<Vec<_>>().join("\n")
    }

    fn units(&self) -> impl Iterator<Item = &Unit> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Translate(u) => Some(u),
            Segment::Keep(_) => None,
        })
    }
}

pub struct Job {
    /// target_lang, source_lang, context, glossary and form are used; text, history,
    /// max_tokens and stream are set per request.
    pub request: TranslateRequest,
    pub seed: Vec<HistoryEntry>,
    pub history_bytes: usize,
    pub record: Option<PathBuf>,
}

/// Translates `document` and writes it to `sink` in document order. Cancel by dropping
/// the future. A document without pieces is written as is, without contacting the daemon.
pub async fn translate<B: Backend, S: Sink>(
    backend: &B,
    document: Document,
    job: Job,
    sink: &mut S,
) -> Report {
    let batch = Batch {
        request: job.request,
        seed: job.seed,
        history_bytes: job.history_bytes,
        record: job.record,
        items: document.segments.into_iter().map(into_item).collect(),
    };
    drive(backend, batch, &split_adapter, sink).await
}

impl DocUnit for Unit {
    fn source(&self) -> String {
        Unit::source(self).to_owned()
    }

    fn text(&self) -> String {
        Unit::text(self)
    }

    fn finish(&self, raw: &str) -> Result<Outcome, UnitError> {
        match Unit::finish(self, raw) {
            Ok(finished) => Ok(Outcome {
                text: finished.text,
                issues: finished
                    .issues
                    .iter()
                    .map(|i| UnitIssue {
                        structure: matches!(i, Issue::Structure { .. }),
                        message: i.to_string(),
                    })
                    .collect(),
            }),
            Err(FinishError::Empty) => Err(UnitError::Empty),
            Err(FinishError::Placeholders) => Err(UnitError::Markers),
        }
    }

    fn history_pair(&self, raw: &str) -> Option<(String, String)> {
        Unit::history_pair(self, raw)
    }
}

fn into_item(segment: Segment) -> Item<Unit> {
    match segment {
        Segment::Keep(s) => Item::Keep(s),
        Segment::Translate(u) => Item::Unit(u),
    }
}

fn split_adapter(unit: &Unit) -> Option<Vec<Item<Unit>>> {
    segment::split_unit(unit).map(|segments| segments.into_iter().map(into_item).collect())
}

pub(crate) struct Batch<U> {
    pub request: TranslateRequest,
    pub seed: Vec<HistoryEntry>,
    pub history_bytes: usize,
    pub record: Option<PathBuf>,
    pub items: Vec<Item<U>>,
}

/// 1.5 x chars + 256, saturating at u32::MAX; counts every char of `text`, placeholders included.
///
/// The largest legitimate output measured was 0.57 tokens per source char (number-heavy
/// text into ja/zh/ko). Without a cap, one request with mismatched history ran away to
/// 17k tokens. Raise the factor (for example to 2) if normal prose starts hitting `length`.
pub(crate) fn max_tokens_for(text: &str) -> u32 {
    let chars = text.chars().count() as u64;
    let tokens = chars.saturating_mul(3) / 2 + 256;
    u32::try_from(tokens).unwrap_or(u32::MAX)
}

struct Run<'s, S> {
    sink: &'s mut S,
    warnings: Vec<Warning>,
    pool: Vec<HistoryEntry>,
    record: Option<PathBuf>,
    /// Top-level pieces with at least one degraded part.
    degraded: usize,
    piece_degraded: bool,
    append_failed: bool,
}

impl<S: Sink> Run<'_, S> {
    async fn write(&mut self, s: &str, piece: Option<&str>) -> Result<(), Stop> {
        self.sink.write(s).await.map_err(|e| Stop {
            piece: piece.map(str::to_owned),
            cause: StopCause::Output(e),
        })
    }

    async fn warn(&mut self, label: &str, kind: WarningKind, message: String) {
        let warning = Warning {
            piece: label.to_owned(),
            kind,
            message,
        };
        self.sink.warn(&warning).await;
        if kind.degrades() {
            self.piece_degraded = true;
        }
        self.warnings.push(warning);
    }

    /// Adds the pair to the pool (and the history file) if the piece qualifies as history.
    async fn record<U: DocUnit>(
        &mut self,
        unit: &U,
        raw: &str,
        finish_reason: Option<&str>,
        label: &str,
    ) {
        let Some((source, translation)) = unit.history_pair(raw) else {
            return;
        };
        if let Err(why) = check_history_eligible(finish_reason, &translation) {
            self.warn(
                label,
                WarningKind::NotHistory,
                format!("not used as history: {why}"),
            )
            .await;
            return;
        }
        let entry = HistoryEntry {
            source,
            translation,
        };
        if !self.append_failed
            && let Some(path) = &self.record
            && let Err(e) = append_history(path, &entry)
        {
            let message = format!("cannot append to history {}: {e}", path.display());
            self.append_failed = true;
            self.warn(label, WarningKind::HistoryAppend, message).await;
        }
        self.pool.push(entry);
    }
}

enum Entry<U> {
    Keep(String),
    /// `sretry`: the unit came from `split`, so a structure issue no longer splits it again.
    Unit(U, bool),
}

fn push_items<U>(stack: &mut Vec<Entry<U>>, items: Vec<Item<U>>) {
    for item in items.into_iter().rev() {
        stack.push(match item {
            Item::Keep(s) => Entry::Keep(s),
            Item::Unit(u) => Entry::Unit(u, true),
        });
    }
}

/// Translates `batch.items` one unit at a time and writes the reassembled document to `sink`.
pub(crate) async fn drive<U: DocUnit, B: Backend, S: Sink>(
    backend: &B,
    batch: Batch<U>,
    split: SplitFn<'_, U>,
    sink: &mut S,
) -> Report {
    let n = batch
        .items
        .iter()
        .filter(|i| matches!(i, Item::Unit(_)))
        .count();
    let preflighted = n > 0 && (batch.history_bytes > 0 || batch.record.is_some());
    if preflighted && let Err(e) = backend.require_history().await {
        return Report {
            pieces: n,
            stop: Some(Stop {
                piece: None,
                cause: StopCause::Client(e),
            }),
            ..Default::default()
        };
    }
    let mut run = Run {
        sink,
        warnings: Vec::new(),
        pool: batch.seed,
        record: batch.record,
        degraded: 0,
        piece_degraded: false,
        append_failed: false,
    };
    let ctx = Ctx {
        backend,
        base: &batch.request,
        split,
        history_bytes: batch.history_bytes,
        preflighted,
    };
    let mut index = 0usize;
    let mut stop = None;
    for item in batch.items {
        let result = match item {
            Item::Keep(s) => run.write(&s, None).await,
            Item::Unit(u) => {
                index += 1;
                let result = ctx.piece(&mut run, u, index, n).await;
                if std::mem::take(&mut run.piece_degraded) {
                    run.degraded += 1;
                }
                result
            }
        };
        if let Err(s) = result {
            stop = Some(s);
            break;
        }
    }
    Report {
        pieces: n,
        degraded: run.degraded,
        warnings: run.warnings,
        stop,
    }
}

struct Ctx<'a, B, U> {
    backend: &'a B,
    base: &'a TranslateRequest,
    split: SplitFn<'a, U>,
    history_bytes: usize,
    preflighted: bool,
}

impl<B: Backend, U: DocUnit> Ctx<'_, B, U> {
    /// Translates one top-level unit, splitting it as needed; the stack avoids async recursion.
    ///
    /// One original unit costs at most 2h+3 requests (h = its holes): each marker failure
    /// removes a hole by splitting at it, plus at most one structure split.
    async fn piece<S: Sink>(
        &self,
        run: &mut Run<'_, S>,
        unit: U,
        index: usize,
        total: usize,
    ) -> Result<(), Stop> {
        let top_label = format!("{index}/{total}");
        let mut stack = vec![Entry::Unit(unit, false)];
        let mut sub = 0usize;
        while let Some(entry) = stack.pop() {
            let (u, sretry) = match entry {
                Entry::Keep(s) => {
                    run.write(&s, Some(&top_label)).await?;
                    continue;
                }
                Entry::Unit(u, sretry) => (u, sretry),
            };
            let label = if sretry {
                sub += 1;
                format!("{top_label}.{sub}")
            } else {
                top_label.clone()
            };
            let stop = |cause| Stop {
                piece: Some(label.clone()),
                cause,
            };
            let text = u.text();
            let mut req = self.base.clone();
            req.max_tokens = Some(max_tokens_for(&text));
            req.text = text;
            req.stream = true;
            req.history = select_history(&run.pool, self.history_bytes);
            loop {
                debug_assert!(req.history.is_empty() || self.preflighted);
                run.sink
                    .progress(&ProgressEvent::Piece {
                        label: label.clone(),
                        index,
                        total,
                        chars: req.text.chars().count(),
                    })
                    .await;
                let attempt = self.backend.attempt(&req, &mut *run.sink).await;
                match attempt {
                    Attempt::Done {
                        text: raw,
                        finish_reason,
                    } if finish_reason.as_deref() == Some("length") => {
                        if let Some(items) = (self.split)(&u) {
                            push_items(&mut stack, items);
                            break;
                        }
                        let kept = u
                            .finish(&raw)
                            .map(|f| f.text)
                            .unwrap_or_else(|_| u.source());
                        run.write(&kept, Some(&label)).await?;
                        run.warn(
                            &label,
                            WarningKind::Truncated,
                            "translation truncated (finish_reason: length)".into(),
                        )
                        .await;
                        break;
                    }
                    Attempt::Done {
                        text: raw,
                        finish_reason,
                    } => {
                        match u.finish(&raw) {
                            Err(UnitError::Empty) => {
                                run.write(&u.source(), Some(&label)).await?;
                                run.warn(
                                    &label,
                                    WarningKind::Empty,
                                    "empty translation; left untranslated".into(),
                                )
                                .await;
                            }
                            Err(UnitError::Markers) => {
                                if let Some(items) = (self.split)(&u) {
                                    push_items(&mut stack, items);
                                } else {
                                    run.write(&u.source(), Some(&label)).await?;
                                    run.warn(
                                        &label,
                                        WarningKind::MarkersLost,
                                        "verbatim-block markers lost or reordered; left untranslated"
                                            .into(),
                                    )
                                    .await;
                                }
                            }
                            Ok(f) => {
                                let structure = f.issues.iter().any(|i| i.structure);
                                if structure
                                    && !sretry
                                    && let Some(items) = (self.split)(&u)
                                {
                                    push_items(&mut stack, items);
                                } else {
                                    run.write(&f.text, Some(&label)).await?;
                                    for issue in &f.issues {
                                        let kind = if issue.structure {
                                            WarningKind::Structure
                                        } else {
                                            WarningKind::Inline
                                        };
                                        run.warn(&label, kind, issue.message.clone()).await;
                                    }
                                    if f.issues.is_empty() {
                                        run.record(&u, &raw, finish_reason.as_deref(), &label)
                                            .await;
                                    }
                                }
                            }
                        }
                        break;
                    }
                    Attempt::ContextTooLong(_) if !req.history.is_empty() => {
                        drop_oldest_half(&mut req.history);
                    }
                    Attempt::ContextTooLong(message) => {
                        if let Some(items) = (self.split)(&u) {
                            push_items(&mut stack, items);
                            break;
                        }
                        return Err(stop(StopCause::Daemon {
                            code: ErrorCode::ContextTooLong,
                            message,
                        }));
                    }
                    Attempt::Daemon { code, message } => {
                        return Err(stop(StopCause::Daemon { code, message }));
                    }
                    Attempt::Client(e) => return Err(stop(StopCause::Client(e))),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct T {
        src: String,
    }

    fn t(s: &str) -> T {
        T { src: s.into() }
    }

    impl DocUnit for T {
        fn source(&self) -> String {
            self.src.clone()
        }
        fn text(&self) -> String {
            self.src.clone()
        }
        fn finish(&self, raw: &str) -> Result<Outcome, UnitError> {
            let trimmed = raw.trim();
            let issue = |structure: bool, message: &str| Outcome {
                text: trimmed.to_owned(),
                issues: vec![UnitIssue {
                    structure,
                    message: message.into(),
                }],
            };
            if trimmed.is_empty() {
                Err(UnitError::Empty)
            } else if trimmed.starts_with("!MARKERS") {
                Err(UnitError::Markers)
            } else if trimmed.starts_with("!STRUCT") {
                Ok(issue(true, "structure changed"))
            } else if trimmed.starts_with("!INLINE") {
                Ok(issue(false, "inline elements changed"))
            } else {
                Ok(Outcome {
                    text: trimmed.to_owned(),
                    issues: vec![],
                })
            }
        }
        fn history_pair(&self, raw: &str) -> Option<(String, String)> {
            Some((self.src.clone(), raw.trim().to_owned()))
        }
    }

    /// A `Sync` counter with the read accessor the assertions use.
    #[derive(Default)]
    struct Counter(Mutex<usize>);

    impl Counter {
        fn get(&self) -> usize {
            *self.0.lock().unwrap()
        }
        fn bump(&self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    #[derive(Default)]
    struct ScriptedBackend {
        script: Mutex<VecDeque<Attempt>>,
        requests: Mutex<Vec<TranslateRequest>>,
        preflights: Counter,
        preflight_result: Mutex<Option<Error>>,
    }

    impl ScriptedBackend {
        fn new(script: Vec<Attempt>) -> Self {
            Self {
                script: Mutex::new(script.into()),
                ..Default::default()
            }
        }
        fn requests(&self) -> Vec<TranslateRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Backend for ScriptedBackend {
        async fn require_history(&self) -> Result<(), Error> {
            self.preflights.bump();
            assert!(
                self.requests.lock().unwrap().is_empty(),
                "preflight after a request"
            );
            match self.preflight_result.lock().unwrap().take() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
        async fn attempt<S: Sink>(&self, req: &TranslateRequest, _: &mut S) -> Attempt {
            self.requests.lock().unwrap().push(req.clone());
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .expect("script exhausted")
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        out: String,
        err: String,
        events: Vec<ProgressEvent>,
    }

    impl Sink for RecordingSink {
        async fn write(&mut self, text: &str) -> std::io::Result<()> {
            self.out.push_str(text);
            Ok(())
        }
        async fn warn(&mut self, w: &Warning) {
            let line = match w.kind {
                WarningKind::HistoryAppend => format!("error: {w}\n"),
                _ => format!("warning: {w}\n"),
            };
            self.err.push_str(&line);
        }
        async fn progress(&mut self, event: &ProgressEvent) {
            self.events.push(event.clone());
        }
    }

    /// Mirrors the CLI's exit code and trailing stderr lines for a finished run.
    fn render(report: &Report) -> (i32, String) {
        let mut err = String::new();
        let code = match &report.stop {
            Some(stop) => {
                err.push_str(&format!("error: {}\n", stop.cause));
                if let Some(label) = &stop.piece {
                    err.push_str(&format!(
                        "error: stopped at piece {label}; stdout holds everything translated before it\n"
                    ));
                }
                match stop.cause {
                    StopCause::Client(Error::DaemonUnreachable { .. }) => 3,
                    _ => 1,
                }
            }
            None => {
                if report.degraded > 0 {
                    err.push_str(&format!(
                        "error: {} of {} pieces were not fully translated; see the warnings above\n",
                        report.degraded, report.pieces
                    ));
                }
                if report.degraded > 0 || report.history_append_failed() {
                    1
                } else {
                    0
                }
            }
        };
        (code, err)
    }

    fn stop_done(s: &str) -> Attempt {
        Attempt::Done {
            text: s.into(),
            finish_reason: Some("stop".into()),
        }
    }

    fn length_done(s: &str) -> Attempt {
        Attempt::Done {
            text: s.into(),
            finish_reason: Some("length".into()),
        }
    }

    fn ctl() -> Attempt {
        Attempt::ContextTooLong("too long".into())
    }

    fn pair(s: &str, t: &str) -> HistoryEntry {
        HistoryEntry {
            source: s.into(),
            translation: t.into(),
        }
    }

    fn job(items: Vec<Item<T>>) -> Batch<T> {
        Batch {
            request: TranslateRequest {
                target_lang: "ja".into(),
                stream: true,
                ..Default::default()
            },
            seed: vec![],
            history_bytes: 8000,
            record: None,
            items,
        }
    }

    fn units(srcs: &[&str]) -> Vec<Item<T>> {
        srcs.iter().map(|s| Item::Unit(t(s))).collect()
    }

    /// Splits a unit "x|y" into [x, Keep " ", y]; anything without '|' cannot be split.
    fn bar_split(u: &T) -> Option<Vec<Item<T>>> {
        let (l, r) = u.src.split_once('|')?;
        Some(vec![
            Item::Unit(t(l)),
            Item::Keep(" ".into()),
            Item::Unit(t(r)),
        ])
    }

    fn no_split(_: &T) -> Option<Vec<Item<T>>> {
        None
    }

    struct Ran {
        report: Report,
        code: i32,
        out: String,
        err: String,
    }

    async fn run(backend: &ScriptedBackend, job: Batch<T>, split: SplitFn<'_, T>) -> Ran {
        let mut sink = RecordingSink::default();
        let report = drive(backend, job, split, &mut sink).await;
        let (code, trailer) = render(&report);
        Ran {
            report,
            code,
            out: sink.out,
            err: sink.err + &trailer,
        }
    }

    #[tokio::test]
    async fn all_stop_history_grows() {
        let b = ScriptedBackend::new(vec![stop_done("A"), stop_done("B"), stop_done("C")]);
        let mut items = units(&["a", "b", "c"]);
        items.insert(1, Item::Keep("\n\n".into()));
        let r = run(&b, job(items), &no_split).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.out, "A\n\nBC");
        let reqs = b.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].history, vec![]);
        assert_eq!(reqs[1].history, vec![pair("a", "A")]);
        assert_eq!(reqs[2].history, vec![pair("a", "A"), pair("b", "B")]);
        assert_eq!(reqs[0].text, "a");
        assert!(reqs.iter().all(|r| r.stream && r.target_lang == "ja"));
        assert_eq!(reqs[0].max_tokens, Some(max_tokens_for("a")));
        assert_eq!(b.preflights.get(), 1);
        assert_eq!(r.err, "");
    }

    #[tokio::test]
    async fn budget_zero_sends_no_history_and_no_preflight() {
        let b = ScriptedBackend::new(vec![stop_done("A"), stop_done("B")]);
        let mut j = job(units(&["a", "b"]));
        j.history_bytes = 0;
        let r = run(&b, j, &no_split).await;
        assert_eq!(r.code, 0);
        assert!(b.requests().iter().all(|r| r.history.is_empty()));
        assert_eq!(b.preflights.get(), 0);
    }

    #[tokio::test]
    async fn preflight_once_before_first_request() {
        // the scripted backend asserts that no request precedes the preflight
        let b = ScriptedBackend::new(vec![stop_done("A")]);
        let r = run(&b, job(units(&["a"])), &no_split).await;
        assert_eq!(r.code, 0);
        assert_eq!(b.preflights.get(), 1);
        assert_eq!(b.requests().len(), 1);
    }

    #[tokio::test]
    async fn preflight_failure_writes_nothing() {
        let b = ScriptedBackend::new(vec![]);
        *b.preflight_result.lock().unwrap() = Some(Error::HistoryUnsupported);
        let mut items = vec![Item::Keep("lead\n".into())];
        items.extend(units(&["a"]));
        let r = run(&b, job(items), &no_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "");
        assert!(r.err.contains("does not support translation history"));
        assert!(b.requests().is_empty());
    }

    #[tokio::test]
    async fn no_units_no_preflight() {
        let b = ScriptedBackend::new(vec![]);
        let r = run(&b, job(vec![Item::Keep("x".into())]), &no_split).await;
        assert_eq!(r.code, 0);
        assert_eq!(r.out, "x");
        assert_eq!(b.preflights.get(), 0);
    }

    #[tokio::test]
    async fn split_parts_share_history_after_one_preflight() {
        let b = ScriptedBackend::new(vec![length_done("trunc"), stop_done("X"), stop_done("Y")]);
        let r = run(&b, job(units(&["x|y"])), &bar_split).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.out, "X Y");
        assert_eq!(b.preflights.get(), 1);
        let reqs = b.requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[1].text, "x");
        assert_eq!(reqs[2].text, "y");
        assert_eq!(reqs[2].history, vec![pair("x", "X")]);
    }

    #[tokio::test]
    async fn seed_comes_first() {
        let b = ScriptedBackend::new(vec![stop_done("A"), stop_done("B")]);
        let mut j = job(units(&["a", "b"]));
        j.seed = vec![pair("s1", "t1"), pair("s2", "t2")];
        let r = run(&b, j, &no_split).await;
        assert_eq!(r.code, 0);
        let reqs = b.requests();
        assert_eq!(reqs[0].history, vec![pair("s1", "t1"), pair("s2", "t2")]);
        assert_eq!(reqs[1].history.last(), Some(&pair("a", "A")));
        assert_eq!(reqs[1].history.len(), 3);
    }

    #[tokio::test]
    async fn context_too_long_halves_history() {
        let b = ScriptedBackend::new(vec![ctl(), stop_done("A")]);
        let mut j = job(units(&["a"]));
        j.seed = (1..=4)
            .map(|i| pair(&format!("s{i}"), &format!("t{i}")))
            .collect();
        let r = run(&b, j, &no_split).await;
        assert_eq!(r.code, 0);
        assert_eq!(r.out, "A");
        let reqs = b.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].history.len(), 4);
        assert_eq!(reqs[1].text, reqs[0].text);
        assert_eq!(reqs[1].history, vec![pair("s3", "t3"), pair("s4", "t4")]);
    }

    #[tokio::test]
    async fn context_too_long_without_history_splits() {
        let b = ScriptedBackend::new(vec![ctl(), stop_done("A2"), stop_done("B2")]);
        let mut j = job(units(&["a|b"]));
        j.history_bytes = 0;
        let r = run(&b, j, &bar_split).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.out, "A2 B2");
    }

    #[tokio::test]
    async fn context_too_long_unsplittable_aborts() {
        let b = ScriptedBackend::new(vec![stop_done("A"), ctl(), ctl()]);
        let mut items = units(&["a", "b"]);
        items.insert(1, Item::Keep("\n".into()));
        let r = run(&b, job(items), &no_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "A\n");
        assert!(
            r.err.contains("error: context_too_long: too long"),
            "{}",
            r.err
        );
        assert!(r.err.contains("stopped at piece 2/2"), "{}", r.err);
        // the first try had history, the retry had none
        assert_eq!(b.requests().len(), 3);
        assert!(b.requests()[2].history.is_empty());
    }

    #[tokio::test]
    async fn length_splits_and_drops_truncated_text() {
        let b = ScriptedBackend::new(vec![
            length_done("TRUNC"),
            stop_done("X"),
            stop_done("Y"),
            stop_done("C"),
        ]);
        let r = run(&b, job(units(&["x|y", "c"])), &bar_split).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.out, "X YC");
        assert!(!r.out.contains("TRUNC"));
        let reqs = b.requests();
        assert_eq!(reqs[2].history, vec![pair("x", "X")]);
        assert_eq!(reqs[3].history, vec![pair("x", "X"), pair("y", "Y")]);
    }

    #[tokio::test]
    async fn degraded_parts_count_their_piece_once() {
        let b = ScriptedBackend::new(vec![
            length_done("TRUNC"),
            length_done("X1"),
            length_done("Y1"),
        ]);
        let r = run(&b, job(units(&["x|y"])), &bar_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "X1 Y1");
        assert!(
            r.err
                .contains("warning: piece 1/1.1: translation truncated")
        );
        assert!(
            r.err
                .contains("warning: piece 1/1.2: translation truncated")
        );
        assert!(
            r.err
                .contains("error: 1 of 1 pieces were not fully translated; see the warnings above"),
            "{}",
            r.err
        );
    }

    struct FailingSink;

    impl Sink for FailingSink {
        async fn write(&mut self, _: &str) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        async fn warn(&mut self, _: &Warning) {}
        async fn progress(&mut self, _: &ProgressEvent) {}
    }

    #[tokio::test]
    async fn stdout_write_error_aborts() {
        let b = ScriptedBackend::new(vec![stop_done("A"), stop_done("B")]);
        let report = drive(&b, job(units(&["a", "b"])), &no_split, &mut FailingSink).await;
        let (code, err) = render(&report);
        assert_eq!(code, 1);
        assert!(err.contains("error: cannot write output: "), "{err}");
        assert_eq!(b.requests().len(), 1);
    }

    #[tokio::test]
    async fn length_split_part_with_structure_issue_is_not_split_again() {
        let b = ScriptedBackend::new(vec![
            length_done("TRUNC"),
            stop_done("!STRUCT x"),
            stop_done("Y"),
        ]);
        // "x|y" splits into "x" and "y"; a split closure that would also split "x" must not be used
        let split = |u: &T| -> Option<Vec<Item<T>>> {
            if u.src == "x" {
                Some(vec![Item::Unit(t("x1")), Item::Unit(t("x2"))])
            } else {
                bar_split(u)
            }
        };
        let r = run(&b, job(units(&["x|y"])), &split).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.out, "!STRUCT x Y");
        assert_eq!(b.requests().len(), 3);
        assert!(
            r.err.contains("warning: piece 1/1.1: structure changed"),
            "{}",
            r.err
        );
        assert_eq!(b.requests()[2].history, vec![]);
    }

    #[tokio::test]
    async fn length_unsplittable_keeps_truncated_text() {
        let b = ScriptedBackend::new(vec![length_done(" TRUNC "), stop_done("B")]);
        let r = run(&b, job(units(&["a", "b"])), &no_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "TRUNCB");
        assert!(
            r.err
                .contains("warning: piece 1/2: translation truncated (finish_reason: length)"),
            "{}",
            r.err
        );
        assert!(
            r.err
                .contains("error: 1 of 2 pieces were not fully translated; see the warnings above"),
            "{}",
            r.err
        );
        assert_eq!(b.requests()[1].history, vec![]);
    }

    #[tokio::test]
    async fn empty_translation_keeps_source() {
        let b = ScriptedBackend::new(vec![stop_done(" \n "), stop_done("B")]);
        let r = run(&b, job(units(&["a", "b"])), &no_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "aB");
        assert!(
            r.err
                .contains("piece 1/2: empty translation; left untranslated")
        );
        assert_eq!(b.requests()[1].history, vec![]);
    }

    #[tokio::test]
    async fn structure_issue_splits_original() {
        let b = ScriptedBackend::new(vec![
            stop_done("!STRUCT"),
            stop_done("X"),
            stop_done("Y"),
            stop_done("C"),
        ]);
        let r = run(&b, job(units(&["x|y", "c"])), &bar_split).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.out, "X YC");
        assert_eq!(
            b.requests()[3].history,
            vec![pair("x", "X"), pair("y", "Y")]
        );
        assert_eq!(r.err, "");
    }

    #[tokio::test]
    async fn structure_issue_in_part_is_kept() {
        let b = ScriptedBackend::new(vec![
            stop_done("!STRUCT"),
            stop_done("!STRUCT x"),
            stop_done("Y"),
        ]);
        let split = |u: &T| -> Option<Vec<Item<T>>> {
            if u.src == "x" {
                Some(vec![Item::Unit(t("x1")), Item::Unit(t("x2"))])
            } else {
                bar_split(u)
            }
        };
        let r = run(&b, job(units(&["x|y"])), &split).await;
        assert_eq!(r.code, 0);
        assert_eq!(r.out, "!STRUCT x Y");
        assert_eq!(b.requests().len(), 3);
        assert!(r.err.contains("warning: piece 1/1.1: structure changed"));
        assert_eq!(b.requests()[2].history, vec![]);
    }

    #[tokio::test]
    async fn daemon_error_aborts() {
        let b = ScriptedBackend::new(vec![
            stop_done("A"),
            Attempt::Daemon {
                code: ErrorCode::Internal,
                message: "boom".into(),
            },
        ]);
        let items = vec![
            Item::Unit(t("a")),
            Item::Keep("\n\n".into()),
            Item::Unit(t("b")),
            Item::Keep("tail".into()),
        ];
        let r = run(&b, job(items), &no_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "A\n\n");
        assert!(r.err.contains("error: internal: boom"), "{}", r.err);
        assert!(
            r.err.contains(
                "error: stopped at piece 2/2; stdout holds everything translated before it"
            )
        );
    }

    #[tokio::test]
    async fn unreachable_exits_3() {
        let b = ScriptedBackend::new(vec![
            stop_done("A"),
            Attempt::Client(Error::DaemonUnreachable {
                socket_path: "/nonexistent".into(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            }),
        ]);
        let r = run(&b, job(units(&["a", "b", "c"])), &no_split).await;
        assert_eq!(r.code, 3);
        assert_eq!(r.out, "A");
        assert!(matches!(
            r.report.stop,
            Some(Stop {
                cause: StopCause::Client(Error::DaemonUnreachable { .. }),
                ..
            })
        ));
        assert!(r.err.contains("stopped at piece 2/3"));
    }

    #[tokio::test]
    async fn record_appends_eligible_pairs_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        std::fs::write(&path, "{\"source\":\"s\",\"translation\":\"t\"}\n").unwrap();
        let b = ScriptedBackend::new(vec![stop_done(" A \n"), length_done("T"), stop_done("C")]);
        let mut j = job(units(&["a", "b", "c"]));
        j.seed = vec![pair("s", "t")];
        j.record = Some(path.clone());
        let r = run(&b, j, &no_split).await;
        assert_eq!(r.code, 1);
        let lines: Vec<HistoryEntry> =
            plamo_client::parse_history(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let (s, tr) = t("a").history_pair(" A \n").unwrap();
        assert_eq!(lines, vec![pair("s", "t"), pair(&s, &tr), pair("c", "C")]);
    }

    #[tokio::test]
    async fn append_failure_reported_once_and_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing").join("h.jsonl");
        let b = ScriptedBackend::new(vec![stop_done("A"), stop_done("B")]);
        let mut j = job(units(&["a", "b"]));
        j.record = Some(path);
        let r = run(&b, j, &no_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "AB");
        assert_eq!(r.err.matches("cannot append to history").count(), 1);
        assert_eq!(b.requests()[1].history, vec![pair("a", "A")]);
    }

    #[test]
    fn max_tokens_for_scales_with_chars() {
        assert_eq!(max_tokens_for(&"a".repeat(10)), 271);
        assert_eq!(max_tokens_for(&"あ".repeat(1000)), 1756);
        assert_eq!(max_tokens_for(""), 256);
    }

    #[tokio::test]
    async fn markers_lost_splits() {
        let b = ScriptedBackend::new(vec![stop_done("!MARKERS"), stop_done("X"), stop_done("Y")]);
        let r = run(&b, job(units(&["x|y"])), &bar_split).await;
        assert_eq!(r.code, 0);
        assert_eq!(r.out, "X Y");
        assert_eq!(r.err, "");
    }

    #[tokio::test]
    async fn markers_lost_unsplittable_keeps_source() {
        let b = ScriptedBackend::new(vec![stop_done("!MARKERS")]);
        let r = run(&b, job(units(&["src"])), &no_split).await;
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "src");
        assert!(
            r.err.contains("verbatim-block markers lost or reordered"),
            "{}",
            r.err
        );
    }

    #[tokio::test]
    async fn inline_issue_warns_and_skips_history() {
        let b = ScriptedBackend::new(vec![stop_done("!INLINE a"), stop_done("B")]);
        let r = run(&b, job(units(&["a", "b"])), &bar_split).await;
        assert_eq!(r.code, 0);
        assert_eq!(r.out, "!INLINE aB");
        assert!(
            r.err
                .contains("warning: piece 1/2: inline elements changed")
        );
        assert_eq!(b.requests()[1].history, vec![]);
        assert_eq!(b.requests().len(), 2);
    }

    /// Serves one connection: reads the request line, writes `lines`, then closes.
    fn serve_once(lines: &'static [&'static str]) -> (tempfile::TempDir, PathBuf) {
        use std::io::{BufRead, BufReader, Write};
        let (dir, socket) = socket_dir();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(conn.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            for l in lines {
                conn.write_all(format!("{l}\n").as_bytes()).unwrap();
            }
        });
        (dir, socket)
    }

    async fn client_attempt(lines: &'static [&'static str]) -> Attempt {
        let (_dir, socket) = serve_once(lines);
        let client = Client::new(socket);
        ClientBackend::new(&client)
            .attempt(&TranslateRequest::default(), &mut RecordingSink::default())
            .await
    }

    #[tokio::test]
    async fn client_backend_maps_events() {
        let done = client_attempt(&[
            r#"{"type":"progress","stage":"loading"}"#,
            r#"{"type":"delta","text":"He"}"#,
            r#"{"type":"done","text":"Hello","finish_reason":"length"}"#,
        ])
        .await;
        assert!(
            matches!(&done, Attempt::Done { text, finish_reason }
                if text == "Hello" && finish_reason.as_deref() == Some("length")),
            "{done:?}"
        );

        let ctl =
            client_attempt(&[r#"{"type":"error","code":"context_too_long","message":"m"}"#]).await;
        assert!(
            matches!(&ctl, Attempt::ContextTooLong(m) if m == "m"),
            "{ctl:?}"
        );

        let daemon =
            client_attempt(&[r#"{"type":"error","code":"internal","message":"boom"}"#]).await;
        assert!(
            matches!(&daemon, Attempt::Daemon { code: ErrorCode::Internal, message } if message == "boom"),
            "{daemon:?}"
        );

        let closed = client_attempt(&[r#"{"type":"delta","text":"He"}"#]).await;
        assert!(
            matches!(&closed, Attempt::Client(Error::Protocol(m))
                if m == "connection closed before terminal event"),
            "{closed:?}"
        );
    }

    #[tokio::test]
    async fn client_backend_reports_unreachable() {
        let client = Client::new("/nonexistent/plamo-translate-test.sock".into())
            .with_connect_timeout(std::time::Duration::from_millis(50));
        let a = ClientBackend::new(&client)
            .attempt(&TranslateRequest::default(), &mut RecordingSink::default())
            .await;
        assert!(
            matches!(a, Attempt::Client(Error::DaemonUnreachable { .. })),
            "{a:?}"
        );
    }

    #[tokio::test]
    async fn ineligible_pair_warns() {
        let b = ScriptedBackend::new(vec![
            Attempt::Done {
                text: "A".into(),
                finish_reason: None,
            },
            stop_done("B"),
        ]);
        let r = run(&b, job(units(&["a", "b"])), &no_split).await;
        assert_eq!(r.code, 0);
        assert!(
            r.err
                .contains("warning: piece 1/2: not used as history: the translation did not finish normally (finish_reason: none)"),
            "{}",
            r.err
        );
        assert_eq!(b.requests()[1].history, vec![]);
    }

    /// A short directory for a Unix socket (macOS caps socket paths near 104 bytes).
    fn socket_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("pt")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("d.sock");
        (dir, socket)
    }

    fn public_job(history_bytes: usize) -> Job {
        Job {
            request: TranslateRequest {
                target_lang: "ja".into(),
                ..Default::default()
            },
            seed: vec![],
            history_bytes,
            record: None,
        }
    }

    #[tokio::test]
    async fn sink_receives_piece_and_generated_events() {
        let (_dir, socket) = serve_once(&[
            r#"{"type":"progress","stage":"loading"}"#,
            r#"{"type":"delta","text":"He"}"#,
            r#"{"type":"delta","text":"llo"}"#,
            r#"{"type":"done","text":"Hello","finish_reason":"stop"}"#,
        ]);
        let client = Client::new(socket);
        let doc = prepare("Hi there.\n", Some(Format::Text), &Limits::default());
        let chars = doc.translatable_text().chars().count();
        let mut sink = RecordingSink::default();
        let report = translate(&ClientBackend::new(&client), doc, public_job(0), &mut sink).await;
        assert!(report.clean(), "{report:?}");
        assert_eq!(sink.out, "Hello\n");
        assert_eq!(
            sink.events,
            vec![
                ProgressEvent::Piece {
                    label: "1/1".into(),
                    index: 1,
                    total: 1,
                    chars,
                },
                ProgressEvent::Stage {
                    stage: "loading".into(),
                    done_bytes: None,
                    total_bytes: None,
                },
                ProgressEvent::Generated { chars: 2 },
                ProgressEvent::Generated { chars: 5 },
            ]
        );
    }

    #[tokio::test]
    async fn report_counts_degraded_once_per_piece() {
        let b = ScriptedBackend::new(vec![
            length_done("TRUNC"),
            length_done("X1"),
            length_done("Y1"),
        ]);
        let r = run(&b, job(units(&["x|y"])), &bar_split).await;
        assert_eq!(r.report.pieces, 1);
        assert_eq!(r.report.degraded, 1);
        let kinds: Vec<(&str, WarningKind)> = r
            .report
            .warnings
            .iter()
            .map(|w| (w.piece.as_str(), w.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("1/1.1", WarningKind::Truncated),
                ("1/1.2", WarningKind::Truncated)
            ]
        );
        assert!(r.report.complete());
        assert!(!r.report.clean());
        assert!(!r.report.history_append_failed());
    }

    #[test]
    fn prepare_none_detects_format_and_reports_notes() {
        let input = "# Title\n\nSome prose.\n\n<div>html text</div>\n";
        let doc = prepare(input, None, &Limits::default());
        assert_eq!(doc.format(), Format::Markdown);
        assert!(doc.format_detected());
        assert_eq!(
            doc.notes(),
            [Note {
                line: 5,
                kind: crate::NoteKind::HtmlText
            }]
        );
        assert!(doc.pieces() > 0);
        assert!(doc.translatable_text().contains("Some prose."));
        assert!(!doc.translatable_text().contains("html text"));

        let doc = prepare(input, Some(Format::Text), &Limits::default());
        assert_eq!(doc.format(), Format::Text);
        assert!(!doc.format_detected());
    }

    #[tokio::test]
    async fn translate_with_zero_pieces_writes_keeps_without_backend() {
        let input = "```\nfn main() {}\n```\n";
        let doc = prepare(input, Some(Format::Markdown), &Limits::default());
        assert_eq!(doc.pieces(), 0);
        assert_eq!(doc.translatable_text(), "");
        let b = ScriptedBackend::new(vec![]);
        let mut sink = RecordingSink::default();
        let report = translate(&b, doc, public_job(8000), &mut sink).await;
        assert!(report.clean(), "{report:?}");
        assert_eq!(report.pieces, 0);
        assert_eq!(sink.out, input);
        assert_eq!(b.preflights.get(), 0);
        assert!(b.requests().is_empty());
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn client_backend_event_timeout_yields_timed_out() {
        let (_dir, socket) = socket_dir();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (_conn, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let client = Client::new(socket);
        let a = ClientBackend::new(&client)
            .with_event_timeout(Duration::from_millis(50))
            .attempt(&TranslateRequest::default(), &mut RecordingSink::default())
            .await;
        assert!(
            matches!(&a, Attempt::Client(Error::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut),
            "{a:?}"
        );
    }

    #[tokio::test]
    async fn client_backend_event_timeout_resets_per_event() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (_dir, socket) = socket_dir();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (conn, _) = listener.accept().await.unwrap();
            let (read, mut write) = conn.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            for _ in 0..10 {
                tokio::time::sleep(Duration::from_millis(30)).await;
                write
                    .write_all(b"{\"type\":\"delta\",\"text\":\"x\"}\n")
                    .await
                    .unwrap();
            }
            write
                .write_all(
                    b"{\"type\":\"done\",\"text\":\"xxxxxxxxxx\",\"finish_reason\":\"stop\"}\n",
                )
                .await
                .unwrap();
        });
        let client = Client::new(socket);
        let a = ClientBackend::new(&client)
            .with_event_timeout(Duration::from_millis(100))
            .attempt(&TranslateRequest::default(), &mut RecordingSink::default())
            .await;
        assert!(matches!(&a, Attempt::Done { .. }), "{a:?}");
    }

    #[test]
    fn translate_future_is_send() {
        fn assert_send<T: Send>(_: &T) {}
        let client = Client::new("/nonexistent/plamo-translate-test.sock".into());
        let backend = ClientBackend::new(&client);
        let doc = prepare("Hello.\n", None, &Limits::default());
        let mut sink = RecordingSink::default();
        let future = translate(&backend, doc, public_job(8000), &mut sink);
        assert_send(&future);
    }
}
