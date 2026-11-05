const RESERVED = '<|plamo:'

export function createMemory() {
  let pairs = []
  return {
    add(pair) {
      pairs.push({ ja: pair.ja, en: pair.en })
    },
    size() {
      return pairs.length
    },
    clear() {
      pairs = []
    },
    select(direction, { maxPairs, maxChars }) {
      const picked = []
      let chars = 0
      for (let i = pairs.length - 1; i >= 0 && picked.length < maxPairs; i--) {
        const { ja, en } = pairs[i]
        chars += ja.length + en.length
        if (chars > maxChars) break
        picked.push(direction === 'ja-en' ? { source: ja, translation: en } : { source: en, translation: ja })
      }
      return picked.reverse()
    },
  }
}

export function isEligiblePair(source, translation, finishReason) {
  return (
    finishReason === 'stop' &&
    source.trim() !== '' &&
    translation.trim() !== '' &&
    !source.includes(RESERVED) &&
    !translation.includes(RESERVED)
  )
}

export function dropOldestHalf(pairs) {
  return pairs.slice(Math.ceil(pairs.length / 2))
}

export function toJsonl(pairs) {
  return pairs.map((p) => JSON.stringify({ source: p.source, translation: p.translation }) + '\n').join('')
}
