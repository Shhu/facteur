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

/** The table's own style; a bare one (no layout of its author's) gets lines. */
export function tableStyle(table: Attr): string | null {
  return table('style') ?? (bare(table) ? TABLE_STYLE : null)
}

export function cellStyle(cell: Attr, table: Attr): string | null {
  return cell('style') ?? (bare(table) ? CELL_STYLE : null)
}
