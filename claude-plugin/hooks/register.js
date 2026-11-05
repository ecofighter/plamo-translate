import { resolveConfig } from './lib/config.js'
import { createMemory, dropOldestHalf, isEligiblePair, toJsonl } from './lib/memory.js'
import { buildArgs, parseResult } from './lib/cli.js'
import { splitEdges, splitFences } from './lib/segments.js'
import {
  containsJapanese,
  decideSubmission,
  isMostlyJapanese,
  isPassthroughCommand,
  LABELS,
  withFences,
} from './lib/input.js'
import { createDisplayBuffer, tableShapeMatches } from './lib/display.js'

const STATUS_TEXT = 'ja⇄en'
const INPUT_TIMEOUT_MS = 60000
const OUTPUT_TIMEOUT_MS = 20000
// Verified in an interactive session: an empty displayContent hides the batch (spec 5.2).
const HOLD_INCOMPLETE = true
const DROP_REASON = '送信をやめました。'

const confirmQuestion = (en) => `英訳を確認してください。\n\n${en}\n\nこの英訳で送りますか?`
const failureQuestion = (reason) => `翻訳に失敗しました (${reason})。どうしますか?`
const crashReason = (kind) => `日英レイヤでエラーが起きたため送信を止めました (${kind})。/ja off で無効にできます。`

async function historyPath($) {
  const tmp = ((await $.env.get('TMPDIR')) ?? '/tmp').replace(/\/$/, '')
  return `${tmp}/plamo-ja-bridge-${await $.session.id()}.jsonl`
}

// MessageDisplay calls for one reply arrive concurrently; one translation at a time keeps the shared
// history file intact and the memory in reply order.
function translate($, ctx, text, direction, timeoutMs) {
  const run = ctx.queue.then(() => translateNow($, ctx, text, direction, timeoutMs))
  ctx.queue = run.catch(() => {})
  return run
}

// Runs one translation, shrinking the history on context_too_long the way the MCP server does.
async function translateNow($, ctx, text, direction, timeoutMs) {
  const [from, to] = direction === 'ja-en' ? ['ja', 'en'] : ['en', 'ja']
  let history = ctx.memory.select(direction, { maxPairs: ctx.cfg.historyPairs, maxChars: ctx.cfg.historyChars })
  for (;;) {
    let historyFile
    if (history.length > 0) {
      historyFile = await historyPath($)
      await $.fs.write(historyFile, toJsonl(history))
    }
    let run
    try {
      run = await $.process.run([ctx.cfg.translator, ...buildArgs({ from, to, form: ctx.cfg.form, historyFile })], {
        stdin: text,
        timeoutMs,
      })
    } catch (err) {
      return { ok: false, reason: err?.message || 'timeout', contextTooLong: false }
    }
    const result = parseResult(run)
    if (result.ok || !result.contextTooLong || history.length === 0) return result
    history = dropOldestHalf(history)
  }
}

async function ask($, question, options) {
  try {
    return await $.ui.ask(question, options)
  } catch {
    return null
  }
}

async function submitJapanese($, ctx, e, next) {
  const segments = splitFences(e.text)
  const needsTranslation = (s) => s.kind === 'text' && containsJapanese(s.text)
  if (!segments.some(needsTranslation)) return next(e)

  const pairs = []
  let english = ''
  for (const s of segments) {
    if (!needsTranslation(s)) {
      english += s.text
      continue
    }
    const { lead, core, trail } = splitEdges(s.text)
    const result = await translate($, ctx, core, 'ja-en', INPUT_TIMEOUT_MS)
    if (!result.ok) {
      const answer = await ask($, failureQuestion(result.reason), [LABELS.original, LABELS.cancel])
      return answer === LABELS.original ? next(e) : { drop: DROP_REASON }
    }
    const translation = result.text.trim()
    pairs.push({ ja: core, en: translation, finishReason: result.finishReason })
    english += lead + translation + trail
  }

  const answer = ctx.cfg.confirm
    ? await ask($, confirmQuestion(english), [LABELS.send, LABELS.original, LABELS.cancel])
    : LABELS.send
  const decision = decideSubmission(answer)
  if (decision === 'drop') return { drop: DROP_REASON }
  if (decision === 'send-original') return next(e)
  if (decision === 'send-translation') {
    for (const p of pairs) if (isEligiblePair(p.ja, p.en, p.finishReason)) ctx.memory.add({ ja: p.ja, en: p.en })
    return next({ ...e, text: english })
  }
  const hasFence = segments.some((s) => s.kind === 'fence')
  const ja = e.text.trim()
  const en = decision.custom.trim()
  if (!hasFence && isEligiblePair(ja, en, 'stop')) ctx.memory.add({ ja, en })
  return next({ ...e, text: withFences(decision.custom, segments) })
}

// A block that cannot be translated is shown as written: the display is the only thing at stake.
async function renderBlock($, ctx, block) {
  if (block.kind !== 'paragraph' && block.kind !== 'table') return block.text
  const { lead, core, trail } = splitEdges(block.text)
  if (core === '' || isMostlyJapanese(core)) return block.text
  let result
  try {
    result = await translate($, ctx, core, 'en-ja', OUTPUT_TIMEOUT_MS)
  } catch {
    return block.text
  }
  if (!result.ok) return block.text
  const ja = result.text.trim()
  if (block.kind === 'table' && !tableShapeMatches(core, ja)) return block.text
  if (isEligiblePair(core, ja, result.finishReason)) ctx.memory.add({ ja, en: core })
  return lead + ja + trail
}

export function register(on, options) {
  const ctx = { cfg: resolveConfig(options), memory: createMemory(), queue: Promise.resolve() }
  const displayBuffers = new Map()
  let enabled = ctx.cfg.enabledByDefault

  const stateLabel = () => (enabled ? 'オン' : 'オフ')

  on('session.start', async ($, e, next) => {
    ctx.memory.clear()
    enabled = ctx.cfg.enabledByDefault
    await $.command.register({
      name: 'ja',
      description: '日英レイヤ (plamo-translate) のオン・オフ',
      argumentHint: '[on|off|status]',
    })
    $.ui.status(enabled ? STATUS_TEXT : undefined)
    return next(e)
  })

  // session.start does not fire after /clear or /resume, and the old conversation's pairs no longer apply.
  on('session.end', async ($, e, next) => {
    if (e.reason === 'clear' || e.reason === 'resume') {
      ctx.memory.clear()
      displayBuffers.clear()
    }
    return next(e)
  })

  on('command.run', { command: 'ja' }, async ($, e) => {
    const arg = (e.args ?? '').trim()
    if (arg === 'status') {
      return { text: `日英レイヤ: ${stateLabel()} / 対訳 ${ctx.memory.size()} 組 / 翻訳: ${ctx.cfg.translator}` }
    }
    enabled = arg === 'on' ? true : arg === 'off' ? false : !enabled
    $.ui.status(enabled ? STATUS_TEXT : undefined)
    return { text: `日英レイヤ: ${stateLabel()}` }
  })

  on('prompt.submit', async ($, e, next) => {
    if (!enabled || isPassthroughCommand(e.text)) return next(e)
    return submitJapanese($, ctx, e, next)
  }).catch(async ($, e, next) => {
    if (next.called) return next(e)
    return { drop: crashReason(next.error.kind) }
  })

  on('classic.MessageDisplay', async ($, e, next) => {
    const held = displayBuffers.get(e.message_id)
    if (!enabled) {
      if (!held) return next(e)
      displayBuffers.delete(e.message_id)
      return { displayContent: held.takeAll().map((b) => b.text).join('') + e.delta }
    }
    const buffer = held ?? createDisplayBuffer({ flushLines: ctx.cfg.flushLines, holdIncomplete: HOLD_INCOMPLETE })
    displayBuffers.set(e.message_id, buffer)
    const blocks = buffer.push(e.delta, e.final)
    if (e.final) displayBuffers.delete(e.message_id)
    let shown = ''
    for (const block of blocks) shown += await renderBlock($, ctx, block)
    return { displayContent: shown }
  })
}
