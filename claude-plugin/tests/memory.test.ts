import { expect, test } from 'claude-code/testing'
import { createMemory, isEligiblePair, dropOldestHalf, toJsonl } from '../hooks/lib/memory.js'

test('select orients pairs by direction', () => {
  const m = createMemory()
  m.add({ ja: 'こんにちは', en: 'Hello' })
  expect(m.select('ja-en', { maxPairs: 8, maxChars: 6000 })).toEqual([{ source: 'こんにちは', translation: 'Hello' }])
  expect(m.select('en-ja', { maxPairs: 8, maxChars: 6000 })).toEqual([{ source: 'Hello', translation: 'こんにちは' }])
})

test('select keeps the newest pairs, oldest first', () => {
  const m = createMemory()
  for (let i = 0; i < 10; i++) m.add({ ja: `j${i}`, en: `e${i}` })
  const got = m.select('ja-en', { maxPairs: 8, maxChars: 6000 })
  expect(got.map((p) => p.source)).toEqual(['j2', 'j3', 'j4', 'j5', 'j6', 'j7', 'j8', 'j9'])
})

test('select stops at the character budget', () => {
  const m = createMemory()
  m.add({ ja: 'aaaa', en: 'bbbb' })
  m.add({ ja: 'cc', en: 'dd' })
  m.add({ ja: 'ee', en: 'ff' })
  expect(m.select('ja-en', { maxPairs: 8, maxChars: 9 }).map((p) => p.source)).toEqual(['cc', 'ee'])
})

test('a pair larger than the budget ends the selection', () => {
  const m = createMemory()
  m.add({ ja: 'old', en: 'old' })
  m.add({ ja: 'x'.repeat(50), en: 'y'.repeat(50) })
  expect(m.select('ja-en', { maxPairs: 8, maxChars: 20 })).toEqual([])
})

test('size and clear', () => {
  const m = createMemory()
  m.add({ ja: 'a', en: 'b' })
  expect(m.size()).toBe(1)
  m.clear()
  expect(m.size()).toBe(0)
})

test('eligibility rules', () => {
  expect(isEligiblePair('原文', 'source', 'stop')).toBe(true)
  expect(isEligiblePair('原文', 'source', 'length')).toBe(false)
  expect(isEligiblePair('原文', '  \n', 'stop')).toBe(false)
  expect(isEligiblePair('  ', 'x', 'stop')).toBe(false)
  expect(isEligiblePair('原文', 'a <|plamo:msg|> b', 'stop')).toBe(false)
})

test('dropOldestHalf removes ceil(n/2) oldest pairs', () => {
  expect(dropOldestHalf([1, 2, 3, 4, 5])).toEqual([4, 5])
  expect(dropOldestHalf([1])).toEqual([])
  expect(dropOldestHalf([])).toEqual([])
})

test('toJsonl writes one parsable pair per line', () => {
  const pairs = [
    { source: '一行目\n二行目', translation: 'line one\nline two' },
    { source: '"引用"', translation: 'quote' },
  ]
  const out = toJsonl(pairs)
  expect(out.endsWith('\n')).toBe(true)
  expect(out.trimEnd().split('\n').map((l) => JSON.parse(l))).toEqual(pairs)
})
