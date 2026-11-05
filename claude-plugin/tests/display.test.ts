import { expect, test } from 'claude-code/testing'
import { createDisplayBuffer, tableShapeMatches } from '../hooks/lib/display.js'

const held = () => createDisplayBuffer({ flushLines: 20, holdIncomplete: true })
const text = (blocks: Array<{ text: string }>) => blocks.map((b) => b.text).join('')

test('a paragraph is held until a blank line', () => {
  const b = held()
  expect(b.push('para one\n', false)).toEqual([])
  expect(b.push('\n', false)).toEqual([
    { kind: 'paragraph', text: 'para one\n' },
    { kind: 'blank', text: '\n' },
  ])
  expect(b.isEmpty()).toBe(true)
})

test('a fence is returned once it closes', () => {
  const b = held()
  expect(b.push('```js\nx()\n', false)).toEqual([])
  expect(b.push('```\n', false)).toEqual([{ kind: 'fence', text: '```js\nx()\n```\n' }])
})

test('a fence opener ends the paragraph before it', () => {
  const b = held()
  expect(b.push('Run:\n```\nls\n```\n', false)).toEqual([
    { kind: 'paragraph', text: 'Run:\n' },
    { kind: 'fence', text: '```\nls\n```\n' },
  ])
})

test('a fence indented inside a list item is shown as a fence', () => {
  const b = held()
  expect(b.push('- step one:\n    ```sh\n    ls\n    ```\n', false)).toEqual([
    { kind: 'paragraph', text: '- step one:\n' },
    { kind: 'fence', text: '    ```sh\n    ls\n    ```\n' },
  ])
})

test('a table ends at a non-table line', () => {
  const b = held()
  expect(b.push('| a | b |\n|---|---|\n', false)).toEqual([])
  expect(b.push('after\n', false)).toEqual([{ kind: 'table', text: '| a | b |\n|---|---|\n' }])
})

test('long paragraphs are flushed at flushLines', () => {
  const b = createDisplayBuffer({ flushLines: 3, holdIncomplete: true })
  expect(b.push('1\n2\n3\n4\n5\n', false)).toEqual([{ kind: 'paragraph', text: '1\n2\n3\n' }])
})

test('final returns everything left, including a partial line', () => {
  const b = held()
  b.push('para\n', false)
  expect(b.push('tail', true)).toEqual([{ kind: 'paragraph', text: 'para\ntail' }])
  expect(b.isEmpty()).toBe(true)
})

test('an unclosed fence is returned at final', () => {
  const b = held()
  expect(b.push('```\nopen\n', true)).toEqual([{ kind: 'fence', text: '```\nopen\n' }])
})

test('preserves blank lines between blocks', () => {
  const b = held()
  const input = ['Intro.\n', '\n', '\n', '- a\n- b\n', '\n', '```\nc\n\nd\n```\n', '\n', 'End.']
  const out: Array<{ text: string }> = []
  input.forEach((d, i) => out.push(...b.push(d, i === input.length - 1)))
  expect(text(out)).toBe(input.join(''))
})

test('without holding, every push returns its lines', () => {
  const b = createDisplayBuffer({ flushLines: 20, holdIncomplete: false })
  expect(b.push('half a\n', false)).toEqual([{ kind: 'paragraph', text: 'half a\n' }])
  expect(b.push('```\nx\n', false)).toEqual([{ kind: 'fence', text: '```\nx\n' }])
  expect(b.push('y\n```\nz\n', false)).toEqual([
    { kind: 'fence', text: 'y\n```\n' },
    { kind: 'paragraph', text: 'z\n' },
  ])
})

test('takeAll drains held text', () => {
  const b = held()
  b.push('held\n', false)
  expect(text(b.takeAll())).toBe('held\n')
  expect(b.isEmpty()).toBe(true)
})

test('tableShapeMatches compares rows and pipes', () => {
  expect(tableShapeMatches('| a | b |\n|---|---|', '| あ | い |\n|---|---|')).toBe(true)
  expect(tableShapeMatches('| a | b |\n|---|---|', '| あ い |\n|---|---|')).toBe(false)
  expect(tableShapeMatches('| a | b |\n|---|---|', '| あ | い |')).toBe(false)
})
