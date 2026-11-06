use crate::{EXIT_INTERRUPTED, ExitCode};
use clap::Args;
use futures::StreamExt;
use plamo_client::{
    Client, Config, Error, ErrorCode, Event, GlossaryEntry, HistoryEntry, Status, TranslateRequest,
    append_history, history_entry, merge_glossaries, parse_glossary, parse_history, resolve_target,
};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct RequestOpts {
    /// Target language (default: from config, "auto" picks en/ja)
    #[arg(short = 't', long = "to")]
    pub to: Option<String>,
    /// Source language
    #[arg(short = 's', long = "from")]
    pub from: Option<String>,
    #[arg(long, conflicts_with = "context_file")]
    pub context: Option<String>,
    #[arg(long, value_name = "FILE")]
    pub context_file: Option<PathBuf>,
    /// TSV glossary file (repeatable)
    #[arg(long, value_name = "FILE")]
    pub glossary: Vec<PathBuf>,
    /// polite or plain (target ja only; validated by the daemon)
    #[arg(long)]
    pub form: Option<String>,
}

#[derive(Args)]
pub struct TranslateArgs {
    /// Text to translate (read from stdin when omitted)
    text: Option<String>,
    #[command(flatten)]
    opts: RequestOpts,
    #[arg(long)]
    no_stream: bool,
    /// Print the final done event as one JSON line
    #[arg(long)]
    json: bool,
    /// JSONL translation history: {"source": ..., "translation": ...} per line, oldest first
    #[arg(long, value_name = "FILE")]
    history: Option<PathBuf>,
    /// Append this translation to the --history file (skipped if truncated or empty)
    #[arg(long, requires = "history")]
    history_append: bool,
    /// Send only the last N history entries
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..), requires = "history")]
    history_last: Option<u32>,
}

/// Request options with their files read; `to` stays unresolved because
/// "auto" depends on the text.
pub(crate) struct LoadedOpts {
    pub to: Option<String>,
    pub from: Option<String>,
    pub context: Option<String>,
    pub glossary: Vec<GlossaryEntry>,
    pub form: Option<String>,
}

struct Prepared {
    req: TranslateRequest,
    /// The history file to append the result to (`--history-append`).
    record: Option<PathBuf>,
}

pub(crate) fn usage_error(msg: impl std::fmt::Display) -> i32 {
    eprintln!("error: {msg}");
    ExitCode::Usage as i32
}

fn read_glossary(path: &Path) -> Result<Vec<GlossaryEntry>, String> {
    let s = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read glossary {}: {e}", path.display()))?;
    parse_glossary(&s).map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn read_history(
    path: &Path,
    append: bool,
    last: Option<u32>,
) -> Result<Vec<HistoryEntry>, String> {
    if path.as_os_str() == "-" {
        return Err(
            "--history does not read stdin (stdin is reserved for TEXT); pass a file path".into(),
        );
    }
    let s = match std::fs::read_to_string(path) {
        Ok(s) => s,
        // the first --history-append of a document creates the file
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && append => String::new(),
        Err(e) => return Err(format!("cannot read history {}: {e}", path.display())),
    };
    let mut history = parse_history(&s).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(n) = last {
        let keep = usize::try_from(n).unwrap_or(usize::MAX);
        let drop = history.len().saturating_sub(keep);
        history.drain(..drop);
    }
    Ok(history)
}

pub(crate) fn load_opts(config: &Config, opts: RequestOpts) -> Result<LoadedOpts, String> {
    let context = match opts.context_file {
        Some(p) => Some(
            std::fs::read_to_string(&p)
                .map_err(|e| format!("cannot read context file {}: {e}", p.display()))?,
        ),
        None => opts.context,
    };
    let mut lists = Vec::new();
    if let Some(p) = &config.glossary_file
        && p.exists()
    {
        lists.push(read_glossary(p)?);
    }
    for p in &opts.glossary {
        lists.push(read_glossary(p)?);
    }
    Ok(LoadedOpts {
        to: opts.to,
        from: opts.from,
        context,
        glossary: merge_glossaries(lists),
        form: opts.form,
    })
}

fn build_request(config: &Config, args: TranslateArgs) -> Result<Prepared, String> {
    let text = match args.text {
        Some(t) => t,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("cannot read stdin: {e}"))?;
            buf
        }
    };
    if text.trim().is_empty() {
        return Err("no text given (pass TEXT or pipe stdin)".into());
    }
    let opts = load_opts(config, args.opts)?;
    let history = match &args.history {
        Some(p) => read_history(p, args.history_append, args.history_last)?,
        None => Vec::new(),
    };
    let record = args.history.filter(|_| args.history_append);
    let target_lang = match opts.to {
        Some(t) => t,
        None => resolve_target(&text, &config.default_target),
    };
    Ok(Prepared {
        req: TranslateRequest {
            text,
            target_lang,
            source_lang: opts.from,
            context: opts.context,
            history,
            glossary: opts.glossary,
            form: opts.form,
            stream: !args.no_stream,
            max_tokens: None,
        },
        record,
    })
}

pub(crate) fn report_error(err: &Error) -> i32 {
    write_error(&mut std::io::stderr(), err)
}

/// `report_error` to an arbitrary writer, so callers can capture the message.
pub(crate) fn write_error(w: &mut dyn Write, err: &Error) -> i32 {
    match err {
        Error::DaemonUnreachable {
            socket_path,
            source,
        } => {
            let _ = writeln!(
                w,
                "error: cannot reach daemon at {}: {source}\nhint: run `plamo-translate daemon install` to set it up",
                socket_path.display()
            );
            ExitCode::Unreachable as i32
        }
        other => {
            let _ = writeln!(w, "error: {other}");
            ExitCode::Translation as i32
        }
    }
}

pub(crate) struct Progress {
    pub(crate) enabled: bool,
    pub(crate) dirty: bool,
    pub(crate) prefix: String,
}

impl Progress {
    pub(crate) fn show(&mut self, stage: &str, done: Option<u64>, total: Option<u64>) {
        if !self.enabled {
            return;
        }
        let line = match (stage, done, total) {
            ("downloading", Some(d), Some(t)) if t > 0 => format!("downloading {}%", d * 100 / t),
            ("downloading", ..) => "downloading".to_string(),
            _ => "loading model…".to_string(),
        };
        self.line(&line);
    }

    pub(crate) fn line(&mut self, s: &str) {
        if !self.enabled {
            return;
        }
        eprint!("\r\x1b[K{}{s}", self.prefix);
        self.dirty = true;
    }

    pub(crate) fn clear(&mut self) {
        if self.dirty {
            eprint!("\r\x1b[K");
            self.dirty = false;
        }
    }
}

fn done_json(
    text: &str,
    finish_reason: &Option<String>,
    usage: &Option<plamo_client::Usage>,
    model: &Option<String>,
    revision: &Option<String>,
) -> String {
    serde_json::json!({
        "type": "done",
        "text": text,
        "finish_reason": finish_reason,
        "usage": usage.as_ref().map(|u| serde_json::json!({
            "input_tokens": u.input_tokens,
            "output_tokens": u.output_tokens,
        })),
        "model": model,
        "revision": revision,
    })
    .to_string()
}

pub async fn run(client: &Client, config: &Config, args: TranslateArgs) -> i32 {
    let json = args.json;
    let no_stream = args.no_stream;
    let prepared = match build_request(config, args) {
        Ok(p) => p,
        Err(m) => return usage_error(m),
    };
    tokio::select! {
        code = translate(client, &prepared, json, no_stream) => code,
        _ = tokio::signal::ctrl_c() => EXIT_INTERRUPTED,
    }
}

async fn translate(client: &Client, prepared: &Prepared, json: bool, no_stream: bool) -> i32 {
    let req = &prepared.req;
    // Client::translate only checks the daemon when history is sent; an append-only run
    // must still fail on a daemon that cannot take the history it is building.
    if prepared.record.is_some()
        && req.history.is_empty()
        && let Err(e) = client.require_history().await
    {
        return report_error(&e);
    }
    let mut events = match client.translate(req).await {
        Ok(s) => s,
        Err(e) => return report_error(&e),
    };
    let mut progress = Progress {
        enabled: std::io::stderr().is_terminal(),
        dirty: false,
        prefix: String::new(),
    };
    let stdout = std::io::stdout();
    let mut last_byte: Option<u8> = None;
    let write = |s: &str, last: &mut Option<u8>| {
        let mut out = stdout.lock();
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
        if let Some(b) = s.as_bytes().last() {
            *last = Some(*b);
        }
    };
    while let Some(item) = events.next().await {
        let event = match item {
            Ok(e) => e,
            Err(e) => {
                progress.clear();
                return report_error(&e);
            }
        };
        match event {
            Event::Progress {
                stage,
                done_bytes,
                total_bytes,
            } => progress.show(&stage, done_bytes, total_bytes),
            Event::Delta { text } => {
                progress.clear();
                if !json && !no_stream {
                    write(&text, &mut last_byte);
                }
            }
            Event::Done {
                text,
                finish_reason,
                usage,
                model,
                revision,
            } => {
                progress.clear();
                if json {
                    write(
                        &format!(
                            "{}\n",
                            done_json(&text, &finish_reason, &usage, &model, &revision)
                        ),
                        &mut last_byte,
                    );
                } else {
                    if no_stream {
                        write(&text, &mut last_byte);
                    }
                    if last_byte != Some(b'\n') {
                        write("\n", &mut last_byte);
                    }
                }
                return match &prepared.record {
                    Some(path) => record_history(path, &req.text, finish_reason.as_deref(), &text),
                    None => ExitCode::Ok as i32,
                };
            }
            Event::Error { code, message } => {
                progress.clear();
                return report_daemon_error(&code, &message);
            }
            Event::Status(_) | Event::Unknown => {}
        }
    }
    ExitCode::Translation as i32
}

fn record_history(path: &Path, source: &str, finish_reason: Option<&str>, text: &str) -> i32 {
    match history_entry(source, finish_reason, text) {
        Ok(entry) => match append_history(path, &entry) {
            Ok(()) => ExitCode::Ok as i32,
            Err(e) => {
                eprintln!("error: cannot append to history {}: {e}", path.display());
                ExitCode::Translation as i32
            }
        },
        Err(reason) => {
            eprintln!("warning: translation not added to history: {reason}");
            ExitCode::Ok as i32
        }
    }
}

pub(crate) fn report_daemon_error(code: &ErrorCode, message: &str) -> i32 {
    write_daemon_error(&mut std::io::stderr(), code, message)
}

/// `report_daemon_error` to an arbitrary writer.
pub(crate) fn write_daemon_error(w: &mut dyn Write, code: &ErrorCode, message: &str) -> i32 {
    let _ = writeln!(w, "error: {}: {message}", snake_case(&format!("{code:?}")));
    ExitCode::Translation as i32
}

fn snake_case(pascal: &str) -> String {
    let mut out = String::new();
    for (i, c) in pascal.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    out
}

pub(crate) fn format_status(s: &Status) -> String {
    format!(
        "model: {}\nrevision: {}\nloaded: {}\nuptime_secs: {}\nidle_timeout_secs: {}\nqueue_len: {}\nversion: {}\nprotocol: {}\nfeatures: {}\ntemplate_verified: {}\n",
        s.model,
        s.revision,
        s.loaded,
        s.uptime_secs,
        s.idle_timeout_secs,
        s.queue_len,
        s.version,
        s.protocol,
        match s.features.as_deref() {
            None | Some([]) => "-".to_owned(),
            Some(f) => f.join(", "),
        },
        s.template_verified
            .map_or_else(|| "-".to_owned(), |v| v.to_string()),
    )
}

pub async fn status(client: &Client) -> i32 {
    match client.status().await {
        Ok(s) => {
            print!("{}", format_status(&s));
            ExitCode::Ok as i32
        }
        Err(e) => report_error(&e),
    }
}

pub async fn pull(client: &Client) -> i32 {
    tokio::select! {
        code = load_model(client) => code,
        _ = tokio::signal::ctrl_c() => EXIT_INTERRUPTED,
    }
}

async fn load_model(client: &Client) -> i32 {
    let mut events = match client.load().await {
        Ok(s) => s,
        Err(e) => return report_error(&e),
    };
    let mut progress = Progress {
        enabled: std::io::stderr().is_terminal(),
        dirty: false,
        prefix: String::new(),
    };
    while let Some(item) = events.next().await {
        match item {
            Err(e) => {
                progress.clear();
                return report_error(&e);
            }
            Ok(Event::Progress {
                stage,
                done_bytes,
                total_bytes,
            }) => progress.show(&stage, done_bytes, total_bytes),
            Ok(Event::Done {
                model, revision, ..
            }) => {
                progress.clear();
                println!(
                    "model ready: {} @ {}",
                    model.as_deref().unwrap_or("-"),
                    revision.as_deref().unwrap_or("-")
                );
                return ExitCode::Ok as i32;
            }
            Ok(Event::Error { code, message }) => {
                progress.clear();
                return report_daemon_error(&code, &message);
            }
            Ok(Event::Delta { .. } | Event::Status(_) | Event::Unknown) => {}
        }
    }
    ExitCode::Translation as i32
}
