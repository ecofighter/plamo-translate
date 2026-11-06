import { expect, test } from 'claude-code/testing'
import { ABORTED, buildDocArgs, docTimeoutMs, parseDocRun, untilAborted, utf8Bytes } from '../hooks/lib/doc.js'

const line = (o: object) => JSON.stringify(o) + '\n'
const summary = (o: object = {}) =>
  line({ type: 'summary', v: 1, exit_code: 0, complete: true, degraded: 0, pieces: 2, ...o })
const run = (o: object = {}) => ({
  exitCode: 0,
  stdout: '訳文\n',
  stderr: summary(),
  isStdoutTruncated: false,
  isStderrTruncated: false,
  ...o,
})

test('buildDocArgs puts --report json and options after doc, --form only for ja, --history only with a file', () => {
  expect(buildDocArgs({ from: 'ja', to: 'en', form: 'plain' })).toEqual([
    'doc', '--report', 'json', '-s', 'ja', '-t', 'en', '--format', 'markdown',
  ])
  expect(buildDocArgs({ from: 'en', to: 'ja', form: 'plain', historyFile: '/t/h.jsonl' })).toEqual([
    'doc', '--report', 'json', '-s', 'en', '-t', 'ja', '--format', 'markdown',
    '--form', 'plain', '--history', '/t/h.jsonl',
  ])
})

test('parseDocRun accepts a complete run', () => {
  expect(parseDocRun(run())).toEqual({ ok: true, text: '訳文\n', degraded: 0, pieces: 2, notes: 0 })
})

test('parseDocRun accepts a degraded run', () => {
  const r = parseDocRun(run({ exitCode: 1, stderr: summary({ exit_code: 1, degraded: 1, pieces: 3 }) }))
  expect(r).toMatchObject({ ok: true, degraded: 1, pieces: 3 })
})

test('parseDocRun fails a stopped run with the daemon error', () => {
  const stderr =
    line({ type: 'error', kind: 'daemon', code: 'internal', message: 'boom' }) +
    summary({ exit_code: 3, complete: false })
  const r = parseDocRun(run({ exitCode: 3, stdout: '', stderr }))
  expect(r).toEqual({ ok: false, reason: 'internal: boom' })
})

test('parseDocRun marks a binary without --report as too old', () => {
  const r = parseDocRun(run({ exitCode: 2, stdout: '', stderr: "error: unexpected argument '--report' found\n" }))
  expect(r).toMatchObject({ ok: false, unsupported: true, tooOld: true })
})

test('parseDocRun fails without a summary', () => {
  expect(parseDocRun(run({ stderr: '' }))).toMatchObject({ ok: false, reason: 'no report' })
  expect(parseDocRun(run({ stderr: 'not json\n' + summary() }))).toMatchObject({ ok: false, reason: 'no report' })
  expect(parseDocRun(run({ stderr: summary() + line({ type: 'note', message: 'x' }) }))).toMatchObject({
    ok: false, reason: 'no report',
  })
})

test('parseDocRun fails when the summary exit code differs from the process', () => {
  expect(parseDocRun(run({ exitCode: 1 }))).toMatchObject({ ok: false, reason: 'no report' })
})

test('parseDocRun fails a truncated capture', () => {
  expect(parseDocRun(run({ isStdoutTruncated: true }))).toMatchObject({ ok: false, reason: 'output too large' })
  expect(parseDocRun(run({ isStderrTruncated: true }))).toMatchObject({ ok: false, reason: 'output too large' })
})

test('parseDocRun counts notes', () => {
  const stderr = line({ type: 'note', message: 'a' }) + line({ type: 'other' }) + line({ type: 'note', message: 'b' }) + summary()
  expect(parseDocRun(run({ stderr }))).toMatchObject({ ok: true, notes: 2 })
})

test('parseDocRun skips blank lines', () => {
  const stderr = line({ type: 'note', message: 'a' }) + '\n  \n' + summary() + '\n'
  expect(parseDocRun(run({ stderr }))).toMatchObject({ ok: true, notes: 1 })
})

test('parseDocRun fails an unknown report version', () => {
  expect(parseDocRun(run({ stderr: summary({ v: 2 }) }))).toEqual({ ok: false, reason: 'unsupported report version' })
})

test('docTimeoutMs scales and caps', () => {
  expect(docTimeoutMs(0)).toBe(60000)
  expect(docTimeoutMs(3000)).toBe(120000)
  expect(docTimeoutMs(1e6)).toBe(600000)
})

test('utf8Bytes counts bytes', () => {
  expect(utf8Bytes('あa')).toBe(4)
})

test('untilAborted resolves ABORTED at once when the signal aborts, and the value otherwise', async () => {
  expect(await untilAborted(Promise.resolve(7), new AbortController().signal)).toBe(7)

  const ac = new AbortController()
  const pending = untilAborted(new Promise(() => {}), ac.signal)
  ac.abort()
  expect(await pending).toBe(ABORTED)

  const already = new AbortController()
  already.abort()
  expect(await untilAborted(new Promise(() => {}), already.signal)).toBe(ABORTED)
})
