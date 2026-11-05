# Protocol fixtures

Shared by the Python daemon tests and the Rust client tests. One scenario per `*.ndjson` file.

Line layout:

1. `{"fake": {...}}` configures the daemon's FakeEngine. Rust readers skip this line.
2. The request: one JSON request line. When the scenario needs raw bytes, `{"raw": "<string>"}` instead; send the string as-is (plus `\n`).
3. Remaining lines: the expected events, in order.

`fake` keys:

- `tokens: [str]`: tokens the engine emits.
- `input_tokens: int`: reported input token count. It does not depend on the request, so `error_context_too_long_history.ndjson` shows only that a request with history reaches the token-budget check; that history counts toward the budget is tested by the daemon's server unit tests.
- `load_error: str`: model load fails with this message.
- `download: bool`: loading goes through a `downloading` progress phase.

The FakeEngine ends every uninterrupted generation with `finish_reason` `"stop"` when `len(tokens) < max_tokens`, `"length"` otherwise (the same rule as mlx-lm, whose eos token is not part of `tokens`). It has no validation role: semantic `invalid_request` errors come from the daemon's validator, which runs before the request is queued or the model is loaded.

The daemon test starts the FakeEngine with the model already loaded unless `download` or `load_error` is set, so only those scenarios include load `progress` events.

Every fixture whose request carries a `history` array states `"stream"` explicitly: the Rust client always serializes `stream`, and the client tests compare fixture requests with its serialization.

Matching: the string `"<any>"` matches any value. Expected events are matched as a subset of the actual event's keys, except keys prefixed with `x_`, which only exist to exercise forward compatibility. Daemon tests ignore `x_` keys in expected events; client tests feed the lines verbatim and must ignore unknown keys.

Event fields that fixtures assert where they matter:

- `done.finish_reason` (`"stop"` | `"length"`): present on the `done` of a `translate` request, absent on the `done` of `load` (`translate_history.ndjson`, `load_with_download.ndjson`).
- `status.features`: request-side capabilities; currently `["history"]` (`status.ndjson`).
- `status.template_verified` (bool): whether the loaded snapshot's chat template is the one the daemon's PFML3 builder was verified against; omitted while the model is not loaded. Not asserted by fixtures.
