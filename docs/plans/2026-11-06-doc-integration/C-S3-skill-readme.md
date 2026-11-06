# 付録 C: 設計 S3 (r2) — translate-document skill、MCP の同梱、README

> 本計画書 (../2026-11-06-doc-integration.md) の一部。本文と食い違う箇所は本文が優先する。原文 (英語) のまま収録。SKILL.md の正本は同じディレクトリの SKILL.md。

## 1. Conclusion

**Q1 Skill.**
- **Name and location:** `translate-document` at `claude-plugin/skills/translate-document/SKILL.md`. The default scan finds it, so plugin.json needs no `skills` key. No supporting files.
- **Invocation:** the user types `/plamo-ja-bridge:translate-document`, or Claude invokes it itself.
- **Frontmatter:**
  - `description` and `when_to_use` are English and include Japanese trigger phrases. Together they are 704 characters, under the 1,536 limit [V: python count].
  - `allowed-tools: Bash(plamo-translate doc *)` only (r1 also listed `status` and `--version`).
  - `user-invocable` and `disable-model-invocation` are omitted, so they keep their defaults (true and false).
  - `argument-hint: "[file ...] [target language]"`.
- **Language:** the skill text is in English.
- **Behaviour the skill text fixes:**
  - Target language: the one the user named. If none was named, `en` for a Japanese source and `ja` otherwise; Claude says which it chose.
  - `-s` comes from reading `head -c 2000` of the input. If source and target are the same language, Claude asks instead of running.
  - Paths are single-quoted.
  - Before running, Claude checks that the output path differs from the input path and does not already exist.
  - There is no here-document mode. Pasted text is saved to a file first.
  - Claude always passes `--report json`.
  - The CLI is "too old" only when the exit code is 2 **and** stderr contains `unexpected argument '--report'`. There is no automatic fallback.
  - If the summary's `v` is not 1, Claude shows the summary line and does not interpret it.
  - The results table describes truncated pieces correctly: a partial translation, with the rest of that piece missing. Warnings carry no line numbers, so Claude locates degraded passages by reading the output.
  - Claude reminds the user of the license obligation and adds the notice only if the user agrees.

Full SKILL.md (identical to `S3-SKILL.md`):

````markdown
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

If `v` is not 1, show the summary line to the user and do not interpret it. Other lines: `{"type":"format",...}`, `{"type":"note","line":N,"kind":"html"|"indented_code"|"reserved_marker","message":...}` (a block kept untranslated), `{"type":"warning","piece":"K/M","kind":...,"degrades":bool,"message":...}`, `{"type":"error","kind":...,"message":...}`. Ignore fields and kinds you do not know. Warnings name pieces by number only (`3/12`), not by line.

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
````

**Q2 `.mcp.json`: not bundled.** The plugin ships no `.mcp.json` and no `mcpServers`. Registering the MCP server stays the user's job; the README says so (R8), and its existing `claude mcp add` instructions are unchanged (r1's `-s user` change is withdrawn). Claude Desktop chat users keep the `claude_desktop_config.json` instructions.

**Q3 README.** The edits are R1–R4, R6–R9 in section 5. R5 is now only the change to the `translate` paragraph's opening.
- R2, R3 and R9 ship in S1's U4.
- R4 ships in U5.
- R1, R5 and R6 ship in U6.
- R7 and R8 ship in S3's P1.

**Q4 Manifests.**
- **What this skill requires:** plugin.json `version` must be higher than `0.1.1` (target `0.2.0`) when P1 is pushed. Claude Code pins users to the manifest version, so the skill does not reach them otherwise [V: plugins-reference "version"]. One bump per release; the orchestrator assigns it (S2 unit 8 also plans 0.1.1 → 0.2.0).
- plugin.json `description`: `"Translate Japanese prompts to English and English replies to Japanese, and translate documents, with plamo-translate"`.
- No `skills` key.
- marketplace.json entry `description`: `"Japanese-English translation layer and document translation skill for Claude Code using plamo-translate"`. Still no `version` in the entry.

## 2. Rationale

- **Skill in English.** The model is the reader, and the plugin's internals (plugin.json strings, code comments) are English. Japanese trigger phrases go in `when_to_use` because users ask in Japanese. Slash commands are not translated by `/ja` [V: README].
- **CLI through Bash rather than MCP in Claude Code.**
  - It needs no MCP registration.
  - `run_in_background` has no 10-minute cap [V: the Bash tool spec in this environment says the max timeout applies to "a foreground command" and that a background command "keeps running across turns and re-invokes you when it exits"].
  - The exit code plus the JSON summary is a fixed contract (G-5).
  - Neither the source nor the translation enters the context.
- **Pre-approving only `plamo-translate doc *`.**
  - The user's request is the consent, and redirect targets are still checked against the user's file rules [V: permissions docs].
  - Accepted residual risk: the `--history-append`, `--glossary`, `--context-file` and `--history` paths are not checked. The skill confines these files to the output directory.
  - The grant lasts only for the invoking turn [V: skills docs]. Later turns prompt, and the skill tells Claude that this is expected.
  - `status` and `--version` are used only in failure paths, so they were dropped (C-7).
- **No here-document mode.** In the tool result, the translation on stdout and the JSON Lines on stderr arrive mixed. Saving to a file keeps the two streams apart (B-2).
- **Explicit check that output ≠ input.** `>` truncates the file before `doc` reads stdin, so an output name equal to the input name (for example `guide.ja.md` with `-t ja`) would destroy the source (B-1).
- **No automatic fallback for a too-old CLI.** G-12 requires treating it as too old. A text-stderr fallback would need a second parsing contract.
- **Not bundling MCP.**
  - In Claude Code the skill covers file translation without MCP.
  - A bundled server whose command differs from the user's registration (the README tells Desktop users to use an absolute path) adds a second server with the same tools, and `claude mcp list` warns about it.
  - Configuring the binary path would need a new userConfig key.
  - Claude Desktop chat cannot load the plugin, so bundling helps nowhere that MCP is the only route.
  - The tool-schema context cost is secondary: a manual registration pays it too, but only when the user opts in.

## 3. Alternatives considered and rejected

- **Bundling `.mcp.json`** (`{"mcpServers":{"plamo-translate":{"command":"${user_config.mcpServer}"}}}` with a new userConfig key `mcpServer`). See section 2.
- **`allowed-tools: Bash(${user_config.translator} doc *)`.** The docs list only `CLAUDE_SKILL_DIR`, `CLAUDE_PROJECT_DIR`, `CLAUDE_PLUGIN_ROOT` and `CLAUDE_PLUGIN_DATA` as substituted in allowed-tools rules [V: skills docs].
- **No pre-approval.** Every run would prompt, and it adds no safety beyond the redirect checks.
- **Here-document input for pasted text (r1).** stdout and stderr arrive mixed (B-2).
- **`2> report.jsonl`** on every run. It leaves a file in the user's tree on every run. Glossary and history files are created only on request and are named in the final message.
- **`set -C` (noclobber) before the command.** The compound command would not match the pre-approval rule, so every run would prompt. The `ls` check is used instead.
- **`.part` file plus `mv`.** `mv` needs its own approval, and the summary already says whether the file is complete.
- **Skill text in Japanese; `disable-model-invocation: true`; a `reference.md` with the full schema.** Same reasons as r1: no benefit to the model; the skill must trigger from plain "translate this file" requests; the skill needs only the summary and the warning kinds.
- **`-s user` in the README's `claude mcp add` example (r1).** Not part of this task (C-5). Listed in section 6.

## 4. Assumptions made

- [I] `Bash(plamo-translate doc *)` matches `plamo-translate doc ... < 'in' > 'out'`. The docs say a rule "allows the command, not the target" and that redirect targets are checked separately.
- [I] The grant also applies when Claude invokes the skill itself, not only when the user types the slash command. The docs say "the turn that invokes this skill". Smoke test step 10 checks both cases.
- [I] `${user_config.translator}` in the skill body is substituted with the default when the user has saved no value. It is now only a hint in the "command not found" row, so a literal placeholder does no harm.
- [I] The Bash tool result shows a non-zero exit code, which the too-old check needs because that case has no summary line.
- [I] The thresholds (30 KB → background) are estimates.
- [V] The daemon serves requests from a FIFO deque, one at a time (`server.py:67` `deque`, `:161` `popleft`, `:275` `busy` beyond `MAX_QUEUE = 8`). Not verified: how the `/ja` hooks behave while a long `doc` run occupies the daemon (delay, timeouts, `busy`). That is S2's design. The skill only claims that its own steps do not change with `/ja`.
- [V] A truncated piece writes the partial translation, and the rest of that piece is not written (`driver.rs` lines 369-379; test `length_unsplittable_keeps_truncated_text` asserts output `"TRUNCB"`).

## 5. Implications for implementation

| Unit | Commit | Depends on | Files |
|---|---|---|---|
| U4 (S1) | adds R2, R3, R9 | — | README.md |
| U5 (S1) | adds R4 | U4 | README.md |
| U6 (S1) | adds R1, R5, R6 | U2 | README.md |
| **P1 (S3)** | `feat(plugin): add translate-document skill for plamo-translate doc` | U4 and U5 committed, and pushed no later than P1 | `claude-plugin/skills/translate-document/SKILL.md` (copy of docs/plans/2026-11-06-doc-integration/SKILL.md), `claude-plugin/.claude-plugin/plugin.json` (description; version per the orchestrator's single bump), `.claude-plugin/marketplace.json` (description), README.md (R7, R8) |

**Release order (B-7).** The marketplace and the flake live in the same repository, so pushing P1 publishes CLI 0.2.0 at the same time. Users still update the CLI (flake input, `darwin-rebuild` or `nix profile upgrade`) and the plugin separately. A user with a new plugin and an old CLI gets the skill's too-old message, which tells them to update. R8 states the requirement.

### README edits (exact Japanese text)

**R1 (U6).** Replace the intro bullet on line 7 with:
`` - `plamo-translate-mcp`: Rust 製の stdio MCP server (`translate` と `translate_document` tool) ``

**R2 (U4).** In the option table of `### 文書の翻訳`, insert after the `--history-bytes` row:
```
| `--report text\|json` | stderr の形式。既定は `text`。`json` では stderr を 1 行に 1 つの JSON (JSON Lines) で書き、最後の行に結果の要約を出す (下記)。stdout は変わらない。0.2.0 から |
```

**R3 (U4).** In the same section:
- Change `` `TEXT`、`--no-stream`、`--json` はない。`` to `` `TEXT`、`--no-stream`、`--json` はない。プログラムから結果を読むときは `--report json` を使う。``
- After the line `終了コードは単体のときと同じ。…130 Ctrl-C。` append:

````
`--report json` の stderr は次の行からなる。知らない `type` や項目は無視すること。

| `type` | 内容 |
|---|---|
| `format` | 入力の形式 (`format`: `text` / `markdown`、`detected`: 自動で判定したか) |
| `note` | 訳さずに残したブロック (`line`、`kind`: `indented_code` / `html` / `reserved_marker`、`message`) |
| `warning` | ピースの警告 (`piece`、`kind`: `truncated` / `empty` / `markers_lost` / `structure` / `inline` / `not_history`、`degrades`: 訳しきれなかったか、`message`)。`--verbose` に関係なくすべて出る |
| `error` | エラー (`kind`: `daemon` / `client` / `output` / `usage` / `history_append`、`message`。`code` と `piece` がつくこともある) |
| `summary` | 最後に 1 行だけ出る要約 |

```json
{"type":"summary","v":1,"exit_code":1,"format":"markdown","pieces":12,"degraded":1,"warnings":2,"stopped_at":null,"complete":true,"history_append_failed":false}
```

`v` は形式の版で、項目の意味が変わるときだけ上がる。`exit_code` はプロセスの終了コードと同じ。`complete` は stdout に文書の最後まで書いたか、`stopped_at` は止まったピース (止まらなければ `null`)、`degraded` は訳しきれなかったピースの数、`history_append_failed` は履歴への追記に失敗したか。入力を読む前の使い方の誤りでは `format` が `null` になる。オプションの解析や設定ファイルの読み込みに失敗したとき (終了コード 2) と、プロセスが強制終了されたときは `summary` が出ないので、`summary` がなければ失敗として扱う。`--report` のない古い CLI (0.2.0 より前) は `error: unexpected argument '--report' found` を出し、何も訳さずに終了コード 2 で終わる。
````

**R4 (U5).** In the same table:
- Replace the `--format` row with:
  `` | `--format auto\|text\|markdown` | 入力の形式。既定は `auto` で、内容から判定する。判定の結果は `--verbose` のときだけ stderr に `note: input format: markdown (auto-detected; use --format to override)` で出る | ``
- Insert after the R2 row:
  `` | `--verbose` | 判定した入力の形式 (`note: input format: …`) と、履歴に使わなかったピースの警告 (`warning: piece N/M: not used as history: …`) も stderr に出す。ほかの警告とエラーは指定しなくても出る。訳と終了コードは変わらない。`--report json` では無視される。0.2.0 から | ``

**R5 (U6).** In `## 使い方 (MCP)`, in the paragraph starting `` `translate` tool が 1 つ使える。引数は ``, replace that opening with `` `translate` と `translate_document` の 2 つの tool が使える。`translate` は短い文向けで、引数は ``. The rest of that paragraph is unchanged. The `claude mcp add` line is unchanged.

**R6 (U6).** Replace the timeout paragraph (`1 回の呼び出しのタイムアウトは 300 秒…変更できる。`) with:

````
`translate_document` は `plamo-translate doc` と同じ方法で文書全体を訳す。引数は次のとおり。

| 引数 | 内容 |
|---|---|
| `text` / `input_path` | 訳す文書。どちらか一方を渡す。`input_path` は UTF-8 のファイルの絶対パス |
| `output_path` | 訳を書くファイルの絶対パス。`input_path` と別のファイルにする。指定すると、結果は訳ではなく `wrote PATH (M pieces, N bytes)` になる |
| `overwrite` | `output_path` が既にあるとき上書きする。既定は `false` (あればエラー) |
| `format` | `auto` / `text` / `markdown`。既定は `auto` |
| `target_lang` (必須)、`source_lang`、`context`、`glossary`、`form` | `translate` と同じ |
| `history_path` | 履歴の JSONL (CLI の `--history` と同じ形式) の絶対パス。あれば最初に読む |
| `history_append` | 訳したピースを `history_path` に追記する (なければ作る)。`history_path` が必要。既定は `false` |
| `history_bytes` | ピースごとに送る履歴のバイト数。既定は 8000。0 で履歴を使わず、デーモンの対応の確認もしない |

引数の誤り (`text` と `input_path` の両方か片方もない、絶対パスでない、`output_path` が `input_path` と同じかディレクトリ、`history_path` のない `history_append` など) は、デーモンに接続する前にエラーになる。

1 つ目の content は訳 (`output_path` のときは書いたことの要約)。2 つ目の content は訳の一部ではなく、`note:` / `warning:` の行を並べる。`format` が `auto` のときの判定結果、訳さなかったブロック、ピースの警告 (すべての種類)、訳しきれなかったピースの数、履歴の追記の失敗が入る。50 行を超えた分は `note: ... and N more` にまとめる。訳しきれなかったピースがあってもエラーにはならない。途中で止まったときはエラーになり、止まったピースを示す。`text` で渡したときは、それまでの訳を 2 つ目の content で返す。`output_path` のときは `<output_path>.part` に書いていき、最後まで訳せたときだけ `output_path` に名前を変える。失敗すると `.part` が残り、次の呼び出しで上書きされる。同じ `output_path` への同時の呼び出しには対応しない。`text` で渡すと文書が会話に 2 回 (引数と結果) 入るので、ファイルなら `input_path` と `output_path` を使う。

`translate` の 1 回の呼び出しのタイムアウトは 300 秒 (初回のダウンロードを含むため)。環境変数 `PLAMO_TRANSLATE_MCP_TIMEOUT_SECS` で秒数を変更できる。`translate_document` では、この秒数はデーモンから何も届かない時間の上限になる。呼び出し全体の上限は `PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS` (既定 3600 秒)。環境変数は `claude mcp add plamo-translate -e PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS=7200 -- plamo-translate-mcp` のように、Claude Desktop では設定の `"env"` に書く。進捗は MCP の progress 通知で送るが、クライアント側のタイムアウトがそれで延びるかは確かめていない。Claude Code では、長い文書は下の skill (CLI) で訳す方が確実。
````

**R7 (P1).** In the `## Claude Code の日英レイヤ (plamo-ja-bridge)` intro, after `翻訳には \`plamo-translate\` CLI を使うので、CLI とデーモンを先に導入しておく。`, insert:
`文書ファイルを訳す skill (下記) も含む。`

**R8 (P1).** New subsection, the last one in the plugin section (after `### 制限`, before `## 設定 (config.toml)`). S2 does not edit this location.

````
### 文書ファイルの翻訳 (skill)

`translate-document` skill を含む。Markdown やテキストのファイルの翻訳を頼むと (例: 「docs/guide.md を英訳して」)、Claude が `plamo-translate doc` を実行して訳を別のファイルに書く。`/plamo-ja-bridge:translate-document docs/guide.md en` で明示的に呼ぶこともできる。`/ja` のオン・オフとは関係なく使える。

- 0.2.0 以降の CLI が必要 (`--report json` を使う)。plugin と CLI は別々に更新されるので、CLI が古いと訳さずに更新を促す
- 出力は元のファイルと同じ場所の `<名前>.<言語>.<拡張子>` (`README.md` → `README.ja.md`)。同じ名前のファイルがあれば上書きの前に確認する。訳先を指定しなければ、日本語の文書は英語に、それ以外は日本語に訳す
- `plamo-translate doc` は、skill を呼び出したターンの間だけ確認なしで実行する。後のターンでの再実行や、出力ファイルへの書き込み (`>`) は、通常どおり権限の確認を受ける
- 用語集や履歴のファイルは、頼んだときだけ出力と同じ場所に作る
- 訳しきれなかったピースや途中での停止は、Claude が `--report json` の要約をもとに報告する
- 訳を公開するときは、モデルの出力であることを明示する (「ライセンス」を参照)。Claude は最後にそのことを伝え、ファイルの先頭に注記を入れるかを尋ねる
- この plugin は MCP server を含まない。MCP の `translate_document` を使うときは「使い方 (MCP)」のとおり別に登録する
````

**R9 (U4).** Append a bullet to `## トラブルシュート`:
`` - `error: unexpected argument '--report' found`: CLI が 0.2.0 より古い。`plamo-translate --version` で確かめ、更新する ``

S3 does not touch the plugin section's `### 使い方`, `### 設定 (userConfig)` or `### 制限` (S2 owns them).

### Completion criteria (from the repo root)

P1:
1. `cmp claude-plugin/skills/translate-document/SKILL.md docs/plans/2026-11-06-doc-integration/SKILL.md` prints nothing and exits 0.
2. `claude plugin validate --strict claude-plugin/skills` exits 0, last line `✔ Validation passed` [V in SCRATCH]. Validating the plugin root does not parse skills [V], so this separate run is needed.
3. `claude plugin validate claude-plugin` and `claude plugin validate .` both exit 0 with `✔ Validation passed` [V in SCRATCH].
4. `claude --plugin-dir ./claude-plugin plugin details plamo-ja-bridge` prints a first line `plamo-ja-bridge 0.2.0` (or whatever version the orchestrator assigns, which must be higher than 0.1.1), `Skills (1)  translate-document` and `MCP servers (0)` [V in SCRATCH with 0.2.0].
5. `ls claude-plugin/.mcp.json` fails, and `grep -c mcpServers claude-plugin/.claude-plugin/plugin.json` prints `0`.
6. `claude plugin test claude-plugin` prints `0 fail`, and the pass count is unchanged by P1 [V: 98 in SCRATCH].
7. `grep -c '^allowed-tools: Bash(plamo-translate doc \*)$' claude-plugin/skills/translate-document/SKILL.md` prints `1`; `grep -c -- '--report json' claude-plugin/skills/translate-document/SKILL.md` prints `3`.
8. Schema cross-check (run after U4 and P1): `for k in v exit_code format pieces degraded warnings stopped_at complete history_append_failed; do grep -q "\"$k\"" claude-plugin/skills/translate-document/SKILL.md && grep -q "\"$k\"" README.md || echo "MISSING $k"; done` prints nothing.
9. `grep -c '文書ファイルの翻訳 (skill)' README.md` prints `1`.
10. Manual smoke test (not a gate; needs CLI 0.2.0 and a running daemon). In a temporary directory containing `sample.md`, run `claude --plugin-dir <repo>/claude-plugin`.
    - (a) Ask `sample.md を英訳して`. Expect: an `ls` check, then `plamo-translate doc -t en -s ja --format markdown --report json < 'sample.md' > 'sample.en.md'` with no permission prompt for the command itself, and a final message containing the license reminder.
    - (b) Type `/plamo-ja-bridge:translate-document sample.md en` and confirm the same command again runs without a prompt.
    - (c) With CLI 0.1.0 on PATH, expect the "too old" message and no retry.

README units:
- After U4: `grep -c "unexpected argument '--report' found" README.md` prints `2`, and `grep -c '"type":"summary"' README.md` prints `1`.
- After U5: `` grep -c 'この `note:` は `auto` のときだけ出る' README.md `` prints `0`, and `` grep -c '^| `--verbose`' README.md `` prints `1`.
- After U6: `grep -c 'tool が 1 つ使える' README.md` prints `0`, `grep -c 'PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS' README.md` prints `2`, and `grep -c 'and N more' README.md` prints `1`.

## 6. Unresolved items (with defaults)

- **`${user_config.translator}` substitution when the user saved no value.** Default: keep it as a hint only. If smoke step 10 shows the placeholder literally, replace the sentence with `ask the user for the path of the plamo-translate CLI`.
- **Bash output truncation when a run emits hundreds of warnings.** Default: accept it; a missing summary is treated as failure.
- **The 30 KB background threshold.** Default: keep it and tune after real runs.
- **The `/ja` hooks while a long `doc` run is in progress** (delays, `busy`). Default: S2's responsibility. The skill tells Claude to run files sequentially, which keeps at most one skill request queued in the daemon at a time.
- **The README's `claude mcp add` defaults to local scope** [V: `claude mcp add --help`]. Default: unchanged in this release; the orchestrator may schedule `-s user` separately.
- **Plugin eval cases for skill triggering.** Default: not in this release.

## 7. Additional checks

- r2 skill: `claude plugin validate --strict claude-plugin/skills` passed; the plugin and marketplace validations passed; `plugin details` shows `plamo-ja-bridge 0.2.0`, `Skills (1)  translate-document` and `MCP servers (0)`; `claude plugin test` gave `98 pass, 0 fail` [V in SCRATCH].
- The skill body contains no `` !` `` and no `$<digit>` [V: grep].
- Truncated pieces: `driver.rs` keeps `u.finish(&raw)` (the partial translation) or the source if that fails, then warns `translation truncated` [V: driver.rs:369-379, test at line 882].
- The daemon queue is a FIFO `deque` (`server.py:67`, `:161`) [V].
- Validator behaviour: a deliberately broken skill in a temporary directory was reported only when the skills directory was validated directly [V, r1].

