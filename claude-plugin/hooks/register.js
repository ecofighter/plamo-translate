import { resolveConfig } from './lib/config.js'
import { createMemory, dropOldestHalf, isEligiblePair, toJsonl } from './lib/memory.js'
import { buildArgs, parseResult } from './lib/cli.js'
import { splitEdges, splitFences } from './lib/segments.js'
import {
  CONFIRM_PREVIEW_CHARS,
  confirmPreview,
  containsJapanese,
  decideSubmission,
  isMostlyJapanese,
  isPassthroughCommand,
  isTypedByUser,
  LABELS,
  withFences,
} from './lib/input.js'
import { tableShapeMatches } from './lib/display.js'
import { ABORTED, buildDocArgs, docTimeoutMs, parseDocRun, untilAborted, utf8Bytes } from './lib/doc.js'
import {
  abandon,
  ABORTED_REASON,
  arrive,
  beginInput,
  beginTakeover,
  boundedWait,
  BUDGET_MARGIN_MS,
  cancelQueued,
  createPipeline,
  DEADLINE,
  dropMessages,
  endInput,
  endTakeover,
  finalTake,
  findDispatch,
  hasUnsettled,
  MISSED_REASON,
  popIfIdle,
  release,
  rescue,
  runJob,
  runningSlot,
  settle,
  takeoverTimeoutMs,
  takeReady,
  waitBudgetMs,
  waitForWork,
  wake,
  whenSettled,
} from './lib/pipeline.js'

const STATUS_TEXT = 'ja⇄en'
const DOC_STATUS_TEXT = 'ja⇄en 英訳中…'
const INPUT_TIMEOUT_MS = 60000
const INPUT_DOC_BYTES = 3000
const LOG_CHUNK_CHARS = 1900
const SHOW_FULL = '全文を表示'
const DOC_TOO_OLD_NOTICE = '長文の分割翻訳には plamo-translate 0.2.0 以降が必要です。更新してください。'
// Failures a run split into pieces can get past; a timeout is not one, since doc would only take longer.
const DOC_RETRY_REASONS = new Set(['empty translation', 'reserved token in translation'])
const OUTPUT_TIMEOUT_MS = 20000
// Half a streaming batch's wait. The takeover's 3 s floor would rule out every help-run, since a
// streaming batch waits at most 2 s; under 1 s a single call seldom ends before the batch returns.
const HELP_RUN_MIN_MS = 1000
const NOTICE_TIMEOUT_MS = 8000
const DROP_REASON = '送信をやめました。'
const INPUT_FIRST_REASON = '入力の翻訳を優先'
const TABLE_SHAPE_REASON = 'table shape changed'
const PARTIAL_REASON = 'partial'
const RESCUE_NOTICE = '表示の翻訳でエラーが起きたため英語のまま表示しました。'
// The engine replaces the plugin's copy mid-session (a hooks worker respawn) and runs register and
// session.start again; the switch lives in session state so that does not turn the layer off.
const ENABLED = { plugin: 'plamo-ja-bridge', key: 'enabled' }
const COMMAND = {
  name: 'ja',
  description: '日英レイヤ (plamo-translate) のオン・オフ',
  argumentHint: '[on|off|status]',
}

const partialNotice = (degraded, pieces) =>
  `一部 (${degraded}/${pieces} ピース) は英訳しきれていません (日本語のまま、または訳が途中で切れています)。`
const notesNotice = (notes) => `${notes} か所 (コード扱いのブロックなど) は訳さずに残しました。`
const failureQuestion = (reason) => `翻訳に失敗しました (${reason})。どうしますか?`
const crashReason = (kind) => `日英レイヤでエラーが起きたため送信を止めました (${kind})。/ja off で無効にできます。`
const failureNotice = (n, reason) => `一部の段落を英語のまま表示しました (${n} 件: ${reason})。`

const NEVER = new Promise(() => {})

async function loadEnabled($, ctx) {
  const { value } = await $.state.get(ENABLED)
  ctx.enabled = value ?? ctx.cfg.enabledByDefault
  return ctx.enabled
}

// Display dispatches that find the switch unread share one read, so they resume in arrival order.
function loadEnabledOnce($, ctx) {
  ctx.enabledLoad ??= loadEnabled($, ctx).finally(() => {
    ctx.enabledLoad = undefined
  })
  return ctx.enabledLoad
}

async function isEnabled($, ctx) {
  return ctx.enabled ?? (await loadEnabled($, ctx))
}

async function setEnabled($, ctx, value) {
  ctx.enabled = value
  await $.state.set(ENABLED, value)
}

function debug($, text) {
  $.ui.log(text, { to: 'debug' })
}

// For code outside any dispatch, where a throw would end the display worker or turn a wait that
// should never fire into an expired one: losing a diagnostic line is the lesser harm.
function debugQuietly($, text) {
  try {
    debug($, text)
  } catch {}
}

// Translations of input and display can now run at the same time, so each gets a history file of
// its own; slot 0 keeps the file name used before.
function acquireHistorySlot(ctx) {
  let n = 0
  while (ctx.historySlots.has(n)) n++
  ctx.historySlots.add(n)
  return n
}

async function historyPath($, slot) {
  const tmp = ((await $.env.get('TMPDIR')) ?? '/tmp').replace(/\/$/, '')
  const suffix = slot === 0 ? '' : `-${slot}`
  return `${tmp}/plamo-ja-bridge-${await $.session.id()}${suffix}.jsonl`
}

const fixedTimeout = (timeoutMs) => () => ({ timeoutMs })

// Runs one translation, shrinking the history on context_too_long the way the MCP server does.
// `nextCall()` is asked before every daemon call, reruns included: it gives (or resolves to)
// `{ timeoutMs }` for the call, or `{ stopReason }` to start no further call.
async function translateNow($, ctx, text, direction, nextCall) {
  const [from, to] = direction === 'ja-en' ? ['ja', 'en'] : ['en', 'ja']
  let history = ctx.memory.select(direction, { maxPairs: ctx.cfg.historyPairs, maxChars: ctx.cfg.historyChars })
  for (;;) {
    const { timeoutMs, stopReason } = await nextCall()
    if (stopReason !== undefined) return { ok: false, reason: stopReason, contextTooLong: false, stopped: true }
    const slot = history.length > 0 ? acquireHistorySlot(ctx) : undefined
    let run
    try {
      let historyFile
      if (slot !== undefined) {
        historyFile = await historyPath($, slot)
        await $.fs.write(historyFile, toJsonl(history))
      }
      try {
        run = await $.process.run([ctx.cfg.translator, ...buildArgs({ from, to, form: ctx.cfg.form, historyFile })], {
          stdin: text,
          timeoutMs,
        })
      } catch (err) {
        return { ok: false, reason: err?.message || 'timeout', contextTooLong: false }
      }
    } finally {
      if (slot !== undefined) ctx.historySlots.delete(slot)
    }
    const result = parseResult(run)
    if (result.ok || !result.contextTooLong || history.length === 0) return result
    history = dropOldestHalf(history)
  }
}

// The display side starts nothing new while a prompt translates: the daemon serves one request at a
// time, and the prompt is what the user is waiting on.
function translateInput($, ctx, text, signal) {
  beginInput(ctx.display)
  const run = translateNow($, ctx, text, 'ja-en', fixedTimeout(INPUT_TIMEOUT_MS)).finally(() => endInput(ctx.display))
  return untilAborted(run, signal)
}

const docAvailable = (ctx) => ctx.cfg.useDoc && !ctx.docTooOld

// Settles when the process ends, even after the caller stopped waiting: the history file stays claimed
// while the daemon may still read it.
async function execDoc($, ctx, { text, direction, timeoutMs }) {
  const [from, to] = direction === 'ja-en' ? ['ja', 'en'] : ['en', 'ja']
  const history = ctx.memory.select(direction, { maxPairs: ctx.cfg.historyPairs, maxChars: ctx.cfg.historyChars })
  const slot = history.length > 0 ? acquireHistorySlot(ctx) : undefined
  let run
  try {
    let historyFile
    if (slot !== undefined) {
      historyFile = await historyPath($, slot)
      await $.fs.write(historyFile, toJsonl(history))
    }
    try {
      run = await $.process.run([ctx.cfg.translator, ...buildDocArgs({ from, to, form: ctx.cfg.form, historyFile })], {
        stdin: text,
        timeoutMs,
      })
    } catch (err) {
      return { ok: false, reason: err?.message || 'timeout' }
    }
  } finally {
    if (slot !== undefined) ctx.historySlots.delete(slot)
  }
  const result = parseDocRun(run)
  if (result.tooOld) {
    // Both directions may find out at once; the user needs to hear it only once.
    if (!ctx.docTooOld) $.ui.toast(DOC_TOO_OLD_NOTICE)
    ctx.docTooOld = true
  }
  return result
}

// `pauseDisplay` holds new display work while the prompt waits on the run. An abort resumes the
// display at once rather than when the process ends, which can take up to ten minutes; the history
// file stays claimed until then, and other runs use other files.
function runDoc($, ctx, { text, direction, timeoutMs, pauseDisplay = false }, signal) {
  if (!docAvailable(ctx)) return Promise.resolve({ ok: false, unsupported: true })
  let paused = pauseDisplay
  const resumeDisplay = () => {
    if (!paused) return
    paused = false
    endInput(ctx.display)
  }
  if (paused) beginInput(ctx.display)
  const run = execDoc($, ctx, { text, direction, timeoutMs }).finally(resumeDisplay)
  // The display worker has no dispatch, so nothing can abort its run.
  if (!signal) return run
  return untilAborted(run, signal).then((r) => {
    if (r !== ABORTED) return r
    resumeDisplay()
    return { ok: false, reason: 'interrupted' }
  })
}

async function translateInputDoc($, ctx, text, signal) {
  $.ui.status(DOC_STATUS_TEXT)
  try {
    const timeoutMs = docTimeoutMs(utf8Bytes(text))
    return await runDoc($, ctx, { text, direction: 'ja-en', timeoutMs, pauseDisplay: true }, signal)
  } finally {
    $.ui.status(ctx.enabled ? STATUS_TEXT : undefined)
  }
}

async function ask($, question, options, signal) {
  try {
    const answer = await untilAborted($.ui.ask(question, options), signal)
    return answer === ABORTED ? null : answer
  } catch {
    return null
  }
}

// The host draws about 2000 characters of a log line; a cut never splits a surrogate pair.
function logChunks($, text) {
  for (let i = 0; i < text.length; ) {
    let end = Math.min(i + LOG_CHUNK_CHARS, text.length)
    if (end < text.length && /[\ud800-\udbff]/.test(text[end - 1])) end -= 1
    $.ui.log(text.slice(i, end))
    i = end
  }
}

async function confirmEnglish($, english, notices, signal) {
  const question = (notices.length > 0 ? notices.join('\n') + '\n\n' : '') + confirmPreview(english)
  const choices = [LABELS.send, LABELS.original, LABELS.cancel]
  if (english.length <= CONFIRM_PREVIEW_CHARS) return ask($, question, choices, signal)
  const answer = await ask($, question, [...choices, SHOW_FULL], signal)
  if (answer !== SHOW_FULL) return answer
  logChunks($, english)
  return ask($, question, choices, signal)
}

async function askFailure($, e, next, reason) {
  const answer = await ask($, failureQuestion(reason), [LABELS.original, LABELS.cancel], next.signal)
  return answer === LABELS.original ? next(e) : { drop: DROP_REASON }
}

function send(ctx, e, next, segments, english, answer, rememberTranslation) {
  const decision = decideSubmission(answer)
  if (decision === 'drop') return { drop: DROP_REASON }
  if (decision === 'send-original') return next(e)
  if (decision === 'send-translation') {
    rememberTranslation()
    return next({ ...e, text: english })
  }
  const hasFence = segments.some((s) => s.kind === 'fence')
  const ja = e.text.trim()
  const en = decision.custom.trim()
  if (!hasFence && isEligiblePair(ja, en, 'stop')) ctx.memory.add({ ja, en })
  return next({ ...e, text: withFences(decision.custom, segments) })
}

// A doc run that stopped part way is a failure: its report names pieces, not source offsets, so the
// rest cannot be joined on. `failReason` is what the question shows when doc gives no translation.
async function sendDocResult($, ctx, e, next, segments, doc, failReason) {
  if (!doc.ok) return askFailure($, e, next, failReason)
  const english = doc.text
  const notices = []
  if (doc.degraded > 0) notices.push(partialNotice(doc.degraded, doc.pieces))
  if (doc.notes > 0) notices.push(notesNotice(doc.notes))
  const answer =
    ctx.cfg.confirm || notices.length > 0 ? await confirmEnglish($, english, notices, next.signal) : LABELS.send
  return send(ctx, e, next, segments, english, answer, () => {
    const ja = e.text.trim()
    const en = english.trim()
    const clean = notices.length === 0 && !segments.some((s) => s.kind === 'fence')
    const small = ja.length + en.length <= Math.floor(ctx.cfg.historyChars / 2)
    if (clean && small && isEligiblePair(ja, en, 'stop')) ctx.memory.add({ ja, en })
  })
}

const worthDoc = (result) =>
  result.contextTooLong || result.reason.startsWith('finish_reason ') || DOC_RETRY_REASONS.has(result.reason)

async function submitJapanese($, ctx, e, next) {
  const segments = splitFences(e.text)
  const needsTranslation = (s) => s.kind === 'text' && containsJapanese(s.text)
  if (!segments.some(needsTranslation)) return next(e)

  const japaneseBytes = segments.filter(needsTranslation).reduce((n, s) => n + utf8Bytes(s.text), 0)
  if (japaneseBytes > INPUT_DOC_BYTES && docAvailable(ctx)) {
    debug($, `input: ${japaneseBytes} bytes of Japanese; translating the prompt with doc`)
    const doc = await translateInputDoc($, ctx, e.text, next.signal)
    if (next.signal.aborted) return { drop: DROP_REASON }
    // An old CLI leaves the single calls, which still translate a long prompt piece by piece.
    if (!doc.unsupported) return sendDocResult($, ctx, e, next, segments, doc, doc.reason)
    debug($, 'input: doc is unavailable; translating the long prompt piece by piece')
  }

  const pairs = []
  let english = ''
  for (const s of segments) {
    if (!needsTranslation(s)) {
      english += s.text
      continue
    }
    const { lead, core, trail } = splitEdges(s.text)
    const result = await translateInput($, ctx, core, next.signal)
    if (result === ABORTED) return { drop: DROP_REASON }
    if (!result.ok) {
      if (!worthDoc(result) || !docAvailable(ctx)) return askFailure($, e, next, result.reason)
      debug($, `input: single call failed (${result.reason}); translating the prompt with doc`)
      const doc = await translateInputDoc($, ctx, e.text, next.signal)
      if (next.signal.aborted) return { drop: DROP_REASON }
      return sendDocResult($, ctx, e, next, segments, doc, result.reason)
    }
    const translation = result.text.trim()
    pairs.push({ ja: core, en: translation, finishReason: result.finishReason })
    english += lead + translation + trail
  }

  const answer = ctx.cfg.confirm ? await confirmEnglish($, english, [], next.signal) : LABELS.send
  return send(ctx, e, next, segments, english, answer, () => {
    for (const p of pairs) if (isEligiblePair(p.ja, p.en, p.finishReason)) ctx.memory.add({ ja: p.ja, en: p.en })
  })
}

// Returns null when doc turned out to be unavailable, so the caller decides what to show.
// `singleReason` is what a failed doc run reports, when a single call failed first.
async function translateBlockWithDoc($, ctx, block, { lead, core, trail }, { nextCall, signal }, singleReason) {
  const { timeoutMs, stopReason } = await nextCall(docTimeoutMs(utf8Bytes(core)))
  if (stopReason !== undefined) return { text: block.text, failReason: stopReason, stopped: true }
  const doc = await runDoc($, ctx, { text: core, direction: 'en-ja', timeoutMs }, signal)
  if (doc.unsupported) return null
  if (!doc.ok) {
    debug($, `display: doc failed (${doc.reason}); block shown as written`)
    return { text: block.text, failReason: singleReason ?? doc.reason }
  }
  const ja = doc.text.trim()
  if (block.kind === 'table' && !tableShapeMatches(core, ja)) {
    return { text: block.text, failReason: singleReason ?? TABLE_SHAPE_REASON }
  }
  const partial = (doc.degraded ?? 0) > 0
  const small = core.length + ja.length <= Math.floor(ctx.cfg.historyChars / 2)
  if (!partial && doc.notes === 0 && small && isEligiblePair(core, ja, 'stop')) ctx.memory.add({ ja, en: core })
  return { text: lead + ja + trail, partial }
}

// A block that cannot be translated is shown as written: the display is the only thing at stake.
// The pair goes to memory even when the result comes too late to be shown.
// `nextCall(callMax)` gives each daemon call its timeout, at most `callMax`, or a reason to stop.
async function translateBlock($, ctx, slot, { nextCall, signal }) {
  const { block } = slot
  const edges = splitEdges(block.text)
  const { lead, core, trail } = edges
  if (core === '' || isMostlyJapanese(core)) return { text: block.text }
  const calls = { nextCall, signal }

  if (slot.heavy && docAvailable(ctx)) {
    debug($, `display: ${utf8Bytes(core)} bytes in one block; translating it with doc`)
    const doc = await translateBlockWithDoc($, ctx, block, edges, calls, undefined)
    if (doc) return doc
    debug($, 'display: doc is unavailable; translating the long block with a single call')
  }

  const result = await translateNow($, ctx, core, 'en-ja', () => nextCall(OUTPUT_TIMEOUT_MS))
  if (result.stopped) return { text: block.text, failReason: result.reason, stopped: true }
  let failReason
  if (!result.ok) {
    if (!worthDoc(result)) return { text: block.text, failReason: result.reason }
    failReason = result.reason
  } else {
    const ja = result.text.trim()
    if (block.kind !== 'table' || tableShapeMatches(core, ja)) {
      if (isEligiblePair(core, ja, result.finishReason)) ctx.memory.add({ ja, en: core })
      return { text: lead + ja + trail }
    }
    failReason = TABLE_SHAPE_REASON
  }
  if (!docAvailable(ctx)) return { text: block.text, failReason }
  debug($, `display: single call failed (${failReason}); translating the block with doc`)
  return (await translateBlockWithDoc($, ctx, block, edges, calls, failReason)) ?? { text: block.text, failReason }
}

// Outside a takeover a call is bounded only by its own kind's limit.
const uncapped = (callMax) => ({ timeoutMs: callMax })

const translateJob = ($, ctx) => (slot) => translateBlock($, ctx, slot, { nextCall: uncapped })

// Display translations run here, outside any dispatch, so no dispatch waits on them past its budget.
// A newer session.start bumps the generation, and the older loop ends at its next turn.
// Failures get their own debug line: a host that refuses `$` calls from a returned session.start
// hook shows up as every worker job failing.
async function runDisplayWorker($, ctx, generation) {
  const pipe = ctx.display
  while (generation === ctx.workerGen) {
    const job = popIfIdle(pipe)
    if (!job) {
      await waitForWork(pipe)
      continue
    }
    const result = await runJob(pipe, job, translateJob($, ctx))
    if (result.failReason !== undefined) debugQuietly($, `plamo-ja-bridge: worker job failed: ${result.failReason}`)
  }
  debugQuietly($, 'display worker superseded by a newer session start')
}

function startDisplayWorker($, ctx) {
  ctx.workerGen += 1
  wake(ctx.display)
  runDisplayWorker($, ctx, ctx.workerGen).catch(() => {})
}

// A host that cannot time a wait leaves it to its promise and the signal; reading that as an expired
// deadline would show every block untranslated. The first such refusal in a session is logged, since
// it also covers a host cancelling the wait.
function hostSleep($, ctx, ms, options) {
  return $.clock.sleep(ms, options).catch((err) => {
    if (!options?.signal?.aborted && !ctx.sleepRefusalLogged) {
      ctx.sleepRefusalLogged = true
      debugQuietly($, `display: the host clock refused a wait (${err?.message || 'error'}); waits now end only with their work or the signal`)
    }
    return NEVER
  })
}

function clockNow($, signal) {
  return untilAborted($.clock.now().catch(() => Date.now()), signal)
}

// One deadline per dispatch, on the host clock, shared by all of its waits; each wait is also held
// under the budget the engine meters, read fresh, since that is what decides whether the hook is
// treated as absent.
async function dispatchDeadline($, next, final) {
  const now = await clockNow($, next.signal)
  if (now === ABORTED) return ABORTED
  return now + waitBudgetMs(next.budget.remainingMs, final)
}

async function waitMs($, next, deadline) {
  const now = await clockNow($, next.signal)
  if (now === ABORTED) return ABORTED
  return Math.min(deadline - now, next.budget.remainingMs - BUDGET_MARGIN_MS)
}

function waitFor($, ctx, next, ms, promise) {
  return boundedWait(promise, ms, (sleepMs, options) => hostSleep($, ctx, sleepMs, options), next.signal)
}

async function waitUntil($, ctx, next, deadline, promise) {
  const ms = await waitMs($, next, deadline)
  if (ms === ABORTED) return ABORTED
  return waitFor($, ctx, next, ms, promise)
}

// Once a dispatch has shown text, an earlier one of the same message shows nothing more: the engine
// places each batch where its own delta was, so later text from it would appear out of order.
function overtaken(ctx, d) {
  return (ctx.shownSeq.get(d.state) ?? -1) > d.seq
}

function markShown(ctx, d) {
  ctx.shownSeq.set(d.state, Math.max(ctx.shownSeq.get(d.state) ?? -1, d.seq))
}

async function showBatch($, ctx, d, next) {
  const pipe = ctx.display
  const deadline = await dispatchDeadline($, next, false)
  if (deadline === ABORTED) return {}
  const previous = await waitUntil($, ctx, next, deadline, d.prevTurn)
  if (previous === ABORTED || d.abandoned) return {}
  if (previous === DEADLINE) debug($, 'display: an earlier batch of the message was still busy at the deadline')

  const heldBack = 'display: deadline reached; untranslated blocks are held for a later batch'
  while (hasUnsettled(d)) {
    // Checked before popping: a help-run started with too little time left would only run on after
    // return, keeping the daemon busy for a block this batch can no longer show.
    const ms = await waitMs($, next, deadline)
    if (ms === ABORTED || next.signal.aborted || d.abandoned) return {}
    if (ms <= 0) {
      debug($, heldBack)
      break
    }
    const job = ms >= HELP_RUN_MIN_MS ? popIfIdle(pipe) : null
    if (job) debug($, 'display: help-run, translating the next queued block in the batch')
    const waited = await waitFor(
      $,
      ctx,
      next,
      ms,
      job ? runJob(pipe, job, translateJob($, ctx)) : Promise.race([whenSettled(d), waitForWork(pipe)]),
    )
    if (waited === ABORTED || d.abandoned) return {}
    if (waited === DEADLINE) {
      debug($, heldBack)
      break
    }
  }

  if (overtaken(ctx, d)) return { displayContent: '' }
  const text = takeReady(d)
  if (text === null) return {}
  markShown(ctx, d)
  return { displayContent: text }
}

// Translates the blocks the final batch took over with its own calls, which the budget does not
// meter; it bounds itself instead, and gives way to a prompt.
async function takeOver($, ctx, d, slots, next) {
  if (slots.length === 0) return true
  const pipe = ctx.display
  beginTakeover(pipe)
  const stopped = (reason, left) => debug($, `display: takeover stopped (${reason}); ${left} block(s) shown as written`)
  try {
    // The host clock, like the dispatch deadlines, so the takeover's limit is one the host can drive.
    const started = await clockNow($, next.signal)
    if (started === ABORTED) {
      stopped('aborted', slots.length)
      return false
    }
    // Asked before every daemon call, history-halving reruns included, so the takeover stays within
    // its time and starts nothing while a prompt translates.
    const nextCall = async (callMax) => {
      const now = await clockNow($, next.signal)
      if (now === ABORTED) return { stopReason: ABORTED_REASON }
      if (pipe.inputActive > 0) return { stopReason: INPUT_FIRST_REASON }
      const timeoutMs = takeoverTimeoutMs(started, now, callMax)
      return timeoutMs === null ? { stopReason: MISSED_REASON } : { timeoutMs }
    }
    for (const [i, slot] of slots.entries()) {
      if (next.signal.aborted || d.abandoned) {
        stopped(next.signal.aborted ? 'aborted' : 'abandoned', slots.length - i)
        return false
      }
      let result
      try {
        result = await untilAborted(translateBlock($, ctx, slot, { nextCall, signal: next.signal }), next.signal)
      } catch (err) {
        result = { text: slot.block.text, failReason: err?.message || 'error' }
      }
      if (result === ABORTED) {
        stopped('aborted', slots.length - i)
        return false
      }
      if (result.stopped) {
        stopped(result.failReason, slots.length - i)
        for (const rest of slots.slice(i)) settle(rest, { text: rest.block.text, failReason: result.failReason })
        return true
      }
      settle(slot, result)
    }
    return true
  } finally {
    endTakeover(pipe)
  }
}

// Returns false when the dispatch was aborted or abandoned, so the caller makes no further calls.
async function finishMessage($, ctx, d, next) {
  const deadline = await dispatchDeadline($, next, true)
  if (deadline === ABORTED) return false
  const previous = await waitUntil($, ctx, next, deadline, d.prevTurn)
  if (previous === ABORTED || d.abandoned) return false
  if (previous === DEADLINE) debug($, 'display: an earlier batch of the message was still busy at the final deadline')

  const claimed = cancelQueued(ctx.display, d.state)
  const inFlight = runningSlot(d.state)
  if (inFlight) {
    const waited = await waitUntil($, ctx, next, deadline, inFlight.settled)
    if (waited === ABORTED || d.abandoned) return false
    if (waited === DEADLINE) debug($, 'display: an in-flight block missed the final deadline; shown as written')
  }
  return takeOver($, ctx, d, claimed, next)
}

function noticeFailures($, failures, partial) {
  for (const reason of failures) debug($, `display: block shown as written (${reason})`)
  const n = failures.length + partial
  if (n > 0) $.ui.toast(failureNotice(n, failures[0] ?? PARTIAL_REASON), { timeoutMs: NOTICE_TIMEOUT_MS })
}

async function showFinal($, ctx, d, next) {
  ctx.finishing.add(d.state)
  let live = false
  try {
    live = await finishMessage($, ctx, d, next)
  } catch (err) {
    debug($, `display: final batch failed (${err?.message || 'error'}); the rest is shown as written`)
  }
  const { text, failures, partial } = finalTake(ctx.display, d)
  markShown(ctx, d)
  if (live) noticeFailures($, failures, partial)
  return { displayContent: text }
}

export function register(on, options) {
  const cfg = resolveConfig(options)
  const ctx = {
    cfg,
    memory: createMemory(),
    enabled: undefined,
    enabledLoad: undefined,
    display: createPipeline({ flushLines: cfg.flushLines }),
    historySlots: new Set(),
    docTooOld: false,
    workerGen: 0,
    sleepRefusalLogged: false,
    shownSeq: new WeakMap(),
    finishing: new WeakSet(),
  }

  const stateLabel = () => (ctx.enabled ? 'オン' : 'オフ')

  on('session.start', async ($, e, next) => {
    ctx.memory.clear()
    ctx.sleepRefusalLogged = false
    ctx.docTooOld = false
    startDisplayWorker($, ctx)
    await loadEnabled($, ctx)
    await $.command.register(COMMAND)
    $.ui.status(ctx.enabled ? STATUS_TEXT : undefined)
    return next(e)
  })

  // /ja has gone missing mid-session (only /reload-plugins brought it back); each main turn restores it
  // and the status line. Messages of earlier turns get no further batch, so their queued blocks would
  // only delay this turn's reply; a message whose final batch is running finishes on its own.
  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) {
      dropMessages(ctx.display, (state) => state.turnId !== e.turnId && !ctx.finishing.has(state))
    }
    const result = await next(e)
    if (e.agentId !== undefined) return result
    try {
      const commands = await $.command.list()
      if (!commands.some((c) => c.name === COMMAND.name)) await $.command.register(COMMAND)
      $.ui.status((await isEnabled($, ctx)) ? STATUS_TEXT : undefined)
    } catch {}
    return result
  })

  // session.start does not fire after /clear or /resume, and the old conversation's pairs no longer apply.
  on('session.end', async ($, e, next) => {
    if (e.reason === 'clear' || e.reason === 'resume') {
      ctx.memory.clear()
      dropMessages(ctx.display)
    }
    return next(e)
  })

  on('command.run', { command: 'ja' }, async ($, e) => {
    const arg = (e.args ?? '').trim()
    if (arg === 'status') {
      await isEnabled($, ctx)
      return { text: `日英レイヤ: ${stateLabel()} / 対訳 ${ctx.memory.size()} 組 / 翻訳: ${ctx.cfg.translator}` }
    }
    const current = await isEnabled($, ctx)
    await setEnabled($, ctx, arg === 'on' ? true : arg === 'off' ? false : !current)
    $.ui.status(ctx.enabled ? STATUS_TEXT : undefined)
    return { text: `日英レイヤ: ${stateLabel()}` }
  })

  on('prompt.submit', async ($, e, next) => {
    if (!isTypedByUser(e) || isPassthroughCommand(e.text)) return next(e)
    const enabled = await untilAborted(isEnabled($, ctx), next.signal)
    if (enabled === ABORTED) return { drop: DROP_REASON }
    if (!enabled) return next(e)
    return submitJapanese($, ctx, e, next)
  }).catch(async ($, e, next) => {
    if (next.called) return next(e)
    return { drop: crashReason(next.error.kind) }
  })

  on('classic.MessageDisplay', async ($, e, next) => {
    if (ctx.enabled === undefined) {
      // The read may be another dispatch's, so waiting on it is metered like any other wait.
      const loaded = await waitFor($, ctx, next, next.budget.remainingMs - BUDGET_MARGIN_MS, loadEnabledOnce($, ctx))
      if (loaded === DEADLINE) debug($, 'display: the on/off switch was still unread at the deadline; batch shown as written')
      if (loaded === ABORTED || loaded === DEADLINE) return {}
    }
    if (!ctx.enabled) {
      const state = ctx.display.messages.get(e.message_id)
      if (!state) return next(e)
      return { displayContent: release(ctx.display, state) + e.delta }
    }
    // Before any await: arrival order decides the display order.
    const d = arrive(ctx.display, {
      messageId: e.message_id,
      key: e.index,
      delta: e.delta,
      final: e.final,
      turnId: e.turn_id,
    })
    try {
      return await (e.final ? showFinal($, ctx, d, next) : showBatch($, ctx, d, next))
    } finally {
      d.finishTurn()
    }
  }).catch(($, e, next) => {
    const pipe = ctx.display
    const d = findDispatch(pipe, e.message_id, e.index)
    if (!d) {
      if (next.called) return next(e)
      const state = pipe.messages.get(e.message_id)
      debug($, `display: rescue (${next.error?.kind}) before the batch arrived; held text and the batch shown as written`)
      $.ui.toast(RESCUE_NOTICE)
      return { displayContent: (state ? release(pipe, state) : '') + e.delta }
    }
    if (overtaken(ctx, d)) {
      abandon(d)
      debug($, `display: rescue (${next.error?.kind}) of an overtaken batch; a later batch shows its text`)
      return { displayContent: '' }
    }
    const text = rescue(pipe, d, { final: e.final })
    markShown(ctx, d)
    debug($, `display: rescue (${next.error?.kind}); ${text.length} held character(s) shown as written`)
    $.ui.toast(RESCUE_NOTICE)
    return { displayContent: text }
  })
}
