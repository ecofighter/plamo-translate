---
name: translate-document
description: Translate a plain-text or Markdown file with the local PLaMo-3-translate model by running `plamo-translate doc`, writing the translation to a new file next to the source. Keeps code blocks and Markdown structure, keeps terminology consistent across the document, and reports pieces it could not translate.
when_to_use: Use when the user asks to translate a .md, .markdown or .txt file (or several) between Japanese, English or another supported language, e.g. "translate README.md into Japanese", "docs/guide.md を英訳して", "このファイルを和訳して", "PLaMo で翻訳して". Not for source code, JSON/YAML/HTML/PDF/Office files, a sentence or two (translate those yourself), or requests to summarize, rewrite or localize rather than translate.
argument-hint: "[file ...] [target language]"
allowed-tools: Bash(plamo-translate doc *)
---

# Translate documents with `plamo-translate doc`

`plamo-translate doc` reads a document on stdin, translates it piece by piece with the local PLaMo-3-translate model (passing the previous pieces as history so terms stay consistent), and writes the translation to stdout. Code blocks, HTML blocks and Markdown front matter are copied unchanged. It runs on this machine; nothing is sent to a remote service.

Arguments given to this skill, if any: $ARGUMENTS
(Tokens that name existing files are the inputs; a token that is one of the language codes below is the target language.)

## When to use this skill

- Use it for .md, .markdown and .txt files the user wants translated as a whole. If the user pasted a long document and wants it translated with PLaMo, ask for a file name, save the text there, and translate that file.
- Translate it yourself instead when the text is short (a few sentences), when the user asks you to, or when the task is more than translation (summarize, adapt, rewrite).
- Do not use it for source code, structured data or markup other than Markdown; `doc` would leave or damage that content.
- If an MCP tool named `translate_document` (plamo-translate server) is available, prefer this skill in Claude Code anyway: it needs no MCP registration, can run in the background past the tool timeout, and reports a machine-readable summary. Use the MCP tool only when Bash is unavailable or the user asks for it. The MCP `translate` tool is for short texts.
- The steps below do not change when the plugin's `/ja` layer is on.

## Languages

- Codes: `ja`, `en`, `zh-hans`, `zh-hant`, `ko`, `fr`, `de`, `es`, `ja(easy)` (simple Japanese).
- Source (`-s`): look at the first lines of the file (`head -c 2000 '<input>'`) and pass `-s` with its language.
- Target (`-t`, always passed): the language the user named. If none was named: `en` when the source is Japanese, otherwise `ja`; say which you chose.
- If source and target are the same language, do not run; ask the user.

## Command

Run one file per Bash call, from the directory the user is working in. Quote paths with single quotes (write a `'` inside a path as `'\''`):

```sh
plamo-translate doc -t <to> -s <from> --format <markdown|text> --report json [options] < '<input>' > '<output>'
```

- Always pass `--report json`. Never probe with `plamo-translate doc --help`.
- `--format markdown` for .md/.markdown, `--format text` for .txt and for prose that is indented (Markdown treats indented lines as code).
- Write `-t 'ja(easy)'` with quotes. All options go after `doc`.
- Options:
  - `--context '<text>'`: one line describing the document (subject, audience), e.g. `--context 'User guide for a Rust command-line tool'`. Not translated; improves word choice. Use it whenever you know the subject.
  - `--form polite|plain`: Japanese politeness, only with `-t ja`. Use it when the user states a style.
  - `--glossary <file.tsv>`: fixed terms, one `source<TAB>target` per line (a real tab character), `#` starts a comment; repeatable. The user's configured `glossary_file` is applied automatically. Only if the user gives terms in the conversation: write them to `<stem>.glossary.tsv` next to the output, pass it, and mention the file in your final message.
  - `--history <file.jsonl> --history-append`: only when the user wants consistent terms across several related files translated in order (chapters of one manual). Use `plamo-history.<from>-<to>.jsonl` in the output directory, pass the same file to each run in reading order, and mention the file in your final message. Not needed for a single file: `doc` already carries history between pieces.
  - `--history-bytes 0`: turns history off, about 2-3 s faster per piece. Use only when the user prefers speed over consistency.
- Duration: seconds per piece; the first run after a long idle also loads the model (the very first run downloads about 2.4 GB). Give the Bash call `timeout: 600000`. For inputs over about 30 KB, or when a run already timed out, use `run_in_background: true` and wait for the completion notification instead of polling.
- Several files: run them one after another, never in parallel (the daemon handles one request at a time).
- Permission: this skill pre-approves `plamo-translate doc` only in the turn that invoked it. Runs in later turns (reruns, after the user answers a question) may ask the user for permission; that is expected.

## Output file

- Name: `<stem>.<to>.<ext>` in the source's directory: `README.md` -> `README.ja.md`, `notes.txt` -> `notes.en.txt`. If the stem's last dot-separated part is one of the language codes (`guide.en.md`), replace it (`guide.ja.md`). For `ja(easy)` use `ja-easy`. Use the user's name or directory when given.
- Before running, check with `ls` that the output path differs from the input path and does not exist. `>` truncates the file before `doc` reads its input, so an output equal to the input destroys the source. If the output exists, ask before overwriting it.
- Do not edit the output afterwards unless the user asks.

## Reading the result

stdout goes to the output file. stderr (shown in the tool result) is JSON Lines; the last line is the summary:

```json
{"type":"summary","v":1,"exit_code":1,"format":"markdown","pieces":12,"degraded":1,"warnings":2,"stopped_at":null,"complete":true,"history_append_failed":false}
```

If `v` is not 1, show the summary line to the user and do not interpret it. If the summary's `exit_code` differs from the command's exit status, treat the run as having no summary line. Other lines: `{"type":"format",...}`, `{"type":"note","line":N,"kind":"html"|"indented_code"|"reserved_marker","message":...}` (a block kept untranslated), `{"type":"warning","piece":"K/M","kind":...,"degrades":bool,"message":...}`, `{"type":"error","kind":...,"message":...}`. Ignore fields and kinds you do not know. Warnings name pieces by number only (`3/12`), not by line.

| Result | Meaning | What to do |
|---|---|---|
| `exit_code` 0 | Whole document translated | Report the output path and the piece count. |
| `exit_code` 1, `complete: true`, `degraded` > 0 | Every piece was written, but `degraded` pieces are not fully translated: `empty` and `markers_lost` pieces were left in the source language; a `truncated` piece holds only the beginning of its translation, and the rest of that piece is missing | Report how many pieces and which kinds. If the user wants them fixed, find the affected passages by reading the output (source-language text, or a passage that ends abruptly), translate or complete them yourself, and say that the file then mixes PLaMo output with yours. Do not rerun the whole document hoping for a different result. |
| `exit_code` 1, `history_append_failed: true` | Translation is fine; the history file was not updated | Report the `error` line. |
| `exit_code` 1 or 3, `complete: false` | Stopped at piece `stopped_at`; the output file holds only the pieces before it | Report the `error` line and that the output file is partial. Rerun the whole file once the cause is fixed (`doc` cannot resume). |
| `exit_code` 3 | The daemon is unreachable | Tell the user to run `plamo-translate daemon install` (or apply the home-manager module) and check with `plamo-translate status`. Do not retry. |
| `exit_code` 2 with a summary | Usage error (empty input, unreadable history/glossary/context file) | Fix the command from the `error` message. |
| `exit_code` 130 | Interrupted | Report that the output file is partial. |
| Exit code 2, no summary line, stderr contains `unexpected argument '--report'` | The installed CLI is older than 0.2.0 | Stop. Tell the user that this skill needs plamo-translate 0.2.0 or later (`plamo-translate --version` shows the installed one) and that they should update it (`darwin-rebuild switch` after updating the flake input, or `nix profile upgrade`). Offer to translate the file yourself meanwhile. Do not retry without `--report json`. The redirect already created an empty output file; tell the user and offer to remove it. |
| Any other run without a summary line (command not found, killed by the timeout, crash) | Failure; the output file may be partial | Report what stderr shows. If `plamo-translate` is not found, tell the user; the plugin's configured CLI path is `${user_config.translator}`. |

Warnings with `degrades: false`: `structure` / `inline` mean the piece was translated but its Markdown structure or inline elements (links, code spans) changed; mention them and offer to check those pieces. `not_history` needs no action. A `note` of kind `indented_code` (only under `auto`) means an indented block was kept as code; with `--format markdown` such blocks are kept without a note. If they are prose, rerun with `--format text` if the user agrees.

## License notice

The output is machine translation by PLaMo-3-translate under the PLaMo Community License. In your final message, say that the file is PLaMo output and that publishing it requires stating that it is model output, with "Built with PLaMo". Offer to add a notice at the top of the file (after any front matter); add it only if the user agrees. Text for a Japanese output: `> この文書は PLaMo-3-translate による機械翻訳です。Built with PLaMo`; for other languages: `> This document was machine-translated with PLaMo-3-translate. Built with PLaMo`.
