# 開発メモ

コードから読み取りにくい約束ごとをまとめる。利用者向けの説明は README.md にある。

## Rust

- `doc` の処理 (分割、driver、履歴の選択) は `crates/plamo-doc` に置く。ライブラリは何も出力しない。出力は `Sink` を実装する側 (CLI、MCP) が行う。
- MCP (`plamo-translate-mcp`) は CLI クレート (`plamo-translate`) に依存しない。clap などを抱え込まないため。
- stderr への書き込みは `let _ = writeln!(…)` で行う。`eprintln!` は stderr が閉じていると panic する。
- `doc --report json` の stderr (1 行 1 オブジェクト、最後の行が `summary`、`v: 1`) は plugin と skill が読む契約。項目を変えるときは `v` を上げ、`claude-plugin/hooks/lib/doc.js` の `parseDocRun` と `claude-plugin/skills/translate-document/SKILL.md` を合わせる。
- 版: `Cargo.toml` の workspace の `version` と `nix/packages.nix` の `rustArgs.version` は一緒に上げる。daemon (Python) と plugin (`claude-plugin/.claude-plugin/plugin.json`) の版はそれぞれ別に上げる。

## Claude Code plugin (`plamo-ja-bridge`)

### hooks の時間の規則

Claude Code の hooks エンジンには次の規則がある。破ると plugin が「runaway」として外され、`/ja` と `ja⇄en` が消える (戻すには `/reload-plugins`)。テストの harness はこの unload を再現しないので、テストが通っても安心できない。

- 1 回の dispatch には 10 秒の budget がある。普通の promise やタイマーを待つ時間は budget に数えられる。hook 自身の `$` 呼び出し (`$.process.run` など) を待つ時間は数えられない。
- budget が切れた (または abort された) 後も 5 秒以上 hook が動いていると overrun になる。overrun が 2 回続くと plugin が外される。

そのため:

- `$` 呼び出し以外の待ちは、すべて `boundedWait` (`hooks/lib/pipeline.js`) を通し、期限を `next.budget.remainingMs` より短くする。
- `$` 呼び出しの待ちは `untilAborted(…, next.signal)` (`hooks/lib/doc.js`) を通し、abort されたらすぐ戻る。
- 返信の表示の重い処理は、`session.start` から起動する worker と、最後の dispatch のまとめ (最大 `OUTPUT_FINAL_MAX_MS`) で行う。プロンプトの翻訳中は、表示側は新しい翻訳を始めない。
- テストで遅いデーモンを模すときは `mock.clock` を使う。

### 手動の確認 (unload の再現)

1. `claude --debug-file /tmp/cc-debug.log` で起動し、`/ja on` を実行する。
2. `plamo-translate daemon restart` の後、長い応答を頼む (例: `40 段落と表を含めて詳しく説明して`)。最後の和訳の途中で Esc を押す回も試す。
3. `grep -E "hook overran: .*plamo-ja-bridge|plamo-ja-bridge ignored its signal|respawning|plamo-ja-bridge was unloaded" /tmp/cc-debug.log` が何も出さず、`/ja` と `ja⇄en` が残っていること。plugin の debug 行 (`plamo-ja-bridge: …`) で、期限切れやまとめの様子を確かめられる。

### 実測していない値

| 値 | 場所 | 見直す条件 |
|---|---|---|
| 最後のまとめの上限 60 秒 (`OUTPUT_FINAL_MAX_MS`) | `hooks/lib/pipeline.js` | まとめの間に Claude Code が固まるなら 20 秒に下げる |
| `doc` に回すプロンプトの大きさ 3000 バイト (`INPUT_DOC_BYTES`) | `hooks/register.js` | 実際の `doc` の速さを測って決め直す |
| `doc` のタイムアウト 1 バイトあたり 20 ms (`MS_PER_BYTE`) | `hooks/lib/doc.js` | 同上 |

### MCP を同梱しない理由

plugin に `.mcp.json` を入れない。`claude mcp add` で絶対パスを登録した利用者のもとで同じ server が重複し、またバイナリのパスの設定が要るため。長い文書は skill (`translate-document`) が CLI で訳す。
