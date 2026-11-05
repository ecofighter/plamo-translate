import { resolveConfig } from './lib/config.js'
import { createMemory } from './lib/memory.js'

const STATUS_TEXT = 'ja⇄en'

export function register(on, options) {
  const cfg = resolveConfig(options)
  const memory = createMemory()
  let enabled = cfg.enabledByDefault

  const stateLabel = () => (enabled ? 'オン' : 'オフ')

  on('session.start', async ($, e, next) => {
    memory.clear()
    enabled = cfg.enabledByDefault
    await $.command.register({
      name: 'ja',
      description: '日英レイヤ (plamo-translate) のオン・オフ',
      argumentHint: '[on|off|status]',
    })
    $.ui.status(enabled ? STATUS_TEXT : undefined)
    return next(e)
  })

  on('command.run', { command: 'ja' }, async ($, e) => {
    const arg = (e.args ?? '').trim()
    if (arg === 'status') {
      return { text: `日英レイヤ: ${stateLabel()} / 対訳 ${memory.size()} 組 / 翻訳: ${cfg.translator}` }
    }
    enabled = arg === 'on' ? true : arg === 'off' ? false : !enabled
    $.ui.status(enabled ? STATUS_TEXT : undefined)
    return { text: `日英レイヤ: ${stateLabel()}` }
  })
}
