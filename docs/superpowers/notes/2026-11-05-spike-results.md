# Spike results (2026-11-05)

Environment: aarch64-darwin, Task 1 devShell (mlx 0.32.4 GPU, mlx-lm 0.32.2, Python 3.14.8). Model `pfnet/plamo-3-translate-2b-MLX-4bit` @ `63777ddc6dc80aacacd92a1bf6966105f2f6270c`. Result: all three checks passed.

## 1. Model load and translation: PASS

- `mx.default_device()` -> `Device(gpu, 0)`.
- Prompt from `apply_chat_template(..., target_lang="en")`:
  `<|plamo:bos|><|plamo:tag|>dataset<|plamo:msg|>translation<|plamo:tag|>input<|plamo:msg|>会議は来週の月曜日に変更されました。<|plamo:tag|>output<|plamo:key|>lang<|plamo:val|>en<|plamo:msg|>`
- Output (temp 0): `The meeting has been rescheduled for next Monday.` (contains "meeting" and "Monday"; no `<|plamo:tag|>` emitted). `finish_reason == "stop"`.
- Speed/memory (cold, single short request): generation 46.3 tok/s, prompt 89.0 tok/s, MLX peak memory 2.52 GB, process max RSS about 3.5 GB (ru_maxrss in bytes: 3,545,858,048).
- First load downloaded 16 files in about 6m52s (network); the cache lives in `~/Library/Application Support/plamo-translate/huggingface/hub`.
- Warning on stderr at load (harmless): `[transformers] You are using a model of type plamo3 to instantiate a model of type ''...`. The daemon should expect this line.
- Load kwargs worked exactly as in the brief: `load(repo, revision=..., trust_remote_code=True, tokenizer_config={"trust_remote_code": True})`, plus `stream_generate(m, t, prompt, max_tokens=..., sampler=make_sampler(temp=0.0))`.

## 2. Template exception type: PASS (jinja2 TemplateError)

All invalid inputs raise exactly `jinja2.exceptions.TemplateError` (not a subclass). Task 5 should catch `jinja2.exceptions.TemplateError`.

MRO: `jinja2.exceptions.TemplateError -> builtins.Exception -> builtins.BaseException -> builtins.object`

| call | message |
|---|---|
| `target_lang="ja", form="bogus"` | `form must be polite or plain and requires target_lang="ja".` |
| `form="bogus"` (no target_lang) | `target_lang is required, e.g. target_lang="ja".` |
| `text="<|plamo:tag|>x", target_lang="en"` | `message content must not contain reserved PFML tokens (<|plamo:...).` |
| `text="<|plamo:tag|>x"` (no target_lang) | same reserved-token message |

Note: `target_lang` is mandatory (missing it also raises TemplateError), so `form` validity must be tested together with `target_lang="ja"`.

## 3. `launch_activate_socket`: PASS

Working ctypes signature (stdlib only, `ctypes.CDLL(None)`; the symbol is in libSystem):

```python
libc = ctypes.CDLL(None)
libc.launch_activate_socket.argtypes = [ctypes.c_char_p, ctypes.POINTER(ctypes.POINTER(ctypes.c_int)), ctypes.POINTER(ctypes.c_size_t)]
libc.launch_activate_socket.restype = ctypes.c_int
fds = ctypes.POINTER(ctypes.c_int)()
cnt = ctypes.c_size_t()
rc = libc.launch_activate_socket(b"Listeners", ctypes.byref(fds), ctypes.byref(cnt))
fd = fds[0]
libc.free(fds)
srv = socket.socket(fileno=fd)
```

Observed: `rc == 0`, `cnt == 1`; accept + send `ok\n` worked and `nc -U` printed `ok`.

plist: Label `local.plamo-translate.spike`, `ProgramArguments` = absolute Nix Python + script, `Sockets.Listeners.SockPathName`, `SockPathMode = 384`, `StandardErrorPath`. No `RunAtLoad`/`KeepAlive`: launchd starts the job on the first connection. Socket created as `srw-------`.

Gotchas:
- The socket file appears slightly after `launchctl bootstrap` returns (a connection attempted immediately failed once with `nc` exit 1 and no message). Tests/clients that bootstrap then connect should retry briefly.
- A path under the session scratch directory was 124 bytes, over the 104-byte `sun_path` limit; the test used `/tmp/pt-spike/` (removed afterwards). Reinforces the length check on the socket path.
- `launchctl bootout gui/$UID/<label>` returned 0; the test job and files were cleaned up.
