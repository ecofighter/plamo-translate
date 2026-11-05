const DEFAULTS = Object.freeze({
  enabledByDefault: false,
  confirm: true,
  translator: 'plamo-translate',
  historyPairs: 8,
  historyChars: 6000,
  form: undefined,
  flushLines: 20,
})

const isNonNegativeInt = (v) => Number.isInteger(v) && v >= 0
const isPositiveInt = (v) => Number.isInteger(v) && v > 0

export function resolveConfig(options) {
  const o = options ?? {}
  return {
    enabledByDefault: typeof o.enabledByDefault === 'boolean' ? o.enabledByDefault : DEFAULTS.enabledByDefault,
    confirm: typeof o.confirm === 'boolean' ? o.confirm : DEFAULTS.confirm,
    translator: typeof o.translator === 'string' && o.translator.trim() !== '' ? o.translator : DEFAULTS.translator,
    historyPairs: isNonNegativeInt(o.historyPairs) ? o.historyPairs : DEFAULTS.historyPairs,
    historyChars: isNonNegativeInt(o.historyChars) ? o.historyChars : DEFAULTS.historyChars,
    form: o.form === 'polite' || o.form === 'plain' ? o.form : DEFAULTS.form,
    flushLines: isPositiveInt(o.flushLines) ? o.flushLines : DEFAULTS.flushLines,
  }
}
