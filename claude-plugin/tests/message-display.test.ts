import { expect, mock, test } from 'claude-code/testing'

type Run = { argv: string[]; init: { stdin?: string; timeoutMs?: number } }

const done = (text: string) =>
  JSON.stringify({ type: 'done', text, finish_reason: 'stop', model: 'm', revision: 'r', usage: {} }) + '\n'

interface Harness {
  runs: Run[]
  writes: Array<{ path: string; content: string }>
  translate: (run: Run) => { exitCode: number; stdout: string; stderr: string }
  failWrite: boolean
  events: string[]
}

function setup(on: any): Harness {
  const h: Harness = {
    runs: [],
    writes: [],
    translate: (run) => ({ exitCode: 0, stdout: done(`JA:${run.init.stdin}`), stderr: '' }),
    failWrite: false,
    events: [],
  }
  mock.env(on, { TMPDIR: '/tmp' })
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', () => ({ value: undefined }))
  on('ui.status', () => ({ value: undefined }))
  on('session.id', () => ({ value: 'sess1' }))
  on('fs.write', ($: unknown, e: { path: string; content?: string; text?: string }) => {
    if (h.failWrite) return { deny: 'disk full' }
    h.events.push('write')
    h.writes.push({ path: e.path, content: e.content ?? e.text ?? '' })
    return { value: undefined }
  })
  on('process.run', ($: unknown, e: Run) => {
    h.events.push('run')
    h.runs.push(e)
    return { value: h.translate(e) }
  })
  on('classic.MessageDisplay', () => ({}))
  return h
}

async function turnOn($: any) {
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await $.command.run({ command: 'ja', args: 'on' })
}

let index = 0
const show = ($: any, message_id: string, delta: string, final = false) =>
  $.classic.MessageDisplay({
    hook_event_name: 'MessageDisplay',
    session_id: 's',
    transcript_path: '/t',
    cwd: '/w',
    turn_id: 't1',
    message_id,
    index: index++,
    final,
    delta,
  })

test('paragraph translated when complete', async ($, on) => {
  setup(on)
  await turnOn($)
  expect(await show($, 'm1', 'Hello.\n')).toEqual({ displayContent: '' })
  expect(await show($, 'm1', '\n')).toEqual({ displayContent: 'JA:Hello.\n\n' })
})

test('fence shown verbatim', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  expect(await show($, 'm1', '```\ncode\n```\n')).toEqual({ displayContent: '```\ncode\n```\n' })
  expect(h.runs.length).toBe(0)
})

test('final flushes and clears', async ($, on) => {
  setup(on)
  await turnOn($)
  await show($, 'm1', 'Held.\n')
  expect(await show($, 'm1', 'Tail', true)).toEqual({ displayContent: 'JA:Held.\nTail' })
  expect(await show($, 'm1', 'Next.', true)).toEqual({ displayContent: 'JA:Next.' })
})

test('failure shows original', async ($, on) => {
  const h = setup(on)
  h.translate = () => ({ exitCode: 1, stdout: '', stderr: 'error: model_load_failed: x\n' })
  await turnOn($)
  expect(await show($, 'm1', 'Hello.\n\n')).toEqual({ displayContent: 'Hello.\n\n' })
})

test('history write failure shows original', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  await show($, 'm1', 'First.\n\n')
  h.failWrite = true
  expect(await show($, 'm1', 'Second.\n\n')).toEqual({ displayContent: 'Second.\n\n' })
})

test('uses en-ja history and timeout', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  await show($, 'm1', 'First.\n\nSecond.\n\n')
  expect(h.runs[0].argv).toEqual(['plamo-translate', '--json', '--no-stream', '-s', 'en', '-t', 'ja'])
  expect(h.runs[0].init.timeoutMs).toBe(20000)
  expect(h.runs[1].argv.slice(-2)).toEqual(['--history', '/tmp/plamo-ja-bridge-sess1.jsonl'])
  expect(JSON.parse(h.writes[0].content.trim())).toEqual({ source: 'First.', translation: 'JA:First.' })
})

test('table with a changed shape shows the original', async ($, on) => {
  const h = setup(on)
  h.translate = () => ({ exitCode: 0, stdout: done('| あ い |\n|---|---|'), stderr: '' })
  await turnOn($)
  const table = '| a | b |\n|---|---|\n'
  expect(await show($, 'm1', table + '\n')).toEqual({ displayContent: table + '\n' })
})

test('interleaved messages keep separate buffers', async ($, on) => {
  setup(on)
  await turnOn($)
  await show($, 'm1', 'A.\n')
  await show($, 'm2', 'B.\n')
  expect(await show($, 'm1', '\n')).toEqual({ displayContent: 'JA:A.\n\n' })
  expect(await show($, 'm2', '\n')).toEqual({ displayContent: 'JA:B.\n\n' })
})

test('off passes through', async ($, on) => {
  const h = setup(on)
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  expect(await show($, 'm1', 'Hello.\n\n')).toEqual({})
  expect(h.runs.length).toBe(0)
})

test('disabling mid-message flushes held text as original', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  expect(await show($, 'm1', 'Hello.\n')).toEqual({ displayContent: '' })
  await $.command.run({ command: 'ja', args: 'off' })
  expect(await show($, 'm1', 'World.\n')).toEqual({ displayContent: 'Hello.\nWorld.\n' })
  expect(await show($, 'm1', 'Again.\n')).toEqual({})
  expect(h.runs.length).toBe(0)
})

test('Japanese blocks are shown without translation', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  expect(await show($, 'm1', 'すでに日本語です。\n\n')).toEqual({ displayContent: 'すでに日本語です。\n\n' })
  expect(h.runs.length).toBe(0)
})

test('English with a few Japanese words is translated', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  expect(await show($, 'm1', 'The `取得` helper returns None when the cache is empty.\n\n')).toEqual({
    displayContent: 'JA:The `取得` helper returns None when the cache is empty.\n\n',
  })
  expect(h.runs.length).toBe(1)
})

test('concurrent batches translate one at a time', async ($, on) => {
  const h = setup(on)
  await turnOn($)
  await show($, 'm1', 'Seed.\n\n')
  h.events.length = 0
  const [a, b] = await Promise.all([show($, 'm1', 'First.\n\n'), show($, 'm1', 'Second.\n\n')])
  expect([a, b]).toEqual([{ displayContent: 'JA:First.\n\n' }, { displayContent: 'JA:Second.\n\n' }])
  expect(h.events).toEqual(['write', 'run', 'write', 'run'])
})

test('/clear forgets the translation memory', async ($, on) => {
  const h = setup(on)
  on('session.end', () => ({ sessionId: 'sess1' }))
  await turnOn($)
  await show($, 'm1', 'Remember me.\n\n')
  await $.session.end({ reason: 'clear' })
  expect((await $.command.run({ command: 'ja', args: 'status' })).text).toBe(
    '日英レイヤ: オン / 対訳 0 組 / 翻訳: plamo-translate',
  )
  await show($, 'm2', 'Fresh.\n\n')
  expect(h.runs[h.runs.length - 1].argv).not.toContain('--history')
})

interface ClockedHarness {
  clock: ReturnType<typeof mock.clock>
  runs: Run[]
  events: string[]
  toasts: string[]
  logs: string[]
  slow: Set<string>
  failing: Set<string>
}

// The fakes answer on the mock clock: the harness holds every hook to its budget in real time.
function setupClocked(on: any): ClockedHarness {
  const h: ClockedHarness = {
    clock: mock.clock(on),
    runs: [],
    events: [],
    toasts: [],
    logs: [],
    slow: new Set(['Slow.']),
    failing: new Set(),
  }
  mock.env(on, { TMPDIR: '/tmp' })
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', () => ({ value: undefined }))
  on('command.list', () => ({ value: [{ name: 'ja', description: '' }] }))
  on('turn.start', ($: unknown, e: { turnId: string }) => ({ turnId: e.turnId }))
  on('ui.status', () => ({ value: undefined }))
  on('ui.toast', ($: unknown, e: { text: string }) => {
    h.toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.log', ($: unknown, e: { text: string }) => {
    h.logs.push(e.text)
    return { value: undefined }
  })
  on('session.id', () => ({ value: 'sess1' }))
  on('fs.write', () => {
    h.events.push('write')
    return { value: undefined }
  })
  on('process.run', async ($: unknown, e: Run) => {
    h.events.push('run')
    h.runs.push(e)
    const text = e.init.stdin ?? ''
    if (h.slow.has(text)) await h.clock.sleep(30000)
    if (h.failing.has(text)) return { value: { exitCode: 1, stdout: '', stderr: 'error: model_load_failed: x\n' } }
    return { value: { exitCode: 0, stdout: done(`JA:${text}`), stderr: '' } }
  })
  on('classic.MessageDisplay', () => ({}))
  return h
}

async function elapse(h: ClockedHarness, ms: number) {
  await h.clock.settle()
  await h.clock.advance(ms)
}

const ranTexts = (h: ClockedHarness) => h.runs.map((r) => r.init.stdin)

test('a slow block holds later batches instead of letting them through untranslated', async ($, on) => {
  const h = setupClocked(on)
  await turnOn($)
  const first = show($, 'm1', 'Slow.\n\n')
  await elapse(h, 2000)
  expect(await first).toEqual({ displayContent: '' })
  const second = show($, 'm1', 'Next.\n\n')
  await elapse(h, 2000)
  expect(await second).toEqual({ displayContent: '' })
  await elapse(h, 30000)
  expect(await show($, 'm1', '', true)).toEqual({ displayContent: 'JA:Slow.\n\nJA:Next.\n\n' })
})

test('the final batch translates queued blocks itself', async ($, on) => {
  const h = setupClocked(on)
  await turnOn($)
  const inFlight = show($, 'm0', 'Slow.\n\n')
  await elapse(h, 2000)
  expect(await inFlight).toEqual({ displayContent: '' })
  const queued = show($, 'm1', 'A.\n\n')
  await elapse(h, 2000)
  expect(await queued).toEqual({ displayContent: '' })
  expect(await show($, 'm1', 'B.', true)).toEqual({ displayContent: 'JA:A.\n\nJA:B.' })
  expect(ranTexts(h)).toEqual(['Slow.', 'A.', 'B.'])
})

test('the final batch shows an in-flight block that misses its deadline as written, with a notice', async ($, on) => {
  const h = setupClocked(on)
  await turnOn($)
  const first = show($, 'm1', 'Slow.\n\n')
  await elapse(h, 2000)
  expect(await first).toEqual({ displayContent: '' })
  const last = show($, 'm1', '', true)
  await elapse(h, 8000)
  expect(await last).toEqual({ displayContent: 'Slow.\n\n' })
  expect(h.toasts.length).toBe(1)
  expect(h.toasts[0]).toContain('英語のまま')
  expect(ranTexts(h).filter((t) => t === 'Slow.').length).toBe(1)
})

test('a failed block is shown as written with a notice', async ($, on) => {
  const h = setupClocked(on)
  h.failing.add('Bad.')
  await turnOn($)
  expect(await show($, 'm1', 'Bad.\n\n')).toEqual({ displayContent: 'Bad.\n\n' })
  expect(h.toasts).toEqual([])
  expect(await show($, 'm1', 'Good.', true)).toEqual({ displayContent: 'JA:Good.' })
  expect(h.toasts).toEqual(['一部の段落を英語のまま表示しました (1 件: model_load_failed: x)。'])
})

test('disabling mid-message shows pending blocks as written', async ($, on) => {
  const h = setupClocked(on)
  await turnOn($)
  const first = show($, 'm1', 'Slow.\n\n')
  await elapse(h, 2000)
  expect(await first).toEqual({ displayContent: '' })
  await $.command.run({ command: 'ja', args: 'off' })
  expect(await show($, 'm1', 'More.\n')).toEqual({ displayContent: 'Slow.\n\nMore.\n' })
  await elapse(h, 30000)
  expect(await show($, 'm1', 'Last.', true)).toEqual({})
  expect(ranTexts(h)).toEqual(['Slow.'])
})

test("a new turn drops an unfinished message's queued blocks", async ($, on) => {
  const h = setupClocked(on)
  await turnOn($)
  const first = show($, 'm1', 'Slow.\n\n')
  await elapse(h, 2000)
  expect(await first).toEqual({ displayContent: '' })
  const second = show($, 'm1', 'A.\n\n')
  await elapse(h, 2000)
  expect(await second).toEqual({ displayContent: '' })
  await $.turn.start({ text: '', turnId: 't2' })
  await elapse(h, 30000)
  expect(ranTexts(h)).toEqual(['Slow.'])
})

test('a repeated session start keeps one worker', async ($, on) => {
  const h = setup(on)
  const logs: string[] = []
  on('ui.log', ($: unknown, e: { text: string }) => {
    logs.push(e.text)
    return { value: undefined }
  })
  await turnOn($)
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await show($, 'm1', 'Seed.\n\n')
  h.events.length = 0
  const [a, b] = await Promise.all([show($, 'm1', 'First.\n\n'), show($, 'm1', 'Second.\n\n')])
  expect([a, b]).toEqual([{ displayContent: 'JA:First.\n\n' }, { displayContent: 'JA:Second.\n\n' }])
  expect(h.events).toEqual(['write', 'run', 'write', 'run'])
  expect(logs.filter((l) => l.includes('display worker superseded')).length).toBe(1)
})

test('a failed worker job leaves a debug line', async ($, on) => {
  const h = setupClocked(on)
  h.failing.add('Bad.')
  await turnOn($)
  expect(await show($, 'm1', 'Bad.\n\n')).toEqual({ displayContent: 'Bad.\n\n' })
  expect(h.logs).toContain('plamo-ja-bridge: worker job failed: model_load_failed: x')
})

test('a rescued batch leaves a debug line and a notice', async ($, on) => {
  const h = setupClocked(on)
  on('state.get', () => ({ deny: 'broken' }))
  expect(await show($, 'm1', 'Hello.\n\n')).toEqual({ displayContent: 'Hello.\n\n' })
  expect(h.toasts).toEqual(['表示の翻訳でエラーが起きたため英語のまま表示しました。'])
  expect(h.logs.some((l) => l.includes('rescue'))).toBe(true)
})
