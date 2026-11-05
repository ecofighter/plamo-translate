const RESERVED = '<|plamo:'

export function buildArgs({ from, to, form, historyFile }) {
  const args = ['--json', '--no-stream', '-s', from, '-t', to]
  if (form && to === 'ja') args.push('--form', form)
  if (historyFile) args.push('--history', historyFile)
  return args
}

function stderrReason(stderr, exitCode) {
  const line = stderr.split('\n').find((l) => l.trim() !== '')
  return line ? line.replace(/^error:\s*/, '').trim() : `exit code ${exitCode}`
}

export function parseResult({ exitCode, stdout, stderr }) {
  if (exitCode !== 0) {
    return {
      ok: false,
      reason: stderrReason(stderr, exitCode),
      contextTooLong: /^error: context_too_long:/m.test(stderr),
    }
  }
  let done
  try {
    const last = stdout.trim().split('\n').pop()
    done = JSON.parse(last)
  } catch {
    return { ok: false, reason: 'unreadable output', contextTooLong: false }
  }
  if (typeof done?.text !== 'string') return { ok: false, reason: 'unreadable output', contextTooLong: false }
  if (done.finish_reason !== 'stop') {
    return { ok: false, reason: `finish_reason ${done.finish_reason}`, contextTooLong: false }
  }
  if (done.text.trim() === '') return { ok: false, reason: 'empty translation', contextTooLong: false }
  if (done.text.includes(RESERVED)) return { ok: false, reason: 'reserved token in translation', contextTooLong: false }
  return { ok: true, text: done.text, finishReason: done.finish_reason }
}
