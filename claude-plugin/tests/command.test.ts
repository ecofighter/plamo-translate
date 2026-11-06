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

// The engine runs session.start again when it replaces the plugin's copy (a hooks worker respawn).
test('a repeated session start keeps /ja on', async ($, on) => {
  const statuses: Array<string | undefined> = []
  stubSession(on, statuses, [])
  await start($)
  await ja($, 'on')
  await start($)
  expect(statuses[statuses.length - 1]).toBe('ja⇄en')
  expect((await ja($, 'status')).text).toBe('日英レイヤ: オン / 対訳 0 組 / 翻訳: plamo-translate')
})

test('a repeated session start keeps /ja off', async ($, on) => {
  const statuses: Array<string | undefined> = []
  stubSession(on, statuses, [])
  await start($)
  await ja($, 'off')
  await start($)
  expect(statuses[statuses.length - 1]).toBeUndefined()
  expect((await ja($, 'status')).text).toBe('日英レイヤ: オフ / 対訳 0 組 / 翻訳: plamo-translate')
})

test('enabledByDefault applies to the first session start only', { options: { enabledByDefault: true } }, async ($, on) => {
  const statuses: Array<string | undefined> = []
  stubSession(on, statuses, [])
  await start($)
  expect(statuses[statuses.length - 1]).toBe('ja⇄en')
  await ja($, 'off')
  await start($)
  expect(statuses[statuses.length - 1]).toBeUndefined()
})

function stubTurn(on: On) {
  on('turn.start', ($: unknown, e: { turnId: string }) => ({ turnId: e.turnId }))
}

function stubCommandList(on: On, names: () => string[]) {
  stubTurn(on)
  on('command.list', () => ({ value: names().map((name) => ({ name, description: '' })) }))
}

const turn = ($: any, agentId?: string) => $.turn.start({ text: '', turnId: 't1', ...(agentId ? { agentId } : {}) })

test('a turn re-registers /ja when the command went missing', async ($, on) => {
  const statuses: Array<string | undefined> = []
  const registered: string[] = []
  stubSession(on, statuses, registered)
  stubCommandList(on, () => [])
  await start($)
  await ja($, 'on')
  await turn($)
  expect(registered).toEqual(['ja', 'ja'])
  expect(statuses[statuses.length - 1]).toBe('ja⇄en')
})

test('a turn leaves a registered /ja alone', async ($, on) => {
  const registered: string[] = []
  stubSession(on, [], registered)
  stubCommandList(on, () => ['help', 'ja'])
  await start($)
  await turn($)
  expect(registered).toEqual(['ja'])
})

test('a subagent turn does not check the command', async ($, on) => {
  const registered: string[] = []
  stubSession(on, [], registered)
  stubCommandList(on, () => [])
  await start($)
  await turn($, 'agent1')
  expect(registered).toEqual(['ja'])
})

test('a turn survives a failing command list', async ($, on) => {
  stubSession(on, [], [])
  stubTurn(on)
  on('command.list', () => ({ deny: 'unavailable' }))
  await start($)
  await turn($)
})
