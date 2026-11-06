use crate::EXIT_INTERRUPTED;
use crate::ExitCode;
use crate::translate::{
    Progress, RequestOpts, load_opts, read_history, usage_error, write_daemon_error, write_error,
};
use clap::Args;
use plamo_client::{Client, Config, TranslateRequest, resolve_target};
use plamo_doc::{
    ClientBackend, Format, Job, Limits, NoteKind, ProgressEvent, Report, Sink, StopCause, Warning,
    WarningKind, prepare, translate,
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
    let format = match args.format {
        FormatArg::Auto => None,
        FormatArg::Text => Some(Format::Text),
        FormatArg::Markdown => Some(Format::Markdown),
    };
    let doc = prepare(&input, format, &Limits::default());
    let auto = doc.format_detected();

    let mut stderr = std::io::stderr();
    if auto {
        let name = match doc.format() {
            Format::Text => "text",
            Format::Markdown => "markdown",
        };
        let _ = writeln!(
            stderr,
            "note: input format: {name} (auto-detected; use --format to override)"
        );
    }
    for note in doc.notes() {
        let line = note.line;
        match note.kind {
            NoteKind::IndentedCode if auto => {
                let _ = writeln!(
                    stderr,
                    "warning: line {line}: indented block left untranslated as code (use --format text if it is prose)"
                );
            }
            NoteKind::IndentedCode => {}
            NoteKind::HtmlText => {
                let _ = writeln!(stderr, "warning: line {line}: HTML block left untranslated");
            }
            NoteKind::ReservedMarker => {
                let _ = writeln!(
                    stderr,
                    "warning: line {line}: block left untranslated: contains reserved marker text"
                );
            }
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
                eprintln!("error: cannot write stdout: {e}");
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
        Some(report) => finish(&report, &mut sink.stderr),
        None => EXIT_INTERRUPTED,
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
        let prefix = match warning.kind {
            WarningKind::HistoryAppend => "error",
            _ => "warning",
        };
        let _ = writeln!(self.stderr, "{prefix}: {warning}");
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

/// Prints the trailing error lines of a finished run and returns the exit code.
fn finish(report: &Report, stderr: &mut dyn Write) -> i32 {
    match &report.stop {
        Some(stop) => {
            let code = match &stop.cause {
                StopCause::Client(e) => write_error(stderr, e),
                StopCause::Daemon { code, message } => write_daemon_error(stderr, code, message),
                StopCause::Output(e) => {
                    let _ = writeln!(stderr, "error: cannot write stdout: {e}");
                    ExitCode::Translation as i32
                }
            };
            if let Some(label) = &stop.piece {
                let _ = writeln!(
                    stderr,
                    "error: stopped at piece {label}; stdout holds everything translated before it"
                );
            }
            code
        }
        None if report.degraded > 0 => {
            let _ = writeln!(
                stderr,
                "error: {} of {} pieces were not fully translated; see the warnings above",
                report.degraded, report.pieces
            );
            ExitCode::Translation as i32
        }
        None if report.history_append_failed() => ExitCode::Translation as i32,
        None => ExitCode::Ok as i32,
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
}
