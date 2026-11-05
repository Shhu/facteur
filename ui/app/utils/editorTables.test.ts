import { describe, expect, it } from 'vitest'
import { CELL_STYLE, cellStyle, safeStyle, TABLE_STYLE, tableStyle } from './editorTables'

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

describe('styles of a quoted table', () => {
  it('keep their look, never a place over the app', () => {
    const cover = 'position:fixed;inset:0;z-index:2147483647;background:#fff;font:20px system-ui'
    expect(cellStyle(attrs({ style: cover }), attrs({}))).toBe('background:#fff;font:20px system-ui')
    expect(tableStyle(attrs({ style: 'width:600px; transform:translate(0,-900px); margin:-400px 0' }))).toBe('width:600px')
    expect(safeStyle('border:1px solid #ddd;padding:4px 8px;text-align:left')).toBe('border:1px solid #ddd;padding:4px 8px;text-align:left')
    expect(safeStyle('top:0;left:0')).toBeNull()
    expect(safeStyle('margin-top:-.9e3px;background-color:#fff')).toBe('background-color:#fff')
    expect(safeStyle('margin-top:calc(0px - .5e3px)')).toBeNull()
  })
})

