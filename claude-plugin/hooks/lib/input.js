const JAPANESE = /[\u3040-\u309f\u30a0-\u30ff\u4e00-\u9fff\uff65-\uff9f]/

export function containsJapanese(text) {
  return JAPANESE.test(text)
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
