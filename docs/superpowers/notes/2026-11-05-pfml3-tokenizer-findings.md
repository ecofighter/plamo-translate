# PFML3 と tokenizer の調査結果（デーモンでのプロンプト構築）

対象: `pfnet/plamo-3-translate-2b-MLX-4bit@63777ddc6dc80aacacd92a1bf6966105f2f6270c` と `pfnet/plamo-3-translate-0.8b-MLX-4bit@e5fe5b7f26ebaeae48db0ce644b932d1d9b91aaa`、transformers 5.22.1、mlx-lm 0.32.2。どちらのスナップショットも `chat_template.jinja` と `tokenizer_config.json` は同一で、`chat_template.jinja` の sha256 は `cd49e7e0dfa8ec847c163db88dfca89b280dddbb73ddc60ba38c62a420eaced2`。

## 制御トークン
- id: `<|plamo:bos|>`=1、`<|plamo:tag|>`=16、`<|plamo:key|>`=17、`<|plamo:val|>`=18、`<|plamo:msg|>`=19、`<|plamo:index|>`=20、`<|plamo:item|>`=21。unk の id は 0。
- mlx-lm は `config.json` の `eos_token_id` を eos として読み、`eos_token_ids` は `{1, 16}` になる。

## 予約トークンを拒否する理由
- `Plamo3Tokenizer` は Unigram の分割より前に `AhoCorasick.encode` で `<\|plamo:[^|\s]{,64}\|>` に一致する部分を切り出し、語彙にあればその制御 id にする。`"a<|plamo:tag|>b"` は `[97, 16, 98]`、`"a<|plamo:bos|>b"` は `[97, 1, 98]`、`"a<|plamo:reserved:0x106|>b"` は `[97, 262, 98]` になる。`split_special_tokens=True` でも変わらず、無効にする引数はない。
- 語彙にない形（`<|plamo:foo|>`、`<|plamo:` だけ、`<|plamo:tag|`、`<|plamo: tag|>`、`<|PLAMO:tag|>`）は普通の文字列として扱われる。
- したがって安全にエスケープする方法はなく、`text`、`context`、`history`、`glossary` に `<|plamo:` を含む要求は拒否する（chat template と同じ規則）。語彙は revision ごとに変わりうるので、語彙を見ずに部分文字列で判定する。
- `U+EE00`（`BOUNDARY_CHAR`）は tokenizer が黙って捨てる。拒否も文書化もしていない。

## プロンプトの同一性
- `pfml3.build_prompt` の文字列は `apply_chat_template(..., add_generation_prompt=True, tokenize=False)` と一致し、`add_special_tokens=False` で encode した id 列は `apply_chat_template(..., tokenize=True, return_dict=False)` と一致する（`daemon/tests/test_pfml3_oracle_slow.py`、10 ケース × 2 モデル）。
- 先頭の `<|plamo:bos|>` は文字列としてプロンプトに含めるので、encode では `add_special_tokens=False` にする。id 列の先頭は BOS 1個で、以降に BOS は現れない。
- 区切りごとに encode して連結した結果と、全体を1回で encode した結果は同じ。
- 言語コードの検査は `re.fullmatch` を使う。`re.match(r"^...$")` は `"en\n"` を通してしまう（`$` が末尾の改行の前にも一致するため）。

## 出力の扱い
- mlx-lm 0.32.2 の `stream_generate` は、トークンが eos なら detokenizer に渡す前にループを抜ける。そのため停止トークンの文字列は通常 `response.text` に現れない。最後の `GenerationResponse` の `finish_reason` は、最後のトークンが eos なら `"stop"`、そうでなければ `"length"`（`max_tokens` 個目が eos の場合も `"stop"`）。
- デーモンは念のため `done.text` の末尾の `<|plamo:tag|>` / `<|plamo:bos|>` を1個だけ取り除く。

## 観察
- 2b のスナップショットのキャッシュは mlx-lm の `DEFAULT_ALLOW_PATTERNS` で取得された形で、ハブの一覧上 8 ファイル（`.gitattributes`、`LICENSE.mlx-lm`、`LICENSE/en`、`LICENSE/ja`、`README.md`、`README_ja.md` など）が無い。全ファイル対象の `snapshot_download(local_files_only=True)` は `IncompleteSnapshotError`（`LocalEntryNotFoundError` の派生）になり、以前のデーモンはオフラインでロードできなかった。`MlxEngine._resolve_snapshot` は `allow_patterns=MODEL_FILE_PATTERNS`（= `DEFAULT_ALLOW_PATTERNS`）で確認するように直した。mlx-lm 自身の `hf_repo_to_path` も同じ理由で同じ指定をしている。
- tokenizer のエンコードは軽い（0.8b の tokenizer で 12,020 トークンのプロンプトに 14 ms）。`count_tokens` と `stream` で2回エンコードしてもキャッシュは要らない。
