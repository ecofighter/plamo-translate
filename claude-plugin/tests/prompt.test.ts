import { expect, mock, test } from 'claude-code/testing'
import { decideSubmission, LABELS } from '../hooks/lib/input.js'
import { docTimeoutMs, utf8Bytes } from '../hooks/lib/doc.js'

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

test('context_too_long without history fails once, then once with doc', async ($, on) => {
  const h = setup(on)
  h.translate = () => ({ exitCode: 1, stdout: '', stderr: 'error: context_too_long: x\n' })
  h.answers.push(LABELS.cancel)
  await turnOn($)
  expect(await submit($, '長い文')).toEqual({ drop: '送信をやめました。' })
  expect(h.runs.length).toBe(2)
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
  toasts: string[]
  tooLong: Set<string>
  slow: (run: Run) => boolean | number
}

// The fakes answer on the mock clock: the harness holds every hook to its budget in real time.
// `slow` answers true for a 30 s run, or the run's length in milliseconds.
function setupClocked(on: any, slow: (run: Run) => boolean | number): ClockedHarness {
  const h: ClockedHarness = {
    clock: mock.clock(on),
    runs: [],
    finished: [],
    answers: [],
    toasts: [],
    tooLong: new Set(),
    slow,
  }
  mock.env(on, { TMPDIR: '/tmp/' })
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', () => ({ value: undefined }))
  on('ui.status', () => ({ value: undefined }))
  on('ui.toast', ($: unknown, e: { text: string }) => {
    h.toasts.push(e.text)
    return { value: undefined }
  })
  on('session.id', () => ({ value: 'sess1' }))
  on('fs.write', () => ({ value: undefined }))
  on('process.run', async ($: unknown, e: Run) => {
    h.runs.push(e)
    const delay = h.slow(e)
    if (delay) await h.clock.sleep(delay === true ? 30000 : delay)
    const text = e.init.stdin ?? ''
    h.finished.push(text)
    if (h.tooLong.has(text)) return { value: { exitCode: 1, stdout: '', stderr: 'error: context_too_long: x\n' } }
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
const display = ($: any, delta: string, message_id = 'm1', final = false) =>
  $.classic.MessageDisplay({
    hook_event_name: 'MessageDisplay',
    session_id: 's',
    transcript_path: '/t',
    cwd: '/w',
    turn_id: 't1',
    message_id,
    index: displayIndex++,
    final,
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

test('a takeover does not rerun a block while a prompt translates', async ($, on) => {
  const delays: Record<string, number> = { 'Slow.': 100000, 'Long.': 30000, '二つ目': 40000 }
  const h = setupClocked(on, (run) => delays[run.init.stdin ?? ''] ?? 0)
  h.tooLong.add('Long.')
  await turnOn($)
  await submit($, '一つ目')
  const busy = display($, 'Slow.\n\n', 'm0')
  await h.clock.settle()
  await h.clock.advance(2000)
  expect(await busy).toEqual({ displayContent: '' })
  const last = display($, 'Long.', 'm1', true)
  await h.clock.settle()
  const prompt = submit($, '二つ目')
  await h.clock.settle()
  await h.clock.advance(30000)
  expect(await last).toEqual({ displayContent: 'Long.' })
  expect(h.runs.filter((r) => r.init.stdin === 'Long.').length).toBe(1)
  expect(h.toasts).toEqual(['一部の段落を英語のまま表示しました (1 件: 入力の翻訳を優先)。'])
  await h.clock.advance(10000)
  expect(await prompt).toEqual({ text: 'EN<二つ目>' })
})

interface DocHarness {
  runs: Run[]
  writes: Array<{ path: string; content: string }>
  questions: Array<{ question: string; labels: string[] }>
  answers: string[]
  toasts: string[]
  logs: string[]
  statuses: Array<string | undefined>
  single: (run: Run) => { exitCode: number; stdout: string; stderr: string }
  doc: (run: Run) => { exitCode: number; stdout: string; stderr: string }
}

const reportLine = (o: object) => JSON.stringify(o) + '\n'
const docSummary = (o: object = {}) =>
  reportLine({ type: 'summary', v: 1, exit_code: 0, complete: true, degraded: 0, pieces: 1, ...o })
const docOk = (stdout: string) => ({ exitCode: 0, stdout, stderr: docSummary() })
const truncated = (text: string) =>
  JSON.stringify({ type: 'done', text, finish_reason: 'length', model: 'm', revision: 'r', usage: {} }) + '\n'

// The fake branches on the subcommand: `doc` runs answer through `doc`, single calls through `single`.
function setupDoc(on: any): DocHarness {
  const h: DocHarness = {
    runs: [],
    writes: [],
    questions: [],
    answers: [],
    toasts: [],
    logs: [],
    statuses: [],
    single: (run) => ({ exitCode: 0, stdout: done(`EN<${run.init.stdin}>`), stderr: '' }),
    doc: (run) => docOk(`DOC<${run.init.stdin}>\n`),
  }
  mock.env(on, { TMPDIR: '/tmp/' })
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', () => ({ value: undefined }))
  on('ui.status', ($: unknown, e: { text?: string }) => {
    h.statuses.push(e.text)
    return { value: undefined }
  })
  on('ui.toast', ($: unknown, e: { text: string }) => {
    h.toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.log', ($: unknown, e: { text: string; to?: string }) => {
    if (e.to !== 'debug') h.logs.push(e.text)
    return { value: undefined }
  })
  on('session.id', () => ({ value: 'sess1' }))
  on('fs.write', ($: unknown, e: { path: string; content?: string; text?: string; data?: string }) => {
    h.writes.push({ path: e.path, content: e.content ?? e.text ?? e.data ?? '' })
    return { value: undefined }
  })
  on('process.run', ($: unknown, e: Run) => {
    h.runs.push(e)
    return { value: e.argv[1] === 'doc' ? h.doc(e) : h.single(e) }
  })
  on('tool.call', ($: unknown, e: { questions: Array<{ question: string; options?: Array<{ label: string } | string> }> }) => {
    const { question, options = [] } = e.questions[0]
    h.questions.push({ question, labels: options.map((o) => (typeof o === 'string' ? o : o.label)) })
    return { result: { answers: { [question]: h.answers.shift() ?? LABELS.send } } }
  })
  on('prompt.submit', ($: unknown, e: { text: string }) => ({ text: e.text }))
  return h
}

const LONG_JA = '長い段落の文章をここに書きます。'.repeat(70)
const docRuns = (h: DocHarness) => h.runs.filter((r) => r.argv[1] === 'doc')
const lastHistory = (h: DocHarness) =>
  h.writes[h.writes.length - 1].content.trim().split('\n').map((l) => JSON.parse(l))

test('a long prompt goes to doc', async ($, on) => {
  const h = setupDoc(on)
  const english = 'The long paragraph. '.repeat(100)
  h.doc = () => docOk(english)
  await turnOn($)
  expect(utf8Bytes(LONG_JA)).toBeGreaterThan(3000)
  expect(await submit($, LONG_JA)).toEqual({ text: english })
  expect(h.runs.length).toBe(1)
  expect(h.runs[0].argv).toEqual(['plamo-translate', 'doc', '--report', 'json', '-s', 'ja', '-t', 'en', '--format', 'markdown'])
  expect(h.runs[0].init.stdin).toBe(LONG_JA)
  expect(h.runs[0].init.timeoutMs).toBe(docTimeoutMs(utf8Bytes(LONG_JA)))
  expect(h.questions[0].question).toContain('中略')
  expect(h.statuses).toContain('ja⇄en 英訳中…')
  expect(h.statuses[h.statuses.length - 1]).toBe('ja⇄en')
})

test('doc gets --history from memory, never --history-append', async ($, on) => {
  const h = setupDoc(on)
  await turnOn($)
  await submit($, '直して')
  await submit($, LONG_JA)
  const run = docRuns(h)[0]
  expect(run.argv.slice(-2)).toEqual(['--history', '/tmp/plamo-ja-bridge-sess1.jsonl'])
  expect(run.argv).not.toContain('--history-append')
  expect(run.argv).not.toContain('--history-last')
  expect(lastHistory(h)).toEqual([{ source: '直して', translation: 'EN<直して>' }])
})

test('a truncated translation falls back to doc', async ($, on) => {
  const h = setupDoc(on)
  h.single = () => ({ exitCode: 0, stdout: truncated('Fix'), stderr: '' })
  h.doc = () => docOk('Fix it.\n\n```\nx()\n```\n')
  await turnOn($)
  const text = '直して\n\n```\nx()\n```\n'
  expect(await submit($, text)).toEqual({ text: 'Fix it.\n\n```\nx()\n```\n' })
  expect(h.runs.map((r) => r.argv[1])).toEqual(['--json', 'doc'])
  expect(docRuns(h)[0].init.stdin).toBe(text)
})

test('a partial doc translation always asks, even with confirm off', { options: { confirm: false } }, async ($, on) => {
  const h = setupDoc(on)
  h.doc = () => ({
    exitCode: 1,
    stdout: 'Partly translated.\n',
    stderr: reportLine({ type: 'note', message: 'kept as code' }) + docSummary({ exit_code: 1, degraded: 1, pieces: 3 }),
  })
  h.answers.push(LABELS.send)
  await turnOn($)
  expect(await submit($, LONG_JA)).toEqual({ text: 'Partly translated.\n' })
  expect(h.questions.length).toBe(1)
  expect(h.questions[0].question).toBe(
    '一部 (1/3 ピース) は英訳しきれていません (日本語のまま、または訳が途中で切れています)。\n' +
      '1 か所 (コード扱いのブロックなど) は訳さずに残しました。\n\n' +
      CONFIRM_Q('Partly translated.\n'),
  )
  expect(h.questions[0].labels).toEqual([LABELS.send, LABELS.original, LABELS.cancel])

  h.doc = () => docOk('Clean.\n')
  expect(await submit($, LONG_JA)).toEqual({ text: 'Clean.\n' })
  expect(h.questions.length).toBe(1)
})

test('doc stopping mid-run asks to send the original with the single-call reason', async ($, on) => {
  const h = setupDoc(on)
  h.single = () => ({ exitCode: 0, stdout: truncated('Fix'), stderr: '' })
  h.doc = () => ({
    exitCode: 3,
    stdout: 'First piece.\n',
    stderr:
      reportLine({ type: 'error', kind: 'daemon', code: 'internal', message: 'boom' }) +
      docSummary({ exit_code: 3, complete: false }),
  })
  h.answers.push(LABELS.original, LABELS.cancel)
  await turnOn($)
  expect(await submit($, '直して')).toEqual({ text: '直して' })
  expect(h.questions[0].question).toBe('翻訳に失敗しました (finish_reason length)。どうしますか?')
  expect(h.questions[0].labels).toEqual([LABELS.original, LABELS.cancel])

  expect(await submit($, LONG_JA)).toEqual({ drop: '送信をやめました。' })
  expect(h.questions[1].question).toBe('翻訳に失敗しました (internal: boom)。どうしますか?')
  expect(h.questions[1].labels).toEqual([LABELS.original, LABELS.cancel])
})

test('an old CLI disables doc for the session', async ($, on) => {
  const h = setupDoc(on)
  h.doc = () => ({ exitCode: 2, stdout: '', stderr: "error: unexpected argument '--report' found\n" })
  await turnOn($)
  expect(await submit($, LONG_JA)).toEqual({ text: `EN<${LONG_JA}>` })
  expect(await submit($, LONG_JA)).toEqual({ text: `EN<${LONG_JA}>` })
  h.single = () => ({ exitCode: 0, stdout: truncated('Fix'), stderr: '' })
  h.answers.push(LABELS.cancel)
  expect(await submit($, '直して')).toEqual({ drop: '送信をやめました。' })
  expect(h.questions[2].question).toBe('翻訳に失敗しました (finish_reason length)。どうしますか?')
  expect(docRuns(h).length).toBe(1)
  expect(h.toasts).toEqual(['長文の分割翻訳には plamo-translate 0.2.0 以降が必要です。更新してください。'])

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  h.answers.push(LABELS.cancel)
  await submit($, '直して')
  expect(docRuns(h).length).toBe(2)
  expect(h.toasts.length).toBe(2)
})

test('a small clean doc result is remembered, a large one is not', async ($, on) => {
  const h = setupDoc(on)
  await turnOn($)
  h.doc = () => docOk('A long paragraph.\n')
  await submit($, LONG_JA)
  h.doc = () => docOk('Too long. '.repeat(250))
  await submit($, LONG_JA + '二つ目')
  h.doc = () => ({ exitCode: 1, stdout: 'Partly.\n', stderr: docSummary({ exit_code: 1, degraded: 1, pieces: 3 }) })
  await submit($, LONG_JA + '三つ目')
  await submit($, '次')
  expect(lastHistory(h)).toEqual([{ source: LONG_JA, translation: 'A long paragraph.' }])
})

test('useDoc false keeps the per-segment path', { options: { useDoc: false } }, async ($, on) => {
  const h = setupDoc(on)
  await turnOn($)
  expect(await submit($, LONG_JA)).toEqual({ text: `EN<${LONG_JA}>` })
  h.single = () => ({ exitCode: 0, stdout: truncated('Fix'), stderr: '' })
  h.answers.push(LABELS.cancel)
  expect(await submit($, '直して')).toEqual({ drop: '送信をやめました。' })
  expect(h.questions[1].question).toBe('翻訳に失敗しました (finish_reason length)。どうしますか?')
  expect(docRuns(h).length).toBe(0)
})

test('the confirm dialog offers the full text for a long translation', async ($, on) => {
  const h = setupDoc(on)
  const english = Array.from({ length: 400 }, (_, i) => `Sentence ${i}.`).join(' ')
  h.single = () => ({ exitCode: 0, stdout: done(english), stderr: '' })
  h.answers.push('全文を表示', LABELS.send)
  await turnOn($)
  expect(english.length).toBeGreaterThan(1900 * 2)
  expect(await submit($, '直して')).toEqual({ text: english })
  expect(h.questions.length).toBe(2)
  expect(h.questions[0].question).toContain('中略')
  expect(h.questions[0].labels).toEqual([LABELS.send, LABELS.original, LABELS.cancel, '全文を表示'])
  expect(h.questions[1].labels).toEqual([LABELS.send, LABELS.original, LABELS.cancel])
  expect(h.logs.join('')).toBe(english)
  expect(h.logs.length).toBe(Math.ceil(english.length / 1900))
  for (const chunk of h.logs) expect(chunk.length).toBeLessThanOrEqual(1900)
})

// A hook above that settles first, as the user's Esc does: the prompt dispatch beneath it aborts.
const interrupter = {
  name: 'interrupter',
  tier: 'prepend' as const,
  register: (on: any) => {
    on('prompt.submit', async ($: any, e: unknown, next: any) => {
      const inner = next(e)
      inner.catch(() => {})
      return Promise.race([inner, $.clock.sleep(1000).then(() => ({ drop: 'interrupted' }))])
    })
  },
}

test('an aborted long prompt resumes the display before doc ends', { plugins: [interrupter] }, async ($, on) => {
  const h = setupClocked(on, (run) => (run.argv[1] === 'doc' ? 300000 : 0))
  await turnOn($)
  const prompt = submit($, LONG_JA)
  await h.clock.settle()
  const shown = display($, 'A.\n\n')
  await h.clock.settle()
  expect(h.runs.map((r) => r.init.stdin)).toEqual([LONG_JA])
  await h.clock.advance(1000)
  expect(await prompt).toEqual({ drop: 'interrupted' })
  await h.clock.settle()
  expect(h.runs.map((r) => r.init.stdin)).toEqual([LONG_JA, 'A.'])
  expect(h.finished).not.toContain(LONG_JA)
  await h.clock.advance(1000)
  expect(await shown).toEqual({ displayContent: 'JA:A.\n\n' })
})
