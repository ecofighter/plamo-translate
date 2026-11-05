const JAPANESE = /[\u3040-\u309f\u30a0-\u30ff\u4e00-\u9fff\uff65-\uff9f]/

export function containsJapanese(text) {
  return JAPANESE.test(text)
}

export function isPassthroughCommand(text) {
  return /^\s*[/!]/.test(text)
}
