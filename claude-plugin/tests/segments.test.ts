import { expect, test } from 'claude-code/testing'
import { splitFences } from '../hooks/lib/segments.js'

const join = (segs: Array<{ text: string }>) => segs.map((s) => s.text).join('')
const kinds = (segs: Array<{ kind: string }>) => segs.map((s) => s.kind)

test('text without fences is one text segment', () => {
  expect(splitFences('hello\nworld\n')).toEqual([{ kind: 'text', text: 'hello\nworld\n' }])
})

test('a fence between text splits into three segments', () => {
  const input = 'before\n```js\ncode()\n```\nafter\n'
  const segs = splitFences(input)
  expect(kinds(segs)).toEqual(['text', 'fence', 'text'])
  expect(segs[1].text).toBe('```js\ncode()\n```\n')
  expect(join(segs)).toBe(input)
})

test('tilde fences are recognised', () => {
  const input = 'a\n~~~\nx\n~~~\n'
  expect(kinds(splitFences(input))).toEqual(['text', 'fence'])
})

test('a longer fence is not closed by a shorter one', () => {
  const input = '````\n```\ninner\n```\n````\nafter'
  const segs = splitFences(input)
  expect(kinds(segs)).toEqual(['fence', 'text'])
  expect(segs[0].text).toBe('````\n```\ninner\n```\n````\n')
  expect(join(segs)).toBe(input)
})

test('an unclosed fence runs to the end', () => {
  const input = 'a\n```\nopen\n\nstill open'
  const segs = splitFences(input)
  expect(kinds(segs)).toEqual(['text', 'fence'])
  expect(join(segs)).toBe(input)
})

test('blank lines inside a fence do not split it', () => {
  const segs = splitFences('```\na\n\nb\n```\n')
  expect(kinds(segs)).toEqual(['fence'])
})

test('a fence indented inside a list item is a fence', () => {
  const input = '- step one:\n    ```sh\n    ls\n    ```\n- step two\n'
  const segs = splitFences(input)
  expect(kinds(segs)).toEqual(['text', 'fence', 'text'])
  expect(segs[1].text).toBe('    ```sh\n    ls\n    ```\n')
})

test('a fence closed by the other character stays open', () => {
  expect(kinds(splitFences('```\nx\n~~~\ny\n'))).toEqual(['fence'])
})

test('empty input gives no segments', () => {
  expect(splitFences('')).toEqual([])
})
