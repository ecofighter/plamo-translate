import { resolveConfig } from './lib/config.js'
import { createMemory, dropOldestHalf, isEligiblePair, toJsonl } from './lib/memory.js'
import { buildArgs, parseResult } from './lib/cli.js'
import { splitEdges, splitFences } from './lib/segments.js'
import {
  containsJapanese,
  decideSubmission,
  isMostlyJapanese,
  isPassthroughCommand,
  isTypedByUser,
  LABELS,
  withFences,
} from './lib/input.js'
import { createDisplayBuffer, tableShapeMatches } from './lib/display.js'

const STATUS_TEXT = 'ja⇄en'
const INPUT_TIMEOUT_MS = 60000
const OUTPUT_TIMEOUT_MS = 20000
// An empty displayContent hides a batch (checked in an interactive session on Claude Code v2.1.317),
// so a paragraph can be held until it is complete.
const HOLD_INCOMPLETE = true
const DROP_REASON = '送信をやめました。'
// The engine replaces the plugin's copy mid-session (a hooks worker respawn) and runs register and
// session.start again; the switch lives in session state so that does not turn the layer off.
const ENABLED = { plugin: 'plamo-ja-bridge', key: 'enabled' }
const COMMAND = {
  name: 'ja',
  description: '日英レイヤ (plamo-translate) のオン・オフ',
  argumentHint: '[on|off|status]',
}

const confirmQuestion = (en) => `英訳を確認してください。\n\n${en}\n\nこの英訳で送りますか?`
const failureQuestion = (reason) => `翻訳に失敗しました (${reason})。どうしますか?`
const crashReason = (kind) => `日英レイヤでエラーが起きたため送信を止めました (${kind})。/ja off で無効にできます。`

async function loadEnabled($, ctx) {
  const { value } = await $.state.get(ENABLED)
  ctx.enabled = value ?? ctx.cfg.enabledByDefault
  return ctx.enabled
}

async function isEnabled($, ctx) {
  return ctx.enabled ?? (await loadEnabled($, ctx))
}

async function setEnabled($, ctx, value) {
  ctx.enabled = value
  await $.state.set(ENABLED, value)
}

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
  const ctx = { cfg: resolveConfig(options), memory: createMemory(), queue: Promise.resolve(), enabled: undefined }
  const displayBuffers = new Map()

  const stateLabel = () => (ctx.enabled ? 'オン' : 'オフ')

  on('session.start', async ($, e, next) => {
    ctx.memory.clear()
    await loadEnabled($, ctx)
    await $.command.register(COMMAND)
    $.ui.status(ctx.enabled ? STATUS_TEXT : undefined)
    return next(e)
  })

  // /ja has gone missing mid-session (only /reload-plugins brought it back); each main turn restores it
  // and the status line.
  on('turn.start', async ($, e, next) => {
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
      displayBuffers.clear()
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
    if (!isTypedByUser(e) || isPassthroughCommand(e.text) || !(await isEnabled($, ctx))) return next(e)
    return submitJapanese($, ctx, e, next)
  }).catch(async ($, e, next) => {
    if (next.called) return next(e)
    return { drop: crashReason(next.error.kind) }
  })

  on('classic.MessageDisplay', async ($, e, next) => {
    const held = displayBuffers.get(e.message_id)
    if (!(await isEnabled($, ctx))) {
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
