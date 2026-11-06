import { createDisplayBuffer } from './display.js'
import { ABORTED, utf8Bytes } from './doc.js'
import { isMostlyJapanese } from './input.js'
import { splitEdges } from './segments.js'

export const STREAM_WAIT_MS = 2000
export const BUDGET_MARGIN_MS = 2000
export const OUTPUT_FINAL_MAX_MS = 60000
export const MIN_CALL_MS = 3000
export const OUTPUT_DOC_BYTES = 6000

export const DEADLINE = Symbol('deadline')
export const MISSED_REASON = '間に合わず'
export const ABORTED_REASON = '中断'
export { ABORTED }

// An empty displayContent hides a batch, so a paragraph can be held until it is complete.
const HOLD_INCOMPLETE = true
// A `.catch` can arrive after its message's state was finished or dropped; the registry outlives the
// state so the dispatch is still found, and is capped because nothing else would ever prune it.
const MAX_DISPATCHES = 1000

export function createPipeline({ flushLines }) {
  return {
    flushLines,
    messages: new Map(),
    queue: [],
    running: null,
    inputActive: 0,
    takeoverActive: 0,
    wakers: [],
    dispatches: new Map(),
  }
}

export function wake(pipe) {
  const wakers = pipe.wakers
  pipe.wakers = []
  for (const resolve of wakers) resolve()
}

export function waitForWork(pipe) {
  return new Promise((resolve) => pipe.wakers.push(resolve))
}

export function beginInput(pipe) {
  pipe.inputActive++
}

export function endInput(pipe) {
  pipe.inputActive--
  wake(pipe)
}

export function beginTakeover(pipe) {
  pipe.takeoverActive++
}

export function endTakeover(pipe) {
  pipe.takeoverActive--
  wake(pipe)
}

function newMessage(pipe, messageId, turnId) {
  return {
    messageId,
    buffer: createDisplayBuffer({ flushLines: pipe.flushLines, holdIncomplete: HOLD_INCOMPLETE }),
    nextSeq: 0,
    slots: [],
    emitted: 0,
    takeTail: Promise.resolve(),
    turnId,
    failures: [],
    partial: 0,
  }
}

function isTranslatable(block) {
  if (block.kind !== 'paragraph' && block.kind !== 'table') return false
  const { core } = splitEdges(block.text)
  return core !== '' && !isMostlyJapanese(core)
}

function newSlot(block, messageId) {
  let resolve
  const settled = new Promise((r) => (resolve = r))
  const translatable = isTranslatable(block)
  return {
    block,
    messageId,
    state: translatable ? 'queued' : 'ready',
    heavy: translatable && utf8Bytes(splitEdges(block.text).core) > OUTPUT_DOC_BYTES,
    text: translatable ? undefined : block.text,
    failReason: undefined,
    partial: false,
    emitted: false,
    settled,
    markSettled: resolve,
  }
}

// Must run synchronously when the dispatch arrives: arrival order, not e.index, decides the
// display order, and the turn chain is what makes later dispatches wait for earlier ones.
export function arrive(pipe, { messageId, key, delta, final, turnId }) {
  let state = pipe.messages.get(messageId)
  if (!state) {
    state = newMessage(pipe, messageId, turnId)
    pipe.messages.set(messageId, state)
  }
  const slots = state.buffer.push(delta, final).map((block) => newSlot(block, messageId))
  state.slots.push(...slots)
  for (const slot of slots) {
    if (slot.state === 'queued') pipe.queue.push({ slot, messageId, state })
    else slot.markSettled()
  }
  if (slots.some((s) => s.state === 'queued')) wake(pipe)

  const prevTurn = state.takeTail
  let finishTurn
  state.takeTail = new Promise((resolve) => (finishTurn = resolve))
  const dispatch = {
    state,
    key,
    seq: state.nextSeq++,
    final,
    slots,
    end: state.slots.length,
    abandoned: false,
    prevTurn,
    finishTurn,
  }
  const registryKey = dispatchKey(messageId, key)
  pipe.dispatches.delete(registryKey)
  pipe.dispatches.set(registryKey, dispatch)
  if (pipe.dispatches.size > MAX_DISPATCHES) pipe.dispatches.delete(pipe.dispatches.keys().next().value)
  return dispatch
}

const dispatchKey = (messageId, key) => JSON.stringify([messageId, key])

export function findDispatch(pipe, messageId, key) {
  return pipe.dispatches.get(dispatchKey(messageId, key))
}

export function abandon(dispatch) {
  dispatch.abandoned = true
}

const isWaiting = (slot) => slot.state === 'queued' || slot.state === 'running'

function slotsBeforeHeavy(dispatch) {
  const { state } = dispatch
  const pending = []
  for (const slot of state.slots.slice(state.emitted, dispatch.end)) {
    if (slot.heavy) break
    pending.push(slot)
  }
  return pending
}

// What a non-final dispatch waits for before taking: its own slots and those before them, up to
// the first heavy one, which only the final dispatch translates.
export function hasUnsettled(dispatch) {
  return slotsBeforeHeavy(dispatch).some(isWaiting)
}

export function whenSettled(dispatch) {
  return Promise.all(
    slotsBeforeHeavy(dispatch)
      .filter(isWaiting)
      .map((s) => s.settled),
  )
}

function emit(state, slot) {
  slot.state = 'claimed'
  slot.emitted = true
  slot.markSettled()
  state.emitted++
}

// null tells the caller to answer {} for a dispatch whose output the engine no longer uses. Slots of
// later dispatches are left to them: taking them here would show text ahead of a batch that the
// engine may display before this one.
export function takeReady(dispatch) {
  if (dispatch.abandoned) return null
  const { state } = dispatch
  let text = ''
  while (state.emitted < dispatch.end) {
    const slot = state.slots[state.emitted]
    if (slot.state !== 'ready' && slot.state !== 'done') break
    if (slot.failReason !== undefined) state.failures.push(slot.failReason)
    if (slot.partial) state.partial++
    text += slot.text
    emit(state, slot)
  }
  return text
}

function removeJobs(pipe, state) {
  const removed = []
  pipe.queue = pipe.queue.filter((job) => {
    if (job.state !== state) return true
    removed.push(job.slot)
    return false
  })
  return removed
}

// The slots come back claimed but not emitted, so the final dispatch can translate them itself and
// a result from elsewhere cannot overwrite them.
export function cancelQueued(pipe, state) {
  const slots = removeJobs(pipe, state)
  for (const slot of slots) slot.state = 'claimed'
  return slots
}

export function claimAll(pipe, state) {
  removeJobs(pipe, state)
  let text = ''
  while (state.emitted < state.slots.length) {
    const slot = state.slots[state.emitted]
    text += slot.block.text
    emit(state, slot)
  }
  return text
}

// Everything still held for the message, as written, for when it must be shown untranslated now.
export function release(pipe, state) {
  const text = claimAll(pipe, state) + state.buffer.takeAll().map((b) => b.text).join('')
  if (pipe.messages.get(state.messageId) === state) pipe.messages.delete(state.messageId)
  return text
}

// The `.catch` answer: synchronous, and it shows held text instead of losing it. null means no
// dispatch was found: its delta never reached the pipeline and is still the caller's to show.
// A dispatch whose message state is already finished or dropped gets nothing: that state's text was
// shown or given up there, and a newer state under the same message id is not its to claim.
export function rescue(pipe, dispatch, { final }) {
  if (!dispatch) return null
  abandon(dispatch)
  const { state } = dispatch
  if (pipe.messages.get(state.messageId) !== state) return ''
  return final ? release(pipe, state) : claimAll(pipe, state)
}

// For messages that will get no further dispatch (a new turn, /clear, /resume): their queued blocks
// would only delay the display of the current reply.
export function dropMessages(pipe, shouldDrop = () => true) {
  for (const state of [...pipe.messages.values()]) {
    if (!shouldDrop(state)) continue
    claimAll(pipe, state)
    pipe.messages.delete(state.messageId)
  }
}

export function runningSlot(state) {
  return state.slots.find((s) => !s.emitted && s.state === 'running') ?? null
}

export function finalTake(pipe, dispatch, { reason = MISSED_REASON } = {}) {
  const { state } = dispatch
  removeJobs(pipe, state)
  let text = ''
  while (state.emitted < state.slots.length) {
    const slot = state.slots[state.emitted]
    if (slot.text !== undefined) {
      text += slot.text
      if (slot.failReason !== undefined) state.failures.push(slot.failReason)
      if (slot.partial) state.partial++
    } else {
      text += slot.block.text
      state.failures.push(slot.failReason ?? reason)
    }
    emit(state, slot)
  }
  text += state.buffer.takeAll().map((b) => b.text).join('')
  if (pipe.messages.get(state.messageId) === state) pipe.messages.delete(state.messageId)
  return { text, failures: [...state.failures], partial: state.partial }
}

export function popIfIdle(pipe) {
  if (pipe.running !== null || pipe.inputActive > 0 || pipe.takeoverActive > 0) return null
  const index = pipe.queue.findIndex((job) => !job.slot.heavy)
  if (index < 0) return null
  const [job] = pipe.queue.splice(index, 1)
  job.slot.state = 'running'
  pipe.running = job
  return job
}

// A failed block is settled too: `text` is the original and `failReason` says why. Returns false
// when the slot was already shown or reserved elsewhere, so the result is not displayed.
export function settle(slot, { text, failReason, partial = false }) {
  const accepts = !slot.emitted && (slot.state === 'running' || slot.state === 'claimed')
  if (!accepts) return false
  slot.state = 'done'
  slot.text = text
  slot.failReason = failReason
  slot.partial = partial
  slot.markSettled()
  return true
}

export function finishJob(pipe, job, result) {
  const accepted = settle(job.slot, result)
  if (pipe.running === job) pipe.running = null
  wake(pipe)
  return accepted
}

// Ties the end of a job to the end of its translation, whatever happens, so the mutex is never left
// held. Pass the translation itself, not one already cut short by untilAborted: the daemon is still
// busy until it really ends, and its late result can still settle the slot.
export async function runJob(pipe, job, translate) {
  let result
  try {
    const value = await translate(job.slot)
    result = value === ABORTED || !value ? { text: job.slot.block.text, failReason: ABORTED_REASON } : value
  } catch (err) {
    result = { text: job.slot.block.text, failReason: err?.message || 'error' }
  }
  finishJob(pipe, job, result)
  return result
}

// Waiting on anything but the hook's own `$` calls counts against the dispatch budget, so every such
// wait ends before the budget does; a streaming dispatch also must not hold its batch for long.
export function waitBudgetMs(remainingMs, final) {
  const ms = remainingMs - BUDGET_MARGIN_MS
  return final ? ms : Math.min(STREAM_WAIT_MS, ms)
}

// `sleep(ms, { signal })` is the host clock's sleep; its timer is cancelled once the wait ends, so
// nothing is left pending after the hook returns. `signal` (the dispatch's) ends the wait with ABORTED
// at once, because a hook that outlives its abort is counted as overrunning. A non-finite `ms` (an
// unmetered hook) sets no timer, since the clock only takes finite waits.
export async function boundedWait(promise, ms, sleep, signal) {
  if (signal?.aborted) return ABORTED
  if (ms <= 0) return DEADLINE
  const timer = new AbortController()
  let onAbort
  const contenders = [promise]
  if (Number.isFinite(ms)) {
    contenders.push(
      sleep(ms, { signal: timer.signal }).then(
        () => DEADLINE,
        () => DEADLINE,
      ),
    )
  }
  if (signal) {
    contenders.push(
      new Promise((resolve) => {
        onAbort = () => resolve(ABORTED)
        signal.addEventListener('abort', onAbort, { once: true })
      }),
    )
  }
  try {
    return await Promise.race(contenders)
  } finally {
    timer.abort()
    if (onAbort) signal.removeEventListener('abort', onAbort)
  }
}

// The takeover runs on the final dispatch's own `$` calls, which are not metered, so it bounds
// itself: each call gets at most what is left of the takeover time, and it stops when too little is
// left for a useful call.
export function takeoverTimeoutMs(started, now, callMax) {
  const remaining = OUTPUT_FINAL_MAX_MS - (now - started)
  if (remaining < MIN_CALL_MS) return null
  return Math.min(callMax, remaining)
}
