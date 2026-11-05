import { expect, test } from 'claude-code/testing'
import { buildArgs, parseResult } from '../hooks/lib/cli.js'

const done = (text: string, finish_reason = 'stop') =>
  JSON.stringify({ type: 'done', text, finish_reason, model: 'm', revision: 'r', usage: {} }) + '\n'

test('buildArgs orders flags and adds history', () => {
  expect(buildArgs({ from: 'ja', to: 'en', historyFile: '/t/h.jsonl' })).toEqual([
    '--json', '--no-stream', '-s', 'ja', '-t', 'en', '--history', '/t/h.jsonl',
  ])
})

test('form applies only when translating to Japanese', () => {
  expect(buildArgs({ from: 'ja', to: 'en', form: 'plain' })).toEqual(['--json', '--no-stream', '-s', 'ja', '-t', 'en'])
  expect(buildArgs({ from: 'en', to: 'ja', form: 'plain' })).toEqual([
    '--json', '--no-stream', '-s', 'en', '-t', 'ja', '--form', 'plain',
  ])
})

test('a successful run parses text and finish reason', () => {
  expect(parseResult({ exitCode: 0, stdout: done('こんにちは'), stderr: '' })).toEqual({
    ok: true, text: 'こんにちは', finishReason: 'stop',
  })
})

test('context_too_long is recognised from stderr', () => {
  const r = parseResult({ exitCode: 1, stdout: '', stderr: 'error: context_too_long: prompt too long\n' })
  expect(r).toMatchObject({ ok: false, contextTooLong: true })
})

test('other failures are not context_too_long', () => {
  expect(parseResult({ exitCode: 3, stdout: '', stderr: 'error: cannot reach daemon' })).toMatchObject({
    ok: false, contextTooLong: false,
  })
  expect(parseResult({ exitCode: 0, stdout: 'not json', stderr: '' })).toMatchObject({ ok: false })
  expect(parseResult({ exitCode: 0, stdout: done('half', 'length'), stderr: '' })).toMatchObject({ ok: false })
  expect(parseResult({ exitCode: 0, stdout: done('  '), stderr: '' })).toMatchObject({ ok: false })
  expect(parseResult({ exitCode: 0, stdout: done('a <|plamo:msg|>'), stderr: '' })).toMatchObject({ ok: false })
})

test('failures carry a reason', () => {
  const r = parseResult({ exitCode: 3, stdout: '', stderr: 'error: cannot reach daemon\n' })
  expect(r.ok).toBe(false)
  expect((r as { reason: string }).reason).toContain('cannot reach daemon')
})
