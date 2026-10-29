import { describe, expect, it } from 'vitest'
import { CELL_STYLE, cellStyle, TABLE_STYLE, tableStyle } from './editorTables'

const attrs = (a: Record<string, string>) => (name: string) => a[name] ?? null

describe('pasted tables', () => {
  it('draws a bare table (a chat, a spreadsheet) so the recipient sees its grid', () => {
    expect(tableStyle(attrs({}))).toBe(TABLE_STYLE)
    expect(tableStyle(attrs({ class: 'min-w-full' }))).toBe(TABLE_STYLE)
    expect(cellStyle(attrs({}), attrs({}))).toBe(CELL_STYLE)
  })

  it('leaves a laid-out table (a quoted newsletter, a signature) as its author wrote it', () => {
    expect(tableStyle(attrs({ style: 'width:600px' }))).toBe('width:600px')
    expect(tableStyle(attrs({ border: '0', cellpadding: '0' }))).toBeNull()
    expect(tableStyle(attrs({ role: 'presentation' }))).toBeNull()
    expect(cellStyle(attrs({ style: 'padding:8px' }), attrs({}))).toBe('padding:8px')
    expect(cellStyle(attrs({}), attrs({ width: '100%' }))).toBeNull()
  })
})
