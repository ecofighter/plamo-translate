const FENCE_OPEN = /^\s*(`{3,}|~{3,})/
const FENCE_CLOSE = /^\s*(`{3,}|~{3,})\s*$/
const TABLE = /^\s*\|/

const isBlank = (line) => /^\s*$/.test(line)
const fenceMarker = (line) => line.match(FENCE_OPEN)?.[1]
const closes = (line, marker) => {
  const m = line.match(FENCE_CLOSE)
  return Boolean(m) && m[1][0] === marker[0] && m[1].length >= marker.length
}

function splitLines(text, final) {
  const lines = text.match(/[^\n]*\n/g) ?? []
  const consumed = lines.reduce((n, l) => n + l.length, 0)
  if (final && consumed < text.length) lines.push(text.slice(consumed))
  return lines
}

export function createDisplayBuffer({ flushLines, holdIncomplete }) {
  let pending = ''
  // Only used without holding: a fence that was shown open and continues in later pushes.
  let openFence = null

  function push(delta, final) {
    pending += delta
    const lines = splitLines(pending, final)
    const n = lines.length
    const blocks = []
    const emit = (kind, from, to) => blocks.push({ kind, text: lines.slice(from, to).join('') })
    let i = 0

    while (i < n) {
      if (openFence) {
        let j = i
        while (j < n && !closes(lines[j], openFence)) j++
        if (j < n) {
          j++
          openFence = null
        }
        emit('fence', i, j)
        i = j
        continue
      }

      const line = lines[i]
      if (isBlank(line)) {
        let j = i
        while (j < n && isBlank(lines[j])) j++
        emit('blank', i, j)
        i = j
        continue
      }

      const marker = fenceMarker(line)
      if (marker) {
        let k = i + 1
        while (k < n && !closes(lines[k], marker)) k++
        if (k < n) {
          emit('fence', i, k + 1)
          i = k + 1
        } else if (final) {
          emit('fence', i, n)
          i = n
        } else if (!holdIncomplete) {
          emit('fence', i, n)
          openFence = marker
          i = n
        } else break
        continue
      }

      if (TABLE.test(line)) {
        let j = i
        while (j < n && TABLE.test(lines[j])) j++
        if (j < n || final || !holdIncomplete) {
          emit('table', i, j)
          i = j
        } else break
        continue
      }

      let j = i
      while (j < n && !isBlank(lines[j]) && !fenceMarker(lines[j]) && !TABLE.test(lines[j])) j++
      if (j < n || final || !holdIncomplete) {
        emit('paragraph', i, j)
        i = j
      } else if (j - i >= flushLines) {
        emit('paragraph', i, i + flushLines)
        i += flushLines
      } else break
    }

    pending = pending.slice(blocks.reduce((len, b) => len + b.text.length, 0))
    if (final) openFence = null
    return blocks
  }

  return {
    push,
    takeAll: () => push('', true),
    isEmpty: () => pending === '',
  }
}

export function tableShapeMatches(source, translated) {
  const rows = (t) => t.replace(/\n+$/, '').split('\n')
  const pipes = (row) => (row.match(/\|/g) ?? []).length
  const a = rows(source)
  const b = rows(translated)
  return a.length === b.length && a.every((row, i) => pipes(row) === pipes(b[i]))
}
