# 付録 A: 設計 S1 (r2) — plamo-doc ライブラリ、CLI の --report/--verbose、MCP translate_document

> 本計画書 (../2026-11-06-doc-integration.md) の一部。本文と食い違う箇所は本文が優先する。原文 (英語) のまま収録。"SCRATCH" は設計時の検証用クローンで、現存しない。

## 1. Conclusion

**Q1 Crate boundary.** New workspace crate `crates/plamo-doc` (package `plamo-doc`, lib `plamo_doc`). It takes `segment/` (whole directory incl. `testdata/`), `driver.rs` and `history.rs` from `crates/plamo-translate/src/doc/`. Dependencies: `plamo-client`, `pulldown-cmark 0.13 (default-features = false)`, `futures`, `tokio` with only the `time` feature (for the optional per-event timeout in `ClientBackend`; r1 had no tokio). No clap, serde_json, schemars, rmcp. Stays in the CLI (`crates/plamo-translate/src/doc.rs`, one file): `DocArgs` (clap), `FormatArg` (clap `ValueEnum`) and its test `format_arg_defaults_to_auto`, stdin reading, `--history` file loading (`read_history`, `load_opts`), TTY `Progress`, ctrl-c `select!`, exit-code mapping, all stderr text rendering, `--report`/`--verbose`. No daemon (Python) change is required.

**Q2 Library API.** Concrete types, no writers, no exit codes, no printing. `prepare()` splits; `translate()` drives and reports through a caller-supplied `Sink` and returns a structured `Report { pieces, degraded, warnings, stop }`. `Sink` and `Backend` are public traits whose methods are written as `fn ... -> impl Future<Output = ..> + Send` (not `async fn`: the `async_fn_in_trait` lint fires under `-D warnings` for public traits [V: experiment, section 7]); with that form the `translate()` future is `Send` whenever `S: Send` and `B: Sync` [V: experiment]. `ClientBackend` optionally applies a per-event timeout (`with_event_timeout`), which is the MCP inactivity mechanism. Cancellation = drop the future. Public surface is exactly the list in G-2; the segmenter internals (`Segment`, `Unit`, `Finished`, `Issue`, `FinishError`, `split`, `split_unit`, `Split`) stay `pub(crate)`.

**Q3 CLI machine-readable output.** `plamo-translate doc --report json`: stderr becomes JSON Lines (types `format`, `note`, `warning`, `error`, `summary`); human lines and TTY progress are suppressed; stdout unchanged. On every exit path that passes through `doc::run` the last stderr line is `{"type":"summary","v":1,...}`; callers MUST treat an absent summary (clap/config exit 2, kill, timeout, truncated capture) as failure. New `--verbose` (long form only): by default the CLI hides exactly two purely informational lines, `note: input format: ...` and `warning: piece K/M: not used as history: ...`; everything describing the output (untranslated blocks, truncated/empty/markers-lost, structure/inline changes) and every `error:` line prints by default. r1 also hid structure/inline warnings; r2 shows them (C-1). This is a documented-contract change (README says the format note prints under auto); one existing test invocation gains `--verbose`. `--verbose` is ignored under `--report json`. The plugin's current `lib/cli.js` parses the single-call `--json --no-stream` path, which is untouched.

**Q4 MCP tool.** `translate_document`: `text` XOR `input_path` (absolute), optional `output_path` (absolute; result then a write summary), `overwrite` (default false), `format`, `target_lang` (required), `source_lang`, `context`, `glossary`, `form`, `history_path`, `history_append`, `history_bytes` (8000). Text mode success: `content[0]` translation verbatim, optional `content[1]` `note:`/`warning:` lines; degraded pieces = success with notes; a stop = `isError` with the partial translation returned in `content[1]` (r1 discarded it; B-6). File mode writes `<output_path>.part` and renames on completion; `.part` stays on failure. Timeouts: `PLAMO_TRANSLATE_MCP_TIMEOUT_SECS` (default 300 s) is the per-event inactivity limit (implemented in `ClientBackend::with_event_timeout`), and a new `PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS` (default 3600 s) caps the whole call. The `translate` description points long/structured input to `translate_document`.

**Q5 Compatibility.** Factual correction: the installed binary `/etc/profiles/per-user/arakaki/bin/plamo-translate` → `/nix/store/gvqdplbrf1jcv7jrl62bv40d0vl4whmh-plamo-translate-0.1.0/bin/plamo-translate` **does list `doc`** in `--help` and rejects `--report` with exit 2 [V: ran `plamo-translate --help` and `doc --report json` today]. The earlier researcher's report (no `doc`) is outdated or was from another binary. Design: the workspace version is bumped to `0.2.0` in the same commit that adds `--report` (U4), so "`plamo-translate --version` ≥ 0.2.0" is equivalent to "`--report json` and `--verbose` exist" on every build. Fail-safe rule for programmatic callers: always pass `--report json`; a binary without it exits 2 with `error: unexpected argument '--report' found` and translates nothing (this also covers a hypothetical pre-`doc` binary, which would otherwise translate the word "doc", because clap rejects the unknown flag first [I]). `doc --help` exits 0 on old binaries and MUST NOT be used as a probe [V]. MCP clients detect the tool via `tools/list`.

## 2. Rationale

- New crate vs feature in plamo-client: plamo-client is the wire client; a 5.5k-line segmenter behind a feature mixes concerns. New crates need no nix list edits [V: nix/packages.nix:41-44]; no new third-party dependency (pulldown-cmark, futures, tokio, tempfile are in Cargo.lock) [V].
- MCP depending on the CLI crate would pull clap/libc/plist into the MCP binary and couple it to `ExitCode`/`Progress`. Rejected.
- Sink + Report instead of `out`/`err` writers: today diagnostics are rendered inside the driver, which MCP cannot consume; the Report also lets the CLI print a JSON summary from one place.
- Desugared trait methods with `+ Send`: required by the lint under the clippy gate and gives MCP a `Send` future for rmcp's handler; cost: test doubles must be `Sync` (use `Mutex`, not `RefCell`).
- Per-event timeout inside `ClientBackend` instead of an MCP-side watchdog: every daemon event (progress, delta, status, unknown, done, error) and the connect/preflight go through `tokio::time::timeout`, so nothing can be miscounted as idle; it is unit-testable with a silent socket; CLI leaves it `None`.
- `--report json` as a mode, not named `--json`: `doc --json` is asserted to exit 2 in `doc_driver_usage_errors_exit_2` [V] and README says `--json` does not exist for `doc`.
- Verbose scope: `Structure`/`Inline` warnings can be the final outcome of a piece (driver.rs:406-427 keeps the translation and warns when a structure issue appears in a split part or an unsplittable unit; inline issues are always kept) [V], so hiding them would make a document with damaged Markdown look clean; only lines that imply nothing about the output are verbose-only.
- MCP degraded = success with notes: `isError` drops `content[0]`; the `translate` tool already uses a second `note:` block [V].
- `.part` + rename: `overwrite` is honoured atomically; cancel/timeout leave a clearly named partial.
- Version bump in the `--report` commit: avoids builds that have `--report` but report 0.1.0 (A-2).

## 3. Alternatives considered and rejected

- `plamo-client` feature `doc`; MCP depends on the CLI crate — see rationale.
- `#[allow(async_fn_in_trait)]` on the public traits: hides the lint but gives no `Send` guarantee for rmcp; the desugared form costs one line per method.
- `Backend::attempt` returning an event stream: rewrites every `ScriptedBackend` script in 29 tests.
- MCP-side idle watchdog (`Arc<Mutex<Instant>>` + `sleep_until`): only sees sink calls, misses connect/preflight/unmapped events (B-2).
- `--report FILE`: hooks runtime may not read files; SIGINT skips the write.
- Unconditional trailing JSON summary on stderr; hiding degraded warnings/`K of M`/errors behind `--verbose`; hiding structure/inline warnings (r1) — rejected for the "must not look clean" reason.
- `-v` short flag: a later global `-v` would collide; long form only.
- Inline `history` array argument on `translate_document`: the model cannot reconstruct per-piece pairs; `history_path` gives continuity.
- Discarding the partial translation on an MCP text-mode stop (r1): minutes of work lost with no resume path.

## 4. Assumptions made

- The hooks runtime can pass file paths (it passes `--history FILE` today [V: claude-plugin/hooks/lib/cli.js]) but is not assumed to read files; the report travels on stderr.
- Claude Desktop runs the MCP server locally; whether a Desktop user can name a file path is unverified [I]. File mode is justified by Claude Code regardless; text mode is the Desktop path and its context cost (document in args and result) is stated in the tool description.
- `rmcp 3.8.0` `Peer<RoleServer>` is `Clone` and `notify_progress` is `async` [V: server.rs].
- The reviewer accepts the two driver-test assertion rewrites listed in U2 (CLI text that leaves the library).

## 5. Implications for implementation

Units in dependency order; one commit each; commands from the repository root via `nix develop -c`.

### U1 — Create `crates/plamo-doc` by moving the segmenter and history selection (no behaviour change)

Files:
- new `crates/plamo-doc/Cargo.toml`:
  ```toml
  [package]
  name = "plamo-doc"
  version.workspace = true
  edition.workspace = true
  license.workspace = true
  repository.workspace = true

  [dependencies]
  plamo-client = { path = "../plamo-client" }
  futures = "0.3"
  pulldown-cmark = { version = "0.13", default-features = false }
  tokio = { version = "1", features = ["time"] }

  [dev-dependencies]
  tempfile = "3"
  tokio = { version = "1", features = ["rt", "macros", "time", "net", "io-util"] }
  ```
- `git mv crates/plamo-translate/src/doc/segment crates/plamo-doc/src/segment` (incl. `testdata/`); `git mv .../doc/history.rs crates/plamo-doc/src/history.rs`.
- `segment/mod.rs`: delete `FormatArg` + `clap::ValueEnum` derive + test `format_arg_defaults_to_auto` (they move to the CLI in U3; until then keep a copy in `crates/plamo-translate/src/doc/mod.rs`). Add `impl Format { pub fn as_str(&self) -> &'static str }` ("text"/"markdown"); `impl NoteKind { pub fn as_str(&self) -> &'static str }` ("indented_code" | "html" | "reserved_marker"); `impl fmt::Display for Note` → exactly `line {line}: indented block left untranslated as code (use --format text if it is prose)` / `line {line}: HTML block left untranslated` / `line {line}: block left untranslated: contains reserved marker text`.
- `crates/plamo-doc/src/lib.rs`: `mod segment; mod history;` and `pub use segment::{Format, Limits, Note, NoteKind, detect_format};` only. For U1 the CLI still needs `split`, `split_unit`, `Segment`, `Unit`: export them `pub` temporarily with a `// removed in U2` comment; U2 makes them `pub(crate)`.
- `crates/plamo-translate/Cargo.toml`: remove `pulldown-cmark`; add `plamo-doc = { path = "../plamo-doc" }`. Commit the regenerated `Cargo.lock`.

Tests moved unchanged: segment 13+21+24+19+37 = 114 minus `format_arg_defaults_to_auto` = 113, history 1.

Completion: `cargo test --workspace --offline` green; `cargo test -p plamo-doc --offline 2>&1 | grep "test result"` → `114 passed`; plamo-translate unit tests → `58 passed` (172 − 114); `grep -rn pulldown crates/plamo-translate` → none.

### U2 — Move the driver into `plamo-doc` behind the structured API

Files: `git mv crates/plamo-translate/src/doc/driver.rs crates/plamo-doc/src/driver.rs`; new `crates/plamo-doc/src/report.rs`; edit `lib.rs`.

Public API (bodies in `driver.rs`/`report.rs`; re-exported from `lib.rs`):

```rust
use std::future::Future;

// report.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// "K/M" or "K/M.s" — exactly today's label.
    pub piece: String,
    pub kind: WarningKind,
    /// Exactly today's text after "warning: piece K/M: " (HistoryAppend: after "error: ").
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningKind {
    Truncated,     // "translation truncated (finish_reason: length)"                degrades
    Empty,         // "empty translation; left untranslated"                         degrades
    MarkersLost,   // "verbatim-block markers lost or reordered; left untranslated"  degrades
    Structure,     // "structure changed (expected X; got Y)"
    Inline,        // "inline elements changed (missing: ..; added: ..)"
    NotHistory,    // "not used as history: <why>"
    HistoryAppend, // "cannot append to history PATH: <io error>" (at most once per run)
}
impl WarningKind {
    pub fn as_str(self) -> &'static str; // "truncated" | "empty" | "markers_lost" | "structure" | "inline" | "not_history" | "history_append"
    pub fn degrades(self) -> bool;       // Truncated | Empty | MarkersLost
    pub fn informational(self) -> bool;  // NotHistory only (the CLI's verbose-only set; HistoryAppend is not informational)
}
impl fmt::Display for Warning { /* "piece {piece}: {message}"; HistoryAppend: "{message}" */ }

#[derive(Debug)]
pub struct Stop { pub piece: Option<String> /* None = preflight */, pub cause: StopCause }
#[derive(Debug)]
pub enum StopCause {
    Client(plamo_client::Error),  // includes the per-event timeout as Error::Io(kind TimedOut)
    Daemon { code: plamo_client::ErrorCode, message: String },
    Output(std::io::Error),
}
pub fn error_code_name(code: &ErrorCode) -> &'static str; // snake_case, same table as MCP code_name
impl fmt::Display for StopCause { /* Client(e) => "{e}"; Daemon => "{error_code_name}: {message}"; Output(e) => "cannot write output: {e}" */ }

#[derive(Debug, Default)]
pub struct Report {
    pub pieces: usize,            // M
    pub degraded: usize,          // K: top-level pieces with >= 1 degrading warning
    pub warnings: Vec<Warning>,   // emission order; the same objects the Sink received
    pub stop: Option<Stop>,
}
impl Report {
    pub fn complete(&self) -> bool { self.stop.is_none() }
    pub fn history_append_failed(&self) -> bool; // any HistoryAppend warning
    pub fn clean(&self) -> bool { self.complete() && self.degraded == 0 && !self.history_append_failed() }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProgressEvent {
    /// Before each request. `chars` = chars of the text sent.
    Piece { label: String, index: usize, total: usize, chars: usize },
    Stage { stage: String, done_bytes: Option<u64>, total_bytes: Option<u64> },
    /// Cumulative chars received in the current request; one event per daemon delta (no throttling in the library).
    Generated { chars: usize },
}

pub trait Sink: Send {
    /// Appends output text in document order. Err aborts the run with StopCause::Output.
    fn write(&mut self, text: &str) -> impl Future<Output = std::io::Result<()>> + Send;
    fn warn(&mut self, warning: &Warning) -> impl Future<Output = ()> + Send;
    fn progress(&mut self, event: &ProgressEvent) -> impl Future<Output = ()> + Send;
}

// driver.rs
#[derive(Debug)]
pub enum Attempt { Done { text: String, finish_reason: Option<String> }, ContextTooLong(String), Daemon { code: ErrorCode, message: String }, Client(plamo_client::Error) }

pub trait Backend: Sync {
    fn require_history(&self) -> impl Future<Output = Result<(), plamo_client::Error>> + Send;
    /// Forwards Stage/Generated to `sink.progress` for every daemon progress/delta event.
    fn attempt<S: Sink>(&self, req: &TranslateRequest, sink: &mut S) -> impl Future<Output = Attempt> + Send;
}

pub struct ClientBackend<'a> { client: &'a Client, event_timeout: Option<Duration> }
impl<'a> ClientBackend<'a> {
    pub fn new(client: &'a Client) -> Self;
    /// Each connect, the preflight, and each wait for the next daemon event is limited to `d`;
    /// expiry yields Attempt::Client(Error::Io(io::Error::new(ErrorKind::TimedOut, "no event from the daemon for {d:?}"))).
    pub fn with_event_timeout(self, d: Duration) -> Self;
}

pub struct Document { /* private */ }
/// `format: None` = auto-detect.
pub fn prepare(input: &str, format: Option<Format>, limits: &Limits) -> Document;
impl Document {
    pub fn format(&self) -> Format;
    pub fn format_detected(&self) -> bool;
    pub fn notes(&self) -> &[Note];
    pub fn pieces(&self) -> usize;
    /// Units' text() joined by "\n" — input for resolve_target. Empty when pieces() == 0.
    pub fn translatable_text(&self) -> String;
}

pub struct Job {
    /// target_lang, source_lang, context, glossary, form are used; text, history, max_tokens, stream are overwritten per request.
    pub request: TranslateRequest,
    pub seed: Vec<HistoryEntry>,
    pub history_bytes: usize,
    pub record: Option<PathBuf>,
}

/// Cancellation = drop the future. With pieces() == 0: writes the Keeps, no preflight, no daemon contact.
pub async fn translate<B: Backend, S: Sink>(backend: &B, document: Document, job: Job, sink: &mut S) -> Report;
```

Internals: keep the generic `drive<U: DocUnit, B, S>` (`pub(crate)`) with `DocUnit`, `Item`, `Entry`, `Ctx`, `max_tokens_for` as today. `Run` loses `out`/`err`, gains `warnings: Vec<Warning>` and `sink: &mut S`; each `run.warn(label, msg)` becomes `Warning{piece, kind, message}` pushed and sent; `write_error`/`write_daemon_error`/`stop()` become `Stop{piece, cause}`; `drive` emits `ProgressEvent::Piece` where it sets `progress.prefix` today (driver.rs:356-358); `ClientBackend::attempt` emits `Stage`/`Generated` where it calls `progress.show`/`progress.line` (driver.rs:86-94). Timeout wrapping: `tokio::time::timeout(d, self.client.translate_unchecked(req))`, `tokio::time::timeout(d, events.next())`, `tokio::time::timeout(d, self.client.require_history())` when `event_timeout` is `Some`. Retry/split/halving/record logic unchanged; the request bound per unit (2h+3) [V: driver.rs:323-324 comment] still holds, so no infinite loop exists.

Tests (29 driver tests move to `crates/plamo-doc/src/driver.rs`; harness changes; assertions as noted):
- Harness: `ScriptedBackend` uses `Mutex` instead of `RefCell`/`Cell` (must be `Sync`); `attempt<S: Sink>(&self, req, _: &mut S)`. `RecordingSink { out: String, err: String, events: Vec<ProgressEvent> }`: `warn` appends `format!("warning: {w}\n")`, HistoryAppend `format!("error: {w}\n")`. `run()` returns `Ran { report, out, err, code }` where `code` and the trailing lines come from a test-local `render(&Report) -> (i32, String)` that mirrors the CLI (`error: {cause}`, then `error: stopped at piece {label}; stdout holds everything translated before it` if piece is Some; else `error: {degraded} of {pieces} pieces were not fully translated; see the warnings above` when degraded > 0; code table as U3). The CLI's `exit_code`/rendering is authoritative; the harness copy is pinned against it by the U3 unit test `renders_every_warning_kind_and_stop` (same expected strings), which is the drift check.
- Assertions that change: `unreachable_exits_3`: `contains("plamo-translate daemon install")` → `matches!(r.report.stop, Some(Stop{cause: StopCause::Client(Error::DaemonUnreachable{..}), ..}))` (CLI hint covered by `cli_doc::doc_driver_unreachable_daemon_exits_3_with_install_hint`); `stdout_write_error_aborts`: `"error: cannot write stdout: "` → `"error: cannot write output: "` (CLI wording pinned in U3). All others verbatim.
- `client_backend_maps_events`, `client_backend_reports_unreachable`: pass a `RecordingSink`; assertions unchanged.
- New: `sink_receives_piece_and_generated_events`; `report_counts_degraded_once_per_piece` (data of `degraded_parts_count_their_piece_once`: `degraded == 1`, two `Truncated` warnings); `prepare_none_detects_format_and_reports_notes`; `translate_with_zero_pieces_writes_keeps_without_backend`; `client_backend_event_timeout_yields_timed_out` (a `UnixListener` that accepts and never writes; `with_event_timeout(50 ms)` → `Attempt::Client(Error::Io(e))` with `e.kind() == TimedOut`); `client_backend_event_timeout_resets_per_event` (fake writes a delta every 30 ms for 300 ms then done, timeout 100 ms → `Attempt::Done`); `translate_future_is_send` (compile-time `fn assert_send<T: Send>(_: &T)` on `translate(&ClientBackend::new(&client), doc, job, &mut sink)` with a `Send` sink).

CLI in this unit: `doc/mod.rs` adopts the API with a minimal `CliSink` (full version in U3). Delete `crates/plamo-translate/src/doc/driver.rs`, `history.rs`, `segment/`. Restrict `lib.rs` exports to the G-2 list.

Completion: `cargo test --workspace --offline` green; `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` clean (this is where `async_fn_in_trait` would fail; the desugared form passes [V: experiment]); `cargo test -p plamo-doc --offline` → `150 passed` (114 + 29 + 7 new); plamo-translate unit tests → `29 passed`; `cargo test -p plamo-translate --offline --test cli_doc` → `14 passed` with the file unchanged.

### U3 — CLI adopts the library: `doc.rs` with `CliSink`, exit codes, text rendering (no user-visible change)

Files: `crates/plamo-translate/src/doc/mod.rs` → `crates/plamo-translate/src/doc.rs`.

- `FormatArg` + `format_arg_defaults_to_auto` live here; `fn into_format(FormatArg) -> Option<Format>`.
- `struct CliSink { stdout, stderr, progress: Progress, verbose: bool, json: bool }` implementing `Sink`: `write` → `write_all + flush`; `warn` → render (rules U4/U5); `progress` → TTY `Progress` only when `!json` (`Piece` sets prefix `[{label}] `, `Stage` → `show`, `Generated` → `line("{chars} chars")`). All stderr writes in `doc.rs` use `let _ = writeln!(stderr, ..)` — never `eprintln!` (which panics on a closed stderr) (C-7).
- `fn exit_code(report: &Report) -> i32`: `Some(Stop{cause: Client(DaemonUnreachable{..}), ..}) => 3`; other `Some(_) => 1`; `None if degraded > 0 || history_append_failed() => 1`; else `0`.
- `fn render_stop`: `Client(e)` → existing `write_error` (keeps the `daemon install` hint); `Daemon{code,message}` → existing `write_daemon_error`; `Output(e)` → `error: cannot write stdout: {e}`; then `error: stopped at piece {label}; stdout holds everything translated before it` if `piece.is_some()`.
- `run()` order (MUST): (1) read stdin; whitespace-only → usage error exit 2; (2) `load_opts`; (3) `read_history`; (4) `prepare(&input, into_format(args.format), &Limits::default())`; (5) format/notes lines (U5 rules); (6) **if `doc.pieces() == 0`: write the input to stdout verbatim and exit 0 without resolving the target or creating a Job** (keep today's short-circuit; `resolve_target("")` would yield "ja" [V: target.rs] which is meaningless); (7) `target_lang = opts.to.unwrap_or_else(|| resolve_target(&doc.translatable_text(), &config.default_target))`; (8) `Job`; (9) `tokio::select!{ translate(&ClientBackend::new(client), doc, job, &mut sink), ctrl_c() }`; (10) trailing lines + exit code.
- Unit test `renders_every_warning_kind_and_stop` (in `doc.rs`): for each `WarningKind` and each `StopCause` variant, render into a `Vec<u8>` and assert the exact line (pins `structure`/`inline`/`not_history` texts that cli_doc no longer sees by default, and `cannot write stdout`).

Completion: `cargo test --workspace --offline` green; `cli_doc` 14 passed unchanged.

### U4 — `--report json` and the version bump to 0.2.0

Files: `crates/plamo-translate/src/doc.rs`; `crates/plamo-translate/tests/cli_doc.rs` (new tests only); root `Cargo.toml` (`[workspace.package] version = "0.2.0"`); `nix/packages.nix` line 59 `version = "0.2.0";` (the Python daemon's `0.1.0` at line 10 stays); `Cargo.lock`; README option row (S3 wording).

Flag: `#[arg(long, value_enum, default_value_t = ReportArg::Text)] report: ReportArg`, `enum ReportArg { Text, Json }`.

Contract (`--report json`):
- stdout unchanged (translation, incremental, byte-exact).
- stderr: UTF-8 JSON Lines, one object per line, `\n`-terminated, nothing else, no TTY progress. Order: `format`, `note*`, interleaved `warning`/`error`, one `summary` last.
- Objects (fields required unless "optional"; consumers MUST ignore unknown fields/`type`s/`kind`s):
  - `{"type":"format","format":"text"|"markdown","detected":bool}` — emitted once `prepare` has run.
  - `{"type":"note","line":N,"kind":"indented_code"|"html"|"reserved_marker","message":"<Display of Note>"}` — same conditions as the text lines (`indented_code` only when detected).
  - `{"type":"warning","piece":"K/M"|"K/M.s","kind":"truncated"|"empty"|"markers_lost"|"structure"|"inline"|"not_history","degrades":bool,"message":"<Warning.message>"}` — every kind, regardless of `--verbose`.
  - `{"type":"error","kind":"history_append","message":"cannot append to history PATH: ..."}`
  - `{"type":"error","kind":"daemon","code":"<snake code>","message":"...","piece":"K/M"}` (piece optional); `{"type":"error","kind":"client","code":"daemon_unreachable"|"history_unsupported"|"io"|"protocol","message":"<Error Display>","piece":...}` (piece optional); `{"type":"error","kind":"output","message":"cannot write stdout: ...","piece":"K/M"}`; `{"type":"error","kind":"usage","message":"..."}` for every usage error raised inside `doc::run`: empty/whitespace stdin, stdin read failure, `--history -`, history/glossary/context-file read or parse errors.
  - `{"type":"summary","v":1,"exit_code":0|1|2|3|130,"format":"text"|"markdown"|null,"pieces":M,"degraded":K,"warnings":N,"stopped_at":"K/M"|null,"complete":bool,"history_append_failed":bool}` — `format` is `null` when the run ended before `prepare` (usage errors); `pieces`/`degraded`/`warnings` are 0 then; `complete` = stdout holds the whole document; `exit_code` equals the process status; SIGINT → `130, complete:false, stopped_at:null`. The summary is a single line of less than 300 bytes.
- Not emitted (callers MUST treat as failure): clap parse errors and `Config::load` errors (exit 2 before `doc::run`), SIGKILL/timeout kills, panics, captures truncated before the last line (hooks cap stderr at 4 MiB; at most one `warning` object (~150 bytes) per piece attempt plus notes, so truncation is not expected for documents the CLI can process, but the rule stands).
- `--verbose` ignored in json mode.
- Stability: `v` bumps only when an existing field changes meaning or is removed.

Implementation: `enum Diag { Text { verbose: bool }, Json }` on `CliSink`; `serde_json::json!` (dependency exists [V]); json-aware usage error path inside `doc.rs` (translate.rs unchanged); SIGINT branch prints the summary before returning 130.

Tests (new, cli_doc.rs): `doc_report_json_emits_format_warnings_and_summary` (piece 2 `length` on unsplittable paragraph, `--history-bytes 0`: stdout = translation; every stderr line parses; first `format`; a `warning{kind:"truncated",piece:"2/3",degrades:true}`; last `summary{exit_code:1,pieces:3,degraded:1,complete:true,stopped_at:null}`; exit 1); `doc_report_json_summary_on_daemon_error` (`error{kind:"daemon",code:"internal",piece:"2/3"}`, `summary{exit_code:1,complete:false,stopped_at:"2/3"}`, stdout `"X1\n\n"`); `doc_report_json_unreachable` (`error{kind:"client",code:"daemon_unreachable"}`, `summary{exit_code:3}`); `doc_report_json_usage_error_has_null_format` (empty stdin → `error{kind:"usage"}` + `summary{exit_code:2,format:null,pieces:0}`); `doc_report_json_has_no_text_lines` (auto markdown with HTML block: no stderr line starts with `note:`/`warning:`/`error:`); `version_is_0_2_0` (`--version` stdout `plamo-translate 0.2.0\n`).

Completion: `cargo test -p plamo-translate --offline --test cli_doc` → `20 passed`; `cargo run -p plamo-translate --offline -- --version` → `plamo-translate 0.2.0`; `grep -n 'version = "0.2.0"' Cargo.toml nix/packages.nix` → one hit each.

### U5 — `--verbose` (quieter default stderr) — contract change

Files: `doc.rs`; `cli_doc.rs` (one invocation edited + new tests); README (S3).

Flag: `#[arg(long)] verbose: bool` (no short form).

Rule: in `--report text` mode a line is verbose-only iff it says nothing about the output text. Exactly two lines qualify:

| Line | Default | `--verbose` |
|---|---|---|
| `note: input format: ... (auto-detected; use --format to override)` | hidden | shown |
| `warning: piece K/M: not used as history: ...` | hidden | shown |
| `warning: line N: ...` (indented code / HTML / reserved marker) | shown | shown |
| `warning: piece K/M: translation truncated` / `empty translation` / `markers lost` | shown | shown |
| `warning: piece K/M: structure changed (...)` / `inline elements changed (...)` | shown | shown |
| `error: ...` (all), `error: K of M pieces ...` | shown | shown |
| TTY progress | as today | as today |

Implementation: hide when `kind.informational()` (lib) or the line is the format note. `--report json` ignores `--verbose`.

Existing test change (the only one): `doc_driver_auto_format_prints_a_note` first invocation `["doc","-t","en"]` → `["doc","-t","en","--verbose"]`; the second half (`--format markdown` prints no `note:`) is unchanged. Reason: user request; intent preserved. No other cli_doc.rs assertion touches a hidden line [V: grep].

New tests: `doc_default_stderr_hides_format_note_and_not_history` (auto markdown, fake returns `finish_reason: null` → stderr empty, exit 0); `doc_verbose_shows_format_note_and_not_history` (same + `--verbose` → both lines present); `doc_default_stderr_keeps_untranslated_block_warning` (HTML block → `warning: line N: HTML block left untranslated` without `--verbose`).

Impact on the plugin: `claude-plugin/hooks/lib/cli.js` builds `--json --no-stream -s -t [--form] [--history]` for the single-call path and parses its stderr first line [V]; the single-call path is untouched by U4/U5, so existing hooks keep working. S2 owns any `doc` usage and parses `--report json`.

Completion: `cargo test -p plamo-translate --offline --test cli_doc` → `23 passed`; `cargo test -p plamo-translate --offline --test cli_translate` → `29 passed` unchanged.

### U6 — MCP `translate_document`

Files: `crates/plamo-translate-mcp/Cargo.toml` (`plamo-doc = { path = "../plamo-doc" }`), `src/server.rs` + new `src/document.rs`, `tests/mcp.rs`, `Cargo.lock`, README (S3).

Arguments (`TranslateDocumentArgs`, `Deserialize + JsonSchema`):

| field | type | rules |
|---|---|---|
| `text` | string? | exactly one of `text`/`input_path` |
| `input_path` | string? | absolute; read as UTF-8 |
| `output_path` | string? | absolute; result becomes a write summary |
| `overwrite` | bool? (false) | existing `output_path` without `overwrite:true` → error before translating |
| `format` | `"auto"\|"text"\|"markdown"`? (auto) | `DocFormat` with `JsonSchema` |
| `target_lang` | string (required) | as `translate` |
| `source_lang`, `context`, `glossary`, `form` | as `translate` | glossary merged with config as today |
| `history_path` | string? | absolute JSONL; seed read if it exists; `-` rejected |
| `history_append` | bool? (false) | requires `history_path`; appends eligible pieces; creates the file |
| `history_bytes` | u32? (8000) | 0 disables history and the preflight |

Validation (isError, before any daemon contact, in this order; exact message prefixes): `invalid arguments: pass exactly one of text or input_path`; `invalid arguments: input_path must be absolute`; `invalid arguments: output_path must be absolute`; `invalid arguments: history_path must be absolute`; `invalid arguments: history_append requires history_path`; `cannot read input_path PATH: <io>`; `invalid arguments: output_path must differ from input_path` (compared after `std::fs::canonicalize` of `input_path` and of `output_path`'s parent joined with its file name; if `output_path` itself exists, canonicalize it directly); `output_path is a directory`; `output_path exists; pass overwrite=true to replace it`; `cannot create OUTPUT_PATH.part: <io>` (parent missing, permissions — the `.part` is created/truncated here, before translating; a stale `.part` from an earlier failed run is overwritten regardless of `overwrite`, which is documented in the tool description); `cannot read history_path PATH: <error>`. Concurrent calls with the same `output_path` are not coordinated (last rename wins); state this in README.

Result:
- Text mode success: `content[0]` translation verbatim; `content[1]` (only if non-empty) lines joined by `\n`, in order: `note: input format: markdown (auto-detected)` (auto only), `note: <Display of Note>` per note, `warning: <Display of Warning>` per warning (all kinds), `note: K of M pieces were not fully translated; their source text was kept where marked above` when `degraded > 0`, `note: cannot append to history ...` when it failed. Cap: after 50 note/warning lines append `note: ... and N more` and stop.
- File mode success: `content[0]` = `wrote OUTPUT_PATH (M pieces, N bytes)`; `content[1]` as above.
- Stop (isError): `content[0]` = `<StopCause Display>; stopped at piece K/M` + text mode: `; the K-1 pieces before it follow in the next content block; to continue, call again with the untranslated remainder` / file mode: `; the pieces before it are in OUTPUT_PATH.part`. Text mode: `content[1]` = the partial translation (the sink's buffer) when non-empty. Preflight stop: `<StopCause Display>` only. `DaemonUnreachable` uses `describe_client_error`. Per-event timeout (`Client(Error::Io(e))` with `e.kind() == TimedOut`): `translation timed out: no event from the daemon for Ns (set PLAMO_TRANSLATE_MCP_TIMEOUT_SECS to change the limit)`. With `history_append`, the pairs recorded before the stop are only clean pieces [V: driver records only issue-free pieces], so a retry on the remainder with the same `history_path` continues consistently.
- Total cap (`PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS`, default 3600, parsed like `parse_timeout`): `translation timed out after Ns (set PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS to change the limit)`; cancellation: `translation cancelled`. Both leave `OUTPUT_PATH.part` in file mode and return no partial text (the future was dropped).
- Degraded pieces never set `isError`.

Control flow: `tokio::select!{ r = tokio::time::timeout(doc_timeout, translate(&ClientBackend::new(&self.client).with_event_timeout(self.timeout), doc, job, &mut sink)) => .., _ = context.ct.cancelled() => .. }` (two arms; dropping the future closes the socket). `PlamoServer` gains `doc_timeout: Duration` (`new(client, glossary, timeout, doc_timeout)`; `from_env` reads both env vars). The `translate` tool keeps its total-timeout semantics with `self.timeout`.

File sink: `std::fs::File` opened at validation; `write_all + flush` per sink write (local disk, small writes; blocking is acceptable in this single-request server, same as the CLI); on `report.complete()` → `std::fs::rename(part, output_path)` (atomic replace on POSIX; this is how `overwrite:true` replaces the old file).

Progress notifications (progress token present), monotone by construction: keep a `last_sent: f64`. `Piece{index,total}` → `value = (index-1) as f64`, `total = total`, message `piece {label}` (forced); `Generated{chars}` → `value = (index-1) + min(chars / max(1, 1.5*piece_chars), 0.99)`, message `piece {label}: {chars} characters` (250 ms throttle); `Stage` → `value = last_sent + 0.001`, message `loading model` / `downloading model: N%` (forced). Emission rule: send iff `value > last_sent` and `value < index as f64` (never reaches the next piece boundary before that piece starts); otherwise drop. Sub-pieces repeat `index` and restart `chars`, so their early values fall below `last_sent` and are dropped until they exceed it; that is acceptable.

Description text (exact):
`Translate a whole plain-text or Markdown document with the local PLaMo-3-translate model, piece by piece with rolling history so terms stay consistent. Code blocks and HTML are kept verbatim; the output keeps the source structure. Pass the document as text (it then appears in this conversation twice: as the argument and as the result), or as input_path (absolute) to avoid that; pass output_path (absolute) to write the result to a file and get a short summary instead of the text (an existing output_path needs overwrite=true; a stale output_path.part from an earlier failed call is replaced). Use history_path + history_append to keep terminology across several calls on the same document. target_lang, source_lang, context, glossary and form work as in translate. The result's first content block is the translation (or the write summary); a second block with note:/warning: lines describes pieces that could not be translated fully (their source text is kept in place) and is not part of the translation. If the call fails part-way, the error names the piece and the translated part is returned in the second block (text mode) or left in output_path.part (file mode). A document takes seconds per piece; the call fails if the daemon sends nothing for PLAMO_TRANSLATE_MCP_TIMEOUT_SECS (default 300) or runs longer than PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS (default 3600).`

`translate` description: append `For a whole document or Markdown with headings, lists or code blocks, use translate_document instead.`

Client-side limits: Claude Desktop/Code request timeouts and whether they reset on progress notifications are unverified [I]; progress notifications are sent whenever the client supplies a token; nothing else can be done server-side. Note this in README (S3).

Tests (tests/mcp.rs; a multi-connection fake like cli_doc.rs's `fake_daemon_fn` is needed):
- Existing `lists_translate_tool_with_schema`: `assert_eq!(tools.len(), 1)` → `assert_eq!(tools.len(), 2)` and look up the `translate` tool by name; add `assert!(description.contains("translate_document"))`. Reason: the second tool is the feature. All other assertions stay.
- New: `lists_translate_document_with_schema` (required == `["target_lang"]`; `format` enum `["auto","text","markdown"]`; properties include `input_path`, `output_path`, `history_path`, `history_bytes`); `document_text_mode_returns_translation_and_notes`; `document_file_mode_writes_part_then_renames`; `document_file_mode_refuses_overwrite_without_flag`; `document_file_mode_overwrite_replaces`; `document_stop_leaves_part_file` (daemon `internal` on piece 2 → `isError`, `.part` holds `X1\n\n`, final path absent); `document_text_mode_stop_returns_partial_in_second_block` (`content[1]` == `X1\n\n`); `document_rejects_relative_paths`; `document_requires_text_xor_input_path`; `document_rejects_same_input_and_output`; `document_progress_counts_pieces` (first notification `progress == 0.0`, `total == Some(3.0)`, message `piece 1/3`; all values strictly increase; a later one ≥ 1.0); `document_event_timeout_fires` (`timeout` 300 ms, fake accepts and sends nothing → error contains `no event from the daemon`, connection closed); `document_event_timeout_resets_on_activity` (fake sends a delta every 150 ms for 1 s then done, timeout 300 ms → success); `document_total_timeout_fires` (`doc_timeout` 300 ms, fake sends deltas every 50 ms forever → error contains `timed out after`); `document_history_path_append_records_pieces`; `document_history_bytes_zero_skips_preflight`.

Completion: `cargo test -p plamo-translate-mcp --offline --test mcp` → `17 + 16 = 33 passed`; `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` clean.

### Test strategy summary

- Moved unchanged: 113 segment + 1 history tests → plamo-doc; `format_arg_defaults_to_auto` → CLI `doc.rs`.
- Moved with harness adaptation: 29 driver tests → plamo-doc; two assertions rewritten (U2).
- Edited existing assertions: `cli_doc::doc_driver_auto_format_prints_a_note` (adds `--verbose`, U5) and `mcp::lists_translate_tool_with_schema` (tool count 1 → 2 + lookup by name, U6).
- Unchanged files: `tests/cli_translate.rs`, `tests/cli_daemon.rs`, plamo-client tests.
- New: 7 lib unit tests, 1 CLI unit test, 6 (U4) + 3 (U5) cli_doc tests, 16 mcp tests.

## 6. Unresolved items (with defaults)

- `error_code_name` lives in plamo-doc although plamo-client owns `ErrorCode`. Default: plamo-doc now; later `impl ErrorCode { pub fn as_str }` in plamo-client.
- `Report.warnings` duplicates what the Sink received. Default: keep both.
- Whether `translate_document` should accept `history_last`. Default: no.
- Default values 300 s (per event) / 3600 s (total) are untested against a real daemon. Default: keep; both are env-tunable.
- Whether S2 reads `warning` objects or only `summary`. Default: S2 treats "no summary, or `exit_code != 0`, or `!complete`" as failure; `degraded > 0` is S2's call.
- Claude Desktop file-path usability (C-6) and client request timeouts (C-5) are unverified; no server-side action possible beyond progress notifications.

## 7. Additional checks

- `nix develop -c cargo --version` → `cargo 1.99.0` [V]. Baseline tests all pass [V].
- **Lint experiment** [V]: a scratch crate `crates/zz-afit` in SCRATCH with `pub trait Sink { async fn write(..); async fn progress(..); }` and `pub trait Backend { async fn attempt<S: Sink>(..); }` fails `cargo clippy --offline -p zz-afit --all-targets -- -D warnings` with `error: use of async fn in public traits is discouraged as auto trait bounds cannot be specified` (3 errors). Rewritten as `fn ... -> impl Future<Output = ..> + Send` with `Sink: Send`, `Backend: Sync`, clippy is clean and a test `fn assert_send<T: Send>(_: &T)` on the `translate(&ClientBackend(&client), &mut sink)` future compiles and passes (`1 passed`). Impl blocks may still use `async fn`; the compiler checks their futures against the `+ Send` bound. A `RefCell`-based test backend would not satisfy `Sync`, hence `Mutex` in the harness.
- Driver keeps a translation only after a structure issue when the unit came from a split or cannot be split (`sretry` / `split` returns None), and always keeps inline-issue translations, warning in both cases [V: driver.rs:406-427]; so structure/inline warnings describe emitted output.
- `resolve_target("", "auto")` returns `"ja"` (no letters) [V: target.rs]; hence the mandatory short-circuit order in U3.
- `plamo-translate --help` on the installed binary lists `status, doc, model, daemon, help`; the binary resolves to `/nix/store/gvqdplbrf1jcv7jrl62bv40d0vl4whmh-plamo-translate-0.1.0/bin/plamo-translate`; `doc --report json -t en </dev/null` → exit 2 `error: unexpected argument '--report' found`; `doc --help` → exit 0 [V].
- `claude-plugin/hooks/lib/cli.js` builds `--json --no-stream ...` (no `doc`) and parses the first non-empty stderr line and `^error: context_too_long:` [V]; untouched by this plan.
- MCP `ProgressTracker` throttles at 250 ms and forces the first delta/stage change [V: server.rs:21, 416-470].
- `nix/packages.nix` hardcodes `version = "0.1.0"` at lines 10 (Python) and 59 (Rust) [V].

## 8. Conformance criteria

- **G-1 Crate boundary.** MUST: segmentation, driver, history selection live in `crates/plamo-doc`; its `Cargo.toml` MUST NOT depend on `clap`, `rmcp`, `schemars`, `serde_json`; its non-dev `tokio` MUST enable only `time`; `crates/plamo-translate/Cargo.toml` MUST NOT depend on `pulldown-cmark`; `crates/plamo-translate-mcp` MUST NOT depend on `plamo-translate`. Applies to: U1, U2, U6. Check: `grep -E "clap|rmcp|schemars|serde_json" crates/plamo-doc/Cargo.toml` → none; `grep -A0 '^tokio' crates/plamo-doc/Cargo.toml` shows `features = ["time"]` under `[dependencies]`; `grep pulldown crates/plamo-translate/Cargo.toml` → none; `grep '"../plamo-translate"' crates/plamo-translate-mcp/Cargo.toml` → none.
- **G-2 Library surface.** `plamo_doc` MUST export exactly: `prepare`, `translate`, `Document`, `Job`, `Sink`, `ProgressEvent`, `Report`, `Warning`, `WarningKind`, `Stop`, `StopCause`, `error_code_name`, `Backend`, `Attempt`, `ClientBackend`, `Format`, `Limits`, `Note`, `NoteKind`, `detect_format`; `Segment`, `Unit`, `Finished`, `Issue`, `FinishError`, `split`, `split_unit`, `Split` MUST NOT be public after U2. Public trait methods MUST be `fn -> impl Future + Send` (no `async fn` in public traits). The lib MUST NOT print. Applies to: U2. Check: `grep -n "pub use" crates/plamo-doc/src/lib.rs` matches the list; `grep -rn "async fn" crates/plamo-doc/src/report.rs crates/plamo-doc/src/driver.rs | grep -v "impl\|#\[cfg(test)\]"` shows no trait declarations; `grep -n "eprint\|println" crates/plamo-doc/src/*.rs` → none outside tests; clippy gate clean.
- **G-3 Message parity.** `Warning.message`, `Display for Note`, `Display for StopCause::Daemon` MUST equal today's stderr texts after their prefixes; the CLI's `exit_code`/rendering is authoritative and the plamo-doc test harness mirrors it. Applies to: U2, U3. Check: `cli_doc` original 14 tests pass with original assertions (only `doc_driver_auto_format_prints_a_note` gains `--verbose`); `cargo test -p plamo-translate --offline renders_every_warning_kind_and_stop` passes.
- **G-4 CLI stdout contract.** `doc` MUST write only the translation to stdout, incrementally, in every report/verbose mode; MUST NOT write JSON to stdout. Applies to: U3-U5, S2. Check: `doc_report_json_summary_on_daemon_error` asserts stdout `"X1\n\n"`.
- **G-5 Report mode.** With `--report json`, every stderr line MUST parse as a JSON object with string `type`; on every exit path through `doc::run` the last line MUST be `type=summary` with `v`, `exit_code`, `format` (nullable), `pieces`, `degraded`, `warnings`, `stopped_at`, `complete`, `history_append_failed`; `exit_code` MUST equal the process status; no TTY progress. Callers MUST treat an absent summary as failure. Applies to: U4, S2 parser. Check: the six U4 tests; manual `printf 'Hi.\n' | plamo-translate doc -t ja --report json 2>&1 >/dev/null | python3 -c 'import sys,json; L=[json.loads(l) for l in sys.stdin]; assert L[-1]["type"]=="summary"'`.
- **G-6 Verbose rule.** In `--report text` mode without `--verbose` the CLI MUST print every `error:` line, every `warning: line N:` line and every `warning: piece` line except `not used as history`, and MUST NOT print `note: input format` or `not used as history`; with `--verbose` it prints all. `--verbose` MUST NOT change stdout or exit codes and MUST have no short flag. Applies to: U5, S3 (README/skill). Check: the three U5 tests; `plamo-translate doc --help | grep -c "^  -v"` → 0.
- **G-7 Exit codes.** 3 iff stop cause is `Client(DaemonUnreachable)`; 1 for any other stop, degraded > 0 or history append failure; 0 otherwise; 2 usage; 130 SIGINT. The zero-pieces short-circuit MUST run before target resolution. Applies to: U3. Check: cli_doc tests `..._daemon_error_stops_...` (1), `..._unreachable_..._exits_3` (3), `..._usage_errors_exit_2` (2), `..._stale_daemon_...` (1), `..._nothing_to_translate_echoes_input_without_the_daemon` (0, no connection).
- **G-8 MCP tool contract.** Exactly two tools, `translate` and `translate_document`; `translate_document` MUST require only `target_lang`, accept the U6 fields, reject invalid path combinations before daemon contact, return the translation verbatim as `content[0]` in text mode and a `wrote ...` summary in file mode, advisories only in `content[1]`; degraded MUST NOT set `isError`; a stop MUST set `isError`, name the piece, and return the partial translation in `content[1]` (text mode). Applies to: U6, S3. Check: `lists_translate_document_with_schema`, `document_text_mode_returns_translation_and_notes`, `document_rejects_relative_paths`, `document_requires_text_xor_input_path`, `document_text_mode_stop_returns_partial_in_second_block`.
- **G-9 MCP file safety.** MUST create `<output_path>.part` before translating and rename only on `report.complete()`; MUST refuse an existing `output_path` unless `overwrite=true`; MUST reject `output_path == input_path` after canonicalization; MUST NOT delete `.part` on failure. Applies to: U6. Check: `document_file_mode_writes_part_then_renames`, `document_file_mode_refuses_overwrite_without_flag`, `document_file_mode_overwrite_replaces`, `document_stop_leaves_part_file`, `document_rejects_same_input_and_output`.
- **G-10 MCP timeouts/cancel.** `translate_document` MUST use `ClientBackend::with_event_timeout(PLAMO_TRANSLATE_MCP_TIMEOUT_SECS)` (per event, reset by every daemon event incl. connect/preflight) and a total `tokio::time::timeout(PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS)`; MUST close the daemon connection on either timeout and on cancellation; `translate` keeps its total semantics. Applies to: U2, U6. Check: `client_backend_event_timeout_yields_timed_out`, `client_backend_event_timeout_resets_per_event`, `document_event_timeout_fires`, `document_event_timeout_resets_on_activity`, `document_total_timeout_fires`, existing `timeout_is_tool_error`, `cancellation_closes_daemon_connection`.
- **G-11 Cross-reference.** The `translate` description MUST mention `translate_document`. Applies to: U6, S3. Check: `lists_translate_tool_with_schema`.
- **G-12 Version and detection.** The commit adding `--report` MUST also set the workspace version and `nix/packages.nix` `rustArgs.version` to `0.2.0`; programmatic callers (S2, S3) MUST always pass `--report json`, MUST treat exit 2 with stderr matching `unexpected argument '--report'` as "binary too old", and MUST NOT probe with `doc --help`. Applies to: U4, S2, S3. Check: `git show <U4 commit> --stat` touches `Cargo.toml`, `nix/packages.nix`, `Cargo.lock`; `cargo run -p plamo-translate --offline -- --version` → `plamo-translate 0.2.0`.
- **G-13 Test discipline.** Existing assertions MUST remain byte-identical except the four listed changes (two driver assertions in U2; `doc_driver_auto_format_prints_a_note` in U5; `lists_translate_tool_with_schema` in U6), each named in its commit message with the reason. `cargo test --workspace --offline` and `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` MUST be green after every unit. Check: `git diff <base>..HEAD -- crates/plamo-translate/tests/cli_doc.rs` shows only added tests plus one added `"--verbose"` argument; same for `crates/plamo-translate-mcp/tests/mcp.rs` with the tool-count edit.

