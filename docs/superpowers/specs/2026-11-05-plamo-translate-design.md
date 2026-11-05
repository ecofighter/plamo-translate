# plamo-translate 設計書

- 日付: 2026-11-05
- 状態: ドラフト（ユーザーレビュー待ち）
- 改訂: Homebrew tap を v1 から外した。導入経路は home-manager モジュール（nix-darwin に組み込んで使う）を主とした

## 1. 目的と前提

### 目的
自分の Apple Silicon Mac 上で PLaMo-3-translate（MLX 版）をオンデマンドで常駐させ、次の2経路から待ち時間少なく翻訳を呼べるようにする。

- Claude Code などのエージェントから MCP 経由で呼ぶ
- シェルから CLI で呼ぶ

導入と管理は Nix flake で行う。**nix-darwin の構成から宣言的に導入できることを最優先とする**。そのための手段として home-manager モジュールを提供し、nix-darwin には `home-manager.darwinModules.home-manager` 経由で組み込む。加えて、launchctl 操作用の `nix run` app も提供する。

### 利用者と品質水準
主な利用者は作者本人（nix-darwin 環境）。公開は副次的な扱いとし、設定項目やドキュメントは作者が日常的に使う範囲に絞る。

### 成功条件
1. ログイン後、初回の翻訳要求でデーモンが自動起動し、モデルのロード中は進捗を表示したうえで翻訳を返す。
2. 2回目以降は、ロード待ちなしでストリーミング翻訳が始まる。
3. 一定時間使わなければデーモンが終了してメモリを解放し、次の要求で再び自動起動する。
4. nix-darwin 構成内の `home-manager.users.<user>` に `services.plamo-translate.enable = true;` を書いて `darwin-rebuild switch` するだけで、CLI、MCP server、LaunchAgent、設定ファイルがそろう。

### 対象モデル
- コレクション: https://huggingface.co/collections/pfnet/plamo-3-translate
- 既定: `pfnet/plamo-3-translate-2b-MLX-4bit`（約 2.4GB、2026-11-05 時点のリビジョン `63777ddc6dc80aacacd92a1bf6966105f2f6270c`）
- 設定により `0.8b` / `8b` / `31b` の `-MLX-4bit` に差し替えられる。
- 読み込みには `trust_remote_code` が必要（`modeling_plamo_mlx.py`、`tokenization_plamo.py`）。モデル側の依存は `mlx-lm`、`mlx`、`transformers`、`numba`。
- ライセンスは PLaMo Community License（個人・非商用は無償）。

### 非目標（v1）
- Intel Mac と Linux への対応
- 長文の自動分割（上限を超えたらエラーにする）
- 複数モデルの同時常駐、リクエストごとのモデル切り替え
- 翻訳履歴（モデルの chat template が対応する、過去の原文と訳文のペア）の受け渡し
- ファイルの一括翻訳、GUI、リモートからのアクセス
- Homebrew tap と nix-darwin 専用モジュール（§8 に将来の拡張として方針だけを残す）

## 2. 全体構成

```
 Claude Code ──stdio(MCP)──▶ plamo-translate-mcp ─┐
 シェル ─────────────────▶ plamo-translate (CLI) ─┤ NDJSON over Unix socket
                                                   ▼
                   launchd (LaunchAgent, Sockets) ──▶ plamo-translated (Python, mlx-lm)
                     初回接続時に起動                    アイドル N 秒で exit
```

| ユニット | 言語 | 責務 | 依存 |
|---|---|---|---|
| `plamo-translated` | Python | launchd からのソケット受け取り、モデルの取得とロード、推論、アイドル時の終了 | mlx-lm、transformers、numba、huggingface_hub |
| `plamo-client` (crate) | Rust | プロトコル型、UDS 接続、イベントストリーム、パスと設定の解決 | tokio、serde |
| `plamo-translate` | Rust | 翻訳 CLI、`daemon` / `model` サブコマンド（launchctl 操作） | plamo-client |
| `plamo-translate-mcp` | Rust | stdio の MCP server（`translate` tool） | plamo-client、rmcp |
| `flake.nix` | Nix | パッケージ、home-manager モジュール、操作 app、devShell、checks | nixpkgs、home-manager |

責務の境界は「デーモン = 推論とモデル管理、Rust = UX と launchd 操作」とする。PFML3 プロンプトの生成と入力の意味的な検証は、モデル同梱の chat template を使うデーモン側だけが行う。

### リポジトリ構成
```
daemon/                          # Python パッケージ plamo_translated（pyproject.toml, uv.lock, tests/）
crates/plamo-client/
crates/plamo-translate/
crates/plamo-translate-mcp/
protocol/fixtures/*.ndjson       # Python と Rust の両方のテストが読むプロトコル例
nix/{packages.nix,hm-module.nix,launchd.nix}
flake.nix
justfile
docs/superpowers/specs/
```

## 3. ファイル配置（macOS の規約に従う）

| 用途 | パス | 備考 |
|---|---|---|
| 設定 | `$PLAMO_TRANSLATE_CONFIG` → `${XDG_CONFIG_HOME:-~/.config}/plamo-translate/config.toml` | home-manager の `xdg.configFile` で置ける XDG の場所にする。最初に見つかった1ファイルだけを読む（マージしない） |
| アプリデータの基点 | `~/Library/Application Support/plamo-translate/` | ディレクトリは 0700 で作る |
| モデルのウェイト | `<基点>/huggingface/hub/` | デーモンが `HF_HUB_CACHE` に設定する。`~/.cache/huggingface` とは共有しない |
| remote code のキャッシュ | `<基点>/huggingface/modules/` | `HF_MODULES_CACHE` に設定する |
| ソケット | `<基点>/daemon.sock` | plist の `SockPathMode` は 0600（10進で 384）。sun_path の上限 104 バイトは install 時に検証する |
| ログ | `~/Library/Logs/plamo-translate/daemon.log` | デーモンが `RotatingFileHandler`（5MB × 3世代）で書く。Console.app からも見える |
| クラッシュ時の出力 | `~/Library/Logs/plamo-translate/daemon.stderr.log` | plist の `StandardErrorPath` / `StandardOutPath`。Python の起動失敗などを捕捉する |
| LaunchAgent | `~/Library/LaunchAgents/local.plamo-translate.daemon.plist` | home-manager と CLI の `install` のどちらが書いても同じパス。Label は `local.plamo-translate.daemon`、ドメインは `gui/$UID` |

- **ウェイトは Application Support に置く**。Caches はシステムが容量不足時に消すことがあり、消されると数 GB の再ダウンロードになるため。
- **Time Machine から除外する**。ウェイトは再取得できるので、デーモンが `<基点>/huggingface/` を初めて作るときに `tmutil addexclusion`（`-p` なし、root 不要の sticky 除外）を実行する。失敗しても警告ログを出すだけで続行する。
- HF のトークンは既定の場所（`~/.cache/huggingface/token`、または `HF_TOKEN`）をそのまま使う。`HF_HOME` は変えず、`HF_HUB_CACHE` と `HF_MODULES_CACHE` だけを変える。

## 4. 設定ファイル

```toml
# ~/.config/plamo-translate/config.toml（すべて省略可）
model = "pfnet/plamo-3-translate-2b-MLX-4bit"
revision = "63777ddc6dc80aacacd92a1bf6966105f2f6270c"  # 省略時は下記の規則
idle_timeout_secs = 600      # 0 で無期限
max_queue = 8                # 処理待ちの上限。超えたら busy エラー
default_target = "auto"      # "auto" または言語コード
glossary_file = "~/.config/plamo-translate/glossary.tsv"  # 常用の用語集（任意）
socket_path = "~/Library/Application Support/plamo-translate/daemon.sock"
```

- **revision の固定**: `trust_remote_code` はモデルリポジトリの Python コードを実行するため、上流の更新が黙って反映されないようにする。既定モデルには、リリース時に検証したリビジョンをコードに埋め込む。既定以外のモデルで `revision` を省略した場合は `main` を使い、そのリビジョン ID を起動ログに警告として出す。
- 設定はデーモンの起動時に1回だけ読む。変更の反映には `plamo-translate daemon restart` が必要（`status` で現在のモデルとリビジョンを確認できる）。
- `~` の展開は Rust と Python の双方で行う。

## 5. デーモン `plamo-translated`

### モジュール構成
- `activation`: launchd の `launch_activate_socket("Listeners")` を ctypes（libSystem）で呼び、listen 済みの fd を受け取る。`--socket PATH` が与えられたら自分で bind する（開発用・テスト用）。
- `config`: 上記の探索順で TOML（`tomllib`）を読み、dataclass に変換する。
- `engine`: `Engine` プロトコルの定義と、その実装 `MlxEngine`。
  - `ensure_loaded(progress_cb)`: まず `snapshot_download(local_files_only=True)` を試し、ローカルに無ければダウンロードする（ネットワーク確認を毎回しないため）。進捗は `progress_cb` で通知する。その後 `mlx_lm.load(path, model_config={"trust_remote_code": True}, tokenizer_config={"trust_remote_code": True})` でロードする。
  - `count_tokens(req)` と `stream(req) -> Iterator[str]`: `tokenizer.apply_chat_template(messages=[{"role":"user","content":text}], add_generation_prompt=True, tokenize=False, target_lang=..., source_lang=..., context=..., glossary=..., form=...)` でプロンプトを作り、`mlx_lm.stream_generate` を temp 0（`make_sampler(temp=0)`）で回す。stop トークンは `<|plamo:tag|>` と `<|plamo:bos|>`。
  - chat template が出す例外（`TemplateError`）は `invalid_request` に変換し、template のメッセージをそのまま返す。
  - モデル同梱の `serve.py` と同じ MLX 環境変数（`MLX_ENABLE_TF32=0` など）を、import 前に `setdefault` で設定する。`serving_memory.py` のパッチはプロンプトキャッシュ用なので使わない。
- `server`: asyncio の UDS サーバー。
  - 接続の受け付けは並行に行う。生成は GPU を1本しか使わないため、単一ワーカーが FIFO キューから1件ずつ処理する。推論は `asyncio.to_thread` で実行し、トークンは `asyncio.Queue` で接続ハンドラに渡す。
  - キューが `max_queue` 件に達していたら、新しい要求には即座に `busy` を返す。
  - クライアントの切断を検知したら、生成ループのキャンセルフラグを立ててトークン単位で中断する。
  - アイドル監視: 「処理中の要求なし、キュー空、接続なし」の状態が `idle_timeout_secs` 続いたら、ログを出して `exit(0)` する。launchd がソケットを持ち続けるので、次の接続で再起動される。
  - SIGTERM を受けたら、処理中の要求に `error: shutting_down` を送ってから終了する。
- `__main__`: 引数の解析、ログの設定、`HF_HUB_CACHE` などの環境変数の設定、起動。

### プロトコル v1
1接続1リクエストで、UTF-8 の NDJSON（1行1 JSON、`\n` 区切り）を使う。

**リクエスト**（クライアントから1行送り、書き込み側を shutdown する）
```json
{"v":1,"op":"translate","text":"…","target_lang":"en","source_lang":null,
 "context":null,"glossary":[{"source":"推論","target":"inference"}],
 "form":null,"stream":true,"max_tokens":null}
{"v":1,"op":"status"}
{"v":1,"op":"load"}
```
- `op` が `load` の場合は、モデルのダウンロードとロードだけを行い、`progress` と `done` を返す（事前ウォームアップ用）。
- `max_tokens` が null の場合は、`min(4 * 入力トークン数 + 256, 32768 - 入力トークン数)` を使う。
- `target_lang` に `"auto"` は渡さない（auto の解決はクライアント側で行う）。

**レスポンス**（デーモンから複数行）
```json
{"type":"progress","stage":"downloading","done_bytes":123,"total_bytes":2420000000}
{"type":"progress","stage":"loading"}
{"type":"delta","text":"The meeting"}
{"type":"done","text":"<全文>","usage":{"input_tokens":12,"output_tokens":9},"model":"…","revision":"…"}
{"type":"status","model":"…","revision":"…","loaded":true,"uptime_secs":42,"idle_timeout_secs":600,"queue_len":0,"version":"0.1.0","protocol":1}
{"type":"error","code":"…","message":"…"}
```
- `delta` は `stream:true` のときだけ送る。`done`、`status`、`error` は終端イベントで、送った後にデーモンが接続を閉じる。
- `done.text` は常に訳文の全文を持つ。ストリーミングの有無にかかわらず、受信側は `done.text` を正とする。
- エラーコードは次のとおり。

| code | 意味 |
|---|---|
| `invalid_request` | JSON の不正、必須項目の欠落、chat template の検証エラー |
| `unsupported_version` | `v` が非対応 |
| `context_too_long` | 入力トークン数と `max_tokens` の合計が 32,768 を超える（生成前に判定する） |
| `busy` | キューが満杯 |
| `model_load_failed` | ダウンロードまたはロードの失敗（message に原因を入れる） |
| `shutting_down` | 終了処理中 |
| `internal` | その他 |

- 1行の上限は 4 MiB とし、超えたら `invalid_request` を返す。

## 6. Rust 側

### `plamo-client`
- プロトコル型（`Request`、`Event`、`ErrorCode`）を serde で定義する。未知の `type` やフィールドは無視して前方互換を保つ。
- `Client::translate(req) -> impl Stream<Item = Result<Event>>`、`Client::status()`、`Client::load()`。
- 接続のタイムアウトは 5 秒。launchd がプロセスを起動するまで accept が遅れることがあるため、2 秒より長めにとる。読み取りにはタイムアウトを設けない（ロードに時間がかかるため）。呼び出し側が必要に応じて全体のタイムアウトをかける。
- 接続に失敗したら `Error::DaemonUnreachable { socket_path }` を返す。
- 設定の解決（`Config::load()`）と `resolve_target(text, default_target)` を持つ。後者は `auto` のとき、かな・漢字の文字数が英字を含む全言語文字の 30% 以上なら `en`、それ以外は `ja` を返す。
- 用語集 TSV のパーサー（`原語<TAB>訳語`、`#` で始まる行と空行は無視、列数が不正な行は行番号付きでエラー）。

### CLI `plamo-translate`
```
plamo-translate [TEXT] [-t LANG] [-s LANG] [--context TEXT | --context-file FILE]
                [--glossary FILE]... [--form polite|plain] [--no-stream] [--json]
plamo-translate status
plamo-translate model pull          # op=load で事前ダウンロードとロード
plamo-translate model path          # ウェイトの保存先を表示
plamo-translate daemon install [--daemon-bin PATH]
plamo-translate daemon uninstall [--purge]
plamo-translate daemon {start|stop|restart|status|logs [-f]}
```
- TEXT を省略すると stdin を読む。訳文は stdout に出す。進捗表示（「モデルをダウンロード中 45%」「モデルをロード中…」）は、stderr が TTY のときだけ出す。
- `--glossary` は複数指定でき、config の `glossary_file` に追記される形で結合する。同じ原語が重複したら後の指定を優先する。
- `--json` を付けると、`done` イベントをそのまま1行の JSON で出す。
- 終了コード: 0 は成功、1 は翻訳エラー（デーモンが error を返した）、2 は使い方の誤り、3 はデーモンに到達できない場合。終了コード 3 のときは `plamo-translate daemon install` を案内する。

#### launchd の操作
すべて `Launchctl` トレイトの背後に置き、テストでは発行されるコマンド列を検証する。

| サブコマンド | 動作 |
|---|---|
| `install` | plist を生成して `~/Library/LaunchAgents/` に書き、`launchctl bootstrap gui/$UID <plist>` を実行する。既に読み込まれていれば先に `bootout` する。ログとデータのディレクトリも作る |
| `uninstall` | `bootout` して plist を削除する。`--purge` を付けると Application Support とログも削除する（確認プロンプトあり、`--yes` で省略） |
| `start` | `launchctl kickstart gui/$UID/<label>`（ソケットは常に待ち受けているので、プロセスを先に立ち上げるだけ）に続けて op=load を送る |
| `stop` | `launchctl kill SIGTERM gui/$UID/<label>`。モデルは解放されるが、ソケットは残り次の接続で再起動する |
| `restart` | `kickstart -k` |
| `status` | `launchctl print` から読み込み状態と PID を取り、デーモンが動いていれば op=status の結果も合わせて表示する |
| `logs` | `daemon.log` を表示する（`-f` で追従） |

- plist の主な内容: `Label`、`ProgramArguments`（デーモンの実行ファイル）、`Sockets.Listeners.{SockPathName, SockPathMode=384}`、`StandardOutPath` / `StandardErrorPath`、`ProcessType=Interactive`。`KeepAlive` と `RunAtLoad` は指定しない（オンデマンド起動のため）。管理元の識別には `EnvironmentVariables.PLAMO_TRANSLATE_MANAGED_BY` に `cli` または `nix` を入れる（launchd が知らないトップレベルキーを避けるため、環境変数に入れる）。
- `install` と `uninstall` は、既存の plist の管理元が `nix` の場合（home-manager が書いた plist は Nix store へのシンボリックリンクでもある）は、変更を拒否して終了コード 2 を返す。それ以外のサブコマンドは管理元を問わず動く。
- CLI の `install` は、home-manager モジュールを使わない場合（`nix run .#daemon-install` や開発時）のためにある。
- デーモンの実行ファイルは「`--daemon-bin` → `$PLAMO_TRANSLATED_BIN` → `<CLI の実体の場所>/../libexec/bin/plamo-translated` → `PATH`」の順に探す。`install` は、見つかったパスの symlink を解決せずにそのまま plist に書く。`nix profile` 経由なら、プロファイルのパスが更新後も有効なまま残るため。

### MCP server `plamo-translate-mcp`
- rmcp の stdio transport を使う。tool は `translate` の1つだけ。
- 入力スキーマ: `text`（必須）、`target_lang`（必須。description に主要コードと `ja(easy)` を列挙する）、`source_lang`、`context`、`glossary: [{source, target}]`、`form: "polite" | "plain"`（`target_lang=ja` のときだけ有効）。
- 出力は訳文を text content として返す。`usage` は返さない。
- クライアントが `progressToken` を渡してきた場合は、progress 通知でロード状況と、これまでに生成した部分訳の文字数を送る。
- デーモンのエラーは `isError: true` の tool 結果として返す。到達できない場合は導入手順を含める。
- 1回の呼び出しの全体タイムアウトは 300 秒（初回ダウンロードを考慮）。環境変数 `PLAMO_TRANSLATE_MCP_TIMEOUT_SECS` で変更できる。
- config の `glossary_file` は MCP からの呼び出しにも適用する。引数で渡した glossary が優先される。

## 7. Nix flake

- `packages.aarch64-darwin`
  - `plamo-translated`: `python3Packages.buildPythonApplication`。依存は nixpkgs の `mlx`、`mlx-lm`、`transformers`、`numba`、`huggingface-hub`。nixpkgs の版がモデルの要求に足りなければ、flake 内の overlay で上げる。
  - `plamo-translate`: crane で CLI と MCP をビルドする。`postInstall` で `$out/libexec/bin/plamo-translated` へのシンボリックリンクを張り、CLI が Nix 環境でもデーモンを見つけられるようにする。
  - `default`: `plamo-translate`（デーモンを内包する）。
- `homeManagerModules.default`（**主たる導入経路**。nix-darwin 構成の `home-manager.users.<user>.imports` に入れて使う）
  ```nix
  services.plamo-translate = {
    enable = true;
    package = …;          # 既定は self.packages.${pkgs.system}.default
    settings = { model = "…"; idle_timeout_secs = 600; … };  # config.toml に変換
  };
  ```
  - `launchd.agents.plamo-translate = { enable = true; config = { … }; }` で plist を定義する。home-manager が `~/Library/LaunchAgents/` への配置と、activation 時の `launchctl bootout` / `bootstrap` を行う。`config` の主なキーは、`Label`、`ProgramArguments`、`Sockets`、ログのパス、`ProcessType`、`EnvironmentVariables`。
  - plist では `~` が使えないため、ソケットとログのパスは `config.home.homeDirectory` から絶対パスで組み立てる。`settings.socket_path` も同じ値で上書きし、デーモンと CLI のパスを必ず一致させる。
  - ログディレクトリと Application Support 配下のディレクトリは、launchd がソケットを作る前に存在している必要がある。`home.activation` の `lib.hm.dag.entryBefore [ "setupLaunchAgents" ]` で、0700 で作成する（ユーザー権限で動くので chown は不要）。
  - settings は `pkgs.formats.toml` で生成し、`xdg.configFile."plamo-translate/config.toml"` に置く。デーモン、CLI、MCP はいずれも §3 の探索順でこのファイルを読むので、`PLAMO_TRANSLATE_CONFIG` は設定しない。
  - `package` を `home.packages` に追加する。
  - `nix/launchd.nix` に plist 属性の生成関数を置き、モジュールの評価テストからも使う。キー構成は CLI の `install` と揃え、`PLAMO_TRANSLATE_MANAGED_BY=nix` を入れる。
  - flake の inputs に `home-manager` を加える（モジュールの評価テストにだけ使い、`follows` で nixpkgs を揃える）。
- `apps`: `daemon-install`、`daemon-uninstall`、`daemon-start`、`daemon-stop`、`daemon-restart`、`daemon-status`、`daemon-logs`。いずれも `plamo-translate daemon <cmd>` を呼ぶ `writeShellApplication`。
- `devShells.default`: rust toolchain（rust-overlay は使わず、nixpkgs の stable）、uv、python3、just、cargo-insta。
- `checks`: cargo test、pytest（slow を除く）、home-manager モジュールの評価テスト（最小の `homeManagerConfiguration` を評価し、`launchd.agents` から生成された plist の属性と `xdg.configFile` の内容を assert する）。
- `devShells.default` の uv は、デーモンの開発時のテスト実行にだけ使う。パッケージは nixpkgs の Python パッケージで組むので、`uv.lock` はビルドに使わない。

## 8. 将来の拡張（v1 では作らない）

### Homebrew tap
- brew の `mlx` formula は `python@3.15` 向けであり、tokenizer が依存する numba / llvmlite には formula が無く、3.15 対応も未確認。そのため、封入型の formula は保守コストが高い。
- 作る場合は、`uv.lock` から resource を生成するスクリプト、numba / llvmlite の wheel 許可リスト、CLI のデーモン探索での `$(brew --prefix)/opt/...` への正規化を追加する。§6 の CLI `install` はそのまま使える。

### nix-darwin 専用モジュール
- home-manager を使わない nix-darwin 環境向け。`launchd.user.agents` は `/Library/LaunchAgents` に置かれて全ユーザーで読み込まれるため、ユーザー判定のラッパー、root での activation によるディレクトリ作成と chown、`/etc` への設定配置が必要になる。需要が出たら `nix/launchd.nix` の生成関数を再利用して追加する。

## 9. エラー処理の方針
- デーモンは、1リクエストの失敗でプロセスを落とさない。ただしモデルのロードに失敗した場合は、`model_load_failed` を返した後もプロセスを維持し、次の要求で再試行する（ネットワーク断からの回復を想定）。
- launchd の throttle（既定 10 秒）を考慮し、起動直後のクラッシュは `daemon.stderr.log` に残す。`daemon status` は、直近の終了ステータスが非 0 なら stderr ログの末尾を案内する。
- CLI と MCP は、デーモンが返した `message` を加工せずに表示する。そこに CLI 側の文脈（ソケットパスなど）を付け加える。

## 10. テスト方針
- **プロトコル fixtures**: `protocol/fixtures/` に、リクエストとレスポンス列の組を NDJSON で置く（正常系、stream:false、各エラーコード、未知フィールド）。pytest（デーモンが fixture どおりに応答するか）と Rust（クライアントが fixture を正しく解釈するか）の両方から読む。
- **デーモン**（pytest + pytest-asyncio）: `FakeEngine` を差し込んで、FIFO の直列化、`busy`、切断時の中断、アイドル時の exit、SIGTERM、`context_too_long`、template 例外の変換を確認する。`activation` は `--socket` 経路だけをテストする（launchd 経路は手動確認）。実モデル（`0.8b-MLX-4bit`）を使う結合テストは `@pytest.mark.slow` を付ける。
- **plamo-client**: tokio のフェイク UDS サーバーで fixture を再生する。`resolve_target` と TSV パーサーは単体テストで確認する。
- **CLI**: `assert_cmd` に `PLAMO_TRANSLATE_SOCKET` でフェイクサーバーを渡す。plist の生成は `insta` のスナップショットで、launchctl の呼び出しはフェイクの `Launchctl` で、デーモン実行ファイルの探索順は単体テストで確認する。
- **MCP**: rmcp のインプロセスクライアントで `tools/list` と `tools/call` を確認する。
- **Nix**: `checks` でモジュールを評価し、plist の属性を確認する。
- **手動の受け入れ確認**（実装計画の最後に手順として書く）: install → 初回翻訳（ダウンロードとロードの進捗表示）→ 2回目の即応 → アイドル後の自動終了と再起動 → Claude Code からの MCP 呼び出し。

## 11. 実装の段階
0. **事前検証（spike）**: nixpkgs の mlx-lm / numba / transformers でモデルの `model_file` 方式のロードと翻訳が通るか、`launch_activate_socket` を ctypes で呼べるか、home-manager の `launchd.agents` で `Sockets` を含む plist が期待どおりに生成・bootstrap されるか、を確認する。結果に応じて §5 と §7 を修正する。
1. デーモンとプロトコル fixtures
2. `plamo-client` と CLI（`daemon` / `model` サブコマンドを含む）
3. MCP server
4. Nix flake（パッケージ、home-manager モジュール、apps、checks）
5. 作者の nix-darwin 構成への home-manager の導入と本モジュールの組み込み、§10 の手動の受け入れ確認

## 12. 既知のリスク
| リスク | 影響 | 対応 |
|---|---|---|
| nixpkgs の mlx-lm が古く、`model_file` によるロードに未対応 | Nix 版が動かない | overlay で版を上げる |
| 上流の remote code の変更 | 予期しない挙動 | revision を固定する（§4） |
| 作者の nix-darwin 構成に home-manager が未導入 | 導入の初回に構成変更が必要 | 段階5で `home-manager.darwinModules.home-manager` を追加する手順を書く。既存の `users.users.<user>.packages` などと競合しないか確認する |
| launchd の socket activation と Python の相性 | 起動できない | 段階0で確認する。駄目なら `--socket` で自分で bind し、`KeepAlive` で常駐させる方式に退避する（アイドル時の終了はモデルのアンロードに読み替える） |
