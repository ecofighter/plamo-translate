# plamo-translate

Apple silicon 上で PLaMo-3-translate (MLX) を使い、日英を中心とした翻訳をローカルで行うツール。

- `plamo-translated`: Python のデーモン。launchd のソケットアクティベーションで、最初の接続時に起動し、モデルをロードして推論する。アイドルが続くと自分で終了してメモリを解放する
- `plamo-translate`: Rust 製の CLI
- `plamo-translate-mcp`: Rust 製の stdio MCP server (`translate` と `translate_document` tool)
- Nix flake: パッケージ、home-manager モジュール (nix-darwin から使う)、操作用 app

## ライセンス

このリポジトリのコードは [MIT License](LICENSE-MIT) または [Apache License 2.0](LICENSE-APACHE) のどちらかを選んで使える。

このリポジトリはモデルのウェイト、モデルのコード、tokenizer を含まない。モデルは初回のロード時に Hugging Face から利用者の環境へダウンロードされ、その利用にはモデルのライセンスが別に適用される。既定のモデル [`pfnet/plamo-3-translate-2b-MLX-4bit`](https://huggingface.co/pfnet/plamo-3-translate-2b-MLX-4bit) は [PLaMo Community License](https://plamo.preferredai.jp/info/plamo-community-license-ja) で提供されている。個人利用や非営利利用は無償だが、商用利用には PFN への事前登録が必要で、年間売上が 10 億円を超える場合は別の契約が必要になる。翻訳結果を公開するときはモデルの出力であることを明示する必要がある。条件はモデルのページとライセンス本文で確認すること。

Built with PLaMo

## 導入 (nix-darwin + home-manager)

```nix
# flake inputs: plamo-translate.url = "github:ecofighter/plamo-translate"; home-manager.url = "github:nix-community/home-manager";
darwinConfigurations.<host> = nix-darwin.lib.darwinSystem {
  modules = [
    home-manager.darwinModules.home-manager
    { home-manager.users.<user> = { imports = [ plamo-translate.homeManagerModules.default ]; services.plamo-translate.enable = true; }; }
  ];
};
```

`darwin-rebuild switch` すると、`plamo-translate` と `plamo-translate-mcp` が PATH に入り、LaunchAgent (`net.ecofighter.plamo-translate.daemon`) と `~/.config/plamo-translate/config.toml` が配置される。

オプション (`services.plamo-translate`):

| オプション | 内容 |
|---|---|
| `enable` | 有効にする |
| `package` | 使うパッケージ。既定は flake の `packages.aarch64-darwin.default` |
| `settings` | `config.toml` の内容 (下記)。`socket_path` は常にモジュールが上書きする |

```nix
services.plamo-translate = {
  enable = true;
  settings = {
    idle_timeout_secs = 600;
    default_target = "auto";
  };
};
```

設定を変えて再適用すると、デーモンは自動で再起動される。

### home-manager を使わない場合

`nix profile install github:ecofighter/plamo-translate` でプロファイルに入れてから `plamo-translate daemon install` を実行すると、plist を書いて読み込む。plist にはプロファイル内のデーモンのパスが記録されるので、GC で消えない。`nix run .#daemon-install` はストアのパスを GC ルートなしで plist に記録するため、`nix-collect-garbage` の後にデーモンが消える (このとき `daemon install` は警告を出す)。試用以外では使わないこと。`daemon-uninstall`、`daemon-start`、`daemon-stop`、`daemon-restart`、`daemon-status`、`daemon-logs` も app として用意してある。`daemon stop` はデーモンが起動していなければ何もせず終了コード 0 を返す。plist が Nix 管理 (home-manager) のときは、`daemon install` / `daemon uninstall` は変更を拒否して終了コード 2 を返す。

## 使い方 (CLI)

```sh
plamo-translate "会議は来週の月曜日に変更されました。"
echo "Hello, world" | plamo-translate -t ja
plamo-translate -t ja --form plain "Thank you for your help."
plamo-translate --context "ソフトウェアの UI 文言" --glossary ~/glossary.tsv "Commit"
plamo-translate -t en --history h.jsonl --history-append --history-last 8 < part3.txt   # 前の訳を履歴として渡し、訳を履歴に追記する
plamo-translate status
plamo-translate model pull   # モデルを事前にダウンロードしてロードする
plamo-translate model path   # ウェイトの保存先を表示する
```

| オプション | 内容 |
|---|---|
| `TEXT` | 翻訳する文。省略すると stdin から読む。サブコマンド名と同じ文は stdin で渡す |
| `-t, --to LANG` | 訳先の言語。省略時は config の `default_target` (`auto` は日本語なら英語、それ以外なら日本語) |
| `-s, --from LANG` | 原文の言語。省略するとモデルが判定する |
| `--context TEXT` / `--context-file FILE` | 訳さない補足の文脈 (どちらか一方) |
| `--glossary FILE` | TSV の用語集 (原語と訳語をタブで区切る、`#` 始まりはコメント)。複数指定でき、config の `glossary_file` に追加される。重複する原語は後の指定が優先 |
| `--form polite\|plain` | 日本語の丁寧さ。`-t ja` のときだけ有効 |
| `--history FILE` | 翻訳履歴の JSONL。1 行に `{"source": "原文", "translation": "訳文"}` を古い順に書く。同じ文書の前の部分とその訳を渡すと、用語や文体がそろう。stdin は TEXT 用なので `-` は指定できない |
| `--history-append` | 訳し終えたら原文と訳文の組を `--history` のファイルに追記する (なければ作る)。原文は送ったとおり (stdin の末尾の改行も含む) に書く。打ち切られた訳 (`finish_reason` が `stop` 以外)、空の訳、`<\|plamo:` を含む訳は追記せず stderr に警告を出す。履歴が空でもデーモンの対応を先に確かめる |
| `--history-last N` | `--history` のうち新しい N 組だけを送る (ファイルは変えない) |
| `--no-stream` | ストリーミングせず、完了後にまとめて出力する |
| `--json` | 最後の `done` イベント (`finish_reason` を含む) を 1 行の JSON で出力する |

終了コード: 0 成功、1 翻訳エラー、2 使い方の誤り、3 デーモンに到達できない。130 は Ctrl-C。進捗表示は stderr が TTY のときだけ出る。

履歴はデーモンが対応している必要がある。古いデーモンが動いていると `daemon does not support translation history; run "plamo-translate daemon restart"` を出して終了コード 1 で終わる。原文、文脈、履歴、用語集に `<|plamo:` を含めると `invalid_request` になる。履歴を含めた入力と出力が 32,768 トークンを超えると `context_too_long` になる (1 行 4 MiB を超えるリクエストは `invalid_request`)。CLI は履歴を自動では削らないので、`--history-last` で減らす。`--history-append` で訳の出力後に追記に失敗すると終了コード 1 になる (訳は出力済み、履歴には未記録)。同じ履歴ファイルを複数のプロセスから同時に使わないこと。

デーモンの操作:

```sh
plamo-translate daemon status
plamo-translate daemon start | stop | restart
plamo-translate daemon logs [-f]
plamo-translate daemon install [--daemon-bin PATH]
plamo-translate daemon uninstall [--purge [--yes]]   # --purge はモデル、データ、ログも削除する
```

`stop` はプロセスを止めるだけで、ソケットは残る。次の接続で自動的に起動する。

### 文書の翻訳 (`plamo-translate doc`)

プレーンテキストと Markdown の文書を stdin から読み、区切りのよい部分 (ピース) ごとに訳して stdout に書く。直前までの訳を履歴としてピースごとに渡すので、長い文書でも用語や文体がそろう。

```sh
cat doc.md | plamo-translate doc -t ja > doc.ja.md
plamo-translate doc -t en --format text --history-bytes 0 < notes.txt
```

| オプション | 内容 |
|---|---|
| `--format auto\|text\|markdown` | 入力の形式。既定は `auto` で、内容から判定する。判定の結果は `--verbose` のときだけ stderr に `note: input format: markdown (auto-detected; use --format to override)` で出る |
| `--history FILE` | 履歴の JSONL (形式は上と同じ)。文書の前の部分として扱い、最初のピースから履歴に入る。stdin は文書用なので `-` は指定できない |
| `--history-last N` | `--history` のうち新しい N 組だけを読む。読み込み時の絞り込みだけで、訳したピースには効かない |
| `--history-append` | 訳したピースを 1 つ書くたびに、原文と訳文の組を `--history` のファイルに追記する (なければ作る)。途中から再開する機能はない |
| `--history-bytes BYTES` | ピースごとに送る履歴の大きさ (UTF-8 のバイト数、原文と訳文の合計)。既定は 8000。新しい組から古い方へ入れ、収まらなくなったところで止める。最新の 1 組は常に送る。0 で履歴を使わない |
| `--report text\|json` | stderr の形式。既定は `text`。`json` では stderr を 1 行に 1 つの JSON (JSON Lines) で書き、最後の行に結果の要約を出す (下記)。stdout は変わらない。0.2.0 から |
| `--verbose` | 判定した入力の形式 (`note: input format: …`) と、履歴に使わなかったピースの警告 (`warning: piece N/M: not used as history: …`) も stderr に出す。ほかの警告とエラーは指定しなくても出る。訳と終了コードは変わらない。`--report json` では無視される。0.2.0 から |
| `-t, -s, --context, --context-file, --glossary, --form` | 上の表と同じ |

`TEXT`、`--no-stream`、`--json` はない。プログラムから結果を読むときは `--report json` を使う。オプションは `doc` の後ろに書く。`plamo-translate -t ja doc` は単語 "doc" の翻訳になり、`plamo-translate doc` はこのサブコマンドを実行する。単語 "doc" を訳すときは `echo doc | plamo-translate -t en` のように stdin で渡す。

- 訳先を省略して `auto` になる場合、日本語か英語かは文書全体で 1 回だけ決める。日英が混ざった文書では `-t` を指定すること
- コードブロックなど、訳してはいけないブロックはデーモンに送らず、そのまま出力する。HTML ブロックも訳さず、`warning: line N: HTML block left untranslated` を出す。Markdown では字下げのコードブロックもコードとして扱い、`--format auto` のときだけ `warning: line N: indented block left untranslated as code (use --format text if it is prose)` を出す。字下げの文章なら `--format text` を使う
- 参照形式のリンク (`[text][ref]` など) は、出力ではインラインのリンクに書き換わる
- 出力はピースを訳し終えるたびに書く。改行は足さない。訳す対象のない文書は、デーモンに接続せずそのまま出力する
- 打ち切られたピース、空の訳、内部のマーカーが壊れたピースは、原文のまま (打ち切りは訳の途中まで) 書き、`warning: piece N/M: …` を出す。最後に `error: K of M pieces were not fully translated; see the warnings above` を出して終了コード 1 で終わる。構造やインライン要素の違いは警告だけで、終了コードには影響しない
- 途中でエラーになると `error: stopped at piece N/M; stdout holds everything translated before it` を出して止まる。stdout にはそのピースの前までの出力が欠けなく残る
- `--history` で渡した履歴は、文書のそれより前の部分と同じ扱いで、新しい組に押し出されると送られなくなる。毎回効かせたい用語は `--glossary` で指定する
- 履歴を使うので、ピースごとに 2〜3 秒ほど余計にかかる (既定の `--history-bytes` のとき)。履歴を使うときはデーモンの対応を最初に確かめる
- 単体のときと違い、文書に `<|plamo:` が含まれていてもよい。エスケープした形で送り、訳は元のまま戻る。ただしエスケープ済みの形や内部のマーカーのテキストをすでに含むブロックは訳さず、`warning: line N: block left untranslated: contains reserved marker text` を出す
- `context_too_long` になったときは、そのピースの履歴を半分に減らして再試行し、それでも収まらなければピースを分割する。`--history-last` で減らす必要はない
- `--history-append` が記録するのは、警告なしに訳せたピースだけ。記録する原文は実際に送ったテキストで、コードブロックは `<!--pt:kN-->` のマーカーに置き換え、参照形式のリンクはインラインに書き、`<|plamo:` はエスケープした形になる
- 分割したピースのラベルは `piece 3/12.2` のようになる

終了コードは単体のときと同じ。0 成功、1 翻訳エラー (訳しきれなかったピースや追記の失敗を含む)、2 使い方の誤り、3 デーモンに到達できない、130 Ctrl-C。

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

## 使い方 (MCP)

Claude Code (CLI) では次のコマンドで登録する。

```sh
claude mcp add plamo-translate -- plamo-translate-mcp
```

Claude Desktop のチャットでは `~/Library/Application Support/Claude/claude_desktop_config.json` に書く。Finder から起動したアプリはシェルの `PATH` を引き継がないことがあるので、`command -v plamo-translate-mcp` で調べた絶対パスを書く (home-manager なら `/etc/profiles/per-user/<ユーザー名>/bin/plamo-translate-mcp`)。書いたら Claude Desktop を終了して開き直す。

```json
{
  "mcpServers": {
    "plamo-translate": {
      "command": "/etc/profiles/per-user/<ユーザー名>/bin/plamo-translate-mcp"
    }
  }
}
```

Claude Desktop の Code タブのローカルセッションは、このファイルのサーバーも `~/.claude.json` のサーバーも読む。どちらかに登録すれば Code タブでも使える。

`translate` と `translate_document` の 2 つの tool が使える。`translate` は短い文向けで、引数は `text`、`target_lang` (必須。`ja`、`en`、`zh-hans`、`zh-hant`、`ko`、`fr`、`de`、`es`、`ja(easy)` など)、`source_lang`、`context`、`glossary` (`[{source, target}]`)、`form` (`polite` / `plain`、`target_lang=ja` のときだけ)、`history` (`[{source, translation}]`、同じ文書の前の部分とその訳を古い順に)。config の `glossary_file` も適用され、引数の glossary が優先される。履歴を含めてモデルの上限を超える場合は、古い組から半分ずつ削って再試行し、削ったことを `note:` で始まる 2 つ目の content に書く。打ち切られた訳などの履歴に使えない結果にも同じ形で注記する。

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

## Claude Code の日英レイヤ (`plamo-ja-bridge`)

Claude Code の mod。日本語で入力すると英訳して Claude に送り、Claude の英語の応答を段落ごとに和訳して表示する。Claude には英文だけが届き、transcript にも英文が残る。翻訳には `plamo-translate` CLI を使うので、CLI とデーモンを先に導入しておく。文書ファイルを訳す skill (下記) も含む。動作はターミナルの Claude Code v2.1.317 で確かめた。Claude Desktop の Code タブでは確かめていない。

### 導入

このリポジトリが plugin の marketplace (`ecofighter-plamo-translate`) を兼ねている。Claude Code (CLI) と Claude Desktop の Code タブのローカルセッションは同じ設定ファイル (`~/.claude/settings.json` など) を読むので、どちらかで入れれば両方で使える。

**Claude Code (CLI)**: セッションの中で次を実行し、詳細画面で範囲を選ぶ (自分だけなら user scope)。v2.1.275 以降が必要。

```
/plugin install plamo-ja-bridge --marketplace ecofighter/plamo-translate
```

シェルから入れる場合は次のとおり (v2.1.292 以降)。既定は user scope。開いているセッションには `/reload-plugins` で読み込む。

```sh
claude plugin install plamo-ja-bridge --marketplace ecofighter/plamo-translate
```

**Claude Desktop (Code タブ)**: plugin ブラウザには登録済みの marketplace の plugin だけが並ぶ。先にシェルで marketplace を登録する (上の `claude plugin install` で入れた場合は不要)。

```sh
claude plugin marketplace add ecofighter/plamo-translate
```

ローカルセッションでプロンプト欄の横の **+** から **Add plugins** (メニューが **Plugins** なら **Browse plugins**) を開き、`plamo-ja-bridge` を選んで範囲を決める。Code タブは Desktop が持つ Claude Code で動くので、ターミナルの `claude` とバージョンが違うことがある。`/status` の **Claude Code** 行で確かめる。

Desktop は起動時にログインシェルの環境変数を読む。`plamo-translate` が見つからずに翻訳が失敗するときは、`translator` に絶対パス (例: `/etc/profiles/per-user/<ユーザー名>/bin/plamo-translate`) を設定するか、Desktop を終了して開き直す。

この mod は Claude Code 用で、Claude Desktop のチャット (Code タブ以外) では動かない。チャットで翻訳を使うときは上の MCP を登録する。

開発中のチェックアウトから 1 セッションだけ読み込むときは `claude --plugin-dir ./claude-plugin` を使う。

### 使い方

| 入力 | 動き |
|---|---|
| `/ja` | オンとオフを切り替える |
| `/ja on` / `/ja off` | 明示的に切り替える |
| `/ja status` | オン・オフ、対訳の組数、翻訳に使う CLI を表示する |

オンの間はプロンプトの下に `ja⇄en` が表示される。日本語を含むプロンプトを送ると、英訳が確認ダイアログに出る。「この英訳で送る」「原文のまま送る」「やめる」から選ぶか、英文を自分で入力して送る。`/` や `!` で始まる入力と、日本語を含まない入力は翻訳しない。コードフェンスの中は入力でも出力でも翻訳しない。

同じセッションで送ったプロンプトと和訳した段落は対訳として覚え、次の翻訳に履歴として渡す (用語と言い回しがそろう)。対訳はセッションの中だけで使い、`/clear`、`/resume`、mod の読み込み直しで消える。固定の用語は `config.toml` の `glossary_file` で決める。

翻訳に失敗したときは、入力では「原文のまま送る」か「やめる」を選ぶ。出力ではその段落を英語のまま表示する。

### 設定 (userConfig)

`/plugin` の **Installed** タブで `plamo-ja-bridge` を開き、**Configure options** で変える。

| キー | 既定 | 内容 |
|---|---|---|
| `enabledByDefault` | `false` | セッションを始めたときにオンにするか |
| `confirm` | `true` | 送る前に英訳を確認するか |
| `translator` | `plamo-translate` | 翻訳に使う CLI のパス |
| `historyPairs` | `8` | 履歴に入れる対訳の組の上限 |
| `historyChars` | `6000` | 履歴に入れる文字数の上限 |
| `form` | `auto` | 和訳の丁寧さ (`auto` / `polite` / `plain`) |
| `flushLines` | `20` | 空行のない段落を何行で区切って表示するか |

### 制限

- transcript とプロンプト履歴には、送った英文が残る。
- ツールの呼び出しと結果、許可の確認、選択肢つきの質問、思考過程は英語のまま表示する。
- 和訳は Claude の生成より遅れて表示され、応答が終わってから数秒かかることがある。和訳が終わる前に次のプロンプトを送ると、その応答は英語の原文で表示されることがある。
- Claude が日本語で答えた段落は、そのまま表示する。

### 文書ファイルの翻訳 (skill)

`translate-document` skill を含む。Markdown やテキストのファイルの翻訳を頼むと (例: 「docs/guide.md を英訳して」)、Claude が `plamo-translate doc` を実行して訳を別のファイルに書く。`/plamo-ja-bridge:translate-document docs/guide.md en` で明示的に呼ぶこともできる。`/ja` のオン・オフとは関係なく使える。

- 0.2.0 以降の CLI が必要 (`--report json` を使う)。plugin と CLI は別々に更新されるので、CLI が古いと訳さずに更新を促す
- 出力は元のファイルと同じ場所の `<名前>.<言語>.<拡張子>` (`README.md` → `README.ja.md`)。同じ名前のファイルがあれば上書きの前に確認する。訳先を指定しなければ、日本語の文書は英語に、それ以外は日本語に訳す
- `plamo-translate doc` は、skill を呼び出したターンの間だけ確認なしで実行する。後のターンでの再実行や、出力ファイルへの書き込み (`>`) は、通常どおり権限の確認を受ける
- 用語集や履歴のファイルは、頼んだときだけ出力と同じ場所に作る
- 訳しきれなかったピースや途中での停止は、Claude が `--report json` の要約をもとに報告する
- 訳を公開するときは、モデルの出力であることを明示する (「ライセンス」を参照)。Claude は最後にそのことを伝え、ファイルの先頭に注記を入れるかを尋ねる
- この plugin は MCP server を含まない。MCP の `translate_document` を使うときは「使い方 (MCP)」のとおり別に登録する

## 設定 (`config.toml`)

最初に見つかった 1 ファイルだけを読む: `$PLAMO_TRANSLATE_CONFIG`、次に `${XDG_CONFIG_HOME:-~/.config}/plamo-translate/config.toml`。すべて省略可。デーモンの起動時に 1 回だけ読む。

```toml
model = "pfnet/plamo-3-translate-2b-MLX-4bit"
revision = "63777ddc6dc80aacacd92a1bf6966105f2f6270c"  # 既定モデルでは検証済みのリビジョン。他のモデルで省略すると main
idle_timeout_secs = 600      # 0 で無期限
default_target = "auto"      # "auto" または言語コード
glossary_file = "~/.config/plamo-translate/glossary.tsv"
socket_path = "~/Library/Application Support/plamo-translate/daemon.sock"
```

ソケットの場所は `$PLAMO_TRANSLATE_SOCKET` で上書きできる。

## ファイル配置

| 用途 | パス |
|---|---|
| 設定 | `$PLAMO_TRANSLATE_CONFIG` → `${XDG_CONFIG_HOME:-~/.config}/plamo-translate/config.toml` |
| アプリデータの基点 | `~/Library/Application Support/plamo-translate/` (0700) |
| モデルのウェイト | `~/Library/Application Support/plamo-translate/huggingface/hub/` |
| remote code のキャッシュ | `~/Library/Application Support/plamo-translate/huggingface/modules/` |
| ソケット | `~/Library/Application Support/plamo-translate/daemon.sock` (0600) |
| ログ | `~/Library/Logs/plamo-translate/daemon.log` (5MB x 3 世代) |
| クラッシュ時の出力 | `~/Library/Logs/plamo-translate/daemon.stderr.log` |
| LaunchAgent | `~/Library/LaunchAgents/net.ecofighter.plamo-translate.daemon.plist` (Label は `net.ecofighter.plamo-translate.daemon`、ドメインは `gui/$UID`) |

## MLX の更新

MLX 系の `mlx-bin`、`mlx-metal`、`mlx-lm` は `nix/mlx-versions.nix` で同じ組み合わせに固定している。PyPI の最新版に追従するには:

```sh
just bump-mlx   # scripts/bump_mlx.py で版と sha256 を更新 -> nix flake check -> just test-slow
```

通ったら commit する。開発用のテストは `just test` (slow を除く pytest と cargo test)、`just test-slow` (実モデル。固定リビジョンの 2b と 0.8b のキャッシュが必要で、`HF_HUB_OFFLINE=1` で動く)。

## トラブルシュート

- 初回のリクエストはモデルのダウンロード (2B で約 2.4 GB) とロードを行うので時間がかかる。`plamo-translate model pull` で事前に取得できる
- `plamo-translate daemon status`: launchd の読み込み状態、PID、モデルとリビジョンを確認する
- `plamo-translate daemon logs -f`: `daemon.log` を追う
- 起動自体に失敗している (Python のエラーなど) ときは `~/Library/Logs/plamo-translate/daemon.stderr.log` を見る
- 終了コード 3 (デーモンに到達できない): LaunchAgent が読み込まれていない。home-manager で適用するか `plamo-translate daemon install` を実行する
- 設定を変えたのに反映されない: Nix 管理なら再適用、それ以外は `plamo-translate daemon restart`
- `daemon does not support translation history`: 更新前のデーモンが動いている。`plamo-translate daemon restart` を実行する
- `plamo-translate status` の `template_verified: false`: モデルの chat template が検証済みのものと異なる。翻訳は動くが、プロンプトの形式がずれている可能性がある (モデルのロード前は `-` と表示される)
- `error: unexpected argument '--report' found`: CLI が 0.2.0 より古い。`plamo-translate --version` で確かめ、更新する
