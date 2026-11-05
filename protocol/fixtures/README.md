# Protocol fixtures

Shared by the Python daemon tests and the Rust client tests. One scenario per `*.ndjson` file.

Line layout:

1. `{"fake": {...}}` configures the daemon's FakeEngine. Rust readers skip this line.
2. The request: one JSON request line. When the scenario needs raw bytes, `{"raw": "<string>"}` instead; send the string as-is (plus `\n`).
3. Remaining lines: the expected events, in order.

`fake` keys:

- `tokens: [str]`: tokens the engine emits.
- `input_tokens: int`: reported input token count.
- `template_error: str`: the chat template rejects the input with this message.
- `load_error: str`: model load fails with this message.
- `download: bool`: loading goes through a `downloading` progress phase.

The daemon test starts the FakeEngine with the model already loaded unless `download` or `load_error` is set, so only those scenarios include load `progress` events.

Matching: the string `"<any>"` matches any value. Expected events are matched as a subset of the actual event's keys, except keys prefixed with `x_`, which only exist to exercise forward compatibility. Daemon tests ignore `x_` keys in expected events; client tests feed the lines verbatim and must ignore unknown keys.
