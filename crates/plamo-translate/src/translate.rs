use crate::{EXIT_INTERRUPTED, ExitCode};
use clap::Args;
use futures::StreamExt;
use plamo_client::{
    Client, Config, Error, ErrorCode, Event, GlossaryEntry, Status, TranslateRequest,
    merge_glossaries, parse_glossary, resolve_target,
};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct TranslateArgs {
    /// Text to translate (read from stdin when omitted)
    text: Option<String>,
    /// Target language (default: from config, "auto" picks en/ja)
    #[arg(short = 't', long = "to")]
    to: Option<String>,
    /// Source language
    #[arg(short = 's', long = "from")]
    from: Option<String>,
    #[arg(long, conflicts_with = "context_file")]
    context: Option<String>,
    #[arg(long, value_name = "FILE")]
    context_file: Option<PathBuf>,
    /// TSV glossary file (repeatable)
    #[arg(long, value_name = "FILE")]
    glossary: Vec<PathBuf>,
    /// polite or plain (target ja only; validated by the daemon)
    #[arg(long)]
    form: Option<String>,
    #[arg(long)]
    no_stream: bool,
    /// Print the final done event as one JSON line
    #[arg(long)]
    json: bool,
}

fn usage_error(msg: impl std::fmt::Display) -> i32 {
    eprintln!("error: {msg}");
    ExitCode::Usage as i32
}

fn read_glossary(path: &Path) -> Result<Vec<GlossaryEntry>, String> {
    let s = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read glossary {}: {e}", path.display()))?;
    parse_glossary(&s).map_err(|e| format!("{}: {e}", path.display()))
}

fn build_request(config: &Config, args: TranslateArgs) -> Result<TranslateRequest, String> {
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
    let context = match args.context_file {
        Some(p) => Some(
            std::fs::read_to_string(&p)
                .map_err(|e| format!("cannot read context file {}: {e}", p.display()))?,
        ),
        None => args.context,
    };
    let mut lists = Vec::new();
    if let Some(p) = &config.glossary_file
        && p.exists()
    {
        lists.push(read_glossary(p)?);
    }
    for p in &args.glossary {
        lists.push(read_glossary(p)?);
    }
    let target_lang = match args.to {
        Some(t) => t,
        None => resolve_target(&text, &config.default_target),
    };
    Ok(TranslateRequest {
        text,
        target_lang,
        source_lang: args.from,
        context,
        glossary: merge_glossaries(lists),
        form: args.form,
        stream: !args.no_stream,
        max_tokens: None,
    })
}

fn report_error(err: &Error) -> i32 {
    match err {
        Error::DaemonUnreachable {
            socket_path,
            source,
        } => {
            eprintln!(
                "error: cannot reach daemon at {}: {source}\nhint: run `plamo-translate daemon install` to set it up",
                socket_path.display()
            );
            ExitCode::Unreachable as i32
        }
        other => {
            eprintln!("error: {other}");
            ExitCode::Translation as i32
        }
    }
}

struct Progress {
    enabled: bool,
    dirty: bool,
}

impl Progress {
    fn show(&mut self, stage: &str, done: Option<u64>, total: Option<u64>) {
        if !self.enabled {
            return;
        }
        let line = match (stage, done, total) {
            ("downloading", Some(d), Some(t)) if t > 0 => format!("downloading {}%", d * 100 / t),
            ("downloading", ..) => "downloading".to_string(),
            _ => "loading model…".to_string(),
        };
        eprint!("\r\x1b[K{line}");
        self.dirty = true;
    }

    fn clear(&mut self) {
        if self.dirty {
            eprint!("\r\x1b[K");
            self.dirty = false;
        }
    }
}

fn done_json(
    text: &str,
    usage: &Option<plamo_client::Usage>,
    model: &Option<String>,
    revision: &Option<String>,
) -> String {
    serde_json::json!({
        "type": "done",
        "text": text,
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
    let req = match build_request(config, args) {
        Ok(r) => r,
        Err(m) => return usage_error(m),
    };
    tokio::select! {
        code = translate(client, &req, json, no_stream) => code,
        _ = tokio::signal::ctrl_c() => EXIT_INTERRUPTED,
    }
}

async fn translate(client: &Client, req: &TranslateRequest, json: bool, no_stream: bool) -> i32 {
    let mut events = match client.translate(req).await {
        Ok(s) => s,
        Err(e) => return report_error(&e),
    };
    let mut progress = Progress {
        enabled: std::io::stderr().is_terminal(),
        dirty: false,
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
                usage,
                model,
                revision,
            } => {
                progress.clear();
                if json {
                    write(
                        &format!("{}\n", done_json(&text, &usage, &model, &revision)),
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
                return ExitCode::Ok as i32;
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

fn report_daemon_error(code: &ErrorCode, message: &str) -> i32 {
    eprintln!("error: {}: {message}", snake_case(&format!("{code:?}")));
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
        "model: {}\nrevision: {}\nloaded: {}\nuptime_secs: {}\nidle_timeout_secs: {}\nqueue_len: {}\nversion: {}\nprotocol: {}\n",
        s.model,
        s.revision,
        s.loaded,
        s.uptime_secs,
        s.idle_timeout_secs,
        s.queue_len,
        s.version,
        s.protocol
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
