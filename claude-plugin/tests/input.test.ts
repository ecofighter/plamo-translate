import { expect, test } from 'claude-code/testing'
import { containsJapanese, isPassthroughCommand } from '../hooks/lib/input.js'

test('detects hiragana, katakana and kanji', () => {
  expect(containsJapanese('テスト')).toBe(true)
  expect(containsJapanese('漢字')).toBe(true)
  expect(containsJapanese('ひらがな')).toBe(true)
})

test('ascii and full-width digits are not Japanese', () => {
  expect(containsJapanese('hello')).toBe(false)
  expect(containsJapanese('１２３')).toBe(false)
})

test('half-width katakana is Japanese', () => {
  expect(containsJapanese('ﾃｽﾄ')).toBe(true)
  expect(containsJapanese('ｰ')).toBe(true)
})

test('slash and bang commands pass through', () => {
  expect(isPassthroughCommand('/ja on')).toBe(true)
  expect(isPassthroughCommand('  !ls')).toBe(true)
})

test('other prompts do not pass through', () => {
  expect(isPassthroughCommand('a/b')).toBe(false)
  expect(isPassthroughCommand('日本語')).toBe(false)
})
