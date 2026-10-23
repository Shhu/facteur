/** A text colour nobody can read: transparent, or fully see-through. */
export function invisibleColor(color: string): boolean {
  const c = color.trim().toLowerCase()
  if (c === 'transparent') return true
  const inside = /^rgba?\((.*)\)$/.exec(c)?.[1]
  if (!inside) return false
  const parts = inside.split(/[\s,/]+/).filter(Boolean)
  if (parts.length < 4) return false
  const alpha = parts[3]!
  return Number.parseFloat(alpha) === 0
}

/** Text a style writes itself (`::before`, `::after`), which no text node holds. */
export function generatedText(content: string): boolean {
  return !['', 'none', 'normal', '""', '\'\''].includes(content.trim())
}
