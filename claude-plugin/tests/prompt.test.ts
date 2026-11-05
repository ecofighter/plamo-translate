import { expect, mock, test } from 'claude-code/testing'
import { decideSubmission, LABELS } from '../hooks/lib/input.js'

type Run = { argv: string[]; init: { stdin?: string; timeoutMs?: number } }

const done = (text: string) =>
  JSON.stringify({ type: 'done', text, finish_reason: 'stop', model: 'm', revision: 'r', usage: {} }) + '\n'

const CONFIRM_Q = (en: string) => `英訳を確認してください。\n\n${en}\n\nこの英訳で送りますか?`

interface Harness {
  runs: Run[]
  writes: Array<{ path: string; content: string }>
  questions: string[]
  answers: Array<string | null>
  translate: (run: Run) => { exitCode: number; stdout: string; stderr: string }
  failWrite: boolean
}

function setup(on: any): Harness {
  const h: Harness = {
    runs: [],
    writes: [],
    questions: [],
    answers: [],
    translate: (run) => ({ exitCode: 0, stdout: done(`EN<${run.init.stdin}>`), stderr: '' }),
    failWrite: false,
  }
  mock.env(on, { TMPDIR: '/tmp/' })
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', () => ({ value: undefined }))
  on('ui.status', () => ({ value: undefined }))
  on('session.id', () => ({ value: 'sess1' }))
  on('fs.write', ($: unknown, e: { path: string; content?: string; text?: string; data?: string }) => {
    if (h.failWrite) return { deny: 'disk full' }
    h.writes.push({ path: e.path, content: e.content ?? e.text ?? e.data ?? '' })
    return { value: undefined }
  })
  on('process.run', ($: unknown, e: Run) => {
    h.runs.push(e)
    return { value: h.translate(e) }
  })
  on('tool.call', ($: unknown, e: { tool: string; questions: Array<{ question: string }> }) => {
    const question = e.questions[0].question
    h.questions.push(question)
    const answer = h.answers.shift()
    if (answer === null || answer === undefined) return { deny: 'dismissed' }
    return { result: { answers: { [question]: answer } } }
  })
  on('prompt.submit', ($: unknown, e: { text: string }) => ({ text: e.text }))
  return h
}

async function turnOn($: any) {
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await $.command.run({ command: 'ja', args: 'on' })
}

const submit = ($: any, text: string) => $.prompt.submit({ text })

test('decideSubmission maps answers', () => {
  expect(decideSubmission(LABELS.send)).toBe('send-translation')
  expect(decideSubmission(LABELS.original)).toBe('send-original')
  expect(decideSubmission(LABELS.cancel)).toBe('drop')
  expect(decideSubmission(null)).toBe('drop')
  expect(decideSubmission('  ')).toBe('drop')
  expect(decideSubmission('Fix the build')).toEqual({ custom: 'Fix the build' })
})

test('off passes through', async ($, on) => {
  const h = setup(on)
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  expect(await submit($, '直して')).toEqual({ text: '直して' })
  expect(h.runs.length).toBe(0)
})

test('slash and bang pass through', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  expect(await submit($, '/help 日本語')).toEqual({ text: '/help 日本語' })
  expect(await submit($, '!ls 日本語')).toEqual({ text: '!ls 日本語' })
  expect(h.runs.length).toBe(0)
})

test('english passes through', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  expect(await submit($, 'fix the bug')).toEqual({ text: 'fix the bug' })
  expect(h.runs.length).toBe(0)
})

test('fence-only Japanese passes through', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  const text = 'explain this:\n```\nエラー: 失敗\n```\n'
  expect(await submit($, text)).toEqual({ text })
  expect(h.runs.length).toBe(0)
  expect(h.questions.length).toBe(0)
})

test('sends confirmed translation', async ($, on) => {
  const h = setup(on)
  h.answers.push(LABELS.send)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ text: 'EN<直して>' })
  expect(h.runs.length).toBe(1)
  expect(h.runs[0].argv).toEqual(['plamo-translate', '--json', '--no-stream', '-s', 'ja', '-t', 'en'])
  expect(h.runs[0].init.timeoutMs).toBe(60000)
  expect(h.questions).toEqual([CONFIRM_Q('EN<直して>')])
})

test('keeps fences verbatim', async ($, on) => {
  const h = setup(on)
  h.answers.push(LABELS.send)
  await turnOn($)
  const result = await submit($, '直して\n```js\nx()\n```\n確認して')
  expect(result).toEqual({ text: 'EN<直して>\n```js\nx()\n```\nEN<確認して>' })
  expect(h.runs.length).toBe(2)
})

test('sends original on request', async ($, on) => {
  const h = setup(on)
  h.answers.push(LABELS.original)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ text: '直して' })
})

test('drops on cancel', async ($, on) => {
  const h = setup(on)
  h.answers.push(LABELS.cancel)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ drop: '送信をやめました。' })
})

test('drops when dismissed', async ($, on) => {
  const h = setup(on)
  h.answers.push(null)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ drop: '送信をやめました。' })
})

test('sends custom English and remembers it', async ($, on) => {
  const h = setup(on)
  h.answers.push('Please fix it', LABELS.send)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ text: 'Please fix it' })
  await submit($, 'もう一度')
  expect(h.runs[1].argv).toContain('--history')
  expect(h.writes[0].path).toBe('/tmp/plamo-ja-bridge-sess1.jsonl')
  expect(JSON.parse(h.writes[0].content.trim())).toEqual({ source: '直して', translation: 'Please fix it' })
})

test('history accumulates in ja-en order', async ($, on) => {
  const h = setup(on)
  h.answers.push(LABELS.send, LABELS.send, LABELS.send)
  await turnOn($)
  await submit($, '一つ目')
  await submit($, '二つ目')
  await submit($, '三つ目')
  const lines = h.writes[h.writes.length - 1].content.trim().split('\n').map((l) => JSON.parse(l))
  expect(lines).toEqual([
    { source: '一つ目', translation: 'EN<一つ目>' },
    { source: '二つ目', translation: 'EN<二つ目>' },
  ])
})

test('failure asks before sending original', async ($, on) => {
  const h = setup(on)
  h.translate = () => ({ exitCode: 3, stdout: '', stderr: 'error: cannot reach daemon\n' })
  h.answers.push(LABELS.original)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ text: '直して' })
  expect(h.questions).toEqual(['翻訳に失敗しました (cannot reach daemon)。どうしますか?'])
})

test('failure can be cancelled', async ($, on) => {
  const h = setup(on)
  h.translate = () => ({ exitCode: 3, stdout: '', stderr: 'error: cannot reach daemon\n' })
  h.answers.push(LABELS.cancel)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ drop: '送信をやめました。' })
})

test('context_too_long without history fails once', async ($, on) => {
  const h = setup(on)
  h.translate = () => ({ exitCode: 1, stdout: '', stderr: 'error: context_too_long: x\n' })
  h.answers.push(LABELS.cancel)
  await turnOn($)
  expect(await submit($, '長い文')).toEqual({ drop: '送信をやめました。' })
  expect(h.runs.length).toBe(1)
  expect(h.questions[0]).toBe('翻訳に失敗しました (context_too_long: x)。どうしますか?')
})

test('retries with half the history', async ($, on) => {
  const h = setup(on)
  for (let i = 0; i < 5; i++) h.answers.push(LABELS.send)
  await turnOn($)
  for (const t of ['一', '二', '三', '四']) await submit($, t)
  let calls = 0
  h.translate = (run) => {
    calls += 1
    if (calls === 1) return { exitCode: 1, stdout: '', stderr: 'error: context_too_long: x\n' }
    return { exitCode: 0, stdout: done(`EN<${run.init.stdin}>`), stderr: '' }
  }
  const before = h.writes.length
  expect(await submit($, '五')).toEqual({ text: 'EN<五>' })
  const fiveWrites = h.writes.slice(before)
  expect(fiveWrites.map((w) => w.content.trim().split('\n').length)).toEqual([4, 2])
})

test('catch drops when the hook throws', async ($, on) => {
  const h = setup(on)
  h.answers.push(LABELS.send, LABELS.send)
  await turnOn($)
  await submit($, '一つ目')
  h.failWrite = true
  const result = await submit($, '二つ目')
  expect(result).toEqual({
    drop: '日英レイヤでエラーが起きたため送信を止めました (throw)。/ja off で無効にできます。',
  })
})
