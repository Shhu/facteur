type Attr = (name: string) => string | null

/** What a table pasted as plain data gets: lines and room, written inline so every mail client
 *  shows them. */
export const TABLE_STYLE = 'border-collapse:collapse;margin:8px 0'
export const CELL_STYLE = 'border:1px solid #d4d4d8;padding:6px 10px;vertical-align:top;text-align:left'

/** Attributes by which a table shows its author laid it out (a class does not: its sheet stays
 *  on the page it was copied from). */
const LAID_OUT = ['style', 'border', 'cellpadding', 'cellspacing', 'width', 'bgcolor', 'role']

function bare(table: Attr): boolean {
  return LAID_OUT.every(name => table(name) === null)
}

/** What a table or cell of a message may keep in the composer, which is the app's own window: its
 *  look, never a place of its own over the app (`position:fixed` covering every screen). */
const KEPT = /^(?:(?:min-|max-)?(?:width|height)|border(?:-[a-z-]+)?|padding(?:-[a-z]+)?|margin(?:-[a-z]+)?|background(?:-color)?|color|font(?:-[a-z]+)?|text-(?:align|decoration|indent|transform)|vertical-align|line-height|letter-spacing|white-space|word-(?:break|wrap|spacing)|overflow-wrap|table-layout|empty-cells|caption-side|direction|opacity|visibility|display|box-sizing)$/

export function safeStyle(style: string | null): string | null {
  if (style === null) return null
  const kept = style
    .split(';')
    .map(d => d.trim())
    .filter((d) => {
      const at = d.indexOf(':')
      if (at < 1) return false
      const value = d.slice(at + 1)
      // Pulled out of its place by a negative margin, or a background from elsewhere.
      return KEPT.test(d.slice(0, at).trim().toLowerCase()) && !/url\s*\(|expression\s*\(|-\s*[\d.]/i.test(value)
    })
  return kept.length ? kept.join(';') : null
}

/** The table's own style; a bare one (no layout of its author's) gets lines. */
export function tableStyle(table: Attr): string | null {
  return safeStyle(table('style')) ?? (bare(table) ? TABLE_STYLE : null)
}

export function cellStyle(cell: Attr, table: Attr): string | null {
  return safeStyle(cell('style')) ?? (bare(table) ? CELL_STYLE : null)
}
