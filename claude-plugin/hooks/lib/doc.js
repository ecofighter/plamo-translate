export const ABORTED = Symbol('aborted')

const BASE_TIMEOUT_MS = 60000
const MS_PER_BYTE = 20
const MAX_TIMEOUT_MS = 600000
const REPORT_VERSION = 1

export function buildDocArgs({ from, to, form, historyFile }) {
  const args = ['doc', '--report', 'json', '-s', from, '-t', to, '--format', 'markdown']
  if (form && to === 'ja') args.push('--form', form)
  if (historyFile) args.push('--history', historyFile)
  return args
}

export function docTimeoutMs(bytes) {
  return Math.min(MAX_TIMEOUT_MS, BASE_TIMEOUT_MS + MS_PER_BYTE * bytes)
}

export function utf8Bytes(text) {
  return new TextEncoder().encode(text).length
}

export function untilAborted(promise, signal) {
  if (signal.aborted) return Promise.resolve(ABORTED)
  return new Promise((resolve, reject) => {
    const onAbort = () => resolve(ABORTED)
    signal.addEventListener('abort', onAbort, { once: true })
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort)
        resolve(value)
      },
      (error) => {
        signal.removeEventListener('abort', onAbort)
        reject(error)
      },
    )
  })
}

function parseReport(stderr) {
  const records = []
  for (const line of stderr.split('\n')) {
    if (line.trim() === '') continue
    let record
    try {
      record = JSON.parse(line)
    } catch {
      return null
    }
    if (record === null || typeof record !== 'object' || Array.isArray(record)) return null
    records.push(record)
  }
  return records
}

function stoppedReason(errors) {
  const last = errors[errors.length - 1]
  if (!last) return 'stopped'
  if (last.kind === 'daemon') return `${last.code}: ${last.message}`
  return last.message || 'stopped'
}

export function parseDocRun({ exitCode, stdout, stderr, isStdoutTruncated, isStderrTruncated }) {
  if (exitCode === 2 && /unexpected argument '--report'/.test(stderr)) {
    return { ok: false, unsupported: true, tooOld: true }
  }
  if (isStdoutTruncated || isStderrTruncated) return { ok: false, reason: 'output too large' }

  const records = parseReport(stderr)
  const summary = records?.[records.length - 1]
  if (!summary || summary.type !== 'summary' || summary.exit_code !== exitCode) {
    return { ok: false, reason: 'no report' }
  }
  if (summary.v !== REPORT_VERSION) return { ok: false, reason: 'unsupported report version' }

  const errors = records.filter((r) => r.type === 'error')
  if (summary.complete === false) return { ok: false, reason: stoppedReason(errors) }

  if (summary.complete === true && (exitCode === 0 || exitCode === 1) && stdout.trim() !== '') {
    return {
      ok: true,
      text: stdout,
      degraded: summary.degraded,
      pieces: summary.pieces,
      notes: records.filter((r) => r.type === 'note').length,
    }
  }
  return { ok: false, reason: errors[0]?.message || `exit code ${exitCode}` }
}
