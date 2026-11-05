# plamo-translate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** PLaMo-3-translate (MLX) を launchd の socket activation でオンデマンドに常駐させる Python デーモンと、それを使う Rust 製の CLI と MCP server を作り、home-manager モジュール経由で nix-darwin から導入できるようにする。

**Architecture:** 推論とモデル管理は Python デーモン `plamo-translated` が担い、Unix ソケット上の NDJSON（プロトコル v1）で応答する。Rust の `plamo-client` crate がプロトコルを実装し、CLI `plamo-translate` と MCP server `plamo-translate-mcp` が共有する。Nix flake は MLX 系パッケージを PyPI の最新版に固定する overlay、パッケージ、home-manager モジュール、操作用の app を提供する。

**Tech Stack:** Python 3.14（nixpkgs）、mlx-bin / mlx-metal / mlx-lm、transformers 5 系、numba、huggingface_hub、pytest + pytest-asyncio、Rust（tokio、serde、clap、rmcp、plist、assert_cmd、insta）、Nix flakes（crane、home-manager）、just。

**Spec:** `docs/superpowers/specs/2026-11-05-plamo-translate-design.md`（実装者は必ず spec と本計画の両方を読む）

## Global Constraints

- 対象は aarch64-darwin のみ。Intel Mac / Linux 向けのコードや分岐は書かない。
- 既定モデル: `pfnet/plamo-3-translate-2b-MLX-4bit`、既定リビジョン: `63777ddc6dc80aacacd92a1bf6966105f2f6270c`。
- launchd Label: `local.plamo-translate.daemon`。ドメイン: `gui/$UID`。
- 既定ソケット: `~/Library/Application Support/plamo-translate/daemon.sock`。解決順は `$PLAMO_TRANSLATE_SOCKET` → config の `socket_path` → 既定値。`SockPathMode` は 384（0600）。sun_path は 104 バイト未満であること。
- 設定ファイルの探索順: `$PLAMO_TRANSLATE_CONFIG` → `${XDG_CONFIG_HOME:-~/.config}/plamo-translate/config.toml`。最初に見つかった1ファイルだけを読む。ファイルが無ければすべて既定値。
- 設定キーと既定値: `model`、`revision`、`idle_timeout_secs = 600`（0 で無期限）、`default_target = "auto"`、`glossary_file`（任意）、`socket_path`。`~` は Rust と Python の双方で展開する。
- データ: `~/Library/Application Support/plamo-translate/`（0700）。`HF_HUB_CACHE=<基点>/huggingface/hub`、`HF_MODULES_CACHE=<基点>/huggingface/modules`。`HF_HOME` は変えない。
- ログ: `~/Library/Logs/plamo-translate/daemon.log`（RotatingFileHandler、5MB × 3世代）、`daemon.stderr.log`（plist の StandardOutPath / StandardErrorPath）。
- プロトコル v1: 1接続1リクエスト、UTF-8 NDJSON、1行の上限は 4 MiB。クライアントは書き込み側を shutdown しない。処理待ちの上限は定数 8。入力トークン数 + `max_tokens` の上限は 32768。`max_tokens` が null なら `min(4 * input_tokens + 256, 32768 - input_tokens)`。
- エラーコード: `invalid_request`、`unsupported_version`、`context_too_long`、`busy`、`model_load_failed`、`shutting_down`、`internal`。
- CLI の終了コード: 0 は成功、1 は翻訳エラー、2 は使い方の誤り（nix 管理の plist の変更拒否を含む）、3 はデーモンに到達できない。
- `default_target = "auto"` の判定: かな・漢字の文字数が、英字を含む全言語文字の 30% 以上なら `en`、それ以外は `ja`。
- クライアントの接続タイムアウトは 5 秒、読み取りタイムアウトなし。MCP の全体タイムアウトは 300 秒（`PLAMO_TRANSLATE_MCP_TIMEOUT_SECS` で変更可）。
- `form` は `polite` / `plain` のみで、`target_lang=ja` のときだけ有効（検証はデーモンが chat template で行う）。
- plist の管理元は `EnvironmentVariables.PLAMO_TRANSLATE_MANAGED_BY`（`cli` / `nix`）で判定する。
- nixpkgs の `python3Packages.mlx`（CPU 専用ビルド）は使わない。`mlx-bin`、`mlx-metal`（macosVariant "26"）、`mlx-lm` を `nix/mlx-versions.nix` の版に固定する。`mlx-bin` と `mlx-metal` は同じ版にする。
- spec からの逸脱（ユーザーに説明済み）: デーモンのテストは uv ではなく devShell の Nix Python 環境で実行する。`uv.lock` は作らない。

## Review Focus

1. **マルチバイト文字の分割**: 日本語の出力で、トークン境界が UTF-8 の文字の途中に来ても、`delta` に壊れた文字（U+FFFD）が混ざらず、`done.text` が全 delta の連結と一致すること → Task 5 の slow テスト `test_mlx_engine_japanese_output_has_no_replacement_chars` と Task 6 の `test_done_text_equals_concatenated_deltas`。
2. **生成途中の Ctrl-C**: CLI のストリーミング中に Ctrl-C で中断したら、デーモンが生成を打ち切り、直後の次の要求がすぐ処理されること → Task 6 の `test_disconnect_during_generation_frees_worker`。
3. **予約トークンを含む入力**: 原文に `<|plamo:tag|>` などが含まれる場合、クラッシュせずに `invalid_request` が返り、メッセージが template のもの（"must not contain reserved PFML tokens"）であること → Task 5 の slow テスト `test_reserved_token_input_is_invalid_request` と、FakeEngine による Task 6 の fixture `error_template.ndjson`。
4. **ロード中の2件目の要求**: 初回ダウンロードやロードの最中に別のクライアントが接続しても、`busy` にならずに待ち行列に入り、ロード完了後に処理されること → Task 6 の `test_second_request_during_load_is_queued_not_rejected`。
5. **設定ファイルが無い、または空**: 初回起動時に config.toml が無くても、デーモン・CLI・MCP がすべて既定値で動くこと → Task 4 の `test_load_config_missing_file_uses_defaults` と Task 8 の `config_missing_file_uses_defaults`。

---

## File Structure

```
flake.nix                                  # inputs: nixpkgs, crane, home-manager。outputs は nix/ 配下を組み立てるだけ
flake.lock
justfile                                   # test, test-slow, bump-mlx, fmt
nix/mlx-versions.nix                       # MLX 系3パッケージの版と hash（bump_mlx.py が生成）
nix/overlay.nix                            # pythonPackagesExtensions で mlx-metal / mlx-bin / mlx / mlx-lm を差し替え
nix/python-env.nix                         # デーモンの依存一式（devShell とパッケージで共有）
nix/packages.nix                           # plamo-translated, plamo-translate, default
nix/launchd.nix                            # plist 属性の生成関数（HM モジュールと評価テストで共有）
nix/hm-module.nix                          # services.plamo-translate
nix/checks.nix                             # cargo test, pytest, hm-module 評価テスト
scripts/bump_mlx.py                        # PyPI から最新版を取得して nix/mlx-versions.nix を書き換える
protocol/fixtures/*.ndjson                 # プロトコル適合性テストの入出力
daemon/pyproject.toml                      # パッケージ plamo-translated、エントリポイント plamo-translated
daemon/src/plamo_translated/
  __init__.py                              # __version__ = "0.1.0"
  protocol.py                              # リクエストの検証、イベントの直列化、DaemonError
  config.py                                # 設定の探索と読み込み、パス計算
  engine.py                                # Engine Protocol、ProgressEvent
  mlx_engine.py                            # MlxEngine（mlx-lm を使う実装）
  server.py                                # asyncio UDS サーバー、FIFO ワーカー、アイドル終了
  activation.py                            # launch_activate_socket / --socket
  __main__.py                              # 引数、ログ、環境変数、起動
daemon/tests/
  conftest.py                              # FakeEngine、fixture ローダー
  test_protocol.py test_config.py test_server.py test_fixtures.py test_activation.py
  test_mlx_engine_slow.py                  # @pytest.mark.slow
Cargo.toml                                 # workspace
crates/plamo-client/src/{lib,protocol,client,config,target,glossary}.rs
crates/plamo-client/tests/fixtures.rs
crates/plamo-translate/src/main.rs         # clap 定義と振り分け
crates/plamo-translate/src/translate.rs    # 翻訳コマンド
crates/plamo-translate/src/daemon/{mod,plist,launchctl,locate}.rs
crates/plamo-translate/tests/{cli_translate,cli_daemon}.rs
crates/plamo-translate-mcp/src/{main,server}.rs
crates/plamo-translate-mcp/tests/mcp.rs
docs/superpowers/notes/2026-11-05-spike-results.md
README.md
```

---

### Task 1: Nix の基盤（MLX overlay、Python 環境、devShell、bump-mlx）

**Files:**
- Create: `flake.nix`, `nix/mlx-versions.nix`, `nix/overlay.nix`, `nix/python-env.nix`, `scripts/bump_mlx.py`, `justfile`, `.gitignore`

**Interfaces:**
- Produces: `overlays.default`（`python3Packages.{mlx-metal, mlx-bin, mlx, mlx-lm}` を差し替える）、`nix/python-env.nix`（`{ python3 }: python3.withPackages (ps: [ mlx-bin mlx-lm transformers numba huggingface-hub tqdm ] ++ テスト用 [ pytest pytest-asyncio ])` を返す関数。テスト用を含むかは引数 `withTests ? false` で切り替える）、`devShells.aarch64-darwin.default`。

- [ ] **Step 1: `scripts/bump_mlx.py` を書く**

PyPI の JSON API（`https://pypi.org/pypi/<name>/json`）から `mlx`、`mlx-metal`、`mlx-lm` の最新版を取得し、`nix/mlx-versions.nix` を生成する。形式は nixpkgs の `pkgs/development/python-modules/mlx/wheel-sources.nix` と同じ構造にする。

```nix
{
  version = "0.32.4";            # mlx と mlx-metal の共通の版（違えばスクリプトがエラー終了）
  mlx = { "3.14-aarch64-darwin" = { platform = "macosx_26_0_arm64"; dist = "cp314"; hash = "sha256-…"; }; };
  mlx-metal = { "26" = { platform = "macosx_26_0_arm64"; hash = "sha256-…"; }; };
  mlx-lm = { version = "0.32.2"; hash = "sha256-…"; };   # sdist の hash
}
```

hash は PyPI JSON の `digests.sha256`（16進）を SRI 形式（`sha256-` + base64）に変換する。対象の wheel が見つからなければ非 0 で終了する。標準ライブラリだけで書く。

- [ ] **Step 2: 実行して `nix/mlx-versions.nix` を生成する**

Run: `python3 scripts/bump_mlx.py`
Expected: `nix/mlx-versions.nix` が作られ、`version = "0.32.4"`、`mlx-lm.version = "0.32.2"`（2026-11-05 時点）を含む。

- [ ] **Step 3: `nix/overlay.nix` と `nix/python-env.nix` を書く**

`pythonPackagesExtensions` に次を追加する。
- `mlx-metal`: `(pyPrev.mlx-metal.override { macosVariant = "26"; }).overridePythonAttrs` で `version` と `src`（`fetchPypi`、`format = "wheel"`、`dist = python = "py3"`、`abi = "none"`）を差し替える。
- `mlx-bin`: `overridePythonAttrs` で `version` と `src`（`pname = "mlx"`、`dist = python = abi = "cp314"`）を差し替える。
- `mlx = pyFinal.mlx-bin;`（mlx-lm などがこれを参照する。クロージャに CPU 版が入らないようにする）
- `mlx-lm`: `overridePythonAttrs` で `version` と `src`（sdist）を差し替える。

- [ ] **Step 4: `flake.nix` を書く**

inputs は `nixpkgs`（nixos-unstable）、`crane`、`home-manager`（`inputs.nixpkgs.follows = "nixpkgs"`）。この Task では `overlays.default` と `devShells.default` だけを出力する。devShell の中身は `python-env`（`withTests = true`）、`cargo`、`rustc`、`rustfmt`、`clippy`、`rust-analyzer`、`just`、`cargo-insta`。`justfile` には `bump-mlx: python3 scripts/bump_mlx.py && nix flake check && just test-slow` を用意する（`test` と `test-slow` の中身は Task 3 と Task 5 で埋める）。`.gitignore` には `target/`、`result*`、`__pycache__/`、`.pytest_cache/` を入れる。

- [ ] **Step 5: GPU 版の mlx が入ったことを確認する**

Run: `nix develop -c python -c "import mlx.core as mx, mlx_lm, importlib.metadata as m; print(mx.default_device(), mx.__version__, m.version('mlx-lm'), m.version('mlx-metal'))"`
Expected: `Device(gpu, 0) 0.32.4 0.32.2 0.32.4`

- [ ] **Step 6: Commit**

```bash
git add flake.nix flake.lock nix/ scripts/bump_mlx.py justfile .gitignore
git commit -m "build: add nix flake with pinned MLX overlay and devShell"
```

---

### Task 2: 事前検証（spike、判断ゲート）

本 Task のコードは使い捨てで、スクリプトはスクラッチ用ディレクトリに置き、commit しない。commit するのは結果メモだけ。

**Files:**
- Create: `docs/superpowers/notes/2026-11-05-spike-results.md`

**Interfaces:**
- Consumes: Task 1 の devShell
- Produces: 結果メモ。Task 5 と Task 7 はこのメモの結論に従う（例外の型名、`launch_activate_socket` の呼び方）。

- [ ] **Step 1: モデルのロードと翻訳**

devShell で `HF_HUB_CACHE="$HOME/Library/Application Support/plamo-translate/huggingface/hub"` と `HF_MODULES_CACHE` を設定する。そのうえで `mlx_lm.load("pfnet/plamo-3-translate-2b-MLX-4bit", revision="63777ddc6dc80aacacd92a1bf6966105f2f6270c", trust_remote_code=True, tokenizer_config={"trust_remote_code": True})` でロードし、`apply_chat_template([{"role":"user","content":"会議は来週の月曜日に変更されました。"}], add_generation_prompt=True, tokenize=False, target_lang="en")` と `stream_generate`（temp 0）で翻訳する。
Expected: 出力に "meeting" と "Monday" を含む。生成が `<|plamo:tag|>` を出力せずに止まる。`mx.default_device()` が GPU を返す。

- [ ] **Step 2: template の例外の型**

`form="bogus"` と `text="<|plamo:tag|>x"` でそれぞれ `apply_chat_template` を呼び、送出される例外の型と MRO を記録する。
Expected: `jinja2.exceptions.TemplateError` のサブクラス（違えば実際の型をメモに書き、Task 5 はその型を捕捉する）。

- [ ] **Step 3: `launch_activate_socket`**

スクラッチ用ディレクトリに、`Sockets.Listeners.SockPathName=<scratch>/t.sock` を持つ plist と、ctypes で `launch_activate_socket(b"Listeners", byref(fds), byref(cnt))` を呼んで fd を1つ accept し、"ok\n" を返す Python スクリプトを置く。`launchctl bootstrap gui/$UID` で読み込み、`nc -U <scratch>/t.sock` で接続し、終わったら `launchctl bootout` する。
Expected: `ok` が返る。戻り値 0、`cnt == 1`。

- [ ] **Step 4: 結果を記録して判断する**

結果をメモに書く。3項目すべてが期待どおりなら commit して Task 3 へ進む。**どれかが失敗したら、ここで止めてユーザーに報告する。** spec §12 の対応案（transformers の固定、`--socket` + `KeepAlive` 方式への退避など）から選んでもらう。

```bash
git add docs/superpowers/notes/2026-11-05-spike-results.md
git commit -m "docs: record spike results"
```

---

### Task 3: プロトコル fixtures と `protocol.py`

**Files:**
- Create: `daemon/pyproject.toml`, `daemon/src/plamo_translated/{__init__,protocol}.py`, `daemon/tests/test_protocol.py`, `protocol/fixtures/*.ndjson`, `protocol/fixtures/README.md`
- Modify: `justfile`（`test: cd daemon && pytest -m "not slow"`、`cargo test --workspace` は Task 8 で追加）

**Interfaces:**
- Produces（`plamo_translated.protocol`）:
  - `PROTOCOL_VERSION = 1`、`MAX_LINE_BYTES = 4 * 1024 * 1024`、`MAX_CONTEXT_TOKENS = 32768`
  - `@dataclass(frozen=True) class GlossaryEntry: source: str; target: str`
  - `@dataclass(frozen=True) class TranslateRequest: text: str; target_lang: str; source_lang: str | None = None; context: str | None = None; glossary: tuple[GlossaryEntry, ...] = (); form: str | None = None; stream: bool = True; max_tokens: int | None = None`
  - `@dataclass(frozen=True) class StatusRequest`、`@dataclass(frozen=True) class LoadRequest`
  - `class DaemonError(Exception): code: str; message: str`（`__init__(self, code: str, message: str)`）
  - `def parse_request(line: bytes) -> TranslateRequest | StatusRequest | LoadRequest`（失敗時は `DaemonError`。未知のフィールドは無視する）
  - `def encode_event(event: dict) -> bytes`（`json.dumps(..., ensure_ascii=False)` + `b"\n"`）
  - イベント生成ヘルパー: `progress(stage, done_bytes=None, total_bytes=None)`、`delta(text)`、`done(text, input_tokens, output_tokens, model, revision)`、`error(code, message)`、`status(**fields)`。いずれも `dict` を返し、値が None のキーは出力しない。
  - `def default_max_tokens(input_tokens: int) -> int`

**fixtures の形式**（`protocol/fixtures/README.md` に書く）: 1行目は `{"fake": {...}}` で FakeEngine の振る舞いを指定する（Rust 側は読み飛ばす）。2行目はリクエストで、生のバイト列を試す fixture では `{"raw": "<文字列>"}` とする。3行目以降は期待するイベント。値 `"<any>"` は任意の値に一致する。`fake` のキーは `tokens: [str]`、`input_tokens: int`、`load_error: str`（当初あった chat template のエラー指定は、PFML3 をデーモンで組み立てる変更で削除した）、`download: bool`。

用意する fixture: `translate_stream`、`translate_nostream`、`status`、`load_with_download`（progress downloading → loading → done）、`error_invalid_json`（raw `"{not json"`）、`error_missing_target_lang`、`error_unsupported_version`（`"v":2`）、`error_reserved_token`（当初は `error_template`。PFML3 をデーモンで組み立てる変更で改名した）、`error_context_too_long`（input_tokens 32000、max_tokens 1000）、`error_model_load_failed`、`unknown_fields`（リクエストとイベントに未知のキーを含む）。`done` の `model` と `revision` は `"<any>"` にする。

- [ ] **Step 1: 失敗するテストを書く**

```python
def test_parse_translate_minimal():
    r = parse_request(b'{"v":1,"op":"translate","text":"hi","target_lang":"ja"}')
    assert r == TranslateRequest(text="hi", target_lang="ja")

def test_parse_glossary_entries():
    r = parse_request('{"v":1,"op":"translate","text":"x","target_lang":"en","glossary":[{"source":"推論","target":"inference"}]}'.encode())
    assert r.glossary == (GlossaryEntry("推論", "inference"),)

def test_parse_rejects_wrong_version():
    with pytest.raises(DaemonError) as e: parse_request(b'{"v":2,"op":"status"}')
    assert e.value.code == "unsupported_version"

@pytest.mark.parametrize("line", [b"{not json", b'{"v":1,"op":"translate","text":"x"}', b'{"v":1,"op":"nope"}', b'{"v":1,"op":"translate","text":1,"target_lang":"en"}', b'{"v":1,"op":"translate","text":"x","target_lang":"en","glossary":[{"source":"a"}]}'])
def test_parse_invalid_request(line):
    with pytest.raises(DaemonError) as e: parse_request(line)
    assert e.value.code == "invalid_request"

def test_parse_ignores_unknown_fields():
    assert parse_request(b'{"v":1,"op":"status","extra":1}') == StatusRequest()

def test_default_max_tokens():
    assert default_max_tokens(10) == 296
    assert default_max_tokens(10000) == 22768

def test_encode_event_keeps_japanese_and_drops_none():
    line = encode_event(progress("loading"))
    assert line.endswith(b"\n") and line.count(b"\n") == 1
    assert json.loads(line) == {"type": "progress", "stage": "loading"}
    assert "会議".encode() in encode_event(delta("会議"))
```

- [ ] **Step 2: 失敗を確認する**

Run: `nix develop -c bash -c "cd daemon && pytest tests/test_protocol.py -q"`
Expected: FAIL（`ModuleNotFoundError: plamo_translated`）

- [ ] **Step 3: `pyproject.toml`（hatchling、`[project.scripts] plamo-translated = "plamo_translated.__main__:main"`、`[tool.pytest.ini_options]` で `pythonpath = ["src"]`、`asyncio_mode = "auto"`、`markers = ["slow"]`）と `protocol.py` を実装する。fixture ファイルを作る**

- [ ] **Step 4: 成功を確認する**

Run: `nix develop -c bash -c "cd daemon && pytest tests/test_protocol.py -q"`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add daemon/ protocol/ justfile
git commit -m "feat(daemon): add protocol v1 parsing and shared fixtures"
```

---

### Task 4: デーモンの設定とパス（`config.py`）

**Files:**
- Create: `daemon/src/plamo_translated/config.py`, `daemon/tests/test_config.py`

**Interfaces:**
- Produces（`plamo_translated.config`）:
  - `DEFAULT_MODEL = "pfnet/plamo-3-translate-2b-MLX-4bit"`、`DEFAULT_REVISION = "63777ddc6dc80aacacd92a1bf6966105f2f6270c"`
  - `@dataclass(frozen=True) class Paths: data_dir: Path; hub_cache: Path; modules_cache: Path; log_dir: Path; default_socket: Path`、`def paths(home: Path) -> Paths`
  - `@dataclass(frozen=True) class Config: model: str; revision: str; revision_pinned: bool; idle_timeout_secs: int; socket_path: Path; config_file: Path | None`
  - `def find_config_file(env: Mapping[str, str], home: Path) -> Path | None`
  - `def load_config(env: Mapping[str, str], home: Path) -> Config`（デーモンは `glossary_file` と `default_target` を使わないので読まない。未知のキーは無視する）

- [ ] **Step 1: 失敗するテストを書く**

```python
def test_load_config_missing_file_uses_defaults(tmp_path):
    c = load_config({}, tmp_path)
    assert (c.model, c.revision, c.revision_pinned, c.idle_timeout_secs) == (DEFAULT_MODEL, DEFAULT_REVISION, True, 600)
    assert c.socket_path == tmp_path / "Library/Application Support/plamo-translate/daemon.sock"
    assert c.config_file is None

def test_env_config_path_wins(tmp_path): ...       # PLAMO_TRANSLATE_CONFIG の指すファイルが XDG より優先される
def test_xdg_config_home_respected(tmp_path): ...  # XDG_CONFIG_HOME/plamo-translate/config.toml を読む
def test_tilde_expanded_in_socket_path(tmp_path):  # socket_path = "~/x.sock" → tmp_path/"x.sock"
def test_non_default_model_without_revision_uses_main(tmp_path):
    # model = "pfnet/plamo-3-translate-8b-MLX-4bit" のみ → revision == "main", revision_pinned is False
def test_default_model_with_explicit_revision_keeps_it(tmp_path): ...
def test_env_socket_overrides_config(tmp_path):    # PLAMO_TRANSLATE_SOCKET が config より優先される
def test_paths_layout(tmp_path):
    p = paths(tmp_path)
    assert p.hub_cache == tmp_path / "Library/Application Support/plamo-translate/huggingface/hub"
    assert p.log_dir == tmp_path / "Library/Logs/plamo-translate"
```

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c bash -c "cd daemon && pytest tests/test_config.py -q"` / Expected: FAIL

- [ ] **Step 3: `config.py` を実装する**（`tomllib` を使う。不正な TOML は `ValueError` にファイルパスを含めて送出する）

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS

- [ ] **Step 5: Commit** — `git commit -m "feat(daemon): add config discovery and macOS paths"`

---

### Task 5: Engine インターフェースと MlxEngine

**Files:**
- Create: `daemon/src/plamo_translated/{engine,mlx_engine}.py`, `daemon/tests/conftest.py`, `daemon/tests/test_mlx_engine_slow.py`
- Modify: `justfile`（`test-slow: cd daemon && pytest -m slow`）

**Interfaces:**
- Consumes: `TranslateRequest`、`DaemonError`、`Config`、`Paths`
- Produces（`plamo_translated.engine`）:
  - `@dataclass(frozen=True) class ProgressEvent: stage: Literal["downloading", "loading"]; done_bytes: int | None = None; total_bytes: int | None = None`
  - `class Engine(Protocol)`: `model: str`、`revision: str`、`loaded: bool`、`def ensure_loaded(self, progress: Callable[[ProgressEvent], None]) -> None`、`def count_tokens(self, req: TranslateRequest) -> int`、`def stream(self, req: TranslateRequest, max_tokens: int, cancelled: threading.Event) -> Iterator[str]`。失敗はすべて `DaemonError` で表す（ロード失敗は `model_load_failed`、template の検証エラーは `invalid_request`）。
- Produces（`daemon/tests/conftest.py`）: `class FakeEngine(Engine)`。コンストラクタは `tokens: list[str] = ["ok"]`、`input_tokens: int = 5`、`load_error: str | None`、`download: bool = False`、`token_delay: float = 0.0`、`load_delay: float = 0.0`。`calls: list[TranslateRequest]` を記録する。`stream` は `cancelled` を各トークンの前に確認する。fixture の `fake` 行から作る `FakeEngine.from_fixture(d: dict)` も持つ。
- Produces（`plamo_translated.mlx_engine`）: `class MlxEngine(Engine)`、`__init__(self, model: str, revision: str, hub_cache: Path)`。

MlxEngine の要件（spec §5 と spike の結果に従う）:
- `ensure_loaded`: `snapshot_download(model, revision=revision, cache_dir=hub_cache, local_files_only=True)` を試し、`LocalEntryNotFoundError` なら `tqdm_class` を差し込んで本ダウンロードする（バイト数を集計して `ProgressEvent("downloading", done, total)` を通知する。通知は最大 4 回/秒）。続いて `ProgressEvent("loading")` を通知し、`mlx_lm.load(path, trust_remote_code=True, tokenizer_config={"trust_remote_code": True})` でロードする。ロード後に `mx.default_device()` が GPU でなければ、ログに ERROR を出す（処理は続ける）。例外はすべて `DaemonError("model_load_failed", f"{type(e).__name__}: {e}")` にする。ロード済みなら何もしない。
- `count_tokens` / `stream`: spec §5 のとおり `apply_chat_template` でプロンプトを作る（glossary は `[{"source":…, "target":…}]` の list にする。None のキーは渡さない）。spike で確認した template 例外の型を `DaemonError("invalid_request", str(e))` に変換する。`stream` は `mlx_lm.stream_generate(model, tokenizer, prompt, max_tokens=max_tokens, sampler=make_sampler(temp=0.0))` の `response.text` を yield し、`cancelled.is_set()` なら打ち切る。独自の停止判定は書かない。
- import の前に `MLX_ENABLE_TF32=0`、`MLX_ENABLE_CACHE_THRASHING_CHECK=0` を `os.environ.setdefault` で設定する（`__main__` でも設定する。二重設定は無害）。

- [ ] **Step 1: slow テストを書く**（実モデル、`pfnet/plamo-3-translate-0.8b-MLX-4bit` の revision `main`、hub_cache は spike と同じ場所）

```python
pytestmark = pytest.mark.slow

def test_mlx_engine_translates_ja_to_en(engine):
    out = "".join(engine.stream(TranslateRequest(text="会議は来週の月曜日に変更されました。", target_lang="en"), 128, threading.Event()))
    assert "Monday" in out and "<|plamo" not in out

def test_mlx_engine_japanese_output_has_no_replacement_chars(engine):
    parts = list(engine.stream(TranslateRequest(text="The meeting has been moved to next Monday.", target_lang="ja"), 128, threading.Event()))
    assert all("�" not in p for p in parts) and "月曜" in "".join(parts)

def test_reserved_token_input_is_invalid_request(engine):
    with pytest.raises(DaemonError) as e: engine.count_tokens(TranslateRequest(text="<|plamo:tag|>x", target_lang="en"))
    assert e.value.code == "invalid_request" and "reserved PFML tokens" in e.value.message

def test_form_requires_ja_target(engine):
    with pytest.raises(DaemonError) as e: engine.count_tokens(TranslateRequest(text="hi", target_lang="en", form="polite"))
    assert e.value.code == "invalid_request"

def test_cancel_stops_stream(engine):
    ev = threading.Event(); it = engine.stream(TranslateRequest(text="長い文章。" * 50, target_lang="en"), 2048, ev)
    next(it); ev.set(); assert len(list(it)) <= 1

def test_load_failure_maps_to_model_load_failed(tmp_path):
    e = MlxEngine("pfnet/does-not-exist-xyz", "main", tmp_path)
    with pytest.raises(DaemonError) as ex: e.ensure_loaded(lambda p: None)
    assert ex.value.code == "model_load_failed"
```

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c bash -c "cd daemon && pytest -m slow tests/test_mlx_engine_slow.py -q"` / Expected: FAIL（モジュールが無い）

- [ ] **Step 3: `engine.py`、`mlx_engine.py`、`conftest.py`（FakeEngine）を実装する**

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS（初回は 0.8b のダウンロードに時間がかかる）

- [ ] **Step 5: Commit** — `git commit -m "feat(daemon): add Engine interface, MlxEngine and FakeEngine"`

---

### Task 6: サーバー（`server.py`）

**Files:**
- Create: `daemon/src/plamo_translated/server.py`, `daemon/tests/test_server.py`, `daemon/tests/test_fixtures.py`

**Interfaces:**
- Consumes: `protocol`、`Engine`、`FakeEngine`
- Produces（`plamo_translated.server`）:
  - `MAX_QUEUE = 8`
  - `class Server`: `__init__(self, engine: Engine, idle_timeout_secs: int, version: str)`、`async def serve(self, sock: socket.socket) -> None`（アイドル終了またはシャットダウンで return する）、`def request_shutdown(self) -> None`（SIGTERM ハンドラから呼ぶ）
- テスト補助（`test_server.py` 内）: `async def run_server(engine, idle=600) -> tuple[Server, Path, asyncio.Task]` は `tmp_path` に bind した UNIX ソケットで `serve` を起動する。`async def roundtrip(path, line: bytes) -> list[dict]` は1行送って EOF まで読む（書き込み側は閉じない）。

設計の要点（spec §5）:
- 単一ワーカー用の `ThreadPoolExecutor(max_workers=1)` でロードと推論を実行する。トークンは `loop.call_soon_threadsafe(q.put_nowait, …)` で渡す。
- 受け付け → 行を読む（4 MiB 超は `invalid_request`）→ `status` は即答する。`translate` / `load` は、待ち行列が 8 件なら `busy`、そうでなければ待ち行列に入れる。ロード中かどうかは関係ない（Review Focus 4）。
- ワーカーの処理順: `ensure_loaded`（progress を中継）→ `translate` なら `count_tokens` で上限を判定（`context_too_long` のメッセージに実際のトークン数を含める）→ `stream` → `done`。`load` なら `ensure_loaded` の後に `done(text="", input_tokens=0, output_tokens=0, …)`。
- 接続ごとに読み取りを継続し、EOF なら待ち行列から外すか `cancelled.set()` する。
- アイドル: 処理中の要求も待ち行列も接続も無い状態が `idle_timeout_secs` 続いたら（0 なら監視しない）、`server.close()` → 残った接続を処理 → return。
- `request_shutdown`: 処理中と待ち行列中の全接続に `shutting_down` を送って return する。
- 想定外の例外は `internal` を返してログに出し、プロセスは継続する。

- [ ] **Step 1: 失敗するテストを書く**

```python
async def test_fixture_conformance(fixture):  # test_fixtures.py: protocol/fixtures/*.ndjson ごとに parametrize。"<any>" は任意値に一致
    ...

async def test_done_text_equals_concatenated_deltas(tmp_path):
    ev = await roundtrip_with(FakeEngine(tokens=["会", "議は", "変更"]), b'{"v":1,"op":"translate","text":"x","target_lang":"ja"}')
    assert "".join(e["text"] for e in ev if e["type"] == "delta") == ev[-1]["text"] == "会議は変更"

async def test_requests_are_processed_fifo(tmp_path):
    # token_delay=0.05 で3並列に送り、FakeEngine.calls の順序と、各 done の到着順が送信順と一致する

async def test_busy_when_queue_full(tmp_path):
    # token_delay=1 で 1 件処理中 + 8 件待ちにした後、10 件目が即座に {"type":"error","code":"busy"} を受け取る

async def test_second_request_during_load_is_queued_not_rejected(tmp_path):
    # load_delay=0.5。2件を同時に送ると、どちらも done で終わる（busy にならない）

async def test_disconnect_during_generation_frees_worker(tmp_path):
    # tokens=["a"]*1000, token_delay=0.01。最初の delta を受けたら切断する → 次の要求が 0.5 秒以内に done する。FakeEngine が cancelled を観測したことを assert する

async def test_disconnect_while_queued_removes_request(tmp_path):
    # 1件目を処理中に2件目を送って切断すると、2件目は FakeEngine.calls に現れない

async def test_stream_false_has_only_done(tmp_path): ...

async def test_idle_timeout_exits_serve(tmp_path):
    # idle=1。要求を1つ処理した後、serve が 3 秒以内に return する

async def test_idle_does_not_drop_connection_accepted_at_close(tmp_path):
    # idle=1。アイドル判定の直前に接続した要求も done を受け取ってから serve が return する（Server の close 直前にフックを入れて再現する）

async def test_shutdown_sends_shutting_down(tmp_path):
    # 生成中に request_shutdown() → クライアントは {"type":"error","code":"shutting_down"} を受け取り、serve が return する

async def test_line_too_long_is_invalid_request(tmp_path): ...  # 4 MiB + 1 バイト

async def test_status_reports_fields(tmp_path):
    # model, revision, loaded, uptime_secs, idle_timeout_secs, queue_len, version, protocol == 1
```

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c bash -c "cd daemon && pytest tests/test_server.py tests/test_fixtures.py -q"` / Expected: FAIL

- [ ] **Step 3: `server.py` を実装する**

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS。`just test` でもすべて PASS。

- [ ] **Step 5: Commit** — `git commit -m "feat(daemon): add FIFO UDS server with cancellation and idle exit"`

---

### Task 7: 起動処理（`activation.py`、`__main__.py`）

**Files:**
- Create: `daemon/src/plamo_translated/{activation,__main__}.py`, `daemon/tests/test_activation.py`

**Interfaces:**
- Consumes: `Config`、`paths`、`MlxEngine`、`Server`
- Produces:
  - `def launchd_socket(name: str = "Listeners") -> socket.socket`（`launch_activate_socket` を ctypes で呼ぶ。fd 配列は `libc.free` で解放する。fd が1つでなければ `RuntimeError`）
  - `def bind_socket(path: Path) -> socket.socket`（既存のソケットファイルを消して bind し、0600 にする）
  - `def main(argv: list[str] | None = None) -> int`。引数は `--socket PATH` と `--version`。

`main` の処理順: 引数を解析 → `load_config(os.environ, Path.home())` → `HF_HUB_CACHE` / `HF_MODULES_CACHE` / MLX の環境変数を設定 → データとログのディレクトリを 0700 で作る → `<data>/huggingface` を初めて作ったときだけ `tmutil addexclusion <dir>` を実行（失敗しても WARNING を出すだけ）→ ログを設定（RotatingFileHandler、5MB × 3、`daemon.log`）→ revision が固定されていなければ WARNING → ソケットを取得（`--socket` があれば `bind_socket`、なければ `launchd_socket`）→ SIGTERM で `request_shutdown` → `asyncio.run(server.serve(sock))` → 0 を返す。

- [ ] **Step 1: 失敗するテストを書く**

```python
def test_bind_socket_sets_mode_0600(tmp_path):
    s = bind_socket(tmp_path / "d.sock"); assert stat.S_IMODE(os.stat(tmp_path / "d.sock").st_mode) == 0o600

def test_bind_socket_replaces_stale_file(tmp_path): ...

def test_launchd_socket_outside_launchd_raises():
    with pytest.raises(RuntimeError): launchd_socket()

def test_main_with_socket_serves_status_and_exits_on_idle(tmp_path, monkeypatch):
    # HOME=tmp_path、config で idle_timeout_secs=1。FakeEngine に差し替えるため、plamo_translated.__main__.make_engine を monkeypatch する
    # 別スレッドで main(["--socket", str(sock)]) → status を受信 → main が 0 を返す。tmp_path/Library/Logs/plamo-translate/daemon.log が存在する
```

`make_engine(config: Config, paths: Paths) -> Engine` は `__main__` のモジュール関数にする（テストで差し替えるため）。

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c bash -c "cd daemon && pytest tests/test_activation.py -q"` / Expected: FAIL

- [ ] **Step 3: 実装する**

- [ ] **Step 4: 成功を確認する** — 同じコマンドと `just test` で PASS

- [ ] **Step 5: 実機で軽く確認する** — Run: `nix develop -c bash -c 'cd daemon && PYTHONPATH=src python -m plamo_translated --socket /tmp/pt.sock & sleep 2; printf "%s\n" "{\"v\":1,\"op\":\"status\"}" | nc -U /tmp/pt.sock; kill %1'` / Expected: `{"type": "status", …, "protocol": 1}` が1行返る。

- [ ] **Step 6: Commit** — `git commit -m "feat(daemon): add launchd socket activation and entrypoint"`

---

### Task 8: Rust workspace と `plamo-client`

**Files:**
- Create: `Cargo.toml`, `crates/plamo-client/Cargo.toml`, `crates/plamo-client/src/{lib,protocol,client,config,target,glossary}.rs`, `crates/plamo-client/tests/fixtures.rs`
- Modify: `justfile`（`test` に `cargo test --workspace` を追加）

**Interfaces:**
- Produces（`plamo_client`）:
  - `protocol`: `#[derive(Serialize)] pub struct TranslateRequest { pub text: String, pub target_lang: String, pub source_lang: Option<String>, pub context: Option<String>, pub glossary: Vec<GlossaryEntry>, pub form: Option<String>, pub stream: bool, pub max_tokens: Option<u32> }`（直列化時に `"v":1,"op":"translate"` を付ける。`skip_serializing_if` で None と空の Vec を省く）、`#[derive(Serialize, Deserialize, Clone, PartialEq)] pub struct GlossaryEntry { pub source: String, pub target: String }`、`#[serde(tag = "type", rename_all = "snake_case")] pub enum Event { Progress { stage: String, done_bytes: Option<u64>, total_bytes: Option<u64> }, Delta { text: String }, Done { text: String, usage: Option<Usage>, model: Option<String>, revision: Option<String> }, Status(Status), Error { code: ErrorCode, message: String }, #[serde(other)] Unknown }`、`pub struct Usage { pub input_tokens: u32, pub output_tokens: u32 }`、`pub struct Status { pub model: String, pub revision: String, pub loaded: bool, pub uptime_secs: u64, pub idle_timeout_secs: u64, pub queue_len: u32, pub version: String, pub protocol: u32 }`、`#[serde(rename_all = "snake_case")] pub enum ErrorCode { InvalidRequest, UnsupportedVersion, ContextTooLong, Busy, ModelLoadFailed, ShuttingDown, Internal, #[serde(other)] Unknown }`。`Event::is_terminal(&self) -> bool`。
  - `client`: `pub struct Client { socket_path: PathBuf }`、`Client::new(socket_path: PathBuf) -> Self`、`async fn translate(&self, req: &TranslateRequest) -> Result<EventStream, Error>`、`async fn load(&self) -> Result<EventStream, Error>`、`async fn status(&self) -> Result<Status, Error>`。`EventStream` は `futures::Stream<Item = Result<Event, Error>> + Unpin` を実装し、終端イベントの後は `None` を返す。終端イベントの前に EOF が来たら `Error::Protocol("connection closed before terminal event")` を返す。
  - `pub enum Error { DaemonUnreachable { socket_path: PathBuf, source: std::io::Error }, Io(std::io::Error), Protocol(String) }`（thiserror）。接続のタイムアウトは 5 秒で、超えたら `DaemonUnreachable`。
  - `config`: `pub struct Config { pub model: Option<String>, pub revision: Option<String>, pub idle_timeout_secs: Option<u64>, pub default_target: String, pub glossary_file: Option<PathBuf>, pub socket_path: PathBuf, pub config_file: Option<PathBuf> }`、`Config::load_from(env: &impl Fn(&str) -> Option<String>, home: &Path) -> Result<Config, ConfigError>`、`Config::load() -> Result<Config, ConfigError>`（実際の環境変数と HOME を使う）、`pub fn default_socket_path(home: &Path) -> PathBuf`。
  - `target`: `pub fn resolve_target(text: &str, default_target: &str) -> String`
  - `glossary`: `pub fn parse_glossary(s: &str) -> Result<Vec<GlossaryEntry>, GlossaryError>`、`pub struct GlossaryError { pub line: usize, pub message: String }`、`pub fn merge_glossaries(lists: impl IntoIterator<Item = Vec<GlossaryEntry>>) -> Vec<GlossaryEntry>`（同じ source は後勝ちで、最初に出現した位置を保つ）

`resolve_target` の「言語文字」は `char::is_alphabetic()` を満たす文字とする。「かな・漢字」はひらがな U+3040–309F、カタカナ U+30A0–30FF、CJK 統合漢字 U+4E00–9FFF、拡張 A U+3400–4DBF。言語文字が0個なら `ja` を返す。

- [ ] **Step 1: 失敗するテストを書く**

```rust
// target.rs
#[test] fn auto_japanese_goes_to_en() { assert_eq!(resolve_target("会議は月曜日です", "auto"), "en"); }
#[test] fn auto_english_goes_to_ja() { assert_eq!(resolve_target("The meeting is on Monday", "auto"), "ja"); }
#[test] fn auto_threshold_30_percent_is_en() { assert_eq!(resolve_target("あいう abcdefg", "auto"), "en"); }  // 3/10 = 30%
#[test] fn auto_below_threshold_is_ja() { assert_eq!(resolve_target("あい abcdefgh", "auto"), "ja"); }      // 2/10 = 20%
#[test] fn explicit_default_passthrough() { assert_eq!(resolve_target("hello", "zh-hans"), "zh-hans"); }
#[test] fn no_letters_defaults_ja() { assert_eq!(resolve_target("12345 !!", "auto"), "ja"); }
// glossary.rs
#[test] fn parses_tsv_skipping_comments_and_blank() { /* "# c\n\n推論\tinference\n" → 1件 */ }
#[test] fn reports_line_number_on_bad_row() { /* "a\tb\nbad\n" → line == 2 */ }
#[test] fn merge_later_wins_keeps_position() { /* [a→1,b→2] + [a→3] → [a→3,b→2] */ }
// config.rs
#[test] fn config_missing_file_uses_defaults() { /* default_target == "auto", socket_path == home/"Library/Application Support/plamo-translate/daemon.sock" */ }
#[test] fn env_socket_overrides_config() {}
#[test] fn env_config_path_wins_over_xdg() {}
#[test] fn tilde_expanded() {}
// protocol.rs
#[test] fn request_serializes_with_v_and_op_and_omits_none() {
    let j = serde_json::to_value(TranslateRequest { text: "x".into(), target_lang: "en".into(), stream: true, ..Default::default() }).unwrap();
    assert_eq!(j, serde_json::json!({"v":1,"op":"translate","text":"x","target_lang":"en","stream":true}));
}
#[test] fn unknown_event_type_is_unknown() {}
#[test] fn unknown_error_code_is_unknown() {}
// tests/fixtures.rs: protocol/fixtures/*.ndjson を読み、3行目以降の各行が Event に deserialize でき、最後の行が is_terminal() であること。
//   さらにフェイクの UnixListener を立て、fixture の応答行を返させて Client::translate/load/status が同じ Event 列を返すことを確認する（"<any>" を含む行は文字列 "x" や 0 に置換してから送る）
#[tokio::test] async fn unreachable_socket_is_daemon_unreachable() {}
#[tokio::test] async fn eof_before_terminal_is_protocol_error() {}
```

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c cargo test -p plamo-client` / Expected: コンパイルエラーまたは FAIL

- [ ] **Step 3: 実装する**（tokio の `UnixStream`、`BufReader::lines`、serde_json、toml、thiserror、futures）

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS

- [ ] **Step 5: Commit** — `git commit -m "feat(client): add plamo-client crate with protocol, config and helpers"`

---

### Task 9: CLI の翻訳コマンド

**Files:**
- Create: `crates/plamo-translate/Cargo.toml`, `crates/plamo-translate/src/{main,translate}.rs`, `crates/plamo-translate/tests/cli_translate.rs`

**Interfaces:**
- Consumes: `plamo_client::{Client, Config, TranslateRequest, Event, ErrorCode, resolve_target, parse_glossary, merge_glossaries}`
- Produces: バイナリ `plamo-translate`。clap のサブコマンドとして `status`、`model {pull, path}`、`daemon {install, uninstall, start, stop, restart, status, logs}` の枠を作る（daemon と model の中身は Task 10）。`pub enum ExitCode { Ok = 0, Translation = 1, Usage = 2, Unreachable = 3 }`。

要件（spec §6）:
- 引数: `[TEXT]`、`-t/--to`、`-s/--from`、`--context` / `--context-file`（排他）、`--glossary FILE`（複数可）、`--form polite|plain`、`--no-stream`、`--json`。TEXT が無ければ stdin を読む。stdin も空なら終了コード 2。
- glossary は config の `glossary_file`（存在すれば）→ 引数の順に `merge_glossaries` する。
- `delta` を受け取るたびに stdout に書いて flush する。`--no-stream` なら `done.text` だけを出す。最後に改行が無ければ1つ足す。`--json` なら `done` イベントを1行の JSON で出す。
- 進捗は stderr が TTY のときだけ出す（`downloading 45%`、`loading model…`）。
- error イベント → `error: <code>: <message>` を stderr に出し、終了コード 1。`DaemonUnreachable` → ソケットパスと `plamo-translate daemon install` の案内を出し、終了コード 3。
- SIGINT を受けたら接続を閉じて終了コード 130 で抜ける（デーモン側で生成が打ち切られる）。
- `status` サブコマンドは op=status の結果を `key: value` 形式で出す（この Task で実装する）。

- [ ] **Step 1: 失敗するテストを書く**（`assert_cmd` に `PLAMO_TRANSLATE_SOCKET` と `PLAMO_TRANSLATE_CONFIG` を渡す。フェイクデーモンはテスト内で `std::os::unix::net::UnixListener` を別スレッドで回し、受け取ったリクエスト行を記録して、決められた応答行を返す）

```rust
#[test] fn streams_deltas_to_stdout() { /* 応答 delta "The ", delta "meeting", done "The meeting" → stdout == "The meeting\n"、終了コード 0 */ }
#[test] fn reads_stdin_when_no_text() { /* .write_stdin("会議") → 記録されたリクエストの text == "会議" */ }
#[test] fn auto_target_from_japanese_text() { /* -t なし、text "会議は月曜" → target_lang == "en" */ }
#[test] fn glossary_files_merge_config_then_args() { /* config の glossary_file a→1、--glossary a→2 → glossary == [{a,2}] */ }
#[test] fn no_stream_flag_sets_stream_false() {}
#[test] fn json_flag_prints_done_event() {}
#[test] fn daemon_error_exits_1_with_message() { /* error invalid_request "form must be polite or plain…" → stderr に含まれ、終了コード 1 */ }
#[test] fn unreachable_exits_3_with_install_hint() { /* 存在しないソケット → stderr に "plamo-translate daemon install"、終了コード 3 */ }
#[test] fn context_and_context_file_conflict_exits_2() {}
#[test] fn empty_stdin_exits_2() {}
#[test] fn status_prints_model_and_loaded() {}
```

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c cargo test -p plamo-translate --test cli_translate` / Expected: FAIL

- [ ] **Step 3: 実装する**（clap derive、tokio、`std::io::IsTerminal`、`tokio::signal::ctrl_c`）

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS

- [ ] **Step 5: Commit** — `git commit -m "feat(cli): add translate and status commands"`

---

### Task 10: CLI の `daemon` / `model` サブコマンド（plist、launchctl、デーモンの探索）

**Files:**
- Create: `crates/plamo-translate/src/daemon/{mod,plist,launchctl,locate}.rs`, `crates/plamo-translate/tests/cli_daemon.rs`
- Modify: `crates/plamo-translate/src/main.rs`

**Interfaces:**
- Produces:
  - `plist`: `pub struct AgentSpec { pub label: String, pub program: PathBuf, pub socket_path: PathBuf, pub log_dir: PathBuf, pub managed_by: &'static str }`、`pub fn render_plist(spec: &AgentSpec) -> String`（`plist` crate で XML を出力。キーは `Label`、`ProgramArguments=[program]`、`Sockets.Listeners.{SockPathName, SockPathMode=384}`、`StandardOutPath` と `StandardErrorPath`（いずれも `log_dir/daemon.stderr.log`）、`ProcessType=Interactive`、`EnvironmentVariables.PLAMO_TRANSLATE_MANAGED_BY`）、`pub fn read_managed_by(xml: &[u8]) -> Option<String>`
  - `launchctl`: `pub trait Launchctl { fn run(&self, args: &[String]) -> std::io::Result<std::process::Output>; }`、`pub struct SystemLaunchctl;`、テスト用の `RecordingLaunchctl { calls: RefCell<Vec<Vec<String>>>, responses: … }`。ドメイン文字列は `pub fn gui_domain() -> String`（`libc::getuid()`）。
  - `locate`: `pub fn locate_daemon(explicit: Option<&Path>, env_bin: Option<&OsStr>, current_exe: &Path, path_var: Option<&OsStr>) -> Option<PathBuf>`（順序は `--daemon-bin` → `PLAMO_TRANSLATED_BIN` → `<current_exe の親>/../libexec/bin/plamo-translated` → PATH。symlink は解決しない）
  - `mod.rs`: `pub fn run(cmd: DaemonCmd, cfg: &Config, lc: &dyn Launchctl, home: &Path) -> Result<ExitCode>`

各サブコマンドの動作は spec §6 の表のとおり。補足:
- `install`: sun_path が 104 バイト以上なら終了コード 2。既存の plist の `read_managed_by` が `nix` なら終了コード 2（メッセージに「home-manager が管理しています」と出す）。デーモンが見つからなければ終了コード 2。`print` が成功（読み込み済み）なら先に `bootout` → plist を書く → `bootstrap <domain> <plist>`。データ（0700）とログのディレクトリを作る。
- `uninstall`: 管理元が `nix` なら終了コード 2。`--purge` は確認プロンプトを出す（`--yes` で省略）。
- `start` = `kickstart <domain>/<label>`、`stop` = `kill SIGTERM <domain>/<label>`、`restart` = `kickstart -k <domain>/<label>`。
- `status`: `print <domain>/<label>` の出力から `state` と `pid` と `last exit code` を抜き出して表示する。動作中なら op=status の結果も足す。last exit code が 0 以外なら `daemon.stderr.log` の末尾 20 行を表示する。
- `logs [-f]`: `daemon.log` を表示する（`-f` は `tail -f` 相当）。
- `model pull`: op=load を送り、進捗を表示する。`model path`: `<data>/huggingface/hub` を表示する。

- [ ] **Step 1: 失敗するテストを書く**

```rust
#[test] fn plist_snapshot() { insta::assert_snapshot!(render_plist(&AgentSpec { label: "local.plamo-translate.daemon".into(), program: "/nix/profile/libexec/bin/plamo-translated".into(), socket_path: "/Users/u/Library/Application Support/plamo-translate/daemon.sock".into(), log_dir: "/Users/u/Library/Logs/plamo-translate".into(), managed_by: "cli" })); }
#[test] fn read_managed_by_roundtrip() {}
#[test] fn locate_prefers_explicit_then_env_then_libexec_then_path() { /* tempdir に各候補を作り、順に消しながら確認する */ }
#[test] fn locate_does_not_resolve_symlinks() {}
#[test] fn install_bootouts_if_loaded_then_bootstraps() { /* RecordingLaunchctl: print 成功 → calls == [[print,..],[bootout,..],[bootstrap,gui/<uid>,<plist>]] */ }
#[test] fn install_refuses_nix_managed_plist() { /* 既存 plist の managed_by=nix → ExitCode::Usage、calls は空 */ }
#[test] fn install_rejects_long_socket_path() {}
#[test] fn stop_sends_sigterm_kill() { /* [kill, SIGTERM, gui/<uid>/local.plamo-translate.daemon] */ }
#[test] fn restart_uses_kickstart_k() {}
#[test] fn status_parses_launchctl_print() { /* 実機の `launchctl print` 出力の抜粋を fixture として置き、state/pid/last exit code を取り出す */ }
#[test] fn model_path_prints_hub_dir() {}  // assert_cmd、HOME=tempdir
```

`run` は `home` と `Launchctl` を引数に取るので、`install` と `uninstall` のテストは tempdir の HOME で実行する。

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c cargo test -p plamo-translate` / Expected: FAIL

- [ ] **Step 3: 実装する**。`cargo insta review` でスナップショットを確定する（spec の必須キーが揃っていることを目視で確認する）

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS

- [ ] **Step 5: Commit** — `git commit -m "feat(cli): add daemon and model subcommands with launchctl integration"`

---

### Task 11: MCP server

**Files:**
- Create: `crates/plamo-translate-mcp/Cargo.toml`, `crates/plamo-translate-mcp/src/{main,server}.rs`, `crates/plamo-translate-mcp/tests/mcp.rs`

**Interfaces:**
- Consumes: `plamo_client::{Client, Config, TranslateRequest, GlossaryEntry, Event, parse_glossary, merge_glossaries}`
- Produces: バイナリ `plamo-translate-mcp`。`pub struct PlamoServer { client: Client, config_glossary: Vec<GlossaryEntry>, timeout: Duration }`、`#[derive(Deserialize, JsonSchema)] pub struct TranslateArgs { text: String, target_lang: String, source_lang: Option<String>, context: Option<String>, glossary: Option<Vec<GlossaryEntry>>, form: Option<Form> }`、`enum Form { Polite, Plain }`（serde は lowercase）。

要件（spec §6）:
- rmcp の最新版の stdio transport と tool マクロを使う。API は実装時に rmcp のドキュメントで確認する。
- tool 名は `translate`。description に「target_lang の例: ja, en, zh-hans, zh-hant, ko, fr, de, es, ja(easy)。form は target_lang=ja のときだけ有効」を含める。
- MCP では常に `stream:true` で要求する。`progressToken` があれば、`progress` イベントと `delta` の累積文字数を progress 通知で送る。
- 結果は `done.text` を text content にする。error イベントは `isError: true` の結果に `"<code>: <message>"` を入れる。到達できない場合は `isError: true` で導入手順（`home-manager の services.plamo-translate.enable` または `plamo-translate daemon install`）を返す。
- タイムアウトは 300 秒（`PLAMO_TRANSLATE_MCP_TIMEOUT_SECS`）。超えたら `isError: true` で返す。
- glossary は config の `glossary_file` → 引数の順に `merge_glossaries` する。

- [ ] **Step 1: 失敗するテストを書く**（rmcp のインプロセスクライアントを `tokio::io::duplex` でつなぐ。デーモン側は Task 9 と同様のフェイク UDS サーバー）

```rust
#[tokio::test] async fn lists_translate_tool_with_schema() { /* tools/list → name "translate"、required ⊇ {"text","target_lang"}、form の enum == ["polite","plain"] */ }
#[tokio::test] async fn call_returns_done_text() { /* delta x2 + done "The meeting" → content[0].text == "The meeting"、is_error == false */ }
#[tokio::test] async fn daemon_error_is_tool_error() {}
#[tokio::test] async fn unreachable_is_tool_error_with_hint() {}
#[tokio::test] async fn sends_progress_when_token_given() { /* progressToken 付き → 少なくとも1回の progress 通知を受信する */ }
#[tokio::test] async fn timeout_is_tool_error() { /* PLAMO_TRANSLATE_MCP_TIMEOUT_SECS=1 相当を PlamoServer に直接注入し、応答しないフェイクで確認する */ }
#[tokio::test] async fn config_glossary_merged_with_args() {}
```

- [ ] **Step 2: 失敗を確認する** — Run: `nix develop -c cargo test -p plamo-translate-mcp` / Expected: FAIL

- [ ] **Step 3: 実装する**

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS、`just test` もすべて PASS

- [ ] **Step 5: Commit** — `git commit -m "feat(mcp): add stdio MCP server with translate tool"`

---

### Task 12: Nix パッケージ、apps、checks

**Files:**
- Create: `nix/packages.nix`, `nix/checks.nix`
- Modify: `flake.nix`

**Interfaces:**
- Consumes: `nix/overlay.nix`、`nix/python-env.nix`
- Produces: `packages.aarch64-darwin.{plamo-translated, plamo-translate, default}`、`apps.aarch64-darwin.daemon-{install,uninstall,start,stop,restart,status,logs}`、`checks.aarch64-darwin.{cargo-test, pytest, …}`（`hm-module` は Task 13 で追加）

- `plamo-translated`: `python3Packages.buildPythonApplication`（`pyproject = true`、`build-system = [ hatchling ]`、`src = ./daemon`、依存は Task 1 の python-env と同じ集合。`pytestCheckHook` を使い、`disabledTestMarks = [ "slow" ]` 相当で slow を除く）。
- `plamo-translate`: crane の `buildPackage`（workspace 全体。`src` は Cargo の範囲と `protocol/fixtures` を含むようにフィルタする）。`postInstall` で `mkdir -p $out/libexec/bin && ln -s ${plamo-translated}/bin/plamo-translated $out/libexec/bin/plamo-translated`。
- `default = plamo-translate`。
- apps: `writeShellApplication { name = "daemon-<cmd>"; text = "exec ${default}/bin/plamo-translate daemon <cmd> \"$@\""; }`。
- checks: `cargo-test`（crane の `cargoTest`）、`pytest`（`plamo-translated` のビルドに含まれる）、`clippy`（`cargoClippy`、`-D warnings`）。

- [ ] **Step 1: ビルドする** — Run: `nix build .#default && ls -l result/bin result/libexec/bin` / Expected: `plamo-translate`、`plamo-translate-mcp`、`libexec/bin/plamo-translated` が存在する

- [ ] **Step 2: 実行を確認する** — Run: `./result/libexec/bin/plamo-translated --version && ./result/bin/plamo-translate --version` / Expected: どちらも `0.1.0`

- [ ] **Step 3: checks を確認する** — Run: `nix flake check` / Expected: すべて成功

- [ ] **Step 4: Commit** — `git commit -m "build: add nix packages, daemon apps and checks"`

---

### Task 13: home-manager モジュール

**Files:**
- Create: `nix/launchd.nix`, `nix/hm-module.nix`
- Modify: `flake.nix`、`nix/checks.nix`

**Interfaces:**
- Consumes: `packages.default`
- Produces: `homeManagerModules.default`。オプションは `services.plamo-translate.{enable, package, settings}`（`settings` は `pkgs.formats.toml {}` の型）。`nix/launchd.nix` は `{ lib }: { homeDirectory, package, settings }: <launchd.agents.<name>.config の属性セット>` を返す関数。

要件（spec §7）:
- 基点、ソケット、ログのパスは `config.home.homeDirectory` から組み立てる。`settings.socket_path` を強制的にこのパスにする（`lib.mkForce` ではなく、モジュール内で `settings // { socket_path = …; }` として書き出す）。
- `launchd.agents.plamo-translate = { enable = true; config = import ./launchd.nix …; }`。`Label` は `local.plamo-translate.daemon`、`ProgramArguments = [ "${package}/libexec/bin/plamo-translated" ]`、`Sockets.Listeners = { SockPathName = …; SockPathMode = 384; }`、`StandardOutPath` / `StandardErrorPath`、`ProcessType = "Interactive"`、`EnvironmentVariables.PLAMO_TRANSLATE_MANAGED_BY = "nix"`。`waitForNixStore` は既定（true）のまま。
- `xdg.configFile."plamo-translate/config.toml".source = (pkgs.formats.toml {}).generate "config.toml" settings'`。
- `launchd.agents.plamo-translate.restartTriggers = [ config.xdg.configFile."plamo-translate/config.toml".source ]`（このオプションが存在しない home-manager の版なら、評価エラーになるので inputs の版を確認する）。
- `home.activation.plamoTranslateDirs = lib.hm.dag.entryBetween [ "setupLaunchAgents" ] [ "writeBoundary" ] ''run mkdir -p -m 700 "<data>" "<log>"'';`
- `home.packages = [ cfg.package ];`

- [ ] **Step 1: 失敗する評価テストを `nix/checks.nix` に書く**

`home-manager.lib.homeManagerConfiguration { pkgs; modules = [ self.homeManagerModules.default { home = { username = "t"; homeDirectory = "/Users/t"; stateVersion = "25.11"; }; services.plamo-translate = { enable = true; settings.idle_timeout_secs = 120; }; } ]; }` を評価し、`runCommand` 内で次を assert する（`builtins.toJSON` で書き出して `jq -e` で検査する）。
- `launchd.agents.plamo-translate.config.Sockets.Listeners.SockPathName == "/Users/t/Library/Application Support/plamo-translate/daemon.sock"`、`SockPathMode == 384`
- `ProgramArguments` の最後の要素に `libexec/bin/plamo-translated` が含まれる（`wait4path` で包まれた形も許容する）
- `EnvironmentVariables.PLAMO_TRANSLATE_MANAGED_BY == "nix"`、`Label == "local.plamo-translate.daemon"`
- 生成された config.toml に `idle_timeout_secs = 120` と `socket_path = "/Users/t/Library/Application Support/plamo-translate/daemon.sock"` が含まれる
- `home.activation.plamoTranslateDirs` が存在する

- [ ] **Step 2: 失敗を確認する** — Run: `nix build .#checks.aarch64-darwin.hm-module` / Expected: FAIL（`homeManagerModules` が無い）

- [ ] **Step 3: `nix/launchd.nix` と `nix/hm-module.nix` を実装し、`flake.nix` に `homeManagerModules.default` を追加する**

- [ ] **Step 4: 成功を確認する** — 同じコマンドで PASS、`nix flake check` もすべて成功

- [ ] **Step 5: Commit** — `git commit -m "feat(nix): add home-manager module with socket-activated LaunchAgent"`

---

### Task 14: README と受け入れ確認

ユーザーの nix-darwin 構成リポジトリは本リポジトリの外にある。そのため、構成への組み込みは手順を README に書き、ユーザーと一緒に実施する（他のリポジトリを無断で変更しない）。

**Files:**
- Create: `README.md`

- [ ] **Step 1: README を書く**

次の内容を含める。概要、ライセンス上の注意（PLaMo Community License）、nix-darwin への組み込み例、CLI と MCP の使い方、ファイル配置の表（spec §3）、`just bump-mlx` による MLX の更新手順、トラブルシュート（`daemon status`、`daemon logs`、`daemon.stderr.log`）。

組み込み例:
```nix
# flake inputs: plamo-translate.url = "github:<you>/plamo-translate"; home-manager.url = "github:nix-community/home-manager";
darwinConfigurations.<host> = nix-darwin.lib.darwinSystem {
  modules = [
    home-manager.darwinModules.home-manager
    { home-manager.users.<user> = { imports = [ plamo-translate.homeManagerModules.default ]; services.plamo-translate.enable = true; }; }
  ];
};
```
MCP の登録: `claude mcp add plamo-translate -- plamo-translate-mcp`

- [ ] **Step 2: Commit** — `git commit -m "docs: add README"`

- [ ] **Step 3: ユーザーと一緒に受け入れ確認を行う**（ユーザーの nix-darwin 構成に README の例を適用してもらった後）

1. `launchctl print gui/$UID/local.plamo-translate.daemon` → 読み込まれていて、プロセスは動いていない
2. `plamo-translate "会議は来週の月曜日に変更されました。"` → stderr にダウンロードとロードの進捗、stdout に英訳
3. もう一度実行 → ロード表示なしで即座にストリーミング
4. `settings.idle_timeout_secs = 60` で再適用して 70 秒待つ → `daemon status` でプロセスが終了済み → 再度翻訳すると自動で起動する
5. Claude Code で `claude mcp add plamo-translate -- plamo-translate-mcp` → translate tool で翻訳できる
6. `plamo-translate daemon install` → nix 管理なので拒否され、終了コード 2

結果をユーザーに報告する。失敗した項目があれば、superpowers:systematic-debugging で調査する。
