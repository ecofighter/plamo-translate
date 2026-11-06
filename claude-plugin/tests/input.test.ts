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

import { isMostlyJapanese } from '../hooks/lib/input.js'

test('isMostlyJapanese ignores a few Japanese words in English text', () => {
  expect(isMostlyJapanese('The `取得` helper returns None when the cache is empty.')).toBe(false)
  expect(isMostlyJapanese('`Cargo.toml` の `members` は `crates/*` というグロブ指定です。')).toBe(true)
  expect(isMostlyJapanese('すでに日本語です。')).toBe(true)
  expect(isMostlyJapanese('plain English')).toBe(false)
})

import { withFences } from '../hooks/lib/input.js'

test('withFences appends the prompt fences to custom English', () => {
  const segments = [
    { kind: 'text', text: '直して\n' },
    { kind: 'fence', text: '```\nlog\n```\n' },
    { kind: 'text', text: 'あと\n' },
    { kind: 'fence', text: '~~~\nmore\n~~~' },
  ]
  expect(withFences('Fix this\n', segments)).toBe('Fix this\n\n```\nlog\n```\n\n~~~\nmore\n~~~')
  expect(withFences('Fix this', [{ kind: 'text', text: '直して' }])).toBe('Fix this')
})

import { confirmPreview } from '../hooks/lib/input.js'

test('confirmPreview keeps short text and elides long text', () => {
  const short = 'a'.repeat(1500)
  expect(confirmPreview(short)).toBe(`英訳を確認してください。\n\n${short}\n\nこの英訳で送りますか?`)

  const long = 'h'.repeat(1200) + 'm'.repeat(500) + 't'.repeat(300)
  const question = confirmPreview(long)
  expect(question).toBe(
    `英訳を確認してください (全 2000 文字、中略あり)。\n\n${'h'.repeat(1200)}\n\n…(中略 500 文字)…\n\n${'t'.repeat(300)}\n\nこの英訳で送りますか?`,
  )
  expect(question).not.toContain('m')
})
