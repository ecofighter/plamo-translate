import { expect, test } from 'claude-code/testing'
import {
  abandon,
  arrive,
  cancelQueued,
  claimAll,
  createPipeline,
  finalTake,
  finishJob,
  MIN_CALL_MS,
  OUTPUT_DOC_BYTES,
  OUTPUT_FINAL_MAX_MS,
  popIfIdle,
  takeoverTimeoutMs,
  takeReady,
} from '../hooks/lib/pipeline.js'

const pipeline = () => createPipeline({ flushLines: 20 })
const delta = (messageId: string, text: string, final = false, key: unknown = text) => ({
  messageId,
  key,
  delta: text,
  final,
  turnId: 1,
})
const translated = (text: string) => ({ text: `JA:${text}` })
const drain = (p: ReturnType<typeof pipeline>) => {
  for (let job = popIfIdle(p); job; job = popIfIdle(p)) finishJob(p, job, translated(job.slot.block.text))
}

test('deltas are pushed in arrival order and index gaps do not stall', async () => {
  const p = pipeline()
  const d1 = arrive(p, delta('m1', 'One.\n\n', false, 7))
  const d2 = arrive(p, delta('m1', 'Two.\n\n', false, 42))
  expect([d1.seq, d2.seq]).toEqual([0, 1])
  expect(p.queue.map((j) => j.slot.block.text)).toEqual(['One.\n', 'Two.\n'])

  let secondMayTake = false
  d2.prevTurn.then(() => (secondMayTake = true))
  await d1.prevTurn
  await Promise.resolve()
  expect(secondMayTake).toBe(false)
  d1.finishTurn()
  await d2.prevTurn
  expect(secondMayTake).toBe(true)

  drain(p)
  expect(takeReady(d1)).toBe('JA:One.\n\nJA:Two.\n\n')
  expect(takeReady(d2)).toBe('')
})

test('takeReady returns the longest ready prefix once', () => {
  const p = pipeline()
  const d = arrive(p, delta('m1', '```\nx\n```\nFirst.\n\nSecond.\n\n'))
  expect(takeReady(d)).toBe('```\nx\n```\n')
  const first = popIfIdle(p)!
  const second = popIfIdle(p)
  expect(second).toBe(null)
  finishJob(p, first, translated(first.slot.block.text))
  expect(takeReady(d)).toBe('JA:First.\n\n')
  expect(takeReady(d)).toBe('')
  const last = popIfIdle(p)!
  finishJob(p, last, translated(last.slot.block.text))
  expect(takeReady(d)).toBe('JA:Second.\n\n')
  expect(takeReady(d)).toBe('')
})

test('claimAll returns originals and later takes return nothing', () => {
  const p = pipeline()
  const d = arrive(p, delta('m1', 'Done.\n\nRunning.\n\nQueued.\n\nheld'))
  const done = popIfIdle(p)!
  finishJob(p, done, translated(done.slot.block.text))
  const running = popIfIdle(p)!

  expect(claimAll(p, d.state)).toBe('Done.\n\nRunning.\n\nQueued.\n\n')
  expect(p.queue).toEqual([])
  finishJob(p, running, translated(running.slot.block.text))
  expect(takeReady(d)).toBe('')
  expect(finalTake(p, d).text).toBe('held')
})

test('an abandoned dispatch takes nothing', () => {
  const p = pipeline()
  const d1 = arrive(p, delta('m1', '```\nx\n```\n'))
  const d2 = arrive(p, delta('m1', 'Later.\n\n'))
  abandon(d1.state, d1.key)
  expect(d1.abandoned).toBe(true)
  expect(d2.abandoned).toBe(false)
  expect(takeReady(d1)).toBe(null)
  expect(takeReady(d2)).toBe('```\nx\n```\n')
})

test('finalTake returns every un-emitted slot, originals for unsettled ones', () => {
  const p = pipeline()
  const d1 = arrive(p, delta('m1', 'Shown.\n\n'))
  drain(p)
  expect(takeReady(d1)).toBe('JA:Shown.\n\n')

  arrive(p, delta('m1', 'Failed.\n\nRunning.\n\nQueued.\n\n'))
  const failed = popIfIdle(p)!
  finishJob(p, failed, { text: failed.slot.block.text, failReason: 'timeout' })
  const running = popIfIdle(p)!
  const last = arrive(p, delta('m1', 'Tail', true))

  const taken = finalTake(p, last)
  expect(taken.text).toBe('Failed.\n\nRunning.\n\nQueued.\n\nTail')
  expect(taken.failures).toEqual(['timeout', '間に合わず', '間に合わず', '間に合わず'])
  expect(p.messages.has('m1')).toBe(false)
  expect(p.queue).toEqual([])

  finishJob(p, running, translated(running.slot.block.text))
  expect(p.running).toBe(null)
  expect(finalTake(p, last).text).toBe('')
})

test("cancelQueued removes only this message's queued jobs", () => {
  const p = pipeline()
  const a = arrive(p, delta('m1', 'A1.\n\nA2.\n\n'))
  arrive(p, delta('m2', 'B1.\n\n'))
  const heavy = arrive(p, delta('m1', 'x'.repeat(OUTPUT_DOC_BYTES + 1) + '\n\n'))
  const running = popIfIdle(p)!
  expect(running.slot.block.text).toBe('A1.\n')

  const claimed = cancelQueued(p, a.state)
  expect(claimed.map((s) => s.block.text)).toEqual(['A2.\n', heavy.slots[0].block.text])
  expect(claimed.every((s) => s.state === 'claimed')).toBe(true)
  expect(p.queue.map((j) => j.messageId)).toEqual(['m2'])
  expect(running.slot.state).toBe('running')
})

test('popIfIdle pops only when nothing runs and no input or takeover is active', () => {
  const p = pipeline()
  arrive(p, delta('m1', 'x'.repeat(OUTPUT_DOC_BYTES + 1) + '\n\nSmall.\n\nOther.\n\n'))

  p.inputActive = 1
  expect(popIfIdle(p)).toBe(null)
  p.inputActive = 0
  p.takeoverActive = 1
  expect(popIfIdle(p)).toBe(null)
  p.takeoverActive = 0

  const job = popIfIdle(p)!
  expect(job.slot.block.text).toBe('Small.\n')
  expect(job.slot.state).toBe('running')
  expect(p.running).toBe(job)
  expect(popIfIdle(p)).toBe(null)

  finishJob(p, job, translated(job.slot.block.text))
  expect(popIfIdle(p)!.slot.block.text).toBe('Other.\n')
  finishJob(p, p.running!, translated('Other.\n'))
  expect(popIfIdle(p)).toBe(null)
  expect(p.queue.map((j) => j.slot.heavy)).toEqual([true])
})

test('takeoverTimeoutMs caps each call at the remaining takeover time and stops below MIN_CALL_MS', () => {
  const started = 1_000_000
  expect(takeoverTimeoutMs(started, started, 20000)).toBe(20000)
  expect(takeoverTimeoutMs(started, started + 50000, 20000)).toBe(OUTPUT_FINAL_MAX_MS - 50000)
  expect(takeoverTimeoutMs(started, started + 10000, 120000)).toBe(50000)
  expect(takeoverTimeoutMs(started, started + OUTPUT_FINAL_MAX_MS - MIN_CALL_MS, 20000)).toBe(MIN_CALL_MS)
  expect(takeoverTimeoutMs(started, started + OUTPUT_FINAL_MAX_MS - MIN_CALL_MS + 1, 20000)).toBe(null)
  expect(takeoverTimeoutMs(started, started + OUTPUT_FINAL_MAX_MS + 5000, 20000)).toBe(null)
})
