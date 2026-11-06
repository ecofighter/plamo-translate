mod driver;
mod history;
mod segment;

use crate::EXIT_INTERRUPTED;
use crate::ExitCode;
use crate::translate::{Progress, RequestOpts, load_opts, read_history, usage_error};
use clap::Args;
use driver::{ClientBackend, DocUnit, Item, Job, Outcome, UnitError, UnitIssue, drive};
use plamo_client::{Client, Config, TranslateRequest, resolve_target};
use segment::{Format, FormatArg, Issue, Limits, NoteKind, Segment};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

#[derive(Args)]
pub struct DocArgs {
    #[command(flatten)]
    opts: RequestOpts,
    /// Input format
    #[arg(long, value_enum, default_value_t = segment::FormatArg::Auto)]
    format: segment::FormatArg,
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

impl DocUnit for segment::Unit {
    fn source(&self) -> String {
        segment::Unit::source(self).to_owned()
    }

    fn text(&self) -> String {
        segment::Unit::text(self)
    }

    fn finish(&self, raw: &str) -> Result<Outcome, UnitError> {
        match segment::Unit::finish(self, raw) {
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
            Err(segment::FinishError::Empty) => Err(UnitError::Empty),
            Err(segment::FinishError::Placeholders) => Err(UnitError::Markers),
        }
    }

    fn history_pair(&self, raw: &str) -> Option<(String, String)> {
        segment::Unit::history_pair(self, raw)
    }
}

fn into_item(segment: Segment) -> Item<segment::Unit> {
    match segment {
        Segment::Keep(s) => Item::Keep(s),
        Segment::Translate(u) => Item::Unit(u),
    }
}

fn split_adapter(unit: &segment::Unit) -> Option<Vec<Item<segment::Unit>>> {
    segment::split_unit(unit).map(|segments| segments.into_iter().map(into_item).collect())
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
    let auto = args.format == FormatArg::Auto;
    let format = match args.format {
        FormatArg::Auto => segment::detect_format(&input),
        FormatArg::Text => Format::Text,
        FormatArg::Markdown => Format::Markdown,
    };
    let split = segment::split(&input, format, &Limits::default());

    let mut stderr = std::io::stderr();
    if auto {
        let name = match format {
            Format::Text => "text",
            Format::Markdown => "markdown",
        };
        let _ = writeln!(
            stderr,
            "note: input format: {name} (auto-detected; use --format to override)"
        );
    }
    for note in &split.notes {
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

    let units: Vec<String> = split
        .segments
        .iter()
        .filter_map(|s| match s {
            Segment::Translate(u) => Some(u.text()),
            Segment::Keep(_) => None,
        })
        .collect();
    let mut stdout = std::io::stdout();
    if units.is_empty() {
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
        None => resolve_target(&units.join("\n"), &config.default_target),
    };
    let job = Job {
        base: TranslateRequest {
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
        items: split.segments.into_iter().map(into_item).collect(),
    };
    let mut progress = Progress {
        enabled: std::io::stderr().is_terminal(),
        dirty: false,
        prefix: String::new(),
    };
    let backend = ClientBackend(client);
    tokio::select! {
        code = drive(
            &backend,
            job,
            &split_adapter,
            &mut stdout,
            &mut stderr,
            &mut progress,
        ) => code,
        _ = tokio::signal::ctrl_c() => {
            progress.clear();
            EXIT_INTERRUPTED
        }
    }
}
