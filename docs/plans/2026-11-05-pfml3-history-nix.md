# PFML3 プロンプトのデーモン内構築、翻訳履歴、crane から buildRustPackage への移行

- 作成日: 2026-11-05
- Base: `main` @ `2971e1c` (「3. 前提となる現状」と file:line の参照はこの状態を指す)
- 状態: 計画確定 (未解決事項は 10 に既定値付きで記載)
- 付属ファイル: `docs/plans/2026-11-05-pfml3-history-nix/T01-*.patch` … `T09-*.patch`。T-1 から T-9 の参照実装で、Base に順に `git apply` でき、各段階で検証済み。計画書と一緒に削除する

## 1. 目的と成功基準

- 目的
  - デーモン `plamo-translated` が、モデル同梱の chat template (`apply_chat_template`) ではなく自前で PFML3 プロンプトを組み立てる。モデル README の本番向け推奨に従う。入力の検証もデーモンが自前で行い、キュー投入やモデルのロードより前に済ませる
  - 翻訳履歴 (同じ文書の前の原文と訳文の組) をプロトコル、CLI、MCP で扱えるようにする
  - Nix の Rust ビルドを crane から nixpkgs の `rustPlatform.buildRustPackage` に移し、flake input から crane を外す
- 成功基準 (9 の全体検証で確認する)
  - `nix flake check` → `all checks passed!`。crane が flake.lock にない
  - `cd daemon && pytest -m "not slow"`、`HF_HUB_OFFLINE=1 pytest -m slow`、`cargo test --workspace`、clippy `-D warnings` がすべて exit 0
  - `daemon/src` に `apply_chat_template` と `jinja2` が残っていない。chat template は slow テストのオラクルとしてだけ使う
  - 履歴付きリクエストが CLI と MCP から送れ、古いデーモンには明示的なエラーになる

## 2. スコープ

- 対象
  - デーモン: `pfml3` モジュール、検証、履歴、`finish_reason`、`status.features` / `status.template_verified`、ロード時の検査
  - キャッシュ済みスナップショットの判定の不具合修正 (T-7)
  - Rust: `plamo-client`、CLI、MCP の履歴対応
  - 共有 fixture、設計書と README の更新
  - `flake.nix`、`flake.lock`、`nix/packages.nix`、`nix/checks.nix`
- 対象外
  - 長文の自動分割 (今後も非目標)
  - Python パッケージのビルド方式 (`buildPythonApplication` のまま)
  - 8b / 31b モデルのテスト (実行時検査だけで扱う。D-6)
  - naersk の採用 (使わない)

## 3. 前提となる現状

- デーモンは `MlxEngine._prompt_tokens` で `apply_chat_template` を呼んでプロンプトを作り、`jinja2.exceptions.TemplateError` を `invalid_request` に変換している (`daemon/src/plamo_translated/mlx_engine.py:157-182`)。そのため、不正なリクエストもキューに入り、モデルのロードを引き起こしてから拒否される
- `parse_request` は型だけを検査し、意味の検証は template に任せている (`daemon/src/plamo_translated/protocol.py`)
- 設計書 (`docs/superpowers/specs/2026-11-05-plamo-translate-design.md`) は、37 行目で履歴を v1 の非目標とし、59 行目で「PFML3 の生成と検証は chat template を使うデーモン側だけが行う」としている。§5 にプロトコル v1、§6 に Rust 側の記述がある
- 共有 fixture は `protocol/fixtures/*.ndjson` にあり、Python と Rust の両方のテストが読む。`fake.template_error` キーで template の拒否を模擬している (`error_template.ndjson`)
- モデル (固定リビジョン `63777ddc6dc80aacacd92a1bf6966105f2f6270c`) について確認済みの事実
  - 制御トークンの id: bos=1、tag=16、key=17、val=18、msg=19、index=20、item=21。unk=0。`generation_config.json` の eos は [1, 16]
  - `Plamo3Tokenizer` は `<\|plamo:[^|\s]{,64}\|>` で前処理分割する。語彙にある `<|plamo:*|>` は、本文中にあっても制御トークンになる。`split_special_tokens` などで無効にする手段はない
  - 2b と 0.8b (`e5fe5b7f26ebaeae48db0ce644b932d1d9b91aaa`) の `chat_template.jinja` は同一で、sha256 は `cd49e7e0dfa8ec847c163db88dfca89b280dddbb73ddc60ba38c62a420eaced2`
  - mlx-lm 0.32.2 の `stream_generate` は、eos を検出すると detokenize の前に抜ける。そのため停止トークンの文字列は `response.text` に出ない。最後の `GenerationResponse.finish_reason` は `"stop"` か `"length"` で、max_tokens ちょうどで eos が出た場合は `"stop"`
  - キャッシュ済みの 2b スナップショットには mlx-lm の `DEFAULT_ALLOW_PATTERNS` に当たるファイルしかない。現在の `_resolve_snapshot` は全ファイルでのローカル判定で `IncompleteSnapshotError` (`LocalEntryNotFoundError` のサブクラス) を受け、毎回ダウンロード経路に入る。オフラインでは既定モデルをロードできない (再現済み)
- Nix は crane を使い、`nix/packages.nix` で `buildDepsOnly`、`buildPackage`、`fileset.commonCargoSources` を、`nix/checks.nix` で `cargoTest` と `cargoClippy` を使っている。`Cargo.lock` に git 依存はない。このマシンは `sandbox = false`
- mlx-lm は `nix/mlx-versions.nix` で 0.32.2 に固定されている。`just bump-mlx` は `nix flake check` と `just test-slow` を実行する

## 4. 前提条件

| 必要なもの | 用途 | 確認方法 | ない場合 |
|---|---|---|---|
| Nix (flakes 有効) | 全タスク | `nix --version` → exit 0 | 停止して利用者に確認 |
| devShell | 全タスクのテスト | `nix develop -c python3 -c 'import mlx_lm, transformers'` → exit 0 | 停止して利用者に確認 |
| 2b モデルのキャッシュ | T-6、T-7 の slow テスト | `ls "$HOME/Library/Application Support/plamo-translate/huggingface/hub/models--pfnet--plamo-3-translate-2b-MLX-4bit/snapshots/63777ddc6dc80aacacd92a1bf6966105f2f6270c/chat_template.jinja"` → 存在する | `plamo-translate model pull` (ネットワークが必要) を利用者に依頼する |
| 0.8b モデルのキャッシュ | T-5、T-6 の slow テスト | `ls "$HOME/Library/Application Support/plamo-translate/huggingface/hub/models--pfnet--plamo-3-translate-0.8b-MLX-4bit/snapshots/e5fe5b7f26ebaeae48db0ce644b932d1d9b91aaa/chat_template.jinja"` → 存在する | 利用者にダウンロードを依頼する。またはオラクルの対象を `PLAMO_ORACLE_MODELS` で 2b だけにし、その旨を報告する |
| Apple silicon GPU | slow テスト | `nix develop -c python3 -c 'import mlx.core as mx; print(mx.default_device())'` → `Device(gpu, 0)` | slow テストを省き、その旨を報告する |

slow テストはすべて `HF_HUB_OFFLINE=1` を付けて実行する。HF の HUB キャッシュは、デーモンと同じく上記の Application Support 配下を使う (既存の slow テストの conftest に従う)。

## 5. 決定事項

### 5.1 長期的な方針 (変更するには再検討が必要)

実装中にこれらと衝突したら、停止して報告する。

#### D-1: 履歴のワイヤ形式とバージョン方針

- 決定
  - `translate` リクエストに、任意のトップレベルフィールド `history` を足す。値は `{"source": string, "translation": string}` の配列で、古い順に並べ、最後の要素が `text` の直前に訳した組になる
  - キーがない場合、`null`、`[]` はどれも「履歴なし」で、同じプロンプトになる。要素の余分なキーは無視する
  - 件数の上限は設けない。制限は 1 行 4 MiB とトークン上限 (D-8) だけ
  - プロトコルは `v: 1` のまま変えない (追加的な変更)
  - `done` に `finish_reason`、`status` に `features` と `template_verified` を足す (D-7、D-9)
- 例 (実際は 1 行)

```json
{"v":1,"op":"translate",
 "text":"オーナーでフィルタリングすることもできます。",
 "target_lang":"en","source_lang":"ja",
 "history":[
   {"source":"プロジェクトの管理画面を開きます。","translation":"Open the project dashboard."},
   {"source":"進行中のタスクがすべて表示されます。","translation":"It lists all active tasks."}],
 "stream":true}
```

- 理由: 既存のクライアントと fixture を壊さない。組のオブジェクトにしておけば順序を取り違えようがない。`target` ではなく `translation` という名前にしたのは、`target_lang` と並べたときに紛らわしいため (glossary は出荷済みなので `source`/`target` のまま)
- 却下した案
  - `v: 2`: 古いデーモンに対して、履歴のない通常の翻訳まで失敗する
  - chat 形式の `[{"role":...}]`: template の都合を持ち込むだけ
  - キー名 `source`/`target`: `target_lang` と紛らわしい
- 影響範囲: daemon の `parse_request`、Rust の `TranslateRequest`、MCP のスキーマ、fixture
- 見直す条件: 既存フィールドの意味を変える必要が出たとき
- 適合チェック: G-1。`translate_history.ndjson` が両方のスイートで通る。Rust の単体テストが上記 JSON と完全一致する
- リポジトリでの記録先: 設計書 §5 (T-9)、§6 (T-16)

#### D-2: 意味の検証はデーモンが、キュー投入とロードの前に行う

- 決定
  - 意味の検証は、トークナイザもモデルも使わない純粋関数で行う。`_serve_connection` の中で `parse_request` の直後に呼び、`status` / `shutting_down` / `busy` の分岐とキュー投入より前に置く。対象は `op: translate` だけ
  - Rust 側はどの規則についても正とはならない。clap などで UX のための事前チェックをするのはよい
- 理由: 不正なリクエストをキューで待たせたり、数 GB のロードを起こしたりしない。規則を fixture でテストできる
- 適合チェック: G-2。`pytest tests -k validation_precedes_load` が通り、エラーのあとも `engine.loaded is False`
- リポジトリでの記録先: 設計書 §5 (T-9)

#### D-3: 検証の順序と正確なメッセージ

- 決定: コードは `invalid_request` だけを使い、新しいコードは作らない。`fake.template_error` は削除する。2 段階で検査し、最初の失敗を返す
  1. `parse_request` (既存の順序): 行サイズ、JSON、object、`v`、`op`、`stream`、`max_tokens`、`text`、`target_lang`、`source_lang`、`context`、`glossary`、`form`、最後に `history`。既存のメッセージは変えない。新しいメッセージは `field history must be an array`、`history entries must be objects`、`history entries need string source and translation`
  2. 意味の検証 (template と同じ順序): 履歴の組を添字順に (`history[i].source`、`history[i].translation`)、`text`、`target_lang`、`source_lang`、`context`、`form`、glossary を添字順に (`.source`、`.target`)。文字列フィールドでは、空の検査を予約トークンの検査より先に行う
- メッセージ (`<i>` は 0 始まりの 10 進数)

| フィールド | 規則 | メッセージ (完全一致) |
|---|---|---|
| `history[<i>].source` | `value.strip() != ""` | `history[<i>].source must not be empty.` |
| | `"<\|plamo:" not in value` | `history[<i>].source must not contain reserved PFML tokens (<\|plamo:...).` |
| `history[<i>].translation` | 同上 | `history[<i>].translation must not be empty.` / `history[<i>].translation must not contain reserved PFML tokens (<\|plamo:...).` |
| `text` | 同上 | `text must not be empty.` / `text must not contain reserved PFML tokens (<\|plamo:...).` |
| `target_lang` | `value == "ja(easy)"` または `re.fullmatch(r"[a-z]{2,3}(-[a-z]{4})?", value)` | `target_lang must be a lowercase language code, e.g. en, ja, zh-hans, or ja(easy).` |
| `source_lang` (null でなければ) | 同上 | `source_lang must be a lowercase language code, e.g. en, ja, zh-hans, or ja(easy).` |
| `context` (null でなければ) | `<\|plamo:` を含まない。`""` は許し、プロンプトから省く。空白だけならそのまま出す | `context must not contain reserved PFML tokens (<\|plamo:...).` |
| `form` (null でなければ) | `plain` か `polite` で、`target_lang == "ja"` (完全一致。`ja(easy)` は不可) | `form must be polite or plain and requires target_lang="ja".` |
| `glossary[<i>].source` / `.target` | 空でない、`<\|plamo:` を含まない | `glossary[<i>].source must not be empty.` / `glossary[<i>].source must not contain reserved PFML tokens (<\|plamo:...).` (`.target` も同じ形) |

  (表中の `\|` は Markdown 上のエスケープで、実際の文字列は `|`)
- 言語コードは 53 言語の一覧とは照合しない (template も照合していない)。`auto` は規則に通らないので、クライアントが解決し忘れると `invalid_request` になる
- 理由: リクエストに対してメッセージが 1 つに決まるので、fixture とクライアントの期待が一致する
- 適合チェック: G-3。規則ごとの fixture と `error_history_precedence.ndjson`。`git grep -l template_error` が何も出力しない

#### D-4: 予約トークンは拒否し、エスケープしない

- 決定
  - 次のフィールドが部分文字列 `<|plamo:` (大文字小文字を区別) を含んでいたら拒否する: `text`、`context`、`history[].source`、`history[].translation`、`glossary[].source`、`glossary[].target`
  - 文字を挿入する、`AhoCorasick.encode` を迂回するといった方法で、エスケープや再エンコードをしてはならない
  - 言語コードは `re.fullmatch` で検査する (`$` は末尾の改行の前にもマッチするので `"en\n"` が通ってしまう)
- 理由: トークナイザは語彙にある `<|plamo:*|>` を必ず制御トークンに変える (3 を参照)。制御トークンの集合はリビジョンによって変わりうる。検査はトークナイザなしで行う必要がある。そして template の既存の振る舞いと同じになる
- 適合チェック: G-4。`error_reserved_token.ndjson`、`error_bad_lang.ndjson`、`pytest tests -k lang_code`

#### D-5: PFML3 プロンプトの組み立てとトークン化

- 決定
  - リクエストの純粋関数として組み立てる。要素の順序は次のとおりで、空白の追加や除去は一切しない
    1. `<|plamo:bos|>` と `dataset`
    2. `context` (`context != ""` のときだけ)
    3. 履歴の組ごとに `input` (`source_lang` があれば `lang` 属性) と `output` (`lang`=target_lang)
    4. `glossary` (空でないときだけ)
    5. 現在の `input` (`source_lang` があれば `lang` 属性)
    6. `output` (`lang`=target_lang、`form` があれば `form` 属性) と `<|plamo:msg|>`
  - プロンプト文字列全体 (先頭の BOS 文字列を含む) を `add_special_tokens=False` で 1 回の `encode` にかける。id 列は BOS の id 1 つで始まる
  - 「1 回」は「文字列全体を 1 回の呼び出しでエンコードする (分割してつながない、`add_special_tokens=True` を使わない)」という意味。`count_tokens` と `stream` がそれぞれエンコードし、リクエストごとに 2 回になるのは適合している (12,020 トークンで 14 ms)。キャッシュは不要。入れる場合も id を変えてはならない
- D-1 の例から作られるプロンプト (template の出力とバイト単位で一致することを確認済み)

```
<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>プロジェクトの管理画面を開きます。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>Open the project dashboard.<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>進行中のタスクがすべて表示されます。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>It lists all active tasks.<|plamo:tag|>input<|plamo:key|>lang<|plamo:val|>ja<|plamo:msg|>オーナーでフィルタリングすることもできます。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>
```

- 適合チェック: G-5。fast の golden テストと、slow テストでの BOS の検査

#### D-6: chat template はオラクルとして残し、ロード時に検査する

- 決定
  - slow テストで、10 ケースについて builder の出力と `apply_chat_template(..., tokenize=False)` の文字列が一致すること、`tokenize=True` の id が一致すること、BOS がちょうど 1 つで先頭にあることを確かめる。ケースは任意フィールドのすべて、履歴 0/1/3 組、`source_lang` の有無、`context=""`、`form` を網羅する。対象モデルは 2b と 0.8b で、`PLAMO_ORACLE_MODELS` で変えられる
  - `MlxEngine.ensure_loaded` は、ロード後に次を満たさなければ `model_load_failed` で失敗する (ハード検査)
    - 7 つの制御文字列 (bos、tag、key、val、msg、index、item) がそれぞれ unk でない 1 つの id にエンコードされる
    - tag と bos の id が eos id に含まれる
    - 失敗はプロセスの間記憶し、再ロードしない
  - `TEMPLATE_SHA256 = "cd49e7e0dfa8ec847c163db88dfca89b280dddbb73ddc60ba38c62a420eaced2"` と `<snapshot>/chat_template.jinja` を照合して `template_verified` を求める。不一致やファイルがない場合は WARNING を 1 回だけ出し、そのまま使う (ソフト検査)。値は `status.template_verified` で報告する
  - `status` が `loaded: true` なら、`template_verified` は必ず bool で含める。`loaded: false` のときは省いてよい (ロード前は値がないため)
  - モデルのリビジョンを上げるときは、`pytest -m slow` が新しいスナップショットで通ってから `TEMPLATE_SHA256` を更新する
- 理由: モデルが受け取るのは文字列ではなく id。ハード検査で非互換のトークナイザをすぐ止め、ソフト検査では使用を止めずに template のずれを知らせる
- 見直す条件: 8b / 31b の template やトークナイザが 2b と異なると分かったとき
- 適合チェック: G-6。`HF_HUB_OFFLINE=1 pytest -m slow -k oracle`、ロード検査の fast テスト
- リポジトリでの記録先: 設計書 §5、ノート `docs/superpowers/notes/2026-11-05-pfml3-tokenizer-findings.md` (T-9)

#### D-7: `done` の契約と、履歴に使える組の判定 (MCP については利用者の承認で修正済み)

- 決定
  - `translate` の `done` には、mlx-lm のラベルをそのまま `finish_reason` (`"stop"` | `"length"`) として含める。`op: load` の `done` には含めない
  - `done.text` は、デコードした断片をつないだものから、末尾の `<|plamo:tag|>` か `<|plamo:bos|>` を 1 つだけ除いたもの。ほかには手を加えない (trim もしない)。`delta` は best effort で、`done.text` を正とする
  - 履歴に使える組の条件: `finish_reason == "stop"`、`done.text.strip() != ""` (Python の `strip()` と同じ空白集合。Rust では `char::is_whitespace` に U+001C から U+001F を足す)、`"<|plamo:" not in done.text`
  - 使える組は変更せずに履歴へ戻す。使えない組の扱いは、履歴を誰が持つかで分ける
    - 履歴を自分で持つクライアント (CLI) は、その組を履歴に加えない。stderr に 1 行の警告を出してよい
    - 履歴を持たないクライアント (MCP。履歴は呼び出し側のモデルが持つ) は、使えない結果に 2 つ目の content を付ける。中身は `note: do not pass this translation as history: <reason>.` で、`isError` は false のまま。tool の説明に「`note:` で始まる content は訳ではない」と書く
- 理由: 使える組は必ずデーモンの検証を通るので、「変更せずに戻す」が成り立つ。MCP のサーバーは履歴から外すことができないので、呼び出し側に伝えるしかない
- 適合チェック: G-7 (修正後)。`translate_history.ndjson` が `finish_reason` を検査する。`load_with_download.ndjson` には `finish_reason` がない。末尾の停止トークンを除くテスト。Rust の判定関数のテスト。MCP で `length` のときに `note:` が付き、`stop` のときに付かないテスト

#### D-8: トークン上限はデーモンがエラーにし、履歴を削るのはクライアントの役目

- 決定
  - 完全なプロンプト (履歴を含む) のトークン数と `max_tokens` の合計が 32,768 を超えたら、デーモンは `context_too_long` を返す。メッセージは既存の形式で数値を含める: `input <n> tokens + max_tokens <m> = <n+m> exceeds the 32768-token limit` / `input is <n> tokens, leaving no room for output within 32768 tokens`
  - デーモンは履歴、glossary、context を削ってはならない
  - 再試行するならクライアントが行う。その場合は、試行のたびに古い方から `ceil(n/2)` 組を削り、クライアントが指定した `max_tokens` は保ち、履歴が空になったら止める
  - CLI は再試行しない (`--history-last` で減らす)。MCP は再試行し、削ったことを `note:` で伝える
- 理由: 結果が再現できなくなるような、黙った意味の変更をしない。トークン数を組ごとに知らなくても、再試行の回数を抑えられる
- 適合チェック: G-8

#### D-9: 追加的な変更の規則と、機能の告知

- 決定
  - 任意のリクエストフィールドやイベントフィールドを足すだけの変更は、`v: 1` のまま行う。デーモンは未知のリクエストフィールドを、クライアントは未知のイベントフィールドを無視し続ける
  - リクエスト側の機能は `status.features` で告知する。現在の値は `["history"]`
  - 機能に依存するリクエストを送るクライアントは、先に `status.features` を確かめる。ない場合は送らずに `daemon does not support translation history; run "plamo-translate daemon restart"` で失敗する (CLI は exit 1、MCP は `isError`)
  - 肯定の結果はキャッシュしてよいが、この計画ではキャッシュしない (D-12)。否定の結果はキャッシュしない
  - `v` を上げるのは非互換な変更のときだけ
- 理由: 古いデーモンに対して、機能が黙って失われるのを防ぎ、翻訳の前に失敗させる
- 適合チェック: G-9。`status.ndjson` が `"features":["history"]` を検査する。Rust のテストで、`features` のない `status` を返すとエラーになり translate が送られない。`unknown_fields.ndjson` が引き続き通る
- リポジトリでの記録先: 設計書 §5 (T-9)、§6 (T-16)

### 5.2 短中期の設計

正当な理由があれば実装中に調整してよい。調整した場合は記録する。

#### D-10: Nix の Rust ビルド (buildRustPackage)

- パッケージ、`checks.cargo-test`、`checks.clippy` を、共有の `rustArgs` (`pname`、`version`、`src`、`cargoLock.lockFile = ../Cargo.lock`) から作る 3 つの `buildRustPackage` derivation にする。`cargoHash` は使わない。vendor derivation は 3 つで共有される (確認済み)
- パッケージは `doCheck = false` のまま。`postInstall` で `libexec/bin/plamo-translated` へのシンボリックリンクを作り、`meta.mainProgram = "plamo-translate"` を保つ
- `cargo-test` は debug プロファイル (`buildType = "debug"`)、`dontCargoBuild = true`、`cargoTestFlags = [ "--workspace" ]`、`auditable = false`、`installPhase = "touch $out"`。crane は `--release` だったが意図的に変える (debug が 38 s、release が 114 s。コードに `cfg(debug_assertions)` やプロファイルの上書きはなく、違うのはオーバーフロー検査だけで、debug の方が厳しい)
- `clippy` は独自の `buildPhase` で `cargo clippy -j "$NIX_BUILD_CORES" --offline --locked --workspace --all-targets -- -D warnings` を実行し、`doCheck = false`、`auditable = false`、`installPhase = "touch $out"`
- パッケージを `overrideAttrs` してチェックを作らない (`postInstall` を通じて Python と MLX のクロージャに依存してしまう)
- ソースは `Cargo.toml`、`Cargo.lock`、`crates/` 全体 (`*.snap.new`、`*.pending-snap`、`.DS_Store` を除く)、`protocol/fixtures`。新しい fixture やクレートを足しても、ここを編集する必要はない
- コスト: コードを変えたときの `nix flake check` は crane の約 40 s から約 85 s になる。Cargo.lock を変えたときはほぼ同じ。対策はしない
- 見直す条件: コードに `cfg(debug_assertions)` やプロファイルの上書きが入ったら、`buildType = "debug"` を外す

#### D-11: デーモンの実装設計

- 新しい純粋モジュール `daemon/src/plamo_translated/pfml3.py` (mlx を import しない) に次を置く: 定数 (`CONTROL_TOKENS`、`STOP_TOKENS`、`RESERVED_PREFIX`、`TEMPLATE_FILE`、`TEMPLATE_SHA256`)、`validate`、`is_lang_code`、`build_prompt`、`prompt_ids`、`strip_stop_token`、`check_tokenizer`、`template_verified`。完全なコードは T03 の patch にある
- `protocol.py`: `HistoryEntry(source, translation)`、`TranslateRequest.history: tuple[HistoryEntry, ...] = ()` (最後のフィールド)、`_parse_history` (`parse_request` の最後のキーワード引数)、`done(..., finish_reason=None)`
- `engine.py`: `TextStream` (空でない文字列のイテレータに `finish_reason` 属性を持たせる。自然に終われば mlx-lm のラベル、キャンセルされたら `None`)。`Engine.stream -> TextStream`。`Engine.template_verified: bool | None`
- `server.py`: `FEATURES = ("history",)`。`_serve_connection` で `parse_request` の直後に `pfml3.validate`。`_run_job` は、`finish_reason` が `None` (キャンセル以外) なら `internal` エラー (mlx-lm は固定されており、`just bump-mlx` で slow テストが検査する)。`done.text` は `strip_stop_token("".join(pieces))`。`_status()` に `features` と `template_verified` を足す
- `MlxEngine`: jinja2 と `apply_chat_template` を使わない。`_prompt_ids` を介して `pfml3.prompt_ids` を呼ぶ。ロード後に `check_tokenizer` を実行し、失敗を `_incompatible` に記憶する。`template_verified` と `TEMPLATE_DRIFT_WARNING` を持つ。キャッシュの判定には `allow_patterns=MODEL_FILE_PATTERNS` (`mlx_lm.utils.DEFAULT_ALLOW_PATTERNS`) を使う。ダウンロード経路は変えない
- `FakeEngine` (`daemon/tests/conftest.py`): `template_error` を削除する。`stream` は `TextStream` を返し、`len(tokens) < max_tokens` なら `"stop"`、それ以外は `"length"` で終える。`template_verified` は `True if self.loaded else None`
- 依存の変更はない (テストで使う jinja2 は transformers 経由で入る)

#### D-12: Rust 側の実装設計

- `plamo-client`
  - `HistoryEntry { source, translation }` (serde、`cfg_attr(feature = "schemars", derive(JsonSchema))`、doc コメントがスキーマの説明になる)
  - `TranslateRequest.history: Vec<HistoryEntry>`。手書きの `Serialize` の `Wire` で `context` の後に置き、`skip_serializing_if = "<[HistoryEntry]>::is_empty"`
  - `Event::Done.finish_reason: Option<String>`、`Status.features: Option<Vec<String>>`、`Status.template_verified: Option<bool>`、`FEATURE_HISTORY`、`Status::supports`
- 事前確認は `Client::translate` の中で行う。`history` が空でなければ毎回 `status` を送り、キャッシュはしない (デーモンは `status` を busy チェックより前に返すので安い。ロールバック時に古くなるキャッシュを避ける)。`Client::require_history()` も公開する
- 新しいモジュール `history.rs`: `check_history_eligible`、`history_entry`、`Ineligible`、`parse_history` (JSONL)、`HistoryError`、`append_history`、`drop_oldest_half`、`RESERVED_TOKEN_PREFIX`
- CLI: `--history FILE` (JSONL)、`--history-append`、`--history-last N` を足す。処理の順序は T-14 に固定する。`--json` に `finish_reason` を足し、`status` に `features:` と `template_verified:` の行を足す
- MCP: `history` 引数を足す。`context_too_long` のときは半分ずつ削って再試行する。`note:` の content を付ける (D-7、D-8)。古いデーモンには `isError`

## 6. 実装上の制約と規約

- 1 タスク 1 コミット。各コミットの時点で `cd daemon && pytest -m "not slow"` と `cargo test --workspace` が exit 0 になること (D-1 から D-12 のすべて)
- TDD で進める。各タスクでは、先にテストを書いて意図どおりに失敗することを確かめてから実装する (利用者のグローバル指示)。付属 patch のコードは使ってよいが、テストを先にコミット内で書く順序は守る
- 既存のテストは仕様として扱う。変えてよいのは、各タスクに「既存テストの変更」として書いたものだけで、それぞれ方針の変更に対応している。ほかの既存テストが落ちたら実装を直す
- 新しいファイルは、`nix build` の前に `git add` する (flake は追跡中のファイルしか見ない)
- コードやコメントに計画の ID (D-x、T-x、G-x) やこの計画書への参照を書かない
- コマンドはリポジトリのルートから `nix develop -c ...` で実行する (devShell の中にいるならそのままでよい)
- 既存のコードのコメント、ドキュメント、文字列の言語は変えない (日本語のドキュメントは日本語のまま)

## 7. 実装タスク

| ID | タスク | 根拠 | 依存 | 並行可 |
|---|---|---|---|---|
| T-1 | Nix: crane から buildRustPackage へ | D-10 | - | 可 (ほかと独立) |
| T-2 | daemon: 履歴の解析と `done.finish_reason` | D-1, D-7, D-11 | - | - |
| T-3 | daemon: `pfml3` モジュール (純粋) | D-3, D-4, D-5, D-6, D-11 | T-2 | - |
| T-4 | daemon: サーバーでの検証、`template_error` の削除、エラー fixture | D-2, D-3, D-4 | T-3 | - |
| T-5 | daemon: `TextStream`、`finish_reason`、`done.text` の末尾除去 | D-7, D-11 | T-4 | - |
| T-6 | daemon: MlxEngine を builder に切り替え、ロード検査、オラクル | D-5, D-6, D-11 | T-5 | - |
| T-7 | daemon: キャッシュ済みスナップショットの判定修正 | D-6, D-11 | T-6 | - |
| T-8 | daemon: `status.features` / `template_verified`、履歴の fixture | D-1, D-6, D-8, D-9 | T-7 | - |
| T-9 | docs: 設計書 §5、ノート、旧計画書の注記 | D-1 から D-9 | T-8 | - |
| T-10 | client: プロトコル型 | D-1, D-7, D-9, D-12 | - | 可 (daemon 側と独立) |
| T-11 | client: 判定、JSONL、追記、半減 | D-7, D-8, D-12 | T-10 | - |
| T-12 | client: 事前確認 | D-9, D-12 | T-11 | - |
| T-13 | CLI: `--json` の `finish_reason`、`status` の表示 | D-7, D-9, D-12 | T-12 | - |
| T-14 | CLI: `--history` 系のフラグ | D-1, D-7, D-8, D-9, D-12 | T-13 | - |
| T-15 | MCP: `history`、注記、古いデーモン、再試行 | D-1, D-7, D-8, D-9, D-12 | T-12 | - |
| T-16 | docs: README と設計書 §6 | D-1, D-7, D-8, D-9 | T-9, T-14, T-15 | - |
| T-17 | client: 履歴 fixture とシリアライズの一致 | D-1 | T-8, T-10 | - |

### T-1: Nix: crane から buildRustPackage へ

- 目的: crane の input をなくし、nixpkgs 標準の Rust ビルドにする
- 対象: `flake.nix`、`flake.lock`、`nix/packages.nix`、`nix/checks.nix`
- 作業
  - `git apply docs/plans/2026-11-05-pfml3-history-nix/T01-nix-buildrustpackage.patch` で適用できる (Base に対して `git apply --check` 済み。`flake.lock` も含む)。手で書く場合は次のとおり
  - `nix/packages.nix`: 引数を `{ pkgs }:` にする。`cargoSrc =` 以降を次で置き換える

```nix
  # crates/ holds only Rust sources, tests, fixtures, and insta snapshots, so it is taken whole and new
  # crates or fixtures need no edit here. Pending insta snapshots are excluded so a stray one cannot
  # change the build inputs. plamo-client's fixture test reads ../protocol/fixtures.
  cargoSrc = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      (lib.fileset.difference ../crates (
        lib.fileset.fileFilter (
          f: f.hasExt "new" || f.hasExt "pending-snap" || f.name == ".DS_Store"
        ) ../crates
      ))
      ../protocol/fixtures
    ];
  };

  rustArgs = {
    pname = "plamo-translate";
    version = "0.1.0";
    src = cargoSrc;
    cargoLock.lockFile = ../Cargo.lock;
  };

  plamo-translate = pkgs.rustPlatform.buildRustPackage (
    rustArgs
    // {
      # checks.cargo-test runs the suite; avoid running it twice.
      doCheck = false;
      postInstall = ''
        mkdir -p $out/libexec/bin
        ln -s ${plamo-translated}/bin/plamo-translated $out/libexec/bin/plamo-translated
      '';
      meta.mainProgram = "plamo-translate";
    }
  );
in
{
  packages = {
    inherit plamo-translated plamo-translate;
    default = plamo-translate;
  };
  inherit rustArgs;
}
```

  - `nix/checks.nix`: 引数を `{ pkgs, home-manager, hmModule, packages, rustArgs }:` にする。`cargo-test` と `clippy` を次で置き換える。`hmTest`、`pytest`、`hm-module` は変えない

```nix
  # Debug profile, like `cargo test --workspace` in the justfile: a third of the release compile
  # time, and overflow checks stay on. No build phase: `cargo test` builds the binaries the tests run.
  # The checks ship no binaries, so cargo-auditable has nothing to embed.
  cargo-test = pkgs.rustPlatform.buildRustPackage (
    rustArgs
    // {
      pname = "plamo-translate-cargo-test";
      buildType = "debug";
      auditable = false;
      dontCargoBuild = true;
      cargoTestFlags = [ "--workspace" ];
      installPhase = "touch $out";
    }
  );

  clippy = pkgs.rustPlatform.buildRustPackage (
    rustArgs
    // {
      pname = "plamo-translate-clippy";
      auditable = false;
      nativeBuildInputs = [ pkgs.clippy ];
      buildPhase = ''
        runHook preBuild
        cargo clippy -j "$NIX_BUILD_CORES" --offline --locked --workspace --all-targets -- -D warnings
        runHook postBuild
      '';
      doCheck = false;
      installPhase = "touch $out";
    }
  );
```

  - `flake.nix`: `crane.url`、outputs 引数の `crane,`、`craneLib = crane.mkLib pkgs;` を削除する。`build = import ./nix/packages.nix { inherit pkgs; };` にする。checks の import を `import ./nix/checks.nix { inherit pkgs packages home-manager; hmModule = self.homeManagerModules.default; inherit (build) rustArgs; };` にする。devShell は変えない
  - `nix flake lock` を実行する (`• Removed input 'crane'` だけが出る)。`nix flake update` は実行しない (nixpkgs と home-manager が上がってしまう)
  - `docs/superpowers/**` に残る crane の記述は履歴なので残す
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `grep -rn crane flake.nix flake.lock nix/` → 出力なし、exit 1
  - `nix flake metadata --json | python3 -c 'import json,sys;print(sorted(json.load(sys.stdin)["locks"]["nodes"]))'` → `['home-manager', 'nixpkgs', 'root']`
  - `nix build .#packages.aarch64-darwin.default && ls result/bin result/libexec/bin` → `result/bin` には `plamo-translate` と `plamo-translate-mcp` だけ、`result/libexec/bin` には `plamo-translated` だけがある。`result/bin/plamo-translate --help` → exit 0
  - `nix eval --raw .#packages.aarch64-darwin.default.meta.mainProgram` → `plamo-translate`
  - `nix build .#checks.aarch64-darwin.cargo-test` → exit 0。続けて `d=$(nix eval --raw .#checks.aarch64-darwin.cargo-test.drvPath)` を実行し、`nix log $d | grep -c 'test result:'` → 1 以上、`nix log $d | grep -c FAILED` → `0`、`nix log $d | grep 'cargoCheckHook flags'` の行に `--workspace` があり `--profile` がない
  - `nix build .#checks.aarch64-darwin.clippy` → exit 0
  - 負のテスト (コミットしない): `crates/plamo-client/tests/fixtures.rs` の末尾に `#[test] fn lint_probe() { let v = vec![1]; let _ = v.len() == 0; }` を足して `nix build -L .#checks.aarch64-darwin.clippy` → 非 0 で終わり、出力に `error: length comparison to zero` が含まれる。そのあと `git checkout crates/plamo-client/tests/fixtures.rs` で戻す
  - `nix flake check` → 最後の行が `all checks passed!`、exit 0
  - 無関係な変更で再ビルドされないこと: `A=$(nix eval --raw .#checks.aarch64-darwin.cargo-test.drvPath)` を記録する。`README.md`、`nix/launchd.nix`、`daemon/pyproject.toml` に 1 行ずつ足して `git add` し、再評価した値が `A` と同じになる。`crates/plamo-client/src/lib.rs` に `// x` を足して `git add` すると、`A` と異なる値になる。最後に `git reset -q && git checkout README.md nix/launchd.nix daemon/pyproject.toml crates/plamo-client/src/lib.rs` で戻す
- 落とし穴
  - サンドボックスが有効なマシンで cargo-test がソケットの bind/connect で EPERM になったら、cargo-test に `__darwinAllowLocalNetworking = true;` を足す (このマシンでは未検証。Q-2)
  - nixpkgs の hook が変わって clippy が "no matching package" や "--offline was specified" で落ちたら、新しい nixpkgs の cargoSetupHook と見比べる

### T-2: daemon: 履歴の解析と `done.finish_reason`

- 目的: プロトコル層で `history` を受け取り、`done` に `finish_reason` を載せられるようにする
- 対象: `daemon/src/plamo_translated/protocol.py`、`daemon/tests/test_protocol.py`
- 作業: 参照実装は `T02-daemon-U1.patch`
  - `HistoryEntry`、`TranslateRequest.history` (最後のフィールド)、`_parse_history` (`_parse_glossary` と同じ形。メッセージは D-3)、`parse_request` の最後のキーワード引数に `history=_parse_history(obj.get("history"))`、`done(..., finish_reason=None)` (`_event` が `None` を省く)
  - 新しいテスト: `test_parse_history_entries_in_order_ignoring_extra_keys`、`test_parse_absent_null_and_empty_history_are_equal`、`test_parse_rejects_malformed_history`、`test_parse_type_errors_precede_history_errors`、`test_done_carries_finish_reason_after_text`
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0 (参考値: 118 passed)
  - `nix develop -c cargo test --workspace` → exit 0

### T-3: daemon: `pfml3` モジュール (純粋)

- 目的: 検証、組み立て、トークン化、トークナイザ検査、template のハッシュ照合を、mlx に依存しない 1 つのモジュールにまとめる
- 対象: `daemon/src/plamo_translated/pfml3.py` (新規)、`daemon/tests/pfml3_cases.py` (新規)、`daemon/tests/test_pfml3.py` (新規)、`daemon/tests/conftest.py` (`StubTokenizer` を追加)
- 作業: 参照実装は `T03-daemon-U2.patch` (`pfml3.py` の全文、10 ケースの golden 文字列、`StubTokenizer`)
  - `CASES`: `minimal`、`source_lang`、`history_1`、`history_3_source_lang`、`all_fields`、`context_empty`、`context_whitespace`、`whitespace_text`、`form_plain`、`ja_easy`
  - テスト: golden、`test_prompt_ids_encode_once_without_special_tokens` (`calls == [False]`、BOS は先頭に 1 つ)、`check_tokenizer` の受理と 4 つの拒否メッセージ、`template_verified`、`strip_stop_token` (7 ケース)、`test_lang_code` (受理: `en ja abc zh-hans ja(easy)`。拒否: `"en\n" EN zh-Hans auto en-us e "" abcd en- "ja(easy)\n" " en"`)、`test_validate_messages` (D-3 の全メッセージ)、`test_validate_precedence`
  - `check_tokenizer` の詳細メッセージ (接頭辞は `model tokenizer does not expose the PFML3 control tokens expected by the prompt builder: `): `{token} encodes to {ids}`、`{token} encodes to the unk id {id}`、`{token} (id {id}) is not an eos id`。検査は `CONTROL_TOKENS` の順に行い、そのあと tag、bos の順に eos を検査する
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c bash -c 'cd daemon && pytest tests/test_pfml3.py -q'` → exit 0
  - `nix develop -c bash -c 'cd daemon && pytest tests -k lang_code -q'` → exit 0
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0

### T-4: daemon: サーバーでの検証、`template_error` の削除、エラー fixture

- 目的: 検証をキューとロードより前に行い、テンプレートの模擬をなくす
- 対象: `server.py`、`conftest.py`、`test_engine_fakes_and_progress.py`、`test_protocol.py`、`test_server.py`、`protocol/fixtures/`、`protocol/fixtures/README.md`、`docs/superpowers/plans/2026-11-05-plamo-translate.md`
- 作業: 参照実装は `T04-daemon-U3.patch` (fixture の内容はこの patch が正。履歴付きの fixture には `"stream":false` が入っている)
  - `_serve_connection` の既存の `try` の中で、`parse_request` の直後に `if isinstance(req, TranslateRequest): pfml3.validate(req)` を置く
  - `git mv protocol/fixtures/error_template.ndjson protocol/fixtures/error_reserved_token.ndjson` を実行し、内容を `{"fake":{}}`、リクエスト `{"v":1,"op":"translate","text":"<|plamo:tag|>x","target_lang":"en"}`、期待値 `{"type":"error","code":"invalid_request","message":"text must not contain reserved PFML tokens (<|plamo:...)."}` にする
  - 新しい fixture: `error_history_shape`、`error_history_empty_entry`、`error_history_precedence`、`error_form_requires_ja`、`error_bad_lang` (内容は patch のとおり)
  - 新しいテスト: `test_validation_precedes_load` (`FakeEngine()` を未ロードで起動し、上記の予約トークンのリクエストを送る。返信が上記のエラーと完全一致し、そのあとも `engine.loaded is False`、`calls == []`)、`test_validation_precedes_busy_and_shutting_down`
  - fixture README から `template_error` を削除し、FakeEngine は検証を担わない旨を書く。旧計画書の 196、198、311 行目から `template_error` という識別子を除く
  - この時点では MlxEngine はまだ template を使っているので、検証が二重になるが問題ない
- 既存テストの変更 (方針 D-2、D-3 による)
  - `conftest.FakeEngine`: `template_error` (コンストラクタ引数、属性、`from_fixture` のキー、`count_tokens` の分岐) を削除する
  - `test_engine_fakes_and_progress.py::test_fake_engine_errors`: `template_error` の部分を削除する
  - `::test_fake_engine_from_fixture`: 辞書とタプルから `template_error` を除く
  - `test_protocol.py::test_fixture_request_matches_expected_outcome`: ファイル名の集合ではなく規則で判定する。終端がエラーで、そのコードが `ENGINE_ERROR_CODES = {"context_too_long", "model_load_failed"}` に含まれない場合は、`_parse_and_validate(line)` (parse してから、`TranslateRequest` なら `validate`) がそのコードを出すこと、`<any>` でなければメッセージも一致することを確かめる。それ以外の fixture は `_parse_and_validate` を通ること
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c bash -c 'cd daemon && pytest tests -k validation_precedes_load -q'` → exit 0
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0
  - `git grep -l template_error` → 出力なし、exit 1

### T-5: daemon: `TextStream`、`finish_reason`、`done.text` の末尾除去

- 目的: エンジンから終了理由を受け取り、`done` の契約 (D-7) を満たす
- 対象: `engine.py`、`mlx_engine.py`、`server.py`、`conftest.py`、`test_engine_fakes_and_progress.py`、`test_server.py`、`protocol/fixtures/README.md`
- 作業: 参照実装は `T05-daemon-U4.patch`
  - `TextStream` を追加し、`Engine.stream -> TextStream` にする (`template_verified` はまだ足さない)
  - FakeEngine の `_steps`: `tokens[:max_tokens]` を `(token, None)` として出し、最後に `("", "stop" if len(self.tokens) < max_tokens else "length")`。キャンセルされたら理由を出さずに終わる
  - `MlxEngine.stream` は `TextStream(self._generate(...))` を返し、`_generate` は `(response.text, response.finish_reason)` を出す (template はまだ使う)
  - `_run_job` の translate 分岐: キャンセルされていれば何も送らない。`finish_reason is None` なら `DaemonError("internal", "generation ended without a finish reason")`。`done` は `strip_stop_token("".join(pieces))` と `finish_reason` で作る。`load` の `done` は変えない
  - 新しいテスト: `test_fake_engine_finish_reason_follows_mlx_lm`、`test_fake_engine_cancelled_stream_has_no_finish_reason`、`test_text_stream_skips_empty_pieces_and_keeps_finish_reason`、`test_done_strips_one_trailing_stop_token`、`test_done_text_is_unmodified_except_one_stop_suffix`、`test_done_reports_finish_reason`、`test_load_done_has_no_finish_reason`、`test_engine_without_finish_reason_is_internal_error`
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c bash -c 'cd daemon && HF_HUB_OFFLINE=1 pytest -m slow -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0
- 落とし穴: `finish_reason` が欠けたときのエラーは、fast スイートでは検出できない。mlx-lm を上げるときの slow テストだけが検出する

### T-6: daemon: MlxEngine を builder に切り替え、ロード検査、オラクル

- 目的: 本番のプロンプトを template から builder に移し、互換性をロード時とテストで保証する。履歴はこのタスクから実際に使われる
- 対象: `mlx_engine.py`、`engine.py`、`conftest.py`、`test_engine_fakes_and_progress.py`、`test_mlx_engine_load_checks.py` (新規)、`test_pfml3_oracle_slow.py` (新規)、`test_mlx_engine_slow.py`
- 作業: 参照実装は `T06-daemon-U5.patch`
  - `mlx_engine.py`: `import jinja2.exceptions` と `apply_chat_template` を削除する。`_prompt_tokens` を `_prompt_ids` (`pfml3.prompt_ids` を呼ぶ) に置き換える。ロード後に `pfml3.check_tokenizer` を実行し、失敗を `self._incompatible` に記憶する (次の呼び出しでは再ロードせずに同じ `model_load_failed` を出す)。`pfml3.template_verified(Path(path))` が偽なら `log.warning(TEMPLATE_DRIFT_WARNING)` を出す (`TEMPLATE_DRIFT_WARNING = "bundled chat template differs from the one the PFML3 builder was verified against (re-run the slow oracle tests)"`)。デバイス検査を `_warn_unless_gpu` に切り出す。`self.template_verified` を設定する。`MODEL_FILE_PATTERNS` はまだ足さない (T-7)
  - `Engine.template_verified: bool | None` と、FakeEngine のプロパティ `True if self.loaded else None` を足す
  - fast テスト `test_mlx_engine_load_checks.py`: `test_load_reports_verified_template`、`test_altered_or_missing_template_loads_unverified_with_one_warning` (2 回ロードしても WARNING は 1 回)、`test_tokenizer_without_msg_token_fails_load_once_per_process` (`load` は 1 回だけ呼ばれ、`loaded`/`template_verified` は `False`/`None` のまま)、`test_template_sha256_is_the_pinned_value`
  - `test_fake_engine_template_verified_once_loaded`
  - slow `test_pfml3_oracle_slow.py`: `PLAMO_ORACLE_MODELS` (カンマ区切りの `repo@revision`。既定は 2b の固定リビジョンと 0.8b の `e5fe5b7f26ebaeae48db0ce644b932d1d9b91aaa`)。スナップショットは `snapshot_download(..., local_files_only=True, allow_patterns=["*.json","*.py","*.jinja","*.jsonl"])` で解決する (2b はファイル全体を要求するとオフラインで失敗するため)。トークナイザは `mlx_lm.utils.load_tokenizer(snapshot, {"trust_remote_code": True}, eos_token_ids=load_config(snapshot).get("eos_token_id"))` で読む。テストは `test_oracle_prompt_string`、`test_oracle_prompt_ids`、`test_oracle_tokenizer_invariants`、`test_oracle_validate_agrees_with_template` (21 ケースで受理/拒否が一致すること。メッセージは比べない)
  - slow `test_mlx_engine_slow.py`: `test_mlx_engine_reports_stop_and_verified_template`、`test_mlx_engine_reports_length_when_truncated`、`test_mlx_engine_translates_with_history` を足す
- 既存テストの変更 (方針 D-2 による): `test_mlx_engine_slow.py::test_reserved_token_input_is_invalid_request` と `::test_form_requires_ja_target` を削除する (エンジンはもう検証しない。規則は `test_pfml3.py`、fixture、オラクルの一致テストで検査する)
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c bash -c 'cd daemon && HF_HUB_OFFLINE=1 pytest -m slow -k oracle -q'` → exit 0 (参考値: 84 passed)
  - `nix develop -c bash -c 'cd daemon && HF_HUB_OFFLINE=1 pytest -m slow -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0
  - `git grep -n -e apply_chat_template -e jinja2 -- daemon/src` → 出力なし、exit 1

### T-7: daemon: キャッシュ済みスナップショットの判定修正

- 目的: キャッシュ済みの既定モデル (2b) を、オフラインでもダウンロード経路に入らずにロードできるようにする
- 対象: `mlx_engine.py`、`test_mlx_engine_load_checks.py`、`test_mlx_engine_slow.py`
- 作業: 参照実装は `T07-daemon-U6.patch`
  - `from mlx_lm.utils import DEFAULT_ALLOW_PATTERNS` を import し、`MODEL_FILE_PATTERNS = DEFAULT_ALLOW_PATTERNS` とする。理由をコメントに書く (mlx-lm が埋めたキャッシュにはこれらのファイルしかなく、リポジトリ全体で判定すると `IncompleteSnapshotError` になってダウンロードしてしまう)
  - `_resolve_snapshot` のローカル判定に `allow_patterns=MODEL_FILE_PATTERNS` を渡す。ダウンロード経路は変えない
  - テスト (先に書いて、修正前に失敗することを確かめる): `test_cached_snapshot_is_checked_against_model_files_only`、`test_uncached_snapshot_is_downloaded`、slow の `test_default_model_loads_from_cache_with_verified_template` (`mlx_engine.DownloadProgress` が例外を出すように差し替え、2b を `MlxEngine` でロードして `(loaded, template_verified) == (True, True)`)
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c bash -c 'cd daemon && HF_HUB_OFFLINE=1 pytest -m slow -k default_model -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0

### T-8: daemon: `status.features` / `template_verified`、履歴の fixture

- 目的: 履歴を実際に扱えるようになったあとで、はじめて告知する
- 対象: `server.py`、`test_server.py`、`protocol/fixtures/`、`protocol/fixtures/README.md`
- 作業: 参照実装は `T08-daemon-U7.patch`
  - `FEATURES = ("history",)` を足し、`_status()` に `features=list(FEATURES), template_verified=self._engine.template_verified` を足す
  - 新しい fixture: `translate_history.ndjson` (D-1 の 2 組と `"stream":false`。期待値は `{"type":"done","text":"You can also filter by owner.","finish_reason":"stop","usage":{"input_tokens":74,"output_tokens":7},"model":"<any>","revision":"<any>"}`)、`error_context_too_long_history.ndjson` (`"stream":false` 付き)
  - `status.ndjson` の 3 行目に `"features":["history"]` を足す。どの fixture の期待値にも `template_verified` は入れない
  - 新しいテスト: `test_history_reaches_engine_unchanged_and_is_never_dropped`、`test_status_reports_unverified_template`
  - fixture README: `input_tokens` の項目の書き換えと、イベントフィールドの節を足す (内容は patch のとおり)
- 既存テストの変更 (方針 D-9、D-6 による): `test_server.py::test_status_reports_fields` の期待する辞書に `"features": ["history"]` を足し、LOAD のあとに `(loaded, template_verified) == (True, True)` を確かめる
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0
  - `nix build .#checks.aarch64-darwin.pytest --no-link -L` → exit 0
- 落とし穴: 履歴付きの fixture にはすべて明示的な `"stream"` を入れる。T-17 のテストがシリアライズ結果と比べるが、Rust の `Wire` は `stream` を常に出力するため

### T-9: docs: 設計書 §5、ノート、旧計画書の注記

- 目的: 長期方針 D-1 から D-9 のデーモン側を、リポジトリに残す
- 対象: `docs/superpowers/specs/2026-11-05-plamo-translate-design.md`、`docs/superpowers/notes/2026-11-05-pfml3-tokenizer-findings.md` (新規)、`docs/superpowers/plans/2026-11-05-plamo-translate.md`
- 作業: 参照実装は `T09-daemon-U8.patch` (日本語の本文をそのまま使ってよい)
  - 設計書
    - 37 行目の履歴の非目標を削除し、59 行目を「template の規則をデーモンが Python で実装し、template はテストのオラクル」に書き換える
    - §5 に `pfml3` の項を足す (検証の順序とメッセージ、`<|plamo:` を拒否する理由、正規表現、要素の順序、`prompt_ids`、`check_tokenizer`、`template_verified`)
    - `ensure_loaded` の項 (`MODEL_FILE_PATTERNS` とその理由、ロード検査と失敗の記憶、警告、`status.template_verified`) と、`count_tokens`/`stream` の項 (`TextStream` と `finish_reason`) を更新する
    - jinja2 の項を、オラクルとリビジョン更新の手順 (`TEMPLATE_SHA256` は `pytest -m slow` が通ってから更新する。8b/31b は実行時検査だけ) に置き換える
    - server の検証位置と `done.text` の末尾除去を書く
    - リクエスト例に `history`、レスポンス例に `finish_reason`、`features`、`template_verified` を足す
    - バージョン方針 (D-9) を書く
    - `invalid_request` の行を「JSON の不正、型の誤り、必須項目の欠落、意味的な検証エラー（PFML3 の制約）」にする
    - §10 のデーモンのテストの項と、§12 のリスクの行 (template やトークナイザのずれ) を更新する
  - 旧計画書の冒頭に履歴としての注記を 1 つ足す (patch のとおり)
  - ノートに、トークナイザの調査結果 (制御トークンの id、AhoCorasick の前処理分割と拒否する理由、オラクル、BOS、`fullmatch`、mlx-lm の停止処理、2b キャッシュの不具合と修正、エンコードのコスト) を書く
  - 設計書 §6 は T-16 で書く
- 実装者に任せること: 日本語の文言 (参照版をそのまま使ってよい)
- 条件: 常に実施
- 完了条件
  - `git grep -n "chat template の検証エラー" -- docs/superpowers/specs` → 出力なし、exit 1
  - `git grep -n '"features":\["history"\]' -- docs/superpowers/specs` → status の例の行が出力される
  - `test -f docs/superpowers/notes/2026-11-05-pfml3-tokenizer-findings.md` → exit 0
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0

### T-10: client: プロトコル型

- 目的: Rust の型をワイヤ形式 (D-1、D-7、D-9) に合わせる
- 対象: `crates/plamo-client/src/protocol.rs`、`crates/plamo-client/src/lib.rs`、`crates/plamo-translate/src/translate.rs`、`crates/plamo-translate-mcp/src/server.rs` (コンパイルを通すための修正だけ)
- 作業
  - 型は次のとおり。`Wire` の `context` の後に `#[serde(skip_serializing_if = "<[HistoryEntry]>::is_empty")] history: &'a [HistoryEntry],` を置き、リテラルに `history: &self.history` を足す

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct HistoryEntry {
    /// A text translated earlier in the same document.
    pub source: String,
    /// The translation returned for it, unchanged.
    pub translation: String,
}

pub const FEATURE_HISTORY: &str = "history";
impl Status {
    pub fn supports(&self, feature: &str) -> bool {
        self.features.as_deref().is_some_and(|f| f.iter().any(|x| x == feature))
    }
}
```

  - `TranslateRequest` に `history: Vec<HistoryEntry>` (`context` の後) を足す。`Event::Done` に `finish_reason: Option<String>` (`text` の後)、`Status` に `features: Option<Vec<String>>` と `template_verified: Option<bool>` を足す。`HistoryEntry` に `deny_unknown_fields` は付けない
  - `lib.rs` から `HistoryEntry` と `FEATURE_HISTORY` を再エクスポートする
  - コンパイルを通すため、`translate.rs` の `Event::Done { .. }` パターンに `..` を足し、CLI と MCP の `build_request` に `history: Vec::new()` を足す
  - 新しいテスト (`protocol.rs`): `q1_history_request_serializes_exactly` (D-1 の例と `serde_json::Value` で完全一致)、`empty_history_is_omitted`、`done_finish_reason_parsed_and_optional`、`status_features_and_template_verified`
  - `crates/plamo-client/tests/fixtures.rs` の `concrete_events` は変更しない (T-8 は `template_verified` を fixture の期待値に入れないため)
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c cargo test -p plamo-client --lib protocol::tests` → exit 0。出力に `test protocol::tests::q1_history_request_serializes_exactly ... ok` など、4 つの新しいテストがすべて ok と出る
  - `nix develop -c cargo test -p plamo-client --test fixtures` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0
  - `nix develop -c cargo clippy --workspace --all-targets -- -D warnings` → exit 0

### T-11: client: 判定、JSONL、追記、半減

- 目的: CLI と MCP が共有する履歴のヘルパーを作る
- 対象: `crates/plamo-client/src/history.rs` (新規)、`lib.rs`
- 作業

```rust
pub const RESERVED_TOKEN_PREFIX: &str = "<|plamo:";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Ineligible {
    #[error("the translation did not finish normally (finish_reason: {})", .0.as_deref().unwrap_or("none"))]
    NotStopped(Option<String>),
    #[error("the translation is empty")]
    Empty,
    #[error("the translation contains a reserved PFML token (<|plamo:)")]
    ReservedToken,
}

/// Check order: finish_reason, empty, reserved.
pub fn check_history_eligible(finish_reason: Option<&str>, translation: &str) -> Result<(), Ineligible>;
/// `source` and `translation` are copied byte-for-byte.
pub fn history_entry(source: &str, finish_reason: Option<&str>, translation: &str) -> Result<HistoryEntry, Ineligible>;

// Python's str.strip() also strips U+001C..U+001F, which char::is_whitespace does not;
// the daemon's emptiness check is Python's, so match it exactly.
fn is_python_whitespace(c: char) -> bool { c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c) }

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("history line {line}: {message}")]
pub struct HistoryError { pub line: usize, pub message: String }

/// JSONL: one {"source": string, "translation": string} object per line, oldest first.
/// A leading U+FEFF is stripped; whitespace-only lines are skipped; no comments.
pub fn parse_history(s: &str) -> Result<Vec<HistoryEntry>, HistoryError>;
// message = format!("expected {{\"source\": string, \"translation\": string}}: {serde_err}")

/// One write_all of serde_json::to_string(entry) + "\n" to a file opened with create+append;
/// a leading "\n" is written first if the file is non-empty and its last byte is not b'\n'.
pub fn append_history(path: &Path, entry: &HistoryEntry) -> std::io::Result<()>;

/// Removes the oldest ceil(n/2) entries and returns how many were removed.
pub fn drop_oldest_half(history: &mut Vec<HistoryEntry>) -> usize {
    let n = history.len().div_ceil(2);
    history.drain(..n);
    n
}
```

  - 上記をすべて `lib.rs` から再エクスポートする
  - テスト
    - `eligible_only_when_stop_nonblank_and_no_reserved` (表形式): `Some("stop")`+`"Hi"` → Ok。`Some("length")`+`"Hi"` → `NotStopped(Some("length"))`。`None`+`"Hi"` → `NotStopped(None)`。`Some("stop")`+`""`/`" \u{3000}\n"`/`"\u{1f}"` → `Empty`。`Some("stop")`+`"a<|plamo:tag|>"` → `ReservedToken`。`Some("length")`+`""` → `NotStopped` (順序の確認)
    - `history_entry_copies_unchanged` (`" Hi \n"` と `"会議\n"` をそのまま保つ)、`ineligible_messages` (上記の Display 文字列と完全一致)
    - `parse_history_accepts_blank_crlf_bom`、`parse_history_ignores_extra_keys`、`parse_history_reports_line` (3 行目に `translation` がなければ `line == 3`、メッセージは `expected {"source": string, "translation": string}: ` で始まる)、`parse_history_rejects_non_string`
    - `append_history_creates_file` (内容が `{"source":"a","translation":"b"}\n`)、`append_history_adds_missing_newline` (`X` → `X\n{...}\n`)
    - `drop_oldest_half_keeps_newest` (n=1,2,3,4,5 → 残りは 0,1,1,2,2 件で、どれも末尾の要素が順に残る)
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c cargo test -p plamo-client --lib history::tests` → exit 0。上記のテスト名がすべて ok と出る
  - `nix develop -c cargo test --workspace` → exit 0。`nix develop -c cargo clippy --workspace --all-targets -- -D warnings` → exit 0

### T-12: client: 事前確認

- 目的: 履歴付きのリクエストを送る前に、毎回デーモンの対応を確かめる (D-9)
- 対象: `crates/plamo-client/src/client.rs`、`crates/plamo-client/tests/history.rs` (新規)
- 作業

```rust
// Error gains:
#[error("daemon does not support translation history; run \"plamo-translate daemon restart\"")]
HistoryUnsupported,

pub async fn translate(&self, req: &TranslateRequest) -> Result<EventStream, Error> {
    if !req.history.is_empty() { self.require_history().await?; }
    // ... unchanged ...
}

pub async fn require_history(&self) -> Result<(), Error> {
    if self.status().await?.supports(FEATURE_HISTORY) { Ok(()) } else { Err(Error::HistoryUnsupported) }
}
```

  - キャッシュはしない。`status()` のエラー (`DaemonUnreachable` など) はそのまま返す
  - テスト (`tests/history.rs`。接続を順に受け付け、それぞれに台本どおりの返事を 1 つ返して、リクエスト行を記録する tokio のフェイクを使う)
    - `history_request_fails_without_feature_and_sends_no_translate`: `features` のない status を返すと `Err(Error::HistoryUnsupported)` になり、`to_string()` は上記のメッセージと完全一致する。接続は 1 回だけで `op` は `status`。2 回目の accept は 300 ms でタイムアウトする
    - `history_request_with_feature_sends_status_then_translate`
    - `every_history_request_preflights`: 同じクライアントで 2 回送ると、status、translate、status、translate の順になる
    - `no_preflight_without_history`: 接続は 1 回で op は translate
- 実装者に任せること: テスト用フェイクの内部実装
- 条件: 常に実施
- 完了条件
  - `nix develop -c cargo test -p plamo-client --test history` → exit 0、`test result: ok. 4 passed`
  - `nix develop -c cargo test --workspace` → exit 0。clippy → exit 0

### T-13: CLI: `--json` の `finish_reason`、`status` の表示

- 目的: 新しいイベントフィールドを CLI に出す
- 対象: `crates/plamo-translate/src/translate.rs`、`crates/plamo-translate/tests/cli_translate.rs`
- 作業
  - `done_json` に `"finish_reason": <string|null>` を足す (例: `{"type":"done","text":"The meeting","finish_reason":"stop","usage":{...},"model":"m","revision":"r"}`)
  - `format_status` (`daemon status` でも使われる) で、`protocol:` の後に `features: <カンマと空白で連結。ないか空なら "-">` と `template_verified: <true|false|"-">` の 2 行を出す
  - テスト: `json_includes_finish_reason` (`"stop"` の場合と、done に値がないときの `null`)、`status_prints_features_and_template_verified` (`features: history\n` と `template_verified: true\n`。古い status なら `features: -\n` と `template_verified: -\n`)
  - 既存の `status_prints_model_and_loaded` は `contains` で検査しているので変えない
- 実装者に任せること: なし
- 条件: 常に実施
- 完了条件
  - `nix develop -c cargo test -p plamo-translate --test cli_translate` → exit 0。2 つの新しいテストと既存の `status_prints_model_and_loaded`、`json_flag_prints_done_event` が ok と出る
  - `nix develop -c cargo test --workspace` → exit 0。clippy → exit 0

### T-14: CLI: `--history` 系のフラグ

- 目的: 文書を順に訳すときに、履歴をファイルで渡して追記できるようにする
- 対象: `crates/plamo-translate/src/translate.rs`、`crates/plamo-translate/tests/cli_translate.rs`
- 作業
  - フラグ
    - `--history FILE`: `Option<PathBuf>`、`value_name = "FILE"`。doc コメントは `/// JSONL translation history: {"source": ..., "translation": ...} per line, oldest first`
    - `--history-append`: `bool`、`requires = "history"`。doc コメントは `/// Append this translation to the --history file (skipped if truncated or empty)`
    - `--history-last N`: `Option<u32>`、`value_parser = clap::value_parser!(u32).range(1..)`、`requires = "history"`。doc コメントは `/// Send only the last N history entries`
  - 処理の順序 (最初の失敗で終わる)
    - `build_request(config, args) -> Result<Prepared, String>` は同期処理で、`Err` なら `error: <msg>` を出して exit 2。接続はしない
      1. text (TEXT か stdin)。既存の `no text given (pass TEXT or pipe stdin)` の検査をする
      2. context ファイル (既存)
      3. glossary (既存)
      4. `--history` が指定されていれば、次の順に処理する
         - パスが `-` なら `--history does not read stdin (stdin is reserved for TEXT); pass a file path`
         - ファイルを読む。`NotFound` で `--history-append` があれば、空の履歴として扱う。それ以外の読み込みエラーは `cannot read history <path>: <io error>`
         - `parse_history` にかける。エラーなら `<path>: history line <n>: expected {"source": string, "translation": string}: <serde msg>`
         - `--history-last` を適用する
      5. `Prepared { req, record: Option<PathBuf> }` を返す (`record` は `--history-append` のときだけ `Some(path)`)
    - `run` は、既存の `tokio::select!` (`ctrl_c` と並べる) の中で次を行う
      1. `record.is_some() && req.history.is_empty()` なら `client.require_history()` を呼ぶ。履歴ファイルがまだ空でも、古いデーモンを明示的に失敗させるため。CLI が自分で事前確認するのはこの場合だけ
      2. `client.translate(&req)`。古いデーモンなら stderr に `error: daemon does not support translation history; run "plamo-translate daemon restart"` を出して exit 1 (既存の `report_error` で処理する)
      3. `done` を受けたら今までどおり出力する。`record` があれば `history_entry(&req.text, finish_reason.as_deref(), &text)` を呼ぶ
         - `Ok` なら `append_history` で追記する。失敗したら `error: cannot append to history <path>: <io error>` を出して exit 1 (訳は出力済みで、履歴には記録されていない)
         - `Err(reason)` なら stderr に `warning: translation not added to history: <reason>` を出して exit 0。ファイルは変えない
      4. `context_too_long` は再試行しない。既存のエラー出力のまま exit 1
  - 追記する `source` は `req.text` をバイト単位でそのまま使う (stdin の末尾の改行も含む)。ロックはしない (1 ファイルに書き込むのは 1 プロセスという前提)
  - 終了コード: 0 は成功 (使えない組を飛ばした場合も含む)。1 はデーモンのエラー、古いデーモン、追記の失敗。2 は使い方の誤り (clap のエラー、読めない・不正な履歴ファイル)。3 はデーモンに到達できない。130 は SIGINT (このとき追記はしない)
  - テスト: 接続を順に受け付け、各リクエストを `requests` に送って会話ごとの行を返す新しいヘルパー `fake_daemon_seq(conversations: &[&[&str]])` を足す (既存のヘルパーは変えない)。テストは次のとおり
    - `history_file_is_sent_after_status_preflight`、`history_with_stdin_text`、`history_last_sends_tail`
    - `history_last_zero_exits_2`、`history_flags_require_history_exit_2`、`history_dash_exits_2`、`missing_history_file_exits_2`
    - `invalid_history_line_exits_2_without_connecting` (stderr に `h.jsonl: history line 2:` が含まれ、`requests.try_recv()` が Err)
    - `empty_text_wins_over_bad_history`
    - `stale_daemon_with_history_exits_1`
    - `history_append_preflights_even_with_missing_file` (古い status なら exit 1 で、ファイルは作られない)
    - `history_append_writes_eligible_pair` (ファイルの内容が `{"source":"会議","translation":"The meeting"}\n`)
    - `history_append_keeps_stdin_text_verbatim` (`write_stdin("会議\n")` → `{"source":"会議\n","translation":"The meeting"}\n`)
    - `history_append_with_existing_history_preflights_once` (リクエストは status、translate の 2 つだけ)
    - `history_append_skips_truncated` (`finish_reason` が `length` なら exit 0、stdout は `The meeting\n`、stderr に `warning: translation not added to history: the translation did not finish normally (finish_reason: length)` が含まれ、ファイルはない)
- 実装者に任せること: `Prepared` 構造体の名前、テスト用ヘルパーの内部実装
- 条件: 常に実施
- 完了条件
  - `nix develop -c cargo test -p plamo-translate --test cli_translate` → exit 0。上記のテスト名がすべて ok と出る
  - `nix develop -c cargo run -q -p plamo-translate -- --help` の出力に `--history <FILE>`、`--history-append`、`--history-last <N>` が含まれる
  - `nix develop -c cargo test --workspace` → exit 0。clippy → exit 0

### T-15: MCP: `history`、注記、古いデーモン、再試行

- 目的: モデルが履歴を渡せるようにし、使えない結果と削った履歴を in-band で伝える (D-7、D-8)
- 対象: `crates/plamo-translate-mcp/src/server.rs`、`crates/plamo-translate-mcp/tests/mcp.rs`
- 作業
  - `TranslateArgs` に次のフィールドを足し、`build_request` で `history: args.history.unwrap_or_default()` を設定する

```rust
/// Earlier parts of the same document and their translations, oldest first.
/// Pass each translation exactly as this tool returned it.
history: Option<Vec<HistoryEntry>>,
```

  - `TOOL_DESCRIPTION` の末尾に次を足す: ` For a document translated in several calls, pass the earlier parts as history ({source, translation}, oldest first, translation exactly as this tool returned it) to keep terms and style consistent. If the prompt would exceed the model's context window, the oldest history pairs are dropped automatically. A second content block starting with "note:" is not part of the translation; follow it.`
  - 古いデーモン: `describe_client_error` の `other` の分岐で `HistoryUnsupported.to_string()` を返し、結果は `isError: true` になる
  - 再試行 (既存のタイムアウトとキャンセルの `select!` の中で行う。`Progress` は試行の間で共有する。`max_tokens` には触れない)

```rust
enum Attempt { Done { text: String, finish_reason: Option<String> }, ContextTooLong(String), Failed(CallToolResult) }

async fn run(&self, mut req: TranslateRequest, mut progress: Option<Progress>) -> CallToolResult {
    let supplied = req.history.len();
    loop {
        match self.attempt(&req, &mut progress).await {
            Attempt::Done { text, finish_reason } => return success_with_notes(text, finish_reason.as_deref(), supplied, req.history.len()),
            Attempt::ContextTooLong(m) if req.history.is_empty() => return error_result(context_too_long_text(&m, supplied)),
            Attempt::ContextTooLong(_) => { drop_oldest_half(&mut req.history); }
            Attempt::Failed(r) => return r,
        }
    }
}
```

  - `attempt` は今の `run` の本体で、`ErrorCode::ContextTooLong` なら `ContextTooLong(message)`、`done` なら `Done`、それ以外は `Failed(error_result(..))` を返す
  - `success_with_notes`: `blocks[0]` は常に訳をそのまま入れる。次のどれかに当てはまるときだけ `blocks[1]` を足し、行を `"\n"` で連結する
    - `kept < supplied` なら `note: the oldest {supplied - kept} of {supplied} history pairs were dropped to fit the model's context window; only the last {kept} were used.`
    - `finish_reason.is_some()` で `check_history_eligible` が `Err(reason)` なら `note: do not pass this translation as history: {reason}.`
    - `finish_reason == None` (古いデーモン) なら、使えるかどうかの注記は付けない
  - `context_too_long_text(m, supplied)`: `supplied == 0` なら `context_too_long: {m}` (今と同じ)。`supplied > 0` なら `context_too_long: {m} (all {supplied} history pairs were dropped and the request still does not fit; shorten text, context or glossary)`
  - テスト (既存とは別に、`fake_daemon_seq` 相当のヘルパーと 1 つの `requests` チャネルを足す)
    - `schema_has_history_array_of_pairs` (`properties.history.type` に `"array"` が含まれ、items の `$ref` の先が `required == ["source","translation"]` の object。説明に `history` と `note:` が含まれる)
    - `history_is_sent_after_preflight` (content は 1 つ)、`empty_history_skips_preflight` (`"history": []` のとき、リクエストに `history` キーがない)、`stale_daemon_is_tool_error`
    - `context_too_long_drops_oldest_half_and_retries` (3 組。リクエストは status、translate(3)、status、translate(最新の 1 組)、translate(`history` なし) の順。`content[1].text == "note: the oldest 3 of 3 history pairs were dropped to fit the model's context window; only the last 0 were used."`)
    - `context_too_long_with_history_exhausted_is_error` (1 組。`isError` で、テキストは `context_too_long: too long (all 1 history pairs were dropped and the request still does not fit; shorten text, context or glossary)`)
    - `truncated_result_has_history_note` (`"finish_reason":"length"` なら `content[1].text == "note: do not pass this translation as history: the translation did not finish normally (finish_reason: length)."`、`isError` は false)
    - `stop_result_has_no_note`、`every_history_call_preflights`
  - 既存の `daemon_error_is_tool_error` は変えない (履歴がなければ再試行せず、テキストも今と同じという仕様)。既存の `STREAM` には `finish_reason` がないので、既存のテストは content が 1 つのまま
- 実装者に任せること: `attempt` をメソッドにするかクロージャにするか
- 条件: 常に実施
- 完了条件
  - `nix develop -c cargo test -p plamo-translate-mcp --test mcp` → exit 0。新しいテストすべてと既存の 8 テストが ok と出る
  - `nix develop -c cargo test --workspace` → exit 0。clippy → exit 0

### T-16: docs: README と設計書 §6

- 目的: 履歴の使い方と、Rust 側の方針 (D-7 の MCP 修正を含む) をリポジトリに残す
- 対象: `README.md`、`docs/superpowers/specs/2026-11-05-plamo-translate-design.md` §6
- 作業
  - README の `## 使い方 (CLI)` のコードブロックに次の 1 行を足す
    `plamo-translate -t en --history h.jsonl --history-append --history-last 8 < part3.txt   # 前の訳を履歴として渡し、訳を履歴に追記する`
  - オプションの表: `--form` の後に次の 3 行を足し、`--json` の行を置き換える

```
| `--history FILE` | 翻訳履歴の JSONL。1 行に `{"source": "原文", "translation": "訳文"}` を古い順に書く。同じ文書の前の部分とその訳を渡すと、用語や文体がそろう。stdin は TEXT 用なので `-` は指定できない |
| `--history-append` | 訳し終えたら原文と訳文の組を `--history` のファイルに追記する (なければ作る)。原文は送ったとおり (stdin の末尾の改行も含む) に書く。打ち切られた訳 (`finish_reason` が `stop` 以外)、空の訳、`<\|plamo:` を含む訳は追記せず stderr に警告を出す。履歴が空でもデーモンの対応を先に確かめる |
| `--history-last N` | `--history` のうち新しい N 組だけを送る (ファイルは変えない) |
| `--json` | 最後の `done` イベント (`finish_reason` を含む) を 1 行の JSON で出力する |
```

  - 終了コードの行の後に、次の段落を足す

```
履歴はデーモンが対応している必要がある。古いデーモンが動いていると `daemon does not support translation history; run "plamo-translate daemon restart"` を出して終了コード 1 で終わる。原文、文脈、履歴、用語集に `<|plamo:` を含めると `invalid_request` になる。履歴を含めた入力と出力が 32,768 トークンを超えると `context_too_long` になる (1 行 4 MiB を超えるリクエストは `invalid_request`)。CLI は履歴を自動では削らないので、`--history-last` で減らす。`--history-append` で訳の出力後に追記に失敗すると終了コード 1 になる (訳は出力済み、履歴には未記録)。同じ履歴ファイルを複数のプロセスから同時に使わないこと。
```

  - MCP の節の引数の文を、次で置き換える

```
`translate` tool が 1 つ使える。引数は `text`、`target_lang` (必須。`ja`、`en`、`zh-hans`、`zh-hant`、`ko`、`fr`、`de`、`es`、`ja(easy)` など)、`source_lang`、`context`、`glossary` (`[{source, target}]`)、`form` (`polite` / `plain`、`target_lang=ja` のときだけ)、`history` (`[{source, translation}]`、同じ文書の前の部分とその訳を古い順に)。config の `glossary_file` も適用され、引数の glossary が優先される。履歴を含めてモデルの上限を超える場合は、古い組から半分ずつ削って再試行し、削ったことを `note:` で始まる 2 つ目の content に書く。打ち切られた訳などの履歴に使えない結果にも同じ形で注記する。
```

  - トラブルシューティングに次の 2 項目を足す

```
- `daemon does not support translation history`: 更新前のデーモンが動いている。`plamo-translate daemon restart` を実行する
- `plamo-translate status` の `template_verified: false`: モデルの chat template が検証済みのものと異なる。翻訳は動くが、プロンプトの形式がずれている可能性がある (モデルのロード前は `-` と表示される)
```

  - 設計書 §6
    - `### plamo-client` に次の項目を足す
      - `HistoryEntry { source, translation }`、`TranslateRequest.history`（古い順。空なら送らない）、`Event::Done.finish_reason`、`Status.features` / `Status.template_verified` を持つ。
      - `Client::translate` は `history` が空でないたびに、先に `op: status` を送り `features` に `"history"` があるか確かめる（結果は覚えない）。なければ translate を送らず `Error::HistoryUnsupported`（`daemon does not support translation history; run "plamo-translate daemon restart"`）を返す。`Client::require_history()` で明示的にも呼べる。
      - 履歴に使える組の判定 `check_history_eligible`（`finish_reason == "stop"`、Python の `strip()` 相当で空でない、`<|plamo:` を含まない）、`drop_oldest_half`（古い `ceil(n/2)` 組を削る）、履歴 JSONL のパーサーと追記を持つ。意味の検証はデーモンが行い、クライアントは行わない。
    - `### CLI`: 使い方の行に `[--history FILE [--history-append] [--history-last N]]` を足す。README と同じ内容で 3 つのフラグを書く。あわせて次を書く: `--history-append` は履歴が空でも `require_history()` で確かめる、追記する原文は送ったとおり、`--json` は `finish_reason` を含む、`status` は `features` と `template_verified` を表示する、`context_too_long` は再試行しない、古いデーモンなら exit 1、出力後に追記に失敗したら exit 1
    - `### MCP server`: 入力スキーマに `history: [{source, translation}]` を足す。次も書く: 履歴があるときの `context_too_long` は、同じタイムアウトの中で古い方から半分ずつ削って成功するか履歴が空になるまで再送し、削ったことを 2 つ目の `note:` content で伝える (尽きたらエラーテキストで伝える)。MCP では履歴を持つのは呼び出し側のモデルなので、履歴に使えない結果には `note: do not pass this translation as history: <理由>.` を付け、`isError` は false のままにする。古いデーモンには、正確なメッセージで `isError` を返す
- 実装者に任せること: 日本語の細かい言い回し (内容は変えない)
- 条件: 常に実施。T-9 の設計書の編集の後に行う (同じファイルを編集するため)
- 完了条件
  - `git grep -c -e "--history-last" -e "daemon does not support translation history" -- README.md docs/superpowers/specs/2026-11-05-plamo-translate-design.md` → 両方のファイルで 0 以外の件数が出る
  - `git grep -n "do not pass this translation as history" -- docs/superpowers/specs` → 1 行以上出力される

### T-17: client: 履歴 fixture とシリアライズの一致

- 目的: G-1 の「両方のスイートで通る」の Rust 側を、実際に検査する
- 対象: `crates/plamo-client/tests/fixtures.rs`
- 作業
  - 新しいテスト `history_fixture_requests_match_client_serialization` を足す
    - 対象は、`history` が JSON 配列になっているすべての `v:1` translate fixture
    - `TranslateRequest` を `text, target_lang, source_lang, context, form, stream, max_tokens` から組み立て、`history` と `glossary` は `serde_json::from_value` で読む
    - fixture に `stream` キーがなければ、テストは fixture のファイル名を出して失敗する (`stream` を明示するという規約の確認)
    - `serde_json::to_value(req) == fixture のリクエスト` を確かめ、検査した fixture が 1 つ以上あることも確かめる
  - 対象になる fixture は `translate_history`、`error_history_empty_entry`、`error_history_precedence`、`error_context_too_long_history` の 4 つ (`error_history_shape` は `history` が配列でないので対象外)
- 実装者に任せること: なし
- 条件: 常に実施。`test -f protocol/fixtures/translate_history.ndjson` が exit 0 であること (T-8 の後)
- 完了条件
  - `nix develop -c cargo test -p plamo-client --test fixtures` → exit 0。`history_fixture_requests_match_client_serialization`、`client_yields_fixture_events`、`every_fixture_event_line_deserializes_and_ends_terminal` が ok と出る
  - `nix develop -c cargo test --workspace` → exit 0。clippy → exit 0

## 8. 実行順序

T-1 → T-2 → T-3 → T-4 → T-5 → T-6 → T-7 → T-8 → T-9 → T-10 → T-11 → T-12 → T-13 → T-14 → T-15 → T-16 → T-17

- T-1 はほかのどのタスクとも独立している。最初に済ませると、以降の `nix build .#checks...` を新しいビルドで確かめられる
- daemon 側 (T-2 から T-9) と Rust 側 (T-10 から T-15) は、互いにテストを壊さない。どちらのコミットが先でも `cargo test --workspace` と fast pytest は通る (確認済み)。1 つのブランチに直列でコミットするので、上の順序を使う
- 固定の制約: T-17 は T-8 の後 (`translate_history.ndjson` が必要)。T-16 は T-9 の後 (同じ設計書を編集する)
- T-8 (デーモンが `features` を告知する) は、T-6 (デーモンが実際に履歴を使う) の後。履歴を使えるより前に告知しないため

## 9. 検証計画

- Base での基準
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0
  - `nix flake check` → `all checks passed!` (crane 版。各サブタスクが scratch clone で確認済み)
  - 既知の失敗: Base の `_resolve_snapshot` では、2b をオフラインでロードできない (T-7 で直す)。Base の slow テストは 0.8b@main を使うので、この不具合には当たらない
- 全タスク後の全体検証
  - `nix flake check` → `all checks passed!`、exit 0
  - `nix develop -c bash -c 'cd daemon && pytest -m "not slow" -q'` → exit 0
  - `nix develop -c bash -c 'cd daemon && HF_HUB_OFFLINE=1 pytest -m slow -q'` → exit 0
  - `nix develop -c cargo test --workspace` → exit 0
  - `nix develop -c cargo clippy --workspace --all-targets -- -D warnings` → exit 0
  - `git grep -l template_error` → 出力なし。`git grep -n -e apply_chat_template -e jinja2 -- daemon/src` → 出力なし。`grep -rn crane flake.nix flake.lock nix/` → 出力なし
- 利用者による手動確認 (実機の Apple silicon で。成功基準の「実際に使える」を確かめる)
  - home-manager で再適用するか `plamo-translate daemon restart` を実行したあと、`plamo-translate model pull` を実行する。続けて `printf '{"v":1,"op":"status"}\n' | nc -U "$HOME/Library/Application Support/plamo-translate/daemon.sock"` を実行し、`"loaded":true,"features":["history"],"template_verified":true` が含まれることを確かめる。デーモンのログに `bundled chat template differs` がないこと
  - `printf '会議は来週の月曜日に変更されました。' | plamo-translate -t en --history /tmp/h.jsonl --history-append` を 2 回実行する。2 回目の訳が出力され、`/tmp/h.jsonl` に 2 行の組が残ること
  - Claude Code から MCP の `translate` を、`history` を付けて呼べること

## 10. リスクと未解決事項

| ID | 項目 | 影響 | 解決方法と時期 | 未解決の場合の既定値 |
|---|---|---|---|---|
| Q-1 | 8b / 31b の template とトークナイザが 2b と同じかどうか (未確認) | D-6 | 利用者がそのモデルを使うときに、`PLAMO_ORACLE_MODELS` で slow のオラクルを実行する | テストはしない。異なれば、実行時検査が `model_load_failed` を出すか、`template_verified: false` になる |
| Q-2 | Nix サンドボックスが有効なマシンで、cargo-test のソケットテストが通るか (未検証) | T-1 | そのマシンで EPERM が出たときに対応する | `__darwinAllowLocalNetworking` は付けない |
| Q-3 | daemon の `history[i]` のメッセージを、CLI で履歴ファイルの行番号に変換するか | T-14 | 必要になったら別途行う | 変換せず、メッセージをそのまま出す |
| Q-4 | `--history-last` の既定値 | T-14 | 利用者の要望があれば | 既定値なし (全件送る) |
| Q-5 | `template_verified: false` を、translate のたびに警告するか | T-13 | 利用者の要望があれば | `status` でだけ表示する |
| Q-6 | 検証メッセージが template と異なる点 (`target_lang=""` のとき、template は "must not be empty"、この計画は言語コードのメッセージ) | D-3 | 決定済み | D-3 のメッセージを使う。オラクルは受理/拒否だけを比べる |
| Q-7 | U+EE00 (トークナイザの境界文字) を含む入力は、トークナイザが黙って捨てる | D-4 | 問題が報告されたら | 拒否もせず、文書にも書かない |
| Q-8 | `usage.output_tokens` はトークン数ではなく断片の数 | - | 範囲外 | 変えない |
| Q-9 | `Cargo.toml` の version と Nix の `version = "0.1.0"` が二重管理 | T-1 | 範囲外 | 今のまま固定値 |

## 付録: レビューで変わった主な点 (蒸し返し防止のため)

- 予約トークンは「安全にエンコードする」案を検討したが、トークナイザに無効化の手段がないため不可能と確認した。拒否を維持する (D-4)
- 古いデーモンが履歴を黙って捨てる問題 → `status.features` と、送信前の確認を足した (D-9)。肯定結果のキャッシュは、ロールバック時に古くなるので入れない (D-12)
- 言語コードの正規表現は `$` だと `"en\n"` が通る → `re.fullmatch` を必須にした (D-4)
- `finish_reason` を `load` の `done` に付けるかが曖昧だった → `translate` の `done` だけに付ける (D-7)
- `done.text` を履歴に戻すと検証に落ちる場合がある → 履歴に使える組の判定を定めた (D-7)。Rust と Python の空白集合の違い (U+001C から U+001F) も合わせた
- MCP では「通知しない」だと、モデルが打ち切られた訳を履歴に使ってしまう → 利用者の承認を得て方針を修正し、`note:` の content で伝えることを必須にした (D-7)
- ロード前の `status` に `template_verified` がない点 → 方針に適合すると判定した (D-6)。リクエストごとに 2 回エンコードする点も適合 (D-5)
- キャッシュ済みの 2b がオフラインでロードできない既存の不具合を発見し、T-7 で直す
- 非互換のトークナイザで毎回再ロードしてしまう問題 → 失敗をプロセスの間記憶する (D-11)
- cargo-test のプロファイル → release ではなく debug にした (理由は D-10)
- 履歴付きの fixture に `stream` がないと、T-17 のシリアライズ比較が必ず失敗する → すべての履歴付き fixture に `"stream":false` を入れた (T-4、T-8 の patch に反映済み)
- `concrete_events` の条件付き変更 → fixture の期待値に `template_verified` を入れないので不要 (T-10)
