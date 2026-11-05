const OPEN = /^\s*(`{3,}|~{3,})/

function splitLines(text) {
  return text.match(/[^\n]*\n|[^\n]+$/g) ?? []
}

export function splitFences(text) {
  const segments = []
  let current = null
  let fence = null

  const push = (kind, line) => {
    if (current && current.kind === kind) current.text += line
    else {
      current = { kind, text: line }
      segments.push(current)
    }
  }

  for (const line of splitLines(text)) {
    if (fence) {
      push('fence', line)
      const close = line.match(/^\s*(`{3,}|~{3,})\s*$/)
      if (close && close[1][0] === fence[0] && close[1].length >= fence.length) {
        fence = null
        current = null
      }
      continue
    }
    const open = line.match(OPEN)
    if (open) {
      fence = open[1]
      current = null
      push('fence', line)
    } else {
      push('text', line)
    }
  }
  return segments
}
