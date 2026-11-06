//! Translates a sequence of units through the daemon: history, retries and splitting.
//! The driver works only through `DocUnit` and `Item`; only `doc/mod.rs` names the
//! `segment::*` types (the adapter).

use super::history::select_history;
use crate::ExitCode;
use crate::translate::{Progress, write_daemon_error, write_error};
use futures::StreamExt;
use plamo_client::{
    Client, Error, ErrorCode, Event, HistoryEntry, TranslateRequest, append_history,
    check_history_eligible, drop_oldest_half,
};
use std::io::Write;
use std::path::PathBuf;

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

pub(crate) type SplitFn<'a, U> = &'a dyn Fn(&U) -> Option<Vec<Item<U>>>;

#[derive(Debug)]
pub(crate) enum Attempt {
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

pub(crate) trait Backend {
    async fn require_history(&self) -> Result<(), Error>;
    async fn attempt(&self, req: &TranslateRequest, progress: &mut Progress) -> Attempt;
}

pub(crate) struct ClientBackend<'a>(pub &'a Client);

impl Backend for ClientBackend<'_> {
    async fn require_history(&self) -> Result<(), Error> {
        self.0.require_history().await
    }

    async fn attempt(&self, req: &TranslateRequest, progress: &mut Progress) -> Attempt {
        let mut events = match self.0.translate_unchecked(req).await {
            Ok(events) => events,
            Err(e) => return Attempt::Client(e),
        };
        let mut received = 0usize;
        while let Some(event) = events.next().await {
            match event {
                Ok(Event::Progress {
                    stage,
                    done_bytes,
                    total_bytes,
                }) => progress.show(&stage, done_bytes, total_bytes),
                Ok(Event::Delta { text }) => {
                    received += text.chars().count();
                    progress.line(&format!("{received} chars"));
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

pub(crate) struct Job<U> {
    /// target_lang, source_lang, context, glossary, form; text and history are filled per request.
    pub base: TranslateRequest,
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

struct Run<'w> {
    out: &'w mut dyn Write,
    err: &'w mut dyn Write,
    pool: Vec<HistoryEntry>,
    record: Option<PathBuf>,
    /// Top-level pieces with at least one degraded part.
    degraded: usize,
    piece_degraded: bool,
    append_failed: bool,
}

/// Stops the run; the caller prints the specific error before.
struct Abort(i32);

impl Run<'_> {
    fn write(&mut self, s: &str, progress: &mut Progress) -> Result<(), Abort> {
        let res = self
            .out
            .write_all(s.as_bytes())
            .and_then(|()| self.out.flush());
        res.map_err(|e| {
            progress.clear();
            let _ = writeln!(self.err, "error: cannot write stdout: {e}");
            Abort(ExitCode::Translation as i32)
        })
    }

    fn warn(&mut self, label: &str, msg: &str, progress: &mut Progress) {
        progress.clear();
        let _ = writeln!(self.err, "warning: piece {label}: {msg}");
    }

    fn degrade(&mut self, label: &str, msg: &str, progress: &mut Progress) {
        self.warn(label, msg, progress);
        self.piece_degraded = true;
    }

    /// Adds the pair to the pool (and the history file) if the piece qualifies as history.
    fn record<U: DocUnit>(
        &mut self,
        unit: &U,
        raw: &str,
        finish_reason: Option<&str>,
        label: &str,
        progress: &mut Progress,
    ) {
        let Some((source, translation)) = unit.history_pair(raw) else {
            return;
        };
        if let Err(why) = check_history_eligible(finish_reason, &translation) {
            self.warn(label, &format!("not used as history: {why}"), progress);
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
            progress.clear();
            let _ = writeln!(
                self.err,
                "error: cannot append to history {}: {e}",
                path.display()
            );
            self.append_failed = true;
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

/// Translates `job.items` one unit at a time and writes the reassembled document to `out`.
/// Warnings and errors go to `err`; returns the process exit code.
pub(crate) async fn drive<U: DocUnit, B: Backend>(
    backend: &B,
    job: Job<U>,
    split: SplitFn<'_, U>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    progress: &mut Progress,
) -> i32 {
    let code = drive_inner(backend, job, split, out, err, progress).await;
    progress.clear();
    code
}

async fn drive_inner<U: DocUnit, B: Backend>(
    backend: &B,
    job: Job<U>,
    split: SplitFn<'_, U>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    progress: &mut Progress,
) -> i32 {
    let n = job
        .items
        .iter()
        .filter(|i| matches!(i, Item::Unit(_)))
        .count();
    let preflighted = n > 0 && (job.history_bytes > 0 || job.record.is_some());
    if preflighted && let Err(e) = backend.require_history().await {
        progress.clear();
        return write_error(err, &e);
    }
    let mut run = Run {
        out,
        err,
        pool: job.seed,
        record: job.record,
        degraded: 0,
        piece_degraded: false,
        append_failed: false,
    };
    let ctx = Ctx {
        backend,
        base: &job.base,
        split,
        history_bytes: job.history_bytes,
        preflighted,
    };
    let mut index = 0usize;
    for item in job.items {
        let result = match item {
            Item::Keep(s) => run.write(&s, progress),
            Item::Unit(u) => {
                index += 1;
                let result = ctx
                    .piece(&mut run, u, &format!("{index}/{n}"), progress)
                    .await;
                if std::mem::take(&mut run.piece_degraded) {
                    run.degraded += 1;
                }
                result
            }
        };
        if let Err(Abort(code)) = result {
            return code;
        }
    }
    progress.clear();
    let mut code = ExitCode::Ok as i32;
    if run.degraded > 0 {
        let _ = writeln!(
            run.err,
            "error: {} of {n} pieces were not fully translated; see the warnings above",
            run.degraded
        );
        code = ExitCode::Translation as i32;
    }
    if run.append_failed {
        code = ExitCode::Translation as i32;
    }
    code
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
    async fn piece(
        &self,
        run: &mut Run<'_>,
        unit: U,
        top_label: &str,
        progress: &mut Progress,
    ) -> Result<(), Abort> {
        let mut stack = vec![Entry::Unit(unit, false)];
        let mut sub = 0usize;
        while let Some(entry) = stack.pop() {
            let (u, sretry) = match entry {
                Entry::Keep(s) => {
                    run.write(&s, progress)?;
                    continue;
                }
                Entry::Unit(u, sretry) => (u, sretry),
            };
            let label = if sretry {
                sub += 1;
                format!("{top_label}.{sub}")
            } else {
                top_label.to_owned()
            };
            let text = u.text();
            let mut req = self.base.clone();
            req.max_tokens = Some(max_tokens_for(&text));
            req.text = text;
            req.stream = true;
            req.history = select_history(&run.pool, self.history_bytes);
            loop {
                debug_assert!(req.history.is_empty() || self.preflighted);
                progress.prefix.clear();
                progress.line(&format!("[{label}]"));
                progress.prefix = format!("[{label}] ");
                let attempt = self.backend.attempt(&req, progress).await;
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
                        run.write(&kept, progress)?;
                        run.degrade(
                            &label,
                            "translation truncated (finish_reason: length)",
                            progress,
                        );
                        break;
                    }
                    Attempt::Done {
                        text: raw,
                        finish_reason,
                    } => {
                        match u.finish(&raw) {
                            Err(UnitError::Empty) => {
                                run.write(&u.source(), progress)?;
                                run.degrade(
                                    &label,
                                    "empty translation; left untranslated",
                                    progress,
                                );
                            }
                            Err(UnitError::Markers) => {
                                if let Some(items) = (self.split)(&u) {
                                    push_items(&mut stack, items);
                                } else {
                                    run.write(&u.source(), progress)?;
                                    run.degrade(
                                        &label,
                                        "verbatim-block markers lost or reordered; left untranslated",
                                        progress,
                                    );
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
                                    run.write(&f.text, progress)?;
                                    for issue in &f.issues {
                                        run.warn(&label, &issue.message, progress);
                                    }
                                    if f.issues.is_empty() {
                                        run.record(
                                            &u,
                                            &raw,
                                            finish_reason.as_deref(),
                                            &label,
                                            progress,
                                        );
                                    }
                                }
                            }
                        }
                        break;
                    }
                    Attempt::ContextTooLong(_) if !req.history.is_empty() => {
                        drop_oldest_half(&mut req.history);
                    }
                    Attempt::ContextTooLong(m) => {
                        if let Some(items) = (self.split)(&u) {
                            push_items(&mut stack, items);
                            break;
                        }
                        progress.clear();
                        let code = write_daemon_error(run.err, &ErrorCode::ContextTooLong, &m);
                        return Err(stop(run, code, &label));
                    }
                    Attempt::Daemon { code, message } => {
                        progress.clear();
                        let code = write_daemon_error(run.err, &code, &message);
                        return Err(stop(run, code, &label));
                    }
                    Attempt::Client(e) => {
                        progress.clear();
                        let code = write_error(run.err, &e);
                        return Err(stop(run, code, &label));
                    }
                }
            }
        }
        Ok(())
    }
}

fn stop(run: &mut Run<'_>, code: i32, label: &str) -> Abort {
    let _ = writeln!(
        run.err,
        "error: stopped at piece {label}; stdout holds everything translated before it"
    );
    Abort(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

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

    #[derive(Default)]
    struct ScriptedBackend {
        script: RefCell<VecDeque<Attempt>>,
        requests: RefCell<Vec<TranslateRequest>>,
        preflights: Cell<usize>,
        preflight_result: RefCell<Option<Error>>,
    }

    impl ScriptedBackend {
        fn new(script: Vec<Attempt>) -> Self {
            Self {
                script: RefCell::new(script.into()),
                ..Default::default()
            }
        }
        fn requests(&self) -> Vec<TranslateRequest> {
            self.requests.borrow().clone()
        }
    }

    impl Backend for ScriptedBackend {
        async fn require_history(&self) -> Result<(), Error> {
            self.preflights.set(self.preflights.get() + 1);
            assert!(
                self.requests.borrow().is_empty(),
                "preflight after a request"
            );
            match self.preflight_result.borrow_mut().take() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
        async fn attempt(&self, req: &TranslateRequest, _: &mut Progress) -> Attempt {
            self.requests.borrow_mut().push(req.clone());
            self.script
                .borrow_mut()
                .pop_front()
                .expect("script exhausted")
        }
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

    fn job(items: Vec<Item<T>>) -> Job<T> {
        Job {
            base: TranslateRequest {
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
        code: i32,
        out: String,
        err: String,
    }

    async fn run(backend: &ScriptedBackend, job: Job<T>, split: SplitFn<'_, T>) -> Ran {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut progress = Progress {
            enabled: false,
            dirty: false,
            prefix: String::new(),
        };
        let code = drive(backend, job, split, &mut out, &mut err, &mut progress).await;
        Ran {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
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
        *b.preflight_result.borrow_mut() = Some(Error::HistoryUnsupported);
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

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stdout_write_error_aborts() {
        let b = ScriptedBackend::new(vec![stop_done("A"), stop_done("B")]);
        let mut err = Vec::new();
        let mut progress = Progress {
            enabled: false,
            dirty: false,
            prefix: String::new(),
        };
        let code = drive(
            &b,
            job(units(&["a", "b"])),
            &no_split,
            &mut FailingWriter,
            &mut err,
            &mut progress,
        )
        .await;
        let err = String::from_utf8(err).unwrap();
        assert_eq!(code, 1);
        assert!(err.contains("error: cannot write stdout: "), "{err}");
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
        assert!(r.err.contains("plamo-translate daemon install"));
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
        let lines: Vec<HistoryEntry> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
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
        use std::io::{BufRead, BufReader};
        let dir = tempfile::Builder::new()
            .prefix("pt")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("d.sock");
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
        let mut progress = Progress {
            enabled: false,
            dirty: false,
            prefix: String::new(),
        };
        ClientBackend(&client)
            .attempt(&TranslateRequest::default(), &mut progress)
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
        let mut progress = Progress {
            enabled: false,
            dirty: false,
            prefix: String::new(),
        };
        let a = ClientBackend(&client)
            .attempt(&TranslateRequest::default(), &mut progress)
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
}
