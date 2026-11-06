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
  runDeny?: string
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
    if (h.runDeny) return { deny: h.runDeny }
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

test('prompts the user did not type pass through', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  for (const kind of ['peer', 'task-notification', 'scheduled-trigger', 'unclassified']) {
    const e = { text: '計画の報告です', origin: { kind } }
    expect(await $.prompt.submit(e)).toEqual({ text: e.text })
  }
  expect(h.runs.length).toBe(0)
  expect(h.questions.length).toBe(0)
})

test('prompts the user typed are translated', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  for (const kind of ['composer', 'bridge', 'sdk']) {
    h.answers.push(LABELS.send)
    expect(await $.prompt.submit({ text: '直して', origin: { kind } })).toEqual({ text: 'EN<直して>' })
  }
  expect(h.questions.length).toBe(3)
})

test('meta prompts pass through', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  expect(await $.prompt.submit({ text: '直して', isMeta: true })).toEqual({ text: '直して' })
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

test('a translator that cannot start reports why', async ($, on) => {
  const h = setup(on)
  h.runDeny = 'spawn plamo-translate ENOENT'
  h.answers.push(LABELS.cancel)
  await turnOn($)
  await submit($, '直して')
  expect(h.questions[0]).toContain('ENOENT')
})

test('custom English keeps the prompt fences', async ($, on) => {
  const h = setup(on)
  h.answers.push('Fix this')
  await turnOn($)
  expect(await submit($, '直して\n```\nlog\n```\n')).toEqual({ text: 'Fix this\n\n```\nlog\n```\n' })
})

interface ClockedHarness {
  clock: ReturnType<typeof mock.clock>
  runs: Run[]
  finished: string[]
  answers: string[]
  slow: (run: Run) => boolean
}

// The fakes answer on the mock clock: the harness holds every hook to its budget in real time.
function setupClocked(on: any, slow: (run: Run) => boolean): ClockedHarness {
  const h: ClockedHarness = { clock: mock.clock(on), runs: [], finished: [], answers: [], slow }
  mock.env(on, { TMPDIR: '/tmp/' })
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', () => ({ value: undefined }))
  on('ui.status', () => ({ value: undefined }))
  on('session.id', () => ({ value: 'sess1' }))
  on('fs.write', () => ({ value: undefined }))
  on('process.run', async ($: unknown, e: Run) => {
    h.runs.push(e)
    if (h.slow(e)) await h.clock.sleep(30000)
    const text = e.init.stdin ?? ''
    h.finished.push(text)
    const translation = e.argv[e.argv.indexOf('-s') + 1] === 'ja' ? `EN<${text}>` : `JA:${text}`
    return { value: { exitCode: 0, stdout: done(translation), stderr: '' } }
  })
  on('tool.call', ($: unknown, e: { questions: Array<{ question: string }> }) => {
    const question = e.questions[0].question
    return { result: { answers: { [question]: h.answers.shift() ?? LABELS.send } } }
  })
  on('prompt.submit', ($: unknown, e: { text: string }) => ({ text: e.text }))
  on('classic.MessageDisplay', () => ({}))
  return h
}

let displayIndex = 0
const display = ($: any, delta: string) =>
  $.classic.MessageDisplay({
    hook_event_name: 'MessageDisplay',
    session_id: 's',
    transcript_path: '/t',
    cwd: '/w',
    turn_id: 't1',
    message_id: 'm1',
    index: displayIndex++,
    final: false,
    delta,
  })

test('a prompt does not wait behind display translations', async ($, on) => {
  const h = setupClocked(on, (run) => run.init.stdin === 'Slow.')
  await turnOn($)
  await submit($, '一つ目')
  const shown = display($, 'Slow.\n\n')
  await h.clock.settle()
  expect(await submit($, '二つ目')).toEqual({ text: 'EN<二つ目>' })
  expect(h.finished).not.toContain('Slow.')
  const prompt = h.runs.find((r) => r.init.stdin === '二つ目')!
  expect(prompt.argv.slice(-2)).toEqual(['--history', '/tmp/plamo-ja-bridge-sess1-1.jsonl'])
  await h.clock.advance(2000)
  expect(await shown).toEqual({ displayContent: '' })
})

test('a prompt translation pauses the display worker', async ($, on) => {
  const h = setupClocked(on, (run) => run.init.stdin === '直して')
  await turnOn($)
  const prompt = submit($, '直して')
  await h.clock.settle()
  const shown = display($, 'A.\n\n')
  await h.clock.settle()
  await h.clock.advance(2000)
  expect(await shown).toEqual({ displayContent: '' })
  expect(h.runs.map((r) => r.init.stdin)).toEqual(['直して'])
  await h.clock.advance(30000)
  expect(await prompt).toEqual({ text: 'EN<直して>' })
  await h.clock.settle()
  expect(h.runs.map((r) => r.init.stdin)).toEqual(['直して', 'A.'])
})
