//! What a run tells its caller: warnings as they happen, progress, and the final `Report`.
//! Nothing here prints; the caller decides how (and whether) to render it.

use plamo_client::ErrorCode;
use std::fmt;
use std::future::Future;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// "K/M", or "K/M.s" for a part of a split piece.
    pub piece: String,
    pub kind: WarningKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningKind {
    Truncated,
    Empty,
    MarkersLost,
    Structure,
    Inline,
    NotHistory,
    /// Reported at most once per run; later pieces are not appended.
    HistoryAppend,
}

impl WarningKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WarningKind::Truncated => "truncated",
            WarningKind::Empty => "empty",
            WarningKind::MarkersLost => "markers_lost",
            WarningKind::Structure => "structure",
            WarningKind::Inline => "inline",
            WarningKind::NotHistory => "not_history",
            WarningKind::HistoryAppend => "history_append",
        }
    }

    /// The piece's output is not a full translation (truncated or source kept).
    pub fn degrades(self) -> bool {
        matches!(
            self,
            WarningKind::Truncated | WarningKind::Empty | WarningKind::MarkersLost
        )
    }

    /// Says nothing about the output text, so a caller may hide it by default.
    pub fn informational(self) -> bool {
        matches!(self, WarningKind::NotHistory)
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            WarningKind::HistoryAppend => f.write_str(&self.message),
            _ => write!(f, "piece {}: {}", self.piece, self.message),
        }
    }
}

#[derive(Debug)]
pub struct Stop {
    /// The first piece whose output is incomplete (for a write failure between pieces, the
    /// next one); `None` only when the history preflight failed.
    pub piece: Option<String>,
    pub cause: StopCause,
}

#[derive(Debug)]
pub enum StopCause {
    /// Includes the per-event timeout of `ClientBackend` as `Error::Io` of kind `TimedOut`.
    Client(plamo_client::Error),
    Daemon {
        code: ErrorCode,
        message: String,
    },
    Output(std::io::Error),
}

pub fn error_code_name(code: &ErrorCode) -> &'static str {
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

impl fmt::Display for StopCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopCause::Client(e) => write!(f, "{e}"),
            StopCause::Daemon { code, message } => {
                write!(f, "{}: {message}", error_code_name(code))
            }
            StopCause::Output(e) => write!(f, "cannot write output: {e}"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub pieces: usize,
    /// Top-level pieces with at least one degrading warning.
    pub degraded: usize,
    /// In emission order; the same warnings the sink received.
    pub warnings: Vec<Warning>,
    pub stop: Option<Stop>,
}

impl Report {
    pub fn complete(&self) -> bool {
        self.stop.is_none()
    }

    pub fn history_append_failed(&self) -> bool {
        self.warnings
            .iter()
            .any(|w| w.kind == WarningKind::HistoryAppend)
    }

    pub fn clean(&self) -> bool {
        self.complete() && self.degraded == 0 && !self.history_append_failed()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProgressEvent {
    /// Sent before each request; `chars` counts the text sent.
    Piece {
        label: String,
        index: usize,
        total: usize,
        chars: usize,
    },
    Stage {
        stage: String,
        done_bytes: Option<u64>,
        total_bytes: Option<u64>,
    },
    /// Cumulative chars received in the current request, one event per daemon delta.
    Generated { chars: usize },
}

// The methods return `impl Future + Send` rather than being `async fn` so that the
// future of `translate` is `Send` for any `Send` sink (servers spawn it on a runtime).
pub trait Sink: Send {
    /// Appends output text in document order; an error stops the run with `StopCause::Output`.
    fn write(&mut self, text: &str) -> impl Future<Output = std::io::Result<()>> + Send;
    fn warn(&mut self, warning: &Warning) -> impl Future<Output = ()> + Send;
    fn progress(&mut self, event: &ProgressEvent) -> impl Future<Output = ()> + Send;
}
