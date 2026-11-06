import { expect, test } from 'claude-code/testing'
import { resolveConfig } from '../hooks/lib/config.js'

const DEFAULTS = {
  enabledByDefault: false,
  confirm: true,
  translator: 'plamo-translate',
  historyPairs: 8,
  historyChars: 6000,
  form: undefined,
  flushLines: 20,
  useDoc: true,
}

test('defaults when no options are given', () => {
  expect(resolveConfig(undefined)).toEqual(DEFAULTS)
})

test('given options override only their keys', () => {
  expect(resolveConfig({ historyPairs: 3, form: 'plain', confirm: false })).toEqual({
    ...DEFAULTS,
    historyPairs: 3,
    form: 'plain',
    confirm: false,
  })
})

test('auto form means unspecified', () => {
  expect(resolveConfig({ form: 'auto' }).form).toBeUndefined()
})

test('invalid values fall back to defaults', () => {
  expect(
    resolveConfig({ historyPairs: -1, flushLines: 0, form: 'casual', translator: '', historyChars: 'x' }),
  ).toEqual(DEFAULTS)
  expect(resolveConfig({ historyPairs: 2.5 }).historyPairs).toBe(8)
})

test('zero history pairs is allowed', () => {
  expect(resolveConfig({ historyPairs: 0 }).historyPairs).toBe(0)
})

test('useDoc defaults to true', () => {
  expect(resolveConfig({}).useDoc).toBe(true)
  expect(resolveConfig(undefined).useDoc).toBe(true)
})

test('useDoc false is kept, non-boolean falls back', () => {
  expect(resolveConfig({ useDoc: false }).useDoc).toBe(false)
  expect(resolveConfig({ useDoc: 'no' }).useDoc).toBe(true)
  expect(resolveConfig({ useDoc: 0 }).useDoc).toBe(true)
})
