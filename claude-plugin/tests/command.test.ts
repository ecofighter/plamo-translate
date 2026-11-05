import { expect, test } from 'claude-code/testing'

type On = (name: string, fn: (...args: any[]) => unknown) => void

function stubSession(on: On, statuses: Array<string | undefined>, registered: string[]) {
  on('session.start', () => ({ cwd: '/work' }))
  on('command.register', ($: unknown, e: { name: string }) => {
    registered.push(e.name)
    return { value: undefined }
  })
  on('ui.status', ($: unknown, e: { text?: string }) => {
    statuses.push(e.text)
    return { value: undefined }
  })
}

const start = ($: any) => $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
const ja = ($: any, args = '') => $.command.run({ command: 'ja', args })

test('session start registers /ja and starts off', async ($, on) => {
  const statuses: Array<string | undefined> = []
  const registered: string[] = []
  stubSession(on, statuses, registered)
  await start($)
  expect(registered).toEqual(['ja'])
  expect(statuses[statuses.length - 1]).toBeUndefined()
})

test('/ja toggles on and off', async ($, on) => {
  const statuses: Array<string | undefined> = []
  stubSession(on, statuses, [])
  await start($)
  expect((await ja($)).text).toBe('日英レイヤ: オン')
  expect(statuses[statuses.length - 1]).toBe('ja⇄en')
  expect((await ja($)).text).toBe('日英レイヤ: オフ')
  expect(statuses[statuses.length - 1]).toBeUndefined()
})

test('/ja on and /ja off are explicit', async ($, on) => {
  stubSession(on, [], [])
  await start($)
  expect((await ja($, 'on')).text).toBe('日英レイヤ: オン')
  expect((await ja($, 'on')).text).toBe('日英レイヤ: オン')
  expect((await ja($, ' off ')).text).toBe('日英レイヤ: オフ')
})

test('/ja status reports state, pairs and translator', async ($, on) => {
  stubSession(on, [], [])
  await start($)
  expect((await ja($, 'status')).text).toBe('日英レイヤ: オフ / 対訳 0 組 / 翻訳: plamo-translate')
})
