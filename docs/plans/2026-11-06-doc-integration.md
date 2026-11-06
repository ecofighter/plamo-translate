# plamo-translate doc の統合 (plamo-doc ライブラリ、CLI の機械可読出力、MCP translate_document、plugin の長文対応と不具合修正、文書翻訳 skill)

- 作成: 2026-11-06
- 基点: main @ 64a5b1b
- 状態: 計画確定 (実機でしか確かめられない項目は 10 章に既定値つきで残す)
- 付録 (この計画の一部。計画を削除するときはディレクトリごと削除する):
  - `docs/plans/2026-11-06-doc-integration/A-S1-library-cli-mcp.md`: Rust 側の詳細設計 (API のシグネチャ、JSON の形、MCP の引数と文言、テスト名)
  - `docs/plans/2026-11-06-doc-integration/B-S2-plugin-hooks.md`: plugin の hooks の詳細設計 (不具合の原因、表示パイプライン、入力の doc 経路、`lib/doc.js`、テスト名)
  - `docs/plans/2026-11-06-doc-integration/C-S3-skill-readme.md`: skill、MCP を同梱しない判断、README の追記文 (R1〜R9)
  - `docs/plans/2026-11-06-doc-integration/SKILL.md`: skill の正本 (そのまま複写する)
- 付録は英語の設計原文。**本文と付録が食い違うときは本文が優先する** (5.3 の調整を参照)。

## 1. 目的と成功基準

- 目的:
  1. `plamo-translate doc` の処理をライブラリ `plamo-doc` に切り出し、CLI と MCP server の両方から使う
  2. CLI の `doc` に機械可読の結果 (`--report json`) と、標準エラーを静かにする既定 (`--verbose` で詳細) を加える
  3. MCP に `translate_document` tool を加える
  4. plugin (plamo-ja-bridge) で、長い応答の和訳に失敗すると plugin がセッションの残りで消える不具合を直す
  5. plugin が長いプロンプトや単発の翻訳に失敗したブロックで `doc` を使う
  6. Claude が Bash で `plamo-translate doc` を使ってファイルを訳す skill を plugin に同梱する
- 成功基準: 9 章の全体検証がすべて期待どおりになり、9 章の手動確認 (不具合の再現手順) で plugin が消えないこと。

## 2. 範囲

- 範囲内: `crates/` (新しい `plamo-doc`、`plamo-translate`、`plamo-translate-mcp`)、`Cargo.toml`/`Cargo.lock`、`nix/packages.nix` の版、`claude-plugin/` (hooks、tests、skills、plugin.json)、`.claude-plugin/marketplace.json`、`README.md`
- 範囲外:
  - デーモン (Python) の変更。不要と確認済み
  - plugin への `.mcp.json` の同梱 (D-16 で同梱しないと決定)
  - README の `claude mcp add` を `-s user` に変えること (別件)
  - skill の起動の eval

## 3. 背景

- `doc` の処理は CLI クレートの非公開モジュール `crates/plamo-translate/src/doc/{driver.rs,history.rs,segment/}` にある。公開しているのは `DocArgs` と `run` だけ (`doc/mod.rs:1-3`)。driver は訳と診断を `out`/`err` の writer に書き、終了コードを返す。そのため MCP からは使えない。
- 現在の `doc` の診断は stderr のテキスト行 (`note:`/`warning:`/`error:`) だけで、機械可読の出力はない。
- 導入済みの CLI の状態:
  - `/etc/profiles/per-user/arakaki/bin/plamo-translate` は 0.1.0 で、`doc` はあるが `--report` はない。`doc --report json` を付けると終了コード 2 で `error: unexpected argument '--report' found` を出す [V]
  - `doc --help` は古い版でも終了コード 0 になるので、対応の判定には使えない [V]
  - リポジトリの `target/debug` はさらに古い
- `cargo` は `nix develop -c cargo …` で動く (cargo 1.99.0) [V]。
- MCP server (`crates/plamo-translate-mcp/src/server.rs`):
  - rmcp =3.8.0 を使い、`translate` tool が 1 つある
  - タイムアウトは呼び出し全体で 300 秒 (`PLAMO_TRANSLATE_MCP_TIMEOUT_SECS`)
  - progress 通知を送り、キャンセルするとデーモンへのソケットを閉じる
- plugin (`claude-plugin/hooks/register.js`) の現状:
  - 入力も表示もすべての翻訳を 1 本の `ctx.queue` に直列につなぐ
  - `classic.MessageDisplay` には `.catch` がない
  - 定数は `INPUT_TIMEOUT_MS=60000`、`OUTPUT_TIMEOUT_MS=20000`
- Claude Code 2.1.319 の hooks の制約 (`claude-plugin/.claude-plugin/types/claude-code/index.d.ts`。エンジンが書くファイルで、gitignore の対象):
  - 予算: 1 回の dispatch に 10 秒。自分の `$` 呼び出しと `next(e)` の間は時計が止まるが、ただの Promise を待つ間は時計が進む
  - 予算切れの後: `.catch` がなければ `next(e)` が代わりに走る
  - `lingerMs = 5000`
  - `$.process.run` のタイムアウトは最大 600 秒
- **不具合の原因** (付録 B 1.1。ユーザーの観察と一致):
  1. 表示の dispatch が `ctx.queue` を待つうちに 10 秒の予算を超える。予算切れの後も 5 秒以上動き続けると「overrun」になる
  2. overrun が 2 回続くと、エンジンが plugin を runaway と判断し、hooks worker を kill して作り直す
  3. その時点で `.catch` を持つ dispatch が実行中でなければ、plugin はセッションの残りで unload される
  4. ユーザーの観察:
     - `/ja` が消え、`/reload-plugins` か再起動でしか戻らなかった
     - 応答の段落の一部が消えた (保留中のテキストが失われ、実行中の dispatch が reject された)
  5. 根拠:
     - 予算切れと overrun はテストハーネスの実験で確認した
     - unload は Claude Code のバイナリの読解で確認した
     - ハーネスは unload を再現できないので、9 章の手動手順で確かめる
  6. `register.js:169-170` が記録している以前の「/ja が消えた」件も、同じ原因と推測している
- plugin のテスト: `claude plugin test claude-plugin` は基点で 98 件が通る。出力は ` 98 pass` / ` 0 fail` / `Ran 98 tests across 9 files.` の形 [V]。
- デーモンは要求を 1 件ずつ FIFO で処理する。待ちが 8 件を超えると `busy` を返す (`daemon/src/plamo_translated/server.py:29,67,161,275`)。

## 4. 前提条件

| 必要なもの | 用途 | 確認方法 | ない場合 |
|---|---|---|---|
| Nix の dev shell | T-1〜T-6、全体検証 | `nix develop -c cargo --version` → `cargo 1.99.0 …` | 止めてユーザーに聞く |
| Claude Code CLI 2.1.319 以降 | T-7〜T-15 のテスト、`claude plugin validate` | `claude --version` → `2.1.319` 以上 | 止めてユーザーに聞く (hooks の API の版に依存する) |
| エンジンの型定義 `claude-plugin/.claude-plugin/types/claude-code/index.d.ts` | plugin の型 | `ls claude-plugin/.claude-plugin/types/claude-code/index.d.ts` | `claude plugin test claude-plugin` を 1 回実行すると書かれる。それでもなければ止める |
| 動いているデーモン | 手動確認だけ | `plamo-translate status` → `loaded` を含む | 手動確認をユーザーに回す |

## 5. 決定事項

### 5.1 長期の方針 (変えるには再検討が要る)

実装中にこれらと衝突したら、止めて報告すること。詳細とテスト名は付録 A の 8 章 (G-1〜G-13) にある。各 D の「確認」は付録 A の対応する G の "Check" をそのまま使う。

#### D-1: `doc` の処理は新しいクレート `crates/plamo-doc` に置く (G-1、G-2、G-3)
- 決定:
  - 分割 (segment)、driver、履歴の選択を `crates/plamo-doc` (lib `plamo_doc`) に移す
  - 依存は `plamo-client`、`pulldown-cmark 0.13` (default-features = false)、`futures`、`tokio` (features は `time` だけ)
  - CLI に残すもの: clap の引数、stdin、履歴ファイルの読み込み、TTY の進捗、Ctrl-C、終了コード、stderr の文言
  - API の形:
    - `prepare(input, Option<Format>, &Limits) -> Document`
    - `translate<B: Backend, S: Sink>(backend, document, job, sink) -> Report`
    - 公開トレイトのメソッドは `fn … -> impl Future<Output=…> + Send` で書く。`async fn` で書くと clippy の `-D warnings` が `async_fn_in_trait` で落ちることを確認した
    - ライブラリは何も出力しない
  - 公開する名前は付録 A の G-2 の一覧と完全に一致させる
  - 警告と停止の文言は、現在の stderr の文言から接頭辞を除いたものと同じにする
- 理由:
  - MCP が CLI クレートに依存すると、clap などを抱え込む
  - plamo-client は通信の層で、5,500 行の分割処理を混ぜたくない
- 却下した案:
  - plamo-client の feature にする案: 役割が混ざる
  - MCP が CLI クレートに依存する案: 結合が強くなる
  - `#[allow(async_fn_in_trait)]`: `Send` の保証が得られない
- 確認: 付録 A の G-1、G-2、G-3 の Check
- リポジトリに残す場所: クレートの構造そのもの。追加の文書は不要

#### D-2: `doc --report json` を機械可読の契約にする (G-4、G-5、G-7)
- 決定:
  - stdout は訳だけ。区切りごとに書き出す。どのモードでも JSON を stdout に出さない
  - `--report json` のときの stderr:
    - 1 行に 1 つの JSON オブジェクトにする (`format`/`note`/`warning`/`error`/`summary`)
    - `doc::run` を通るすべての終了経路で、最後の行を `{"type":"summary","v":1,"exit_code",…}` にする
    - 項目は付録 A の U4 のとおり
  - summary がないとき (clap の誤り、kill、切り詰め) は、呼び出し側が失敗として扱う
  - 終了コードは現在と同じ
  - 区切りが 0 個のときの近道は、訳先の解決より前に置く
- 理由: plugin、skill、MCP が文字列の解析に頼らずに結果を判定できる。
- 却下した案:
  - `--json` という名前: 既存のテストが `doc --json` を終了コード 2 と決めている
  - `--report FILE`: hooks はファイルを読めない前提で設計している
- 確認: 付録 A の G-4、G-5、G-7 の Check
- リポジトリに残す場所: README (T-4 で R2、R3、R9 を書く)

#### D-3: `--verbose` (ユーザーの提案) (G-6)
- 決定:
  - text モードの既定で隠すのは次の 2 種類だけ。`--verbose` を付けると出る
    - `note: input format: …`
    - `warning: piece K/M: not used as history: …`
  - 訳されなかったもの、打ち切り、構造やインライン要素の変化、すべての `error:` は既定で出す
  - 短いオプション (`-v`) は作らない
  - `--report json` のときは無視する
- 理由: 対話で使うときに静かになる。一方で、一部が訳されていない文書がきれいに見えてはいけない。
- 確認: 付録 A の G-6 の Check
- リポジトリに残す場所: README (T-5 で R4 を書く)

#### D-4: MCP `translate_document` の契約 (G-8、G-9、G-10、G-11)
- 決定:
  - tool は `translate` と `translate_document` の 2 つだけにする
  - 引数と応答の形、検証の順序と文言、`.part` に書いてから rename する手順、タイムアウトの扱い、説明文は、付録 A の U6 のとおり
  - タイムアウト: `PLAMO_TRANSLATE_MCP_TIMEOUT_SECS` はデーモンから何も届かない時間の上限 (`ClientBackend::with_event_timeout`)。新しい `PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS` (既定 3600 秒) で呼び出し全体を打ち切る
  - `translate` の説明文に `translate_document` を案内する一文を加える
- 理由:
  - 文書を引数と結果で 2 回会話に入れない方法 (ファイルのパス) を用意する
  - 長い文書でもタイムアウトしないようにする
- 確認: 付録 A の G-8、G-9、G-10、G-11 の Check
- リポジトリに残す場所: README (T-6 で R1、R5、R6 を書く) と tool の説明文

#### D-5: 版と古い CLI の検出 (G-12)
- 決定:
  - `--report` を加えるコミットで、workspace の版と `nix/packages.nix` の `rustArgs.version` (59 行目) を 0.2.0 にする。Python の 10 行目は 0.1.0 のまま
  - プログラムから呼ぶ側 (plugin、skill) は必ず `--report json` を渡す
  - 終了コード 2 で stderr が `unexpected argument '--report'` に一致したら、「CLI が古い」と判定する
  - `doc --help` で調べてはいけない
- 確認: 付録 A の G-12 の Check
- リポジトリに残す場所: README (R3、R9)

#### D-6: テストの規律 (G-13 と、ユーザーのテスト方針)
- 決定:
  - 既存のテストのアサーションはそのまま残す。変えてよいのは次の 6 件だけで、それぞれコミットメッセージに理由を書く
    - driver のアサーション 2 件 (T-2)
    - `cli_doc::doc_driver_auto_format_prints_a_note` に `--verbose` を足す (T-5)
    - `mcp::lists_translate_tool_with_schema` の tool の数を 2 にし、名前で引く (T-6)
    - plugin の `config.test.ts` の `DEFAULTS` に `useDoc: true` を足す (T-10)
    - plugin の `prompt.test.ts` の `context_too_long without history fails once` を、実行 1 回から 2 回に変え、名前を変える (T-12)
  - テストを先に書き (TDD)、どの単位の後でも `cargo test`、clippy、`claude plugin test` が通るようにする
- 確認: 9 章の差分チェック

### 5.2 短中期の設計 (正当な理由があれば実装中に調整してよい。調整したら記録する)

#### D-10: 不具合の修正に必要な性質 (付録 B 1.1 の P1〜P6)
- P1: 他の dispatch の作業を待つときは、`boundedWait(promise, remainingMs − BUDGET_MARGIN_MS)` で上限を付ける。長い待ちは自分の `$` 呼び出しの中だけで行う
- P2: 自分の `$` 呼び出しは `untilAborted(promise, next.signal)` を通して待ち、中断されたらすぐに返す。中断の後も dispatch が残り続けないようにするため
- P3: 表示の翻訳は、`session.start` から起動した worker で行う。重複を防ぐために世代番号を持たせる
  - worker が動いていなければ、待っている dispatch が先頭のジョブを自分で実行する (help-run)
- P4: 入力の翻訳は表示の待ち行列を待たない。入力を訳している間、表示側は新しい呼び出しを始めない
- P5: `classic.MessageDisplay` に同期の `.catch` を付ける。保留中のテキストを原文のまま返し、該当の slot を取得済みにする
- P6: 期限切れ、代わりの実行、停止、doc への切り替えのたびに、debug ログを 1 行書く
- 根拠: エンジンの方針 (予算切れの後 5 秒残り続けると overrun、2 回続くと unload) を直接断つ。

#### D-11: 表示のパイプライン (付録 B 1.2)
- 新しい純粋なモジュール `lib/pipeline.js`。slot の状態 (`ready|queued|running|done|claimed`) で管理する
- 順序は `e.index` ではなく、届いた順に決める。delta を同期的に push し、メッセージごとの連番を振る
  - 既存のテストは `index++` をテスト全体で共有しているので、`e.index` を使うと止まる
- 最終の dispatch は、どの経路で終わっても未表示の slot をすべて返す
  - 実行待ちのジョブは自分で引き継ぐ (takeover)
  - takeover の上限は `OUTPUT_FINAL_MAX_MS` (60 秒)。1 回の呼び出しの `timeoutMs` は、上限の残り時間で頭打ちにする。残りが 3 秒を切ったら止める
- 訳せなかったブロックは英語のまま表示し、toast で知らせる

#### D-12: 入力での doc の経路 (付録 B 1.3)
- 次の場合は doc の経路に入る
  - 日本語を含む部分が 3000 バイトを超える
  - 単発の翻訳が `finish_reason ≠ stop`、`context_too_long` (履歴を使い切った後)、空の訳、予約トークンのどれかで失敗した。タイムアウトでは doc に切り替えない
- 引数は常に `--format markdown`。履歴は memory から `--history` で渡す。`--history-append` と `--history-last` は使わない
- 確認ダイアログ:
  - 長い英訳は途中を省き、`全文を表示` の選択肢を出す
  - 部分的にしか訳せなかったときは、`confirm: false` でも必ず確認する
- 途中で止まった run は失敗として扱う。区切りの位置が分からないので、残りをつなげない

#### D-13: `lib/doc.js` (付録 B 1.4)
- `runDoc` は doc を `$.process.run` で実行する。`timeoutMs` は `docTimeoutMs(bytes) = min(600000, 60000 + 20 × bytes)`
- `parseDocRun` が判定を一手に引き受ける
- 古い CLI を検出したら、セッションの間 doc を使わない。そのことは toast で 1 回だけ知らせる

#### D-14: 設定
- userConfig に `useDoc` (boolean、既定 `true`) を加える
- 新しい定数: `STREAM_WAIT_MS=2000`、`BUDGET_MARGIN_MS=2000`、`OUTPUT_FINAL_MAX_MS=60000`、`MIN_CALL_MS=3000`、`INPUT_DOC_BYTES=3000`、`OUTPUT_DOC_BYTES=6000`、`CONFIRM_PREVIEW_CHARS=1500`、`LOG_CHUNK_CHARS=1900`
- `ctx.queue` は削除する

#### D-15: skill `translate-document` (付録 C 1 章、正本は `SKILL.md`)
- skill の文章は英語。日本語での頼み方は `when_to_use` に含める
- `allowed-tools: Bash(plamo-translate doc *)` だけを許可する
- 出力は `<stem>.<to>.<ext>`。実行前に `ls` で、出力が入力と別で、まだないことを確かめる
- 必ず `--report json` を付ける。CLI が古ければ止めて、更新を促す

#### D-16: MCP は plugin に同梱しない
- 理由:
  - 絶対パスで登録した利用者のもとで、同じ server が重複する
  - バイナリのパスの設定が要る
  - Claude Desktop のチャットでは plugin を読まない
- README で、MCP は別に登録することを案内する (R8)

#### D-17: README の担当
- CLI と MCP の節: T-4〜T-6 (付録 C の R1〜R6、R9)
- skill の節: T-14 (R7、R8)
- 日英レイヤの `### 使い方` / `### 設定 (userConfig)` / `### 制限`: T-15 (5.3 の Cf-2 と Cf-3)

#### D-18: 版と公開の順序
- plugin.json を 0.1.1 から 0.2.0 に上げるのは T-15 の 1 回だけ
- marketplace と flake は同じリポジトリにあるので、全タスクを push すると CLI 0.2.0 と plugin 0.2.0 が同時に公開される
- 利用者は CLI と plugin を別々に更新する。plugin だけが新しい場合に備えて、古い CLI を検出する経路 (D-5) がある

### 5.3 統合のときに決めた調整 (付録より優先する)

設計どうしの食い違いを次のように解消した。

- **Cf-1 (`--format markdown` の注記)**:
  - 付録 B 1.3 の 8 にある「字下げした文章がコードとして残ると note が出て、確認が強制される」は誤り。`indented_code` の note は形式を自動判定したときだけ出る (付録 A の U4)
  - 入力では `--format markdown` のままにする。コードの翻訳を避けるため
  - 字下げした散文が訳されずに残る場合があることは、README の制限に書く (T-15)
  - `notes > 0` による確認の強制は、残りの note の種類 (`html`、`reserved_marker`) のために残す
- **Cf-2 (日英レイヤの README)**:
  - T-15 で、付録 B の "README facts for S3" の全項目を `### 使い方` / `### 設定 (userConfig)` / `### 制限` に書く
  - 文言は README の既存の文体 (日本語、簡潔、表) に合わせ、実装者が書く
- **Cf-3 (skill の doc 実行と /ja の同時利用)**:
  - デーモンは FIFO なので、hooks の単発の呼び出しは、doc のピース 1 つ分を待つことがある
  - 表示の待ちは 20 秒で打ち切られ、英語のまま toast で知らせる。これは `$` 呼び出しの中なので overrun にはならない
  - 入力の 60 秒は通常は足りる
  - `busy` (待ちが 8 件を超える) は、skill が逐次に実行するので起きない
  - この挙動を T-15 で README の制限に書く。コードの変更はない
- **Cf-4 (部分的な訳の文言)**:
  - 付録 B 1.3 の 4 の文言 `一部 (…) を英訳できず、日本語のまま残っています。` は使わない
  - 代わりに `一部 (${degraded}/${pieces} ピース) は英訳しきれていません (日本語のまま、または訳が途中で切れています)。` を使う
  - `truncated` のピースには途中までの訳が入るため。warning の種類は読まない
- **Cf-5 (版の上げ方)**: D-18 のとおり、T-15 で 1 回だけ上げる。付録 C の P1 の完了条件 4 (`plugin details` が 0.2.0 を表示) は T-15 で確かめる。
- **Cf-6 (summary の検証)**: plugin も skill も、次の 2 つを両方確かめる
  - `v === 1`
  - `exit_code` がプロセスの終了コードと同じ
  - plugin 側: `parseDocRun` の規則 3 の後に「`summary.v !== 1` なら失敗 (`unsupported report version`)」を加え、テストを 1 件足す (T-7)
  - skill 側: `SKILL.md` に一文を加えた (付録 C に埋め込んだ SKILL.md は古いので、正本は `SKILL.md`)
- **Cf-7 (MCP と skill の使い分け)**: 変えない
  - `translate` の説明文は、長い文書を `translate_document` に回すためのもの
  - skill は、Claude Code ではファイルなら skill を使うと書いている
  - 両者は矛盾しない

## 6. 実装上の制約と慣例

- コミットは conventional commits (`feat(cli): …`、`fix(plugin): …`、`feat(mcp): …`、`refactor(doc): …`)。1 タスク 1 コミット。
- コードのコメントは英語で、「なぜ」だけを書く。UI の文字列 (plugin の日本語の文言) は日本語のまま。計画の ID (D-x、T-x) や計画書をコードから参照しない。
- テストを先に書く (D-6)。実装をテストに合わせ、テストを実装に合わせない。テストのデータに合わせたハードコードはしない。
- Rust:
  - すべてのコマンドを `nix develop -c` の下で `--offline` で実行する
  - 新しいサードパーティの依存は加えない (必要なものは `Cargo.lock` にある)
  - stderr への書き込みは `let _ = writeln!(…)` で行う。`eprintln!` は stderr が閉じていると panic するので使わない
- plugin:
  - `$` を変数に代入しない。代入すると module が読み込まれない
  - 待ちはすべて `boundedWait` か `untilAborted` を通す (D-10)
  - テストで遅い fake を作るときは `mock.clock.sleep` を使う。ハーネスは 10 秒の予算を実時間で強制する

## 7. 実装タスク

| ID | タスク | 根拠 | 依存 | 並行 |
|---|---|---|---|---|
| T-1 | `plamo-doc` を作り、segment と history を移す | D-1 | - | Rust の系列 |
| T-2 | driver を `plamo-doc` に移し、構造化した API にする | D-1、D-6 | T-1 | 不可 |
| T-3 | CLI がライブラリを使う (`doc.rs`、`CliSink`、終了コード、文言) | D-1、D-2 | T-2 | 不可 |
| T-4 | `--report json` と版 0.2.0 (README の R2、R3、R9) | D-2、D-5 | T-3 | 不可 |
| T-5 | `--verbose` (README の R4) | D-3、D-6 | T-4 | 不可 |
| T-6 | MCP `translate_document` (README の R1、R5、R6) | D-4、D-6 | T-2 (README の追記は T-5 の後) | T-3〜T-5 と並行可 (README だけは順に) |
| T-7 | plugin `lib/doc.js` | D-13、D-5、Cf-6 | - | plugin の系列。Rust と並行可 |
| T-8 | plugin `lib/pipeline.js` | D-11 | - | T-7 と並行可 |
| T-9 | plugin `input.js` の `confirmPreview` | D-12 | - | 並行可 |
| T-10 | plugin の `useDoc` 設定 | D-14、D-6 | - | 並行可 |
| T-11 | plugin `register.js`: worker、パイプライン、`.catch`、入力を待ち行列から外す (**不具合の修正**) | D-10、D-11 | T-8、T-10 | 不可 |
| T-12 | plugin: 入力での doc | D-12、D-13、Cf-1、Cf-4 | T-7、T-9、T-11 | 不可 |
| T-13 | plugin: 出力での doc | D-11、D-13 | T-7、T-11 | T-12 の後 (register.js を共有するため) |
| T-14 | skill `translate-document` (README の R7、R8、plugin.json と marketplace.json の説明) | D-15、D-16、D-5 | T-4、T-5 | T-7〜T-13 と並行可 (README は T-6 の後) |
| T-15 | 日英レイヤの README、plugin の版 0.2.0 | D-17、D-18、Cf-2、Cf-3 | すべて | 不可 |

### T-1: `plamo-doc` を作り、segment と history を移す
- 目的: 分割処理をライブラリにする (動作は変えない)。
- 対象: `crates/plamo-doc/` (新規)、`crates/plamo-translate/src/doc/`、`crates/plamo-translate/Cargo.toml`、`Cargo.lock`
- 作業: 付録 A の U1 のとおり。`Cargo.toml` の内容、`git mv` の対象、`Format::as_str`、`NoteKind::as_str`、`Display for Note` の文言、一時的に公開する名前を指定してある。
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `nix develop -c cargo test --workspace --offline` → すべて成功
  - `nix develop -c cargo test -p plamo-doc --offline 2>&1 | grep "test result"` → 合計 `114 passed`
  - `grep -rn pulldown crates/plamo-translate` → 出力なし
- 注意: `FormatArg` とそのテストは、T-3 までは CLI 側に複製して残す。

### T-2: driver を `plamo-doc` に移し、構造化した API にする
- 目的: 出力しない API (`Sink`、`Report`、`Backend`、`ClientBackend::with_event_timeout`) を作る。
- 対象: `crates/plamo-doc/src/{driver.rs,report.rs,lib.rs}`、`crates/plamo-translate/src/doc/`
- 作業: 付録 A の U2 のとおり (API のシグネチャ、内部の置き換え、テストハーネスを `Mutex` にすること、変えるアサーション 2 件、新しいテスト 7 件)。
- 実装者に任せること: ライブラリの内部の分け方 (公開する名前は G-2 で固定)。
- 条件: 常に
- 完了条件:
  - `nix develop -c cargo clippy --offline --locked --workspace --all-targets -- -D warnings` → 警告なし
  - `nix develop -c cargo test -p plamo-doc --offline` → `150 passed` (114 + 29 + 7)
  - `nix develop -c cargo test -p plamo-translate --offline --test cli_doc` → `14 passed`。ファイルは変えない
  - `git diff 64a5b1b -- crates/plamo-translate/tests/cli_doc.rs` → 出力なし
- 注意:
  - トレイトのメソッドを `async fn` で書かない (D-1)
  - テストの backend は `Sync` にする必要がある

### T-3: CLI がライブラリを使う
- 目的: 文言と終了コードを CLI 側に集める。利用者から見た変化はない。
- 対象: `crates/plamo-translate/src/doc/mod.rs` を `crates/plamo-translate/src/doc.rs` にする
- 作業: 付録 A の U3 のとおり。`run()` の処理順 (1)〜(10) は必須で、区切りが 0 個のときの近道は訳先の解決より前に置く。テスト `renders_every_warning_kind_and_stop` を加える。
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `nix develop -c cargo test --workspace --offline` → すべて成功
  - `nix develop -c cargo test -p plamo-translate --offline --test cli_doc` → `14 passed`
  - `nix develop -c cargo test -p plamo-translate --offline renders_every_warning_kind_and_stop` → `1 passed`

### T-4: `--report json` と版 0.2.0
- 目的: D-2 と D-5 を実装する。
- 対象: `crates/plamo-translate/src/doc.rs`、`crates/plamo-translate/tests/cli_doc.rs` (テストの追加だけ)、`Cargo.toml` (`[workspace.package] version = "0.2.0"`)、`nix/packages.nix` の 59 行目、`Cargo.lock`、`README.md` (付録 C の R2、R3、R9)
- 作業: 付録 A の U4 のとおり (JSON の各オブジェクトの形、出さない場合、新しいテスト 6 件)。
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `nix develop -c cargo test -p plamo-translate --offline --test cli_doc` → `20 passed`
  - `nix develop -c cargo run -q -p plamo-translate --offline -- --version` → `plamo-translate 0.2.0`
  - `grep -n 'version = "0.2.0"' Cargo.toml nix/packages.nix` → それぞれ 1 件
  - `grep -c "unexpected argument '--report' found" README.md` → `2`
  - `grep -c '"type":"summary"' README.md` → `1`

### T-5: `--verbose`
- 目的: D-3 を実装する (ユーザーの提案。既定の stderr の契約が変わる)。
- 対象: `doc.rs`、`cli_doc.rs` (呼び出し 1 か所を変え、テストを 3 件足す)、`README.md` (R4)
- 作業: 付録 A の U5 のとおり。コミットメッセージに既定の stderr が変わることと、テストを変えた理由を書く。
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `nix develop -c cargo test -p plamo-translate --offline --test cli_doc` → `23 passed`
  - `nix develop -c cargo test -p plamo-translate --offline --test cli_translate` → `29 passed`
  - `nix develop -c cargo run -q -p plamo-translate --offline -- doc --help | grep -c '^  -v'` → `0`
  - `` grep -c '^| `--verbose`' README.md `` → `1`

### T-6: MCP `translate_document`
- 目的: D-4 を実装する。
- 対象: `crates/plamo-translate-mcp/{Cargo.toml,src/server.rs,src/document.rs (新規),tests/mcp.rs}`、`Cargo.lock`、`README.md` (R1、R5、R6)
- 作業: 付録 A の U6 のとおり。引数の表、検証の順序と文言、応答、制御の流れ、ファイルへの書き込み、progress の計算式、説明文の全文、既存のテスト 1 件の変更、新しいテスト 16 件を指定してある。テストには、`cli_doc.rs` の `fake_daemon_fn` のように複数の接続を受ける fake が必要になる。
- 実装者に任せること: `document.rs` と `server.rs` での責務の分け方。
- 条件: 常に
- 完了条件:
  - `nix develop -c cargo test -p plamo-translate-mcp --offline --test mcp` → `33 passed`
  - clippy → 警告なし
  - `grep -c 'tool が 1 つ使える' README.md` → `0`
  - `grep -c 'PLAMO_TRANSLATE_MCP_DOC_TIMEOUT_SECS' README.md` → `2`
- 注意: `translate` の合計タイムアウトの扱いは変えない (G-10)。

### T-7: plugin `lib/doc.js`
- 目的: doc の呼び出しと結果の判定を 1 か所にまとめる (D-13)。
- 対象: `claude-plugin/hooks/lib/doc.js` (新規)、`claude-plugin/tests/doc.test.ts` (新規)
- 作業:
  - 付録 B の Unit 1 (テスト 1〜13) のとおり
  - Cf-6 で、`parseDocRun` の規則 3 の後に `summary.v !== 1` なら `{ok:false, reason:'unsupported report version'}` とする規則を加える
  - テスト `parseDocRun fails an unknown report version` を加える (`v: 2` で失敗)
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `claude plugin test claude-plugin` → ` 0 fail`。`doc.test.ts` の 14 件を含む
  - `grep -c -- "--history-append\|doc --help" claude-plugin/hooks/lib/doc.js` → `0`
  - `grep -c "'--report', 'json'" claude-plugin/hooks/lib/doc.js` → `1`

### T-8: plugin `lib/pipeline.js`
- 目的: 表示の slot と順序を純粋な関数として扱う (D-11)。
- 対象: `claude-plugin/hooks/lib/pipeline.js` (新規)、`claude-plugin/tests/pipeline.test.ts` (新規)
- 作業: 付録 B の Unit 2 (テスト 14〜21) と 1.2 の State / Ordering / Taking turns のとおり。
- 実装者に任せること: 内部のデータ構造の細部 (付録 B の型を満たすこと)。
- 条件: 常に
- 完了条件: `claude plugin test claude-plugin` → ` 0 fail`。`pipeline.test.ts` の 8 件を含む

### T-9: `confirmPreview`
- 目的: 長い英訳の確認文を作る (D-12)。
- 対象: `claude-plugin/hooks/lib/input.js`、`claude-plugin/tests/input.test.ts` (テストの追加だけ)
- 作業: 付録 B の Unit 3 (テスト 22) と 1.3 の 5 の文言のとおり。
- 実装者に任せること: なし
- 条件: 常に
- 完了条件: `claude plugin test claude-plugin` → ` 0 fail`

### T-10: `useDoc` 設定
- 目的: D-14 の設定を加える。
- 対象: `claude-plugin/hooks/lib/config.js`、`claude-plugin/.claude-plugin/plugin.json` (userConfig だけ。版は変えない)、`claude-plugin/tests/config.test.ts`
- 作業:
  - 付録 B の Unit 4 (テスト 23、24) と 1.5 の title / description のとおり
  - `DEFAULTS` に `useDoc: true` を加える。既存のテストの変更として許される (D-6)
- 実装者に任せること: なし
- 条件: 常に
- 完了条件: `claude plugin test claude-plugin` → ` 0 fail`

### T-11: `register.js` の作り直し (不具合の修正)
- 目的: plugin が unload される原因を断つ (D-10、D-11)。
- 対象: `claude-plugin/hooks/register.js`、`claude-plugin/tests/message-display.test.ts` と `prompt.test.ts` (テストの追加だけ)
- 作業:
  - 付録 B 1.1 の P1〜P6 と 1.2 のすべて、1.3 の 1 (入力を待ち行列から外す、履歴の slot) を実装する
  - テスト 25〜33 を先に書く
  - `ctx.queue` は削除する
  - doc の経路は T-12 と T-13 で加える。このタスクでは、単発の呼び出しと worker / takeover だけを作る
- 実装者に任せること: `register.js` の中での関数の分け方。付録 B の名前 (`boundedWait`、`untilAborted`、`runDisplayWorker`、`finalTake` など) は目安。
- 条件: 常に
- 完了条件:
  - `claude plugin test claude-plugin` → ` 0 fail`
  - `grep -c "ctx.queue" claude-plugin/hooks/register.js` → `0`
  - `git diff 64a5b1b -U0 -- claude-plugin/tests/message-display.test.ts claude-plugin/tests/command.test.ts | grep -E '^-[^-]'` → 出力なし
- 注意:
  - 既存のテストは仕様。付録 B の「Existing tests that must pass unchanged」の表のとおり、変えずに通ること
  - 最終の dispatch は、期限切れ、中断、`.catch` のどれで終わっても、必ず `finalTake` を通る
  - 中断の後に `$` 呼び出しを待ち続けない (P2)

### T-12: 入力での doc
- 目的: D-12 を実装する。
- 対象: `claude-plugin/hooks/register.js`、`claude-plugin/tests/prompt.test.ts`
- 作業:
  - 付録 B の Unit 6 (テスト 34〜42) と 1.3 の 2〜7 のとおり
  - 部分的な訳を知らせる文言は Cf-4 のものを使う
  - 既存のテストの変更は D-6 の 1 件だけ (`expect(h.runs.length).toBe(1)` を `toBe(2)` にし、名前を `context_too_long without history fails once, then once with doc` にする)
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `claude plugin test claude-plugin` → ` 0 fail`
  - `git diff 64a5b1b -U0 -- claude-plugin/tests/prompt.test.ts | grep -E '^-[^-]'` → 古いテスト名の行と `expect(h.runs.length).toBe(1)` の 2 行だけ

### T-13: 出力での doc
- 目的: 単発の翻訳では扱えないブロックを doc で訳す (D-11、D-13)。
- 対象: `claude-plugin/hooks/register.js`、`claude-plugin/tests/message-display.test.ts` (テストの追加だけ)
- 作業: 付録 B の Unit 7 (テスト 43〜46) と、1.2 の `translateBlock` の 2、4、6 のとおり。
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `claude plugin test claude-plugin` → ` 145 pass`、` 0 fail`、`Ran 145 tests across 11 files.` (基点の 98 件 + 新しい 47 件)
  - `git diff 64a5b1b -U0 -- claude-plugin/tests/message-display.test.ts | grep -E '^-[^-]'` → 出力なし

### T-14: skill `translate-document`
- 目的: Claude が Bash で `plamo-translate doc` を使ってファイルを訳せるようにする (D-15、D-16)。
- 対象:
  - `claude-plugin/skills/translate-document/SKILL.md` (新規。`docs/plans/2026-11-06-doc-integration/SKILL.md` をそのまま複写する)
  - `claude-plugin/.claude-plugin/plugin.json` (`description` だけ)
  - `.claude-plugin/marketplace.json` (`description`)
  - `README.md` (R7、R8)
- 作業: 付録 C 5 章の P1 と R7、R8、Q4 の説明文のとおり。plugin.json の版は変えない (T-15)。
- 実装者に任せること: なし
- 条件: 常に
- 完了条件:
  - `cmp claude-plugin/skills/translate-document/SKILL.md docs/plans/2026-11-06-doc-integration/SKILL.md` → 出力なし、終了コード 0
  - `claude plugin validate --strict claude-plugin/skills` → 終了コード 0、最後の行が `✔ Validation passed`
  - `claude plugin validate claude-plugin` と `claude plugin validate .` → どちらも終了コード 0
  - `ls claude-plugin/.mcp.json` → 失敗する
  - `grep -c mcpServers claude-plugin/.claude-plugin/plugin.json` → `0`
  - `grep -c '^allowed-tools: Bash(plamo-translate doc \*)$' claude-plugin/skills/translate-document/SKILL.md` → `1`
  - `grep -c -- '--report json' claude-plugin/skills/translate-document/SKILL.md` → `3`
  - `` for k in v exit_code format pieces degraded warnings stopped_at complete history_append_failed; do grep -q "\"$k\"" claude-plugin/skills/translate-document/SKILL.md && grep -q "\"$k\"" README.md || echo "MISSING $k"; done `` → 出力なし
  - `grep -c '文書ファイルの翻訳 (skill)' README.md` → `1`
  - `claude plugin test claude-plugin` → ` 0 fail`
- 注意: 検証ツールは plugin の root を検証しても skill を解析しないので、skills を別に検証する。

### T-15: 日英レイヤの README と plugin の版
- 目的: plugin の変更を利用者に伝え、1 回だけ版を上げる (D-17、D-18)。
- 対象: `README.md` の `## Claude Code の日英レイヤ (plamo-ja-bridge)` の `### 使い方` / `### 設定 (userConfig)` / `### 制限`、`claude-plugin/.claude-plugin/plugin.json` の `version`
- 作業:
  - plugin.json の `"version": "0.1.1"` を `"0.2.0"` にする
  - README に次を反映する
    - 削除: 「和訳が終わる前に次のプロンプトを送ると、その応答は英語の原文で表示されることがある」
    - 追加:
      - 長い応答の最後の部分は、応答の後に最大で約 60 秒遅れて出ることがある
      - 訳せなかった段落は英語のまま表示し、toast で知らせる
      - 入力の翻訳を返信の表示より優先する
      - 長いプロンプト (日本語で約 1000 字以上) と、単発の翻訳に失敗したものは `plamo-translate doc` で訳す。0.2.0 以降が必要で、古いときは一度だけ toast で知らせて元の方法に戻る
      - 確認ダイアログの長い英訳は途中を省き、`全文を表示` で全文を見られる
      - 部分的にしか訳せなかったときは、`confirm` の設定に関係なく確認する
      - `### 設定 (userConfig)` の表に `useDoc` (既定 `true`) の行を足す
      - (Cf-1) 長いプロンプトで字下げした文章は、コードとして扱われ、訳されずに残ることがある
      - (Cf-3) skill などで長い `doc` を実行している間は、日英レイヤの翻訳が遅れたり、英語のまま表示されたりすることがある
      - 途中で `ja⇄en` と `/ja` が消えたら `/reload-plugins` を実行し、debug ログを添えて報告してほしい
- 実装者に任せること: README の日本語の文言 (既存の文体に合わせる。日本語、簡潔、平叙。表を使う)。
- 条件: 常に
- 完了条件:
  - `grep -c '和訳が終わる前に次のプロンプトを送ると' README.md` → `0`
  - `` grep -c '^| `useDoc`' README.md `` → `1`
  - `grep -c '/reload-plugins' README.md` → 1 以上 (基点より 1 件多い)
  - `claude --plugin-dir ./claude-plugin plugin details plamo-ja-bridge` → 1 行目が `plamo-ja-bridge 0.2.0`。`Skills (1)  translate-document` と `MCP servers (0)` を含む
  - `claude plugin test claude-plugin` → ` 145 pass`、` 0 fail`

## 8. 実行順序

```
Rust:    T-1 → T-2 → T-3 → T-4 → T-5
                  └→ T-6 (コードは T-2 の後なら並行可。README の追記は T-5 の後)
plugin:  T-7, T-8, T-9, T-10 (互いに並行可。Rust とも並行可)
         T-8, T-10 → T-11 (不具合の修正) → T-12 → T-13
skill:   T-4, T-5, T-6 → T-14 (plugin の系列と並行可)
最後:    すべて → T-15
```

- README を変えるタスク (T-4、T-5、T-6、T-14、T-15) は、この順にコミットする。同じファイルで衝突するのを避けるため。
- 不具合の修正だけを先に出したい場合は、T-8、T-10、T-11 だけで成り立つ (doc に依存しない)。

## 9. 検証計画

- 基点 (最初の変更の前):
  - `nix develop -c cargo test --workspace --offline` → すべて成功。plamo-client 37+7+5、plamo-translate 172 unit + 12 + 14 + 29、plamo-translate-mcp 4 + 17
  - `claude plugin test claude-plugin` → ` 98 pass`、` 0 fail`
- 全体の検証 (全タスクの後):
  - `nix develop -c cargo test --workspace --offline` → すべて成功
  - `nix develop -c cargo clippy --offline --locked --workspace --all-targets -- -D warnings` → 警告なし
  - `nix flake check` → 終了コード 0
  - `claude plugin test claude-plugin` → ` 145 pass`、` 0 fail`、`Ran 145 tests across 11 files.`
  - `git diff 64a5b1b -- crates/plamo-translate/tests/cli_doc.rs crates/plamo-translate-mcp/tests/mcp.rs claude-plugin/tests | grep -E '^-[^-]'` → D-6 の 6 件に当たる行だけ
- ユーザーによる手動確認:
  - 環境: 実機。CLI 0.2.0 を導入し、デーモンを動かしておく
  - 不具合の再現 (付録 B 5 章の "Manual reproduction"):
    1. `claude --debug-file /tmp/cc-debug.log` で起動し、`/ja on` を実行する
    2. `plamo-translate daemon restart` を実行する
    3. 長い応答を頼む (例: 「40 段落と表を含めて詳しく説明して」)
    4. 期待:
       - `ja⇄en` と `/ja` が残る
       - `grep -E "hook overran: .*plamo-ja-bridge|plamo-ja-bridge ignored its signal|respawning|plamo-ja-bridge was unloaded" /tmp/cc-debug.log` が何も出さない
    5. 最後の翻訳の最中に Esc を押しても、同じ grep が何も出さない
  - skill (付録 C の完了条件 10):
    - 一時ディレクトリで `sample.md を英訳して` と頼む。`ls` の確認の後、`plamo-translate doc -t en -s ja --format markdown --report json < 'sample.md' > 'sample.en.md'` が確認なしで実行され、最後にライセンスの案内が出る
    - `/plamo-ja-bridge:translate-document sample.md en` でも同じように動く
    - CLI 0.1.0 では「古い」と知らせて止まる
  - MCP: Claude Code に `claude mcp add` で登録し、`translate_document` に `input_path` と `output_path` を渡して、`.part` が最後に rename されることを確かめる

## 10. リスクと未解決事項

| ID | 項目 | 影響 | 解決の方法と時期 | 未解決のときの既定 |
|---|---|---|---|---|
| Q-1 | 返った hook から `$` を呼べるか (実セッション) | T-11 の worker | 手動確認で、最終の前に訳が出るかを見る | help-run で補う (実装済みの前提) |
| Q-2 | 最終の dispatch が自分の `$` で 60 秒使うと、エンジンが止まるか | T-11 | 手動確認 | 60 秒。止まるなら `OUTPUT_FINAL_MAX_MS` を 20 秒に下げる (コミットを追加) |
| Q-3 | 実際の doc の速度、`INPUT_DOC_BYTES`=3000、20 ms/バイト | T-12、T-13 | 0.2.0 の導入後に実測する | 既定値のまま |
| Q-4 | `prompt.submit` の最中に `$.ui.status` が再描画されるか | T-12 | 手動確認 | 表示を試みるだけ |
| Q-5 | unload の通知が transcript に出るか | なし | - | 設計には影響しない |
| Q-6 | skill の事前許可が、Claude が自分で起動したときも効くか。`${user_config.translator}` が既定値で置き換わるか | T-14 | 手動確認 (skill) | 確認が出ても動作は正しい。置き換わらなければ、その一文を「CLI のパスをユーザーに聞く」に変える |
| Q-7 | Claude Desktop や Code のクライアント側のタイムアウトが、MCP の progress で延びるか | T-6 | - | README に「確かめていない」と書く (R6) |
| Q-8 | `error_code_name` を plamo-client に移すか | なし | 後日 | plamo-doc に置く |

## 付録: レビューで変わった主な点 (蒸し返しを防ぐため)

- 公開トレイトの `async fn` が clippy で落ちると判明した → `impl Future + Send` の形に改めた (実験で確認)。
- `--verbose` で隠すもの:
  - 初版では構造やインライン要素の警告も隠していた
  - それらは出力された訳の問題を表すので、既定で出すことにした
  - 隠すのは 2 種類だけ
- 当初 (S1) はテキストモードで止まると部分訳を捨てていた → 返すことにした (MCP の `content[1]`)。
- MCP のタイムアウトは、無通信の上限 (per-event) と全体の上限の 2 段にした。初版の監視方式では、接続や事前確認の待ちを無通信として数えられなかった。
- 不具合の原因の変遷:
  - 当初の推定は「待ち行列で予算切れになり、その応答だけが英語になる」だった
  - ユーザーの観察 (`/ja` が消え、`/reload-plugins` でしか戻らない) と、バイナリの読解で「overrun 2 回で unload」と確定した
  - そのため、修正は `.catch` に頼らず、予算切れと居残りそのものを無くす方針にした
- 表示の順序は `e.index` ではなく届いた順に決める。既存のテストが index をテスト全体で共有しているため。
- doc はクロック待ちではなく `process.run` の `timeoutMs` で打ち切る。`$.clock` の待ちが予算に数えられるため。
- 確認ダイアログで英訳の途中を省くときは、`全文を表示` を用意した。中身を見ずに送らせないため。
- skill は、出力が入力と同じになる場合の確認を明示した (`>` が元の文書を消すため)。here-document の方式はやめた (stdout と stderr が混ざるため)。
- MCP の同梱はしない (D-16)。
