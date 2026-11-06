const JAPANESE = /[\u3040-\u309f\u30a0-\u30ff\u4e00-\u9fff\uff65-\uff9f]/

export function containsJapanese(text) {
  return JAPANESE.test(text)
}

// Sub-agent hand-backs, task notifications and other machine-sent prompts often quote Japanese,
// and asking about each one would stall the session.
const TYPED_ORIGINS = new Set(['composer', 'bridge', 'sdk'])

export function isTypedByUser(e) {
  return !e.isMeta && (e.origin === undefined || TYPED_ORIGINS.has(e.origin.kind))
}

export function isPassthroughCommand(text) {
  return /^\s*[/!]/.test(text)
}

export const LABELS = Object.freeze({
  send: 'この英訳で送る',
  original: '原文のまま送る',
  cancel: 'やめる',
})

export function decideSubmission(answer) {
  if (answer === null || answer === undefined || answer.trim() === '' || answer === LABELS.cancel) return 'drop'
  if (answer === LABELS.send) return 'send-translation'
  if (answer === LABELS.original) return 'send-original'
  return { custom: answer }
}

// Claude often quotes a Japanese identifier in an English reply; only mostly-Japanese text is left as written.
export function isMostlyJapanese(text) {
  const japanese = (text.match(new RegExp(JAPANESE.source, 'g')) ?? []).length
  const latin = (text.match(/[A-Za-z]/g) ?? []).length
  return japanese > 0 && japanese / (japanese + latin) >= 0.3
}

// Custom English replaces the prose only; pasted code or logs in fences still reach Claude.
export function withFences(custom, segments) {
  const fences = segments.filter((s) => s.kind === 'fence').map((s) => s.text)
  if (fences.length === 0) return custom
  return [custom.replace(/\n+$/, ''), ...fences.map((f) => f.replace(/\n+$/, ''))].join('\n\n') +
    (fences[fences.length - 1].endsWith('\n') ? '\n' : '')
}

export const CONFIRM_PREVIEW_CHARS = 1500
const PREVIEW_HEAD_CHARS = 1200
const PREVIEW_TAIL_CHARS = 300

// A long translation would push the choices off the screen, so the question shows only its ends.
export function confirmPreview(english) {
  if (english.length <= CONFIRM_PREVIEW_CHARS) {
    return `英訳を確認してください。\n\n${english}\n\nこの英訳で送りますか?`
  }
  const head = english.slice(0, PREVIEW_HEAD_CHARS)
  const tail = english.slice(english.length - PREVIEW_TAIL_CHARS)
  const omitted = english.length - PREVIEW_HEAD_CHARS - PREVIEW_TAIL_CHARS
  return `英訳を確認してください (全 ${english.length} 文字、中略あり)。\n\n${head}\n\n…(中略 ${omitted} 文字)…\n\n${tail}\n\nこの英訳で送りますか?`
}
