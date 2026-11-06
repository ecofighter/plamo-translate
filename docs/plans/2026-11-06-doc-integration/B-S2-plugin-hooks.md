# 付録 B: 設計 S2 (r3) — plamo-ja-bridge の doc 利用と長い応答の不具合

> 本計画書 (../2026-11-06-doc-integration.md) の一部。本文と食い違う箇所は本文が優先する。原文 (英語) のまま収録。SCR は設計時の検証用クローン、BIN は Claude Code 2.1.319 バイナリの strings で、どちらも現存しない ([V] の根拠として残す)。

## 1. Conclusion

### 1.1 Bug: root cause

User facts (authoritative):
1. After the failure, every later reply in the session was English.
2. The `/ja` command was gone. Only `/reload-plugins` or a restart brought translation back; `/ja on` did not.
3. Some paragraphs of the failing reply were missing entirely.

All three fit the plugin being **unloaded by the engine as a runaway**. The chain:

1. **Budget expiry.** Each reply-display dispatch (`classic.MessageDisplay`) awaits the shared `ctx.queue`, a plain promise. That wait counts against the 10 s per-dispatch budget; the dispatch's own `$` calls do not. Past the budget, the hook is absent and `next(e)` answers for it. The same happens to a prompt submitted during the backlog.
   - [V SCR T2]: a dispatch waiting behind two 7 s translations was treated as absent at 10 047 ms, and the raw delta was used.
   - [V SCR T1]: a dispatch's own 9 s `process.run` did not use up its budget.
   - [V SCR P1]: a prompt submitted behind 14 s of display work was dropped with `(timeout)` at 10 003 ms.
2. **The abandoned hook keeps running.** It still waits on the queue and then translates. [V SCR T2, R1]
3. **Overrun.** [V BIN] This happens only when the budget expired:
   - The call site is `let z=E.isExpired(),…;if(h.settled=!0,z&&ue!==void 0)H.abort(new $e(ie)),Vn(ue,e,n)`. The same `isExpired()` gate applies in the `.catch` path (`M=R.isExpired()…if(M&&X!==void 0)Vn(X,e,i)`) and in the stream path (`if(C)x.abort(…),Vn(…)` with `C=H.isExpired()`).
   - `Vn` waits `lingerMs` (5000). If the hook is still running, it logs `hook overran: … still running 5000ms after its budget ran out; ignores its signal` and reports `hasOverrun: true` with effect "counted toward a runaway".
   - Lingering after a user interrupt is only logged (`lingeredMs` in `Xn`, `hasOverrun: false`).
4. **Runaway.** [V BIN]
   - The host counts `hasOverrun` reports per plugin. At `rTs = 2` it records "`${plugin} ignored its signal 2 times in a row: a runaway plugin in the hooks worker`". The counter resets only when a dispatch of the plugin settles without failing (`_Fe`).
   - The worker-death handler `PG` then logs `hooks worker: …; respawning` and rejects every in-flight dispatch (`YNe`).
   - If no in-flight dispatch of the culprit has a `.catch`, it runs `mFe` ("`plamo-ja-bridge was unloaded: it crashed the hooks worker`") and `Y3`, which removes the plugin from the modules the respawn reloads.
   - Result: no hooks and no `/ja` until `/reload-plugins` or a restart. This matches facts 1 and 2.
   - The unload notice goes through `PK`: a dim transcript line or only an error-level debug-log line, depending on a host condition [V code; which branch applied is I].
5. **Missing paragraphs.** Earlier dispatches had returned `''` to hold their text. The abandoned dispatch had already pulled that held text out of the buffer, so it was never shown [V SCR T2]. The worker kill also rejected every other dispatch still in flight [V BIN `YNe`]. This matches fact 3.
6. **Why "in a row" is reached** [I; real latency not measured]:
   - While Claude is streaming, quick dispatches (held-only, fence-only) settle normally and reset the counter.
   - When the reply ends, no further display dispatches arrive. The last translating dispatches, including the final one, are each behind more than 10 s of queue. They expire and report overruns about 5 s later, back to back, with no successful dispatch in between.
   - Trigger: a backlog of at least about 10 s when the reply ends, from a long reply, one slow block (`OUTPUT_TIMEOUT_MS` is 20 s, up to 5 history-halving runs), or a cold model load.
   - [V SCR R1]: two concurrent abandoned dispatches kept running until about 21 s and 28 s. The harness runs hooks on one thread and does not model the kill.
7. **The earlier incident.** register.js:169-170 records "/ja has gone missing mid-session (only /reload-plugins brought it back)". That symptom matches an unload [I]. The `turn.start` re-registration added for it cannot help after an unload, because no hook of the plugin runs any more.

**Fix: required properties**
- **P1. No budget can expire.** Every metered wait (plain promise or timer) goes through `boundedWait(promise, deadlineMs)` with `deadlineMs ≤ next.budget.remainingMs − BUDGET_MARGIN_MS`. Long waits happen only inside the hook's own `$` calls, which are not metered.
- **P2. No hook lingers.** Every await of the hook's own `$` call goes through `untilAborted(promise, next.signal)`, which resolves `ABORTED` as soon as the signal aborts.
  - On `ABORTED` or a deadline, the hook returns at once. A final dispatch first claims everything (see 1.2).
  - The `$` call keeps running in the background, and its late result is ignored or only added to memory.
  - This holds even under the stricter D.TS reading of `lingerMs` (which applies to any abort).
- **P3. Background work outside dispatches.** A display worker is started from `session.start`; D.TS says `session.start` fires for every fresh load [V D.TS 4553-4557].
  - D.TS documents `$` calls from a returned hook in timer callbacks (`$.clock.every(1000, () => $.ui.status("polling"))` [V D.TS 3630-3633]) and a `session.start` spawn loop running past return.
  - Fallback if `$` is unusable after return: the help-run in 1.2 lets a waiting dispatch run the queue head itself.
- **P4. Input is never blocked by display.** `prompt.submit` never awaits display work. While a prompt is being translated, the worker, help-run and takeover start no new calls.
- **P5. `.catch` on `classic.MessageDisplay`.** It is synchronous. It returns the held text instead of losing it and claims the slots so they are not repeated. It is not relied on to prevent an unload (C-1).
- **P6. Recovery.** The enabled flag is in session state [V D.TS 3545-3547], so `/reload-plugins` restores the layer as on. Debug lines (`$.ui.log(…, {to: 'debug'})`) record every deadline hit, help-run, takeover stop and doc fallback. README (S3) says: if `ja⇄en` and `/ja` disappear, run `/reload-plugins`.

### 1.2 Output (reply display) design

**State.** Pure module `lib/pipeline.js`, unit-tested without `$`.
- `MessageState = { buffer, nextSeq, slots[], emitted, takeTail, turnId, failures[], partial }`
- `Slot = { block, state: 'ready'|'queued'|'running'|'done'|'claimed', heavy, text?, failReason? }`
- `Job = { slot, messageId, run($) }` in `ctx.displayQueue` (FIFO), with a mutex `ctx.running` (the job in flight, or null).

**Ordering** (B-1): by **arrival**, not by `e.index`.
- The delta is pushed into the buffer synchronously, before any await. `ctx.enabled` is already cached by `session.start`; if it is undefined, await `loadEnabled`. Awaits of the same promise resume in FIFO order, so arrival order is kept.
- Each dispatch gets `seq = state.nextSeq++` on arrival. Gaps in `e.index` are irrelevant; the existing tests use one global `index++` for all messages.

**Taking turns.**
- Each dispatch creates its `turnDone` promise and chains on the previous one (`takeTail`).
- It resolves `turnDone` in `finally` on **every** exit path (normal, deadline, abandoned, abort), so the next dispatch is never blocked longer than the previous dispatch's own deadline.

**Worker** (P3, B-4):
- `session.start` does `ctx.workerGen += 1` and starts `runDisplayWorker($, ctx, gen)`, a detached loop.
- Each iteration:
  1. Exit if `gen !== ctx.workerGen`.
  2. If `ctx.running === null`, `ctx.inputActive === 0`, `ctx.takeoverActive === 0` and the queue has a non-cancelled, non-heavy job, pop it (synchronously set `ctx.running`). Run `translateBlock($, ctx, slot, {allowDoc: 'fallback'})`, set the slot to `done` with its text and failure reason, and clear `ctx.running`.
  3. Otherwise wait on `ctx.wake`. Enqueue, input end and takeover end resolve it.
- Because the mutex guards every pop, two workers or a worker plus help-runs never overlap.

**Help-run** (B-3 fallback):
- A non-final dispatch about to wait checks the same pop condition. If it holds (the worker is idle, dead or never started), the dispatch pops the head job and runs it with its own `$`: free time, through `untilAborted`.
- It repeats while its slots are unsettled and the condition holds.

**Dispatch (enabled)**
1. Push the delta (synchronously, as above) and create slots.
   - Translatable paragraphs and tables are `queued` and enqueued (wake the worker). Set `heavy` if `utf8Bytes(core) > OUTPUT_DOC_BYTES`; heavy slots are never popped by the worker or by help-run.
   - Everything else is `ready` with `text = block.text`.
2. Deadline: `remainingMs − BUDGET_MARGIN_MS`; for a non-final dispatch, also capped at `STREAM_WAIT_MS`.
3. `boundedWait(prevTurnDone, deadline)`.
4. **Non-final dispatch**:
   - help-run if applicable;
   - `boundedWait` until every slot up to the first heavy or unsettled one has settled;
   - `takeReady`: the longest ready/done prefix, marked `claimed`. Return it (may be `''`). The rest carries forward.
5. **Final dispatch.** Every exit path returns `finalTake` (B-2). Steps:
   - a. Cancel this message's queued jobs and claim their slots for the takeover.
   - b. If a slot of this message is `running`, `boundedWait` for it until the deadline. On a miss, show it as written with reason `間に合わず`. There is no rerun. The late result still goes to memory.
   - c. **Takeover** (`ctx.takeoverActive++`): translate the claimed queued and heavy slots in order with the hook's own `$`, each through `untilAborted`. Each call's `timeoutMs` is `takeoverTimeoutMs(started, now, callMax)` = `min(callMax, OUTPUT_FINAL_MAX_MS − (now − started))`, where `callMax` is `OUTPUT_TIMEOUT_MS` for single calls and `docTimeoutMs(bytes)` for doc; `now` comes from `Date.now()`. Stop and show the rest as written when:
     - the remaining time is under `MIN_CALL_MS` (3000);
     - the signal aborts;
     - `ctx.inputActive > 0` (reason `入力の翻訳を優先`).

     So the takeover lasts at most `OUTPUT_FINAL_MAX_MS` plus the time between calls (B-5).
   - d. `finalTake` returns, in order, every un-emitted slot: translated text, or the original for failed, missed, stopped or unrun slots. It deletes the state and runs `ctx.takeoverActive--` and wake in `finally`.

   A deadline in step 3 or 5b, an abort, or an abandonment never skips `finalTake`. It only turns pending slots into originals.
6. **Abandonment** (B-2, P2):
   - `.catch` sets `abandoned` for this dispatch.
   - A non-final body that sees `abandoned` or an abort after any await returns `{}` without taking. The `.catch` already claimed its slots.
   - A final body that sees it returns `finalTake` immediately, with no further calls.
7. **Notice.** At final, if there are failures or partial results, toast `一部の段落を英語のまま表示しました (${n} 件: ${failures[0]})。` with `timeoutMs: 8000`, plus one debug line per failure.

**`.catch` (P5).** Synchronous.
- Set this dispatch's `abandoned`.
- `claimAll(state)`: every un-emitted slot becomes `claimed` and contributes its original text; its queued jobs are cancelled.
- If `e.final`, append `buffer.takeAll()` and delete the state.
- Toast `表示の翻訳でエラーが起きたため英語のまま表示しました。`

Its own delta is always already in the buffer, because the push is synchronous on arrival.

**`/ja off` mid-message.** Return un-emitted slots' originals, then `buffer.takeAll()` and the delta, and cancel queued jobs.

**`session.end` (clear/resume) and the main `turn.start`.** Clear, or drop states of other turns, and cancel their jobs.

**`translateBlock($, ctx, slot, {allowDoc, timeoutMs})`**
1. If the core is empty or mostly Japanese, return the block as written.
2. A heavy slot (takeover only) uses `runDoc` (en→ja).
3. Otherwise a single call as today (same argv, `OUTPUT_TIMEOUT_MS` or the takeover cap, history halving).
4. If the call fails for a doc-eligible reason and `ctx.docTooOld` is not set, use `runDoc`. Doc-eligible reasons:
   - `finish_reason ≠ stop`;
   - `contextTooLong` with the history used up;
   - `empty translation`;
   - `reserved token in translation`;
   - a table whose shape changed.
5. Other failures show the original and record the reason.
6. doc result handling:
   - `ok`: `lead + text.trim() + trail`; a table must still pass `tableShapeMatches`.
   - `degraded > 0`: show it and count it as partial.
   - Failure: show the original. Keep the single-call reason; the doc reason goes to debug.

### 1.3 Input (prompt) design

1. **Direct runs (P4).**
   - Wrap the translation in `ctx.inputActive++` / `--` (wake in `finally`). Runs use the hook's own `$` through `untilAborted`; abort means drop.
   - Daemon contention (C-2): when a prompt starts, at most one display call is already in flight (≤ `OUTPUT_TIMEOUT_MS` = 20 s single call, or a takeover call capped at its remaining 60 s). It is ahead of the prompt in the serial daemon. Nothing new starts until the prompt is done.
   - The single call's `INPUT_TIMEOUT_MS = 60000` therefore covers the worst ordinary case (20 s plus the prompt). A takeover doc call running when a prompt arrives can still use up the prompt's time; this is accepted and stated in U2.
   - History files come from `ctx.historySlots`: the lowest free n, acquired and released in one `try/finally` around `fs.write` and the run.
     - n = 0: `${TMPDIR}/plamo-ja-bridge-${sid}.jsonl` (the existing path).
     - n ≥ 1: `…-${sid}-${n}.jsonl`.
   - Memory pair order may now interleave input and display pairs out of reply order. This is accepted: `select` takes the most recent pairs either way.
2. **Route.** Use the doc route on the whole `e.text` if all hold:
   - the UTF-8 bytes of the Japanese-containing text segments exceed `INPUT_DOC_BYTES` (3000);
   - `cfg.useDoc` is on;
   - `ctx.docTooOld` is not set.

   Otherwise use the per-segment route, unchanged (`['plamo-translate','--json','--no-stream','-s','ja','-t','en']`, `INPUT_TIMEOUT_MS = 60000`).
3. **Fallback to the doc route.** A segment that fails with `finish_reason ≠ stop`, `contextTooLong` (history used up), `empty translation` or `reserved token` switches to the doc route.
   - A timeout does not.
   - If doc fails, ask today's failure question with the single-call reason.
   - If the binary is too old, toast once per session: `長文の分割翻訳には plamo-translate 0.2.0 以降が必要です。更新してください。`
4. **Doc route.** Show status `ja⇄en 英訳中…`, then restore `STATUS_TEXT`. Call `runDoc({text: e.text, direction: 'ja-en', timeoutMs: docTimeoutMs(utf8Bytes(e.text))})`.
   - ok, `degraded === 0` and `notes === 0`: the normal confirm flow.
   - ok with `degraded > 0` or `notes > 0`: **always ask**, even with `confirm: false`. The question starts with one or both of:
     - `一部 (${degraded}/${pieces} ピース) を英訳できず、日本語のまま残っています。`
     - `${notes} か所 (コード扱いのブロックなど) は訳さずに残しました。`
   - Not ok: `failureQuestion(reason)` with [原文, やめる]. A mid-run stop counts as not ok: S1's report carries piece labels, not source offsets, so the plugin cannot splice the remainder.
5. **Long confirm.**
   - If `english.length ≤ CONFIRM_PREVIEW_CHARS` (1500): today's `confirmQuestion`, unchanged.
   - Otherwise the question is `英訳を確認してください (全 ${n} 文字、中略あり)。\n\n${head 1200}\n\n…(中略 ${m} 文字)…\n\n${tail 300}\n\nこの英訳で送りますか?`, with options [送る, 原文, やめる, `全文を表示`].
   - `全文を表示` writes the English with `$.ui.log` in chunks of ≤ 1900 characters, then asks again with three options.
6. **History.** Pass `--history <slot file>` from `memory.select('ja-en', …)` when it is not empty. Never pass `--history-append` or `--history-last`.
7. **Memory after send.** A doc result adds `{ja: e.text.trim(), en: english.trim()}` only if all hold:
   - no fences;
   - `degraded === 0` and `notes === 0`;
   - `isEligiblePair(…, 'stop')`;
   - `ja.length + en.length ≤ floor(historyChars / 2)`.

   Otherwise nothing is added. The custom-answer path is unchanged.
8. **Format.** Always `--format markdown`. Pasted code in fences is then kept untranslated [V README]. `auto` could treat a fenced prompt as text and translate the code. Indented prose kept as code shows up as notes, which force the confirm.

### 1.4 doc invocation: `lib/doc.js` (S1 r2 contract)

- **`runDoc($, ctx, {text, direction, timeoutMs})`**
  1. If `!cfg.useDoc` or `ctx.docTooOld`, return `{ok: false, unsupported: true}`.
  2. Acquire a history slot and write the history.
  3. Run `$.process.run([translator, ...buildDocArgs(…)], {stdin: text, timeoutMs})` through `untilAborted`. An abort returns `{ok: false, reason: 'interrupted'}`.
  4. Release the slot, then `parseDocRun`. If it reports `tooOld`, set `ctx.docTooOld = true`. `session.start` resets the flag.
- **No probe.** S1 G-12 says a binary without `--report` exits 2 with `unexpected argument '--report'` and translates nothing. The installed 0.1.0 binary (doc but no `--report`) is treated as doc unavailable:
  - G-12 requires passing `--report json` anyway.
  - Parsing 0.1.0's text stderr would be a second parser for a transitional version.
  - The single-call path still works, and the toast tells the user to update.
- **`buildDocArgs({from, to, form, historyFile})`** returns `['doc','--report','json','-s',from,'-t',to,'--format','markdown', ...(form && to==='ja' ? ['--form',form] : []), ...(historyFile ? ['--history',historyFile] : [])]`.
- **`parseDocRun({exitCode, stdout, stderr, isStdoutTruncated, isStderrTruncated})`**, rules applied in order:
  1. Exit 2 with `/unexpected argument '--report'/` → `{ok: false, unsupported: true, tooOld: true}`.
  2. Either stream truncated → fail `'output too large'`.
  3. Split stderr on `\n` and **skip empty or whitespace-only lines** (C-3). Fail with `'no report'` if any remaining line is not a JSON object, if the last line is not `type: 'summary'`, or if `summary.exit_code !== exitCode`. Ignore unknown types and fields.
  4. `complete === false` → fail. The reason comes from the last `error` object: `` `${code}: ${message}` `` when `kind` is `'daemon'`, else `message`, else `'stopped'`.
  5. `complete` with `exit_code ∈ {0,1}` and non-blank stdout → `{ok: true, text: stdout, degraded, pieces, notes: count(type==='note')}`.
  6. Anything else → fail with the first `error.message`, or `` `exit code ${exitCode}` ``.
- **Helpers:**
  - `docTimeoutMs(bytes) = min(600000, 60000 + 20 × bytes)`.
  - `utf8Bytes` uses `TextEncoder` [V SCR E6].
  - `untilAborted(promise, signal)` returns a promise of the value or `ABORTED`. It resolves immediately if the signal is already aborted, and removes its listener.
- **S1 criteria that apply to the plugin:**
  - G-4: stdout is the translation only.
  - G-5: rules 3 and 4 (summary last, missing summary = failure, blank lines skipped).
  - G-7: the summary's exit code is cross-checked against the process exit code.
  - G-12: always `--report json`; the too-old rule; never `doc --help`.

### 1.5 Config

**New userConfig key `useDoc`.** Boolean, default `true`.
- Title: "Use plamo-translate doc for long text".
- Description: "Translate long prompts, and reply blocks a single call cannot handle, with `plamo-translate doc` (needs plamo-translate 0.2.0+)".

**New constants:**

| Constant | Value |
|---|---|
| `STREAM_WAIT_MS` | 2000 |
| `BUDGET_MARGIN_MS` | 2000 |
| `OUTPUT_FINAL_MAX_MS` | 60000 |
| `MIN_CALL_MS` | 3000 |
| `INPUT_DOC_BYTES` | 3000 |
| `OUTPUT_DOC_BYTES` | 6000 |
| `CONFIRM_PREVIEW_CHARS` | 1500 (head 1200, tail 300) |
| `LOG_CHUNK_CHARS` | 1900 |

**Unchanged:** `INPUT_TIMEOUT_MS = 60000`, `OUTPUT_TIMEOUT_MS = 20000`, `HOLD_INCOMPLETE = true`.

**Removed:** `ctx.queue`.

## 2. Rationale

- **The fix targets the engine's actual policy.** Budget expiry plus 5 s of lingering is an overrun; two overruns in a row unload the plugin [V BIN]. P1 removes the expiry and P2 removes the lingering, including after an interrupt. Neither relies on how `.catch` or in-flight state is counted.
- **Long work runs inside the hook's own `$` calls, which cost nothing** [V D.TS HookBudget; SCR T1]. So the final takeover and input do their slow work there, and every other wait is bounded.
- **Pausing for the takeover and input matters:**
  - The takeover is honestly bounded by per-call caps.
  - Pausing the worker during input keeps prompt latency predictable on a serial daemon.
  - Arrival-order sequencing matches what the existing tests specify.
- **The common path is unchanged.** Single calls keep about 0.3 s per block and the existing argv/timeout tests. doc is used only where a single call cannot cope.
- **S1's `summary` gives an unambiguous result and a too-old signal without a probe.**

## 3. Alternatives considered and rejected

| # | Alternative | Why rejected |
|---|---|---|
| A1 | Concurrent daemon requests | Daemon answers `busy` past 8 pending requests [V server.py:29, 275]. |
| A2 | Jobs on the enqueuing dispatch's `$` (r1) | Replaced by the worker plus help-run. |
| A3 | doc over a whole held region | No source offsets after a stop. |
| A4 | Rerunning an in-flight block | It queues behind the stuck request. |
| A5 | `spawn` with a `$.clock` cut | Unresolved metering question; `process.run` timeouts are engine-enforced. |
| A6 | Parsing 0.1.0 stderr | Against G-12. |
| A7 | `--format auto` for prompts | Can translate pasted code. |
| A8 | Ordering by `e.index` | The existing tests use a global counter (B-1). |
| A9 | Relying on `.catch` to keep the plugin loaded | Only dispatches still in flight count (C-1). |

## 4. Assumptions

- [I] In a real session, `{}` displays the raw delta.
- [I] Engine call order is reply order (the "arrival order" used for sequencing).
- [I] `$` calls from a returned hook work in a real session. This is documented for timers. Help-run covers the case where it does not.
- [I] A final dispatch spending up to 60 s in its own `$` calls does not stall the engine (U2).
- [I] doc speed of 20 ms per byte leaves a 2× margin (U3).

## 5. Implications for implementation

### Commit units, in order (tests first)

**Unit 1: `lib/doc.js` (pure); `tests/doc.test.ts`**
1. `buildDocArgs puts --report json and options after doc, --form only for ja, --history only with a file`
2. `parseDocRun accepts a complete run`
3. `parseDocRun accepts a degraded run`: summary `{exit_code: 1, degraded: 1, pieces: 3, complete: true}` → ok, 1, 3.
4. `parseDocRun fails a stopped run with the daemon error`: reason `internal: boom`.
5. `parseDocRun marks a binary without --report as too old`
6. `parseDocRun fails without a summary`: empty stderr, a non-JSON line, and a last line that is not a summary.
7. `parseDocRun fails when the summary exit code differs from the process`
8. `parseDocRun fails a truncated capture`
9. `parseDocRun counts notes`
10. `parseDocRun skips blank lines`: a trailing `\n` and an interior empty line → ok.
11. `docTimeoutMs scales and caps`: 0 → 60000, 3000 → 120000, 10^6 → 600000.
12. `utf8Bytes counts bytes`: `'あa'` → 4.
13. `untilAborted resolves ABORTED at once when the signal aborts, and the value otherwise`

**Unit 2: `lib/pipeline.js` (pure); `tests/pipeline.test.ts`**

14. `deltas are pushed in arrival order and index gaps do not stall`
15. `takeReady returns the longest ready prefix once`
16. `claimAll returns originals and later takes return nothing`
17. `an abandoned dispatch takes nothing`
18. `finalTake returns every un-emitted slot, originals for unsettled ones`
19. `cancelQueued removes only this message's queued jobs`
20. `popIfIdle pops only when nothing runs and no input or takeover is active`
21. `takeoverTimeoutMs caps each call at the remaining takeover time and stops below MIN_CALL_MS`

**Unit 3: `input.js`**

22. `confirmPreview keeps short text and elides long text`

**Unit 4: `config.js` and `plugin.json`**

23. `useDoc defaults to true`
24. `useDoc false is kept, non-boolean falls back`

Existing test change: `config.test.ts` `DEFAULTS` gains `useDoc: true` (new key).

**Unit 5: `register.js`.** Covers the worker, help-run, history slots, the display pipeline, `.catch`, and input off the queue. Tests use their own setup with `mock.clock`. Slow fakes are `on('process.run', async ($, e) => { await clock.sleep(30000); return { value: … } })`, which works [V SCR C4: still pending at 29000, resolved at 30000].

25. `a slow block holds later batches instead of letting them through untranslated`:
    - `'Slow.\n\n'` → advance 2000 → `''`.
    - `'Next.\n\n'` → advance 2000 → `''`.
    - Advance 30000, then final `''` → `'JA:Slow.\n\nJA:Next.\n\n'`.
    - No `{}` anywhere.
26. `the final batch translates queued blocks itself`: runs are `['Slow.', 'A.', 'B.']` (m0 in flight; m1 `A.` queued; m1 final `B.`).
27. `the final batch shows an in-flight block that misses its deadline as written, with a notice`: advance 8000 → `'Slow.\n\n'`, one toast containing `英語のまま`, `Slow.` ran once.
28. `a failed block is shown as written with a notice`
29. `disabling mid-message shows pending blocks as written`
30. `a new turn drops an unfinished message's queued blocks`
31. `a repeated session start keeps one worker`: two `session.start` calls, then concurrent batches → events `['write','run','write','run']`.
32. `a prompt does not wait behind display translations`: resolves while `Slow.` is pending; history path `/tmp/plamo-ja-bridge-sess1-1.jsonl`.
33. `a prompt translation pauses the display worker`: the prompt run sleeps on the mock clock; a display block queued meanwhile does not start until the prompt run returns.

**Unit 6: doc on input (`prompt.test.ts`).** The fake branches on `argv[1] === 'doc'`.

34. `a long prompt goes to doc`:
    - argv `['plamo-translate','doc','--report','json','-s','ja','-t','en','--format','markdown']`;
    - `stdin === text`;
    - `timeoutMs === docTimeoutMs(bytes)`;
    - the question contains `中略`.
35. `doc gets --history from memory, never --history-append`
36. `a truncated translation falls back to doc`
37. `a partial doc translation always asks, even with confirm off`
38. `doc stopping mid-run asks to send the original with the single-call reason`
39. `an old CLI disables doc for the session`: one doc attempt in total and one toast.
40. `a small clean doc result is remembered, a large one is not`
41. `useDoc false keeps the per-segment path`
42. `the confirm dialog offers the full text for a long translation`

Existing test change: `context_too_long without history fails once`.
- `expect(h.runs.length).toBe(1)` becomes `toBe(2)`, and the test is renamed `context_too_long without history fails once, then once with doc`.
- Reason: a deliberate doc fallback. The fake's doc reply has no summary, so it fails, and the question keeps the single-call reason.

**Unit 7: doc on output (`message-display.test.ts`)**

43. `a truncated block falls back to doc`
44. `an oversize block is translated with doc by the final batch`: no single call; argv `['plamo-translate','doc','--report','json','-s','en','-t','ja','--format','markdown']`; `timeoutMs ≤ 60000`.
45. `degraded doc output is shown and noted`
46. `a table whose shape changed is retried with doc`

**Unit 8.** Bump `plugin.json` 0.1.1 → 0.2.0, and hand the README facts to S3.

### Existing tests that must pass unchanged

These tests are the specification. If one fails, the implementation is wrong. Expected behaviour:

| Test | Why it still passes |
|---|---|
| `prompt.test.ts:137-138` | Single-call path unchanged. |
| `message-display.test.ts:105` | Single-call path unchanged. |
| `interleaved messages keep separate buffers` and `final flushes and clears` | Arrival order. |
| `concurrent batches translate one at a time` | Mutex. |
| `history write failure shows original` | The worker catches the `fs.write` rejection and marks the slot failed; the non-final dispatch receives it before its wait ends. Without a mock clock the deadline never fires, and the fake settles at once. |
| `table with a changed shape shows the original` | The doc retry fails for lack of a summary. |
| command tests with a repeated `session.start` | The worker generation. |

### README facts for S3

- Remove "和訳が終わる前に次のプロンプトを送ると…英語の原文で表示".
- Add:
  - The tail of a long reply can appear up to about 60 s after it ends.
  - Blocks that could not be translated are shown in English with a toast.
  - Prompts take priority over reply display.
  - Long prompts (about 1000 Japanese characters or more) and failed translations use `plamo-translate doc`, which needs 0.2.0 or later. Older versions fall back, with a one-time toast.
  - Long English text in the confirm dialog is elided, with a `全文を表示` option.
  - A partial translation is always confirmed.
  - The `useDoc` row.
  - If `ja⇄en` and `/ja` disappear mid-session, run `/reload-plugins`, and please report it with the debug log.

### Completion criteria

Base commit: `64a5b1b`. Output format verified in SCR: lines ` N pass`, ` 0 fail`, `Ran N tests across M files.`

| Command | Expected result |
|---|---|
| `claude plugin test /Users/arakaki/Projects/plamo-translate/claude-plugin` | Contains ` 144 pass`, ` 0 fail` and `Ran 144 tests across 11 files.` (98 + 46; new file `pipeline.test.ts` and `doc.test.ts`, nine existing files) |
| `git -C /Users/arakaki/Projects/plamo-translate diff 64a5b1b -U0 -- claude-plugin/tests/message-display.test.ts claude-plugin/tests/config.test.ts claude-plugin/tests/command.test.ts \| grep -E '^-[^-]'` | No output |
| `git -C /Users/arakaki/Projects/plamo-translate diff 64a5b1b -U0 -- claude-plugin/tests/prompt.test.ts \| grep -E '^-[^-]'` | Exactly the old test-name line and `expect(h.runs.length).toBe(1)` |
| `grep -c "ctx.queue" /Users/arakaki/Projects/plamo-translate/claude-plugin/hooks/register.js` | `0` |
| `grep -c -- "--history-append\|doc --help" /Users/arakaki/Projects/plamo-translate/claude-plugin/hooks/lib/doc.js` | `0` |
| `grep -c "'--report', 'json'" /Users/arakaki/Projects/plamo-translate/claude-plugin/hooks/lib/doc.js` | `1` |

The file count is 11: 9 existing test files plus `doc.test.ts` and `pipeline.test.ts`.

### Manual reproduction (the harness cannot show an unload)

Setup:
1. Run `claude --debug-file /tmp/cc-debug.log`.
2. Run `/ja on`.
3. Run `plamo-translate daemon restart`.
4. Ask for a long reply, for example `40 段落と表を含めて詳しく説明して`.

Before the fix (0.1.1), expect:
- (a) `ja⇄en` disappears;
- (b) `/ja` is an unknown command;
- (c) `/plugin` shows plamo-ja-bridge not loaded or in error;
- (d) `grep -E "hook overran: .*plamo-ja-bridge|plamo-ja-bridge ignored its signal|respawning|plamo-ja-bridge was unloaded" /tmp/cc-debug.log` matches;
- (e) English until `/reload-plugins`.

After the fix, expect:
- (d) matches nothing;
- `/ja` and `ja⇄en` remain;
- the reply is in Japanese, or English blocks appear with a toast;
- the plugin's debug lines show deadlines and takeovers.

Also press Esc during a long reply's final translation, then repeat the grep in (d): it must still match nothing.

## 6. Unresolved items, with defaults

| # | Question | Default |
|---|---|---|
| U1 | Do `$` calls from a returned `session.start` hook work in a real session? Documented for timers [D.TS]. | Help-run covers the failure case. The manual repro shows whether text appears before the final batch. |
| U2 | Does a final dispatch spending up to 60 s in its own `$` calls stall the engine? A prompt that arrives during a takeover doc call can wait behind it at the daemon. | 60 s; lower to 20 s if the manual run shows a stall. |
| U3 | Real doc latency, `INPUT_DOC_BYTES`, and 20 ms per byte. | 3000 bytes, 20 ms per byte. |
| U4 | Does `$.ui.status` repaint during `prompt.submit`? | Set it anyway. |
| U5 | Does the unload notice reach the transcript? | Host-dependent; no design impact. |

## 7. Additional checks

All [V].

**SCR experiments**
- T1, T2, P1, R1, E3, E4, E6, E7: as in r2.
- C4: a `process.run` fake that awaits `mock.clock.sleep(30000)` is still pending at 29000 and resolves at 30000. The run's summary lines read ` 99 pass` / ` 0 fail` / `Ran 99 tests across 10 files.`.

**Test files**
- message-display.test.ts uses a module-global `let index = 0` with `index++` across messages and tests.
- command.test.ts calls `session.start` twice in one test (lines 54-73).

**BIN**
- The three `isExpired()`-gated `Vn` call sites quoted in 1.1.
- `Vn`, `hook_failed`, `rTs = 2`, `_Fe`, `PG`/`mFe`/`Y3`/`YNe`, `pht`.
- `P6n` filters in-flight dispatches whose `caughtBy` includes the plugin. This is the basis for C-1.

**D.TS**
- `next.signal` is an `AbortSignal` (5325).
- Timer callbacks call `$` (3630-3633).
- `session.start` refires per fresh load (4553-4557).

