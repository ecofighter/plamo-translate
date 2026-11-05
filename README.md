# plamo-translate

Apple silicon 上で PLaMo-3-translate (MLX) を使い、日英を中心とした翻訳をローカルで行うツール。

- `plamo-translated`: Python のデーモン。launchd のソケットアクティベーションで、最初の接続時に起動し、モデルをロードして推論する。アイドルが続くと自分で終了してメモリを解放する
- `plamo-translate`: Rust 製の CLI
- `plamo-translate-mcp`: Rust 製の stdio MCP server (`translate` tool)
- Nix flake: パッケージ、home-manager モジュール (nix-darwin から使う)、操作用 app

## ライセンス上の注意

既定のモデル [`pfnet/plamo-3-translate-2b-MLX-4bit`](https://huggingface.co/pfnet/plamo-3-translate-2b-MLX-4bit) は PLaMo Community License で提供されている。個人利用や非営利利用は無償だが、商用利用は収益規模によって PFN への登録が必要になる。条件はモデルのページで確認すること。このリポジトリはモデルのウェイトを含まない。

## 導入 (nix-darwin + home-manager)

```nix
# flake inputs: plamo-translate.url = "github:<you>/plamo-translate"; home-manager.url = "github:nix-community/home-manager";
darwinConfigurations.<host> = nix-darwin.lib.darwinSystem {
  modules = [
    home-manager.darwinModules.home-manager
    { home-manager.users.<user> = { imports = [ plamo-translate.homeManagerModules.default ]; services.plamo-translate.enable = true; }; }
  ];
};
```

`darwin-rebuild switch` すると、`plamo-translate` と `plamo-translate-mcp` が PATH に入り、LaunchAgent (`local.plamo-translate.daemon`) と `~/.config/plamo-translate/config.toml` が配置される。

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

### Nix を使わない場合

`nix run .#daemon-install` (または `plamo-translate daemon install`) で plist を書いて読み込む。`daemon-uninstall`、`daemon-start`、`daemon-stop`、`daemon-restart`、`daemon-status`、`daemon-logs` も app として用意してある。plist が Nix 管理 (home-manager) のときは、`daemon install` / `daemon uninstall` は変更を拒否して終了コード 2 を返す。

## 使い方 (CLI)

```sh
plamo-translate "会議は来週の月曜日に変更されました。"
echo "Hello, world" | plamo-translate -t ja
plamo-translate -t ja --form plain "Thank you for your help."
plamo-translate --context "ソフトウェアの UI 文言" --glossary ~/glossary.tsv "Commit"
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
| `--no-stream` | ストリーミングせず、完了後にまとめて出力する |
| `--json` | 最後の `done` イベントを 1 行の JSON で出力する |

終了コード: 0 成功、1 翻訳エラー、2 使い方の誤り、3 デーモンに到達できない。130 は Ctrl-C。進捗表示は stderr が TTY のときだけ出る。

デーモンの操作:

```sh
plamo-translate daemon status
plamo-translate daemon start | stop | restart
plamo-translate daemon logs [-f]
plamo-translate daemon install [--daemon-bin PATH]
plamo-translate daemon uninstall [--purge [--yes]]   # --purge はモデル、データ、ログも削除する
```

`stop` はプロセスを止めるだけで、ソケットは残る。次の接続で自動的に起動する。

## 使い方 (MCP)

```sh
claude mcp add plamo-translate -- plamo-translate-mcp
```

`translate` tool が 1 つ使える。引数は `text`、`target_lang` (必須。`ja`、`en`、`zh-hans`、`zh-hant`、`ko`、`fr`、`de`、`es`、`ja(easy)` など)、`source_lang`、`context`、`glossary` (`[{source, target}]`)、`form` (`polite` / `plain`、`target_lang=ja` のときだけ)。config の `glossary_file` も適用され、引数の glossary が優先される。

1 回の呼び出しのタイムアウトは 300 秒 (初回のダウンロードを含むため)。環境変数 `PLAMO_TRANSLATE_MCP_TIMEOUT_SECS` で秒数を変更できる。

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
| LaunchAgent | `~/Library/LaunchAgents/local.plamo-translate.daemon.plist` (Label は `local.plamo-translate.daemon`、ドメインは `gui/$UID`) |

## MLX の更新

MLX 系の `mlx-bin`、`mlx-metal`、`mlx-lm` は `nix/mlx-versions.nix` で同じ組み合わせに固定している。PyPI の最新版に追従するには:

```sh
just bump-mlx   # scripts/bump_mlx.py で版と sha256 を更新 -> nix flake check -> just test-slow
```

通ったら commit する。開発用のテストは `just test` (slow を除く pytest と cargo test)、`just test-slow` (実モデル)。

## トラブルシュート

- 初回のリクエストはモデルのダウンロード (2B で約 2.4 GB) とロードを行うので時間がかかる。`plamo-translate model pull` で事前に取得できる
- `plamo-translate daemon status`: launchd の読み込み状態、PID、モデルとリビジョンを確認する
- `plamo-translate daemon logs -f`: `daemon.log` を追う
- 起動自体に失敗している (Python のエラーなど) ときは `~/Library/Logs/plamo-translate/daemon.stderr.log` を見る
- 終了コード 3 (デーモンに到達できない): LaunchAgent が読み込まれていない。home-manager で適用するか `plamo-translate daemon install` を実行する
- 設定を変えたのに反映されない: Nix 管理なら再適用、それ以外は `plamo-translate daemon restart`
