use crate::EXIT_INTERRUPTED;
use crate::ExitCode;
use crate::translate::{
    Progress, RequestOpts, load_opts, read_history, usage_error, write_daemon_error, write_error,
};
use clap::Args;
use plamo_client::{Client, Config, Error, TranslateRequest, resolve_target};
use plamo_doc::{
    ClientBackend, Format, Job, Limits, Note, NoteKind, ProgressEvent, Report, Sink, Stop,
    StopCause, Warning, WarningKind, prepare, translate,
};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, clap::ValueEnum)]
enum FormatArg {
    #[default]
    Auto,
    Text,
    Markdown,
}

#[derive(Args)]
pub struct DocArgs {
    #[command(flatten)]
    opts: RequestOpts,
    /// Input format
    #[arg(long, value_enum, default_value_t = FormatArg::Auto)]
    format: FormatArg,
    /// JSONL history that seeds the run: {"source": ..., "translation": ...} per line, oldest first
    #[arg(long, value_name = "FILE")]
    history: Option<PathBuf>,
    /// Append each translated piece to the --history file as it completes
    #[arg(long, requires = "history")]
    history_append: bool,
    /// Load only the last N entries of the --history file
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..), requires = "history")]
    history_last: Option<u32>,
    // 8000 bytes is about one or two previous pieces. Each 1,000 history tokens adds about
    // 1.8 s of prefill per piece (about 0.23 tokens per byte), so the default costs 2-3 s
    // per piece. The quality benefit was not measured.
    /// History sent with each piece, in UTF-8 bytes (the newest pair is always sent); 0 disables history
    #[arg(long, value_name = "BYTES", default_value_t = 8000)]
    history_bytes: usize,
}

pub async fn run(client: &Client, config: &Config, args: DocArgs) -> i32 {
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        return usage_error(format!("cannot read stdin: {e}"));
    }
    if input.trim().is_empty() {
        return usage_error("no text given (pipe the document to stdin)");
    }
    let opts = match load_opts(config, args.opts) {
        Ok(o) => o,
        Err(m) => return usage_error(m),
    };
    let seed = match &args.history {
        Some(p) if p.as_os_str() == "-" => {
            return usage_error(
                "--history does not read stdin (stdin is reserved for the document); pass a file path",
            );
        }
        Some(p) => match read_history(p, args.history_append, args.history_last) {
            Ok(h) => h,
            Err(m) => return usage_error(m),
        },
        None => Vec::new(),
    };
    let record = args.history.filter(|_| args.history_append);
    let doc = prepare(&input, into_format(args.format), &Limits::default());
    let auto = doc.format_detected();

    let mut stderr = std::io::stderr();
    if auto {
        render_format_note(&mut stderr, doc.format());
    }
    for note in doc.notes() {
        if note.kind != NoteKind::IndentedCode || auto {
            render_note(&mut stderr, note);
        }
    }

    let mut stdout = std::io::stdout();
    // Before resolving the target: with nothing to translate, "auto" has no text to look at.
    if doc.pieces() == 0 {
        let written = stdout
            .write_all(input.as_bytes())
            .and_then(|()| stdout.flush());
        return match written {
            Ok(()) => ExitCode::Ok as i32,
            Err(e) => {
                let _ = writeln!(stderr, "error: cannot write stdout: {e}");
                ExitCode::Translation as i32
            }
        };
    }

    let target_lang = match opts.to {
        Some(t) => t,
        None => resolve_target(&doc.translatable_text(), &config.default_target),
    };
    let job = Job {
        request: TranslateRequest {
            text: String::new(),
            target_lang,
            source_lang: opts.from,
            context: opts.context,
            history: Vec::new(),
            glossary: opts.glossary,
            form: opts.form,
            stream: true,
            max_tokens: None,
        },
        seed,
        history_bytes: args.history_bytes,
        record,
    };
    let mut sink = CliSink {
        stdout,
        stderr,
        progress: Progress {
            enabled: std::io::stderr().is_terminal(),
            dirty: false,
            prefix: String::new(),
        },
    };
    let backend = ClientBackend::new(client);
    let report = {
        let translating = translate(&backend, doc, job, &mut sink);
        tokio::select! {
            report = translating => Some(report),
            _ = tokio::signal::ctrl_c() => None,
        }
    };
    sink.progress.clear();
    match report {
        Some(report) => {
            render_trailer(&mut sink.stderr, &report);
            exit_code(&report)
        }
        None => EXIT_INTERRUPTED,
    }
}

fn into_format(arg: FormatArg) -> Option<Format> {
    match arg {
        FormatArg::Auto => None,
        FormatArg::Text => Some(Format::Text),
        FormatArg::Markdown => Some(Format::Markdown),
    }
}

struct CliSink {
    stdout: std::io::Stdout,
    stderr: std::io::Stderr,
    progress: Progress,
}

impl Sink for CliSink {
    async fn write(&mut self, text: &str) -> std::io::Result<()> {
        self.stdout
            .write_all(text.as_bytes())
            .and_then(|()| self.stdout.flush())
    }

    async fn warn(&mut self, warning: &Warning) {
        self.progress.clear();
        render_warning(&mut self.stderr, warning);
    }

    async fn progress(&mut self, event: &ProgressEvent) {
        match event {
            ProgressEvent::Piece { label, .. } => {
                self.progress.prefix.clear();
                self.progress.line(&format!("[{label}]"));
                self.progress.prefix = format!("[{label}] ");
            }
            ProgressEvent::Stage {
                stage,
                done_bytes,
                total_bytes,
            } => self.progress.show(stage, *done_bytes, *total_bytes),
            ProgressEvent::Generated { chars } => self.progress.line(&format!("{chars} chars")),
        }
    }
}

fn exit_code(report: &Report) -> i32 {
    match &report.stop {
        Some(Stop {
            cause: StopCause::Client(Error::DaemonUnreachable { .. }),
            ..
        }) => ExitCode::Unreachable as i32,
        Some(_) => ExitCode::Translation as i32,
        None if report.degraded > 0 || report.history_append_failed() => {
            ExitCode::Translation as i32
        }
        None => ExitCode::Ok as i32,
    }
}

fn render_format_note(w: &mut dyn Write, format: Format) {
    let _ = writeln!(
        w,
        "note: input format: {} (auto-detected; use --format to override)",
        format.as_str()
    );
}

fn render_note(w: &mut dyn Write, note: &Note) {
    let _ = writeln!(w, "warning: {note}");
}

fn render_warning(w: &mut dyn Write, warning: &Warning) {
    let prefix = match warning.kind {
        WarningKind::HistoryAppend => "error",
        _ => "warning",
    };
    let _ = writeln!(w, "{prefix}: {warning}");
}

fn render_stop(w: &mut dyn Write, stop: &Stop) {
    match &stop.cause {
        StopCause::Client(e) => {
            write_error(w, e);
        }
        StopCause::Daemon { code, message } => {
            write_daemon_error(w, code, message);
        }
        StopCause::Output(e) => {
            let _ = writeln!(w, "error: cannot write stdout: {e}");
        }
    }
    if let Some(label) = &stop.piece {
        let _ = writeln!(
            w,
            "error: stopped at piece {label}; stdout holds everything translated before it"
        );
    }
}

fn render_trailer(w: &mut dyn Write, report: &Report) {
    match &report.stop {
        Some(stop) => render_stop(w, stop),
        None if report.degraded > 0 => {
            let _ = writeln!(
                w,
                "error: {} of {} pieces were not fully translated; see the warnings above",
                report.degraded, report.pieces
            );
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_arg_defaults_to_auto() {
        use clap::ValueEnum;
        assert_eq!(FormatArg::default(), FormatArg::Auto);
        let names: Vec<String> = FormatArg::value_variants()
            .iter()
            .map(|v| v.to_possible_value().unwrap().get_name().to_owned())
            .collect();
        assert_eq!(names, ["auto", "text", "markdown"]);
    }

    fn rendered(f: impl FnOnce(&mut dyn Write)) -> String {
        let mut out = Vec::new();
        f(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn renders_every_warning_kind_and_stop() {
        use plamo_client::ErrorCode;
        use plamo_doc::NoteKind;

        let warn = |kind| Warning {
            piece: "2/3".into(),
            kind,
            message: "msg".into(),
        };
        for (kind, line) in [
            (WarningKind::Truncated, "warning: piece 2/3: msg\n"),
            (WarningKind::Empty, "warning: piece 2/3: msg\n"),
            (WarningKind::MarkersLost, "warning: piece 2/3: msg\n"),
            (WarningKind::Structure, "warning: piece 2/3: msg\n"),
            (WarningKind::Inline, "warning: piece 2/3: msg\n"),
            (WarningKind::NotHistory, "warning: piece 2/3: msg\n"),
            (WarningKind::HistoryAppend, "error: msg\n"),
        ] {
            assert_eq!(rendered(|w| render_warning(w, &warn(kind))), line);
        }

        assert_eq!(
            rendered(|w| render_format_note(w, Format::Markdown)),
            "note: input format: markdown (auto-detected; use --format to override)\n"
        );
        for (kind, line) in [
            (
                NoteKind::IndentedCode,
                "warning: line 7: indented block left untranslated as code (use --format text if it is prose)\n",
            ),
            (
                NoteKind::HtmlText,
                "warning: line 7: HTML block left untranslated\n",
            ),
            (
                NoteKind::ReservedMarker,
                "warning: line 7: block left untranslated: contains reserved marker text\n",
            ),
        ] {
            let note = Note { line: 7, kind };
            assert_eq!(rendered(|w| render_note(w, &note)), line);
        }

        let stop = |cause, piece: Option<&str>| Stop {
            piece: piece.map(str::to_owned),
            cause,
        };
        let tail = "error: stopped at piece 2/3; stdout holds everything translated before it\n";
        let unreachable = || {
            StopCause::Client(Error::DaemonUnreachable {
                socket_path: "/run/pt.sock".into(),
                source: std::io::Error::other("refused"),
            })
        };
        assert_eq!(
            rendered(|w| render_stop(w, &stop(unreachable(), Some("2/3")))),
            format!(
                "error: cannot reach daemon at /run/pt.sock: refused\nhint: run `plamo-translate daemon install` to set it up\n{tail}"
            )
        );
        assert_eq!(
            rendered(|w| render_stop(
                w,
                &stop(StopCause::Client(Error::Protocol("bad".into())), None)
            )),
            "error: protocol error: bad\n"
        );
        assert_eq!(
            rendered(|w| render_stop(
                w,
                &stop(
                    StopCause::Daemon {
                        code: ErrorCode::ContextTooLong,
                        message: "too long".into(),
                    },
                    Some("2/3"),
                )
            )),
            format!("error: context_too_long: too long\n{tail}")
        );
        assert_eq!(
            rendered(|w| render_stop(
                w,
                &stop(
                    StopCause::Output(std::io::Error::other("closed")),
                    Some("2/3")
                )
            )),
            format!("error: cannot write stdout: closed\n{tail}")
        );

        let degraded = Report {
            pieces: 3,
            degraded: 1,
            ..Report::default()
        };
        assert_eq!(
            rendered(|w| render_trailer(w, &degraded)),
            "error: 1 of 3 pieces were not fully translated; see the warnings above\n"
        );
    }

    #[test]
    fn exit_codes_follow_the_stop_cause() {
        let stopped = |cause| Report {
            stop: Some(Stop { piece: None, cause }),
            ..Report::default()
        };
        assert_eq!(
            exit_code(&stopped(StopCause::Client(Error::DaemonUnreachable {
                socket_path: "/s".into(),
                source: std::io::Error::other("x"),
            }))),
            3
        );
        assert_eq!(
            exit_code(&stopped(StopCause::Client(Error::Protocol("x".into())))),
            1
        );
        assert_eq!(
            exit_code(&Report {
                degraded: 1,
                ..Report::default()
            }),
            1
        );
        assert_eq!(exit_code(&Report::default()), 0);
    }
}
