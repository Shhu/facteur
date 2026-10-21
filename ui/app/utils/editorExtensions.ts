import Image from '@tiptap/extension-image'
import { FontSize, TextStyle } from '@tiptap/extension-text-style'
import { Node } from '@tiptap/vue-3'
import type { Editor } from '@tiptap/vue-3'

/** Size goes in the HTML `width`/`height`, which mail clients honour. Tiptap's node view update
 *  does not reapply the size (undo, resize), so we do. */
export const ResizableImage = Image.extend({
  addNodeView() {
    const factory = this.parent?.()
    if (!factory) return null
    return (props) => {
      const view = factory(props)
      const update = view.update?.bind(view)
      view.update = (node, decorations, inner) => {
        if (!update?.(node, decorations, inner)) return false
        const img = view.dom instanceof HTMLElement ? view.dom.querySelector('img') : null
        if (img) {
          const { width, height } = node.attrs as { width: number | null, height: number | null }
          img.style.width = width ? `${width}px` : ''
          img.style.height = height ? `${height}px` : ''
        }
        return true
      }
      return view
    }
  }
}).configure({
  inline: true,
  allowBase64: true,
  // Baseline alignment leaves a gap below images (a logo in a signature column).
  HTMLAttributes: { style: 'vertical-align: middle' },
  resize: {
    enabled: true,
    directions: ['top-left', 'top-right', 'bottom-left', 'bottom-right'],
    minWidth: 16,
    minHeight: 16,
    alwaysPreserveAspectRatio: true
  }
})

const GAP = 16
const DIVIDER = '2px solid #d4d4d8'

interface ColumnAttrs { width: number | null, divider: boolean }

function columnStyle({ width, divider }: ColumnAttrs): string {
  const style = ['vertical-align:middle', `padding:0 ${GAP}px 0 0`]
  if (divider) style.push(`border-left:${DIVIDER}`, `padding-left:${GAP}px`)
  if (width) style.push(`width:${width}px`)
  return style.join(';')
}

/** A borderless table: the only layout every mail client renders. Marked by a class because
 *  `data-*` attributes do not survive draft sanitisation. */
export const Columns = Node.create({
  name: 'columns',
  group: 'block',
  content: 'column+',
  isolating: true,
  parseHTML: () => [{ tag: 'table.facteur-columns', contentElement: 'tr' }],
  renderHTML: () => [
    'table',
    { class: 'facteur-columns', role: 'presentation', cellpadding: '0', cellspacing: '0', border: '0', style: 'border-collapse:collapse' },
    ['tbody', ['tr', 0]]
  ]
})

export const Column = Node.create({
  name: 'column',
  content: 'block+',
  isolating: true,
  addAttributes: () => ({
    width: {
      default: null,
      parseHTML: (el: HTMLElement) => Number.parseInt(el.getAttribute('width') ?? el.style.width) || null,
      renderHTML: () => ({})
    },
    divider: {
      default: false,
      parseHTML: (el: HTMLElement) => !!el.style.borderLeftStyle && el.style.borderLeftStyle !== 'none',
      renderHTML: () => ({})
    }
  }),
  parseHTML: () => [{ tag: 'td.facteur-column' }],
  renderHTML: ({ node }) => {
    const attrs = node.attrs as ColumnAttrs
    return ['td', { class: 'facteur-column', ...(attrs.width ? { width: String(attrs.width) } : {}), style: columnStyle(attrs) }, 0]
  },
  addNodeView() {
    return ({ node, getPos, editor }) => {
      let current = node
      const td = document.createElement('td')
      const content = document.createElement('div')
      const handle = document.createElement('div')
      handle.contentEditable = 'false'
      handle.dataset.columnHandle = ''
      td.append(content, handle)
      const render = () => {
        td.className = 'facteur-column'
        td.setAttribute('style', `${columnStyle(current.attrs as ColumnAttrs)};position:relative`)
      }
      render()

      const commit = (width: number | null) => {
        const pos = getPos()
        if (typeof pos === 'number') editor.view.dispatch(editor.state.tr.setNodeMarkup(pos, undefined, { ...current.attrs, width }))
      }
      handle.addEventListener('pointerdown', (e) => {
        e.preventDefault()
        const startX = e.clientX
        const start = Number.parseFloat(getComputedStyle(td).width)
        const width = (x: number) => Math.max(40, Math.round(start + x - startX))
        handle.setPointerCapture(e.pointerId)
        const move = (ev: PointerEvent) => {
          td.style.width = `${width(ev.clientX)}px`
        }
        const up = (ev: PointerEvent) => {
          handle.removeEventListener('pointermove', move)
          handle.removeEventListener('pointerup', up)
          handle.removeEventListener('pointercancel', up)
          commit(width(ev.clientX))
        }
        handle.addEventListener('pointermove', move)
        handle.addEventListener('pointerup', up)
        handle.addEventListener('pointercancel', up)
      })
      handle.addEventListener('dblclick', () => commit(null))

      return {
        dom: td,
        contentDOM: content,
        update: (next) => {
          if (next.type !== current.type) return false
          current = next
          render()
          return true
        },
        stopEvent: e => e.target === handle,
        ignoreMutation: m => m.target === handle || (m.type === 'attributes' && m.target === td)
      }
    }
  }
})

export const SIGNATURE_CLASS = 'facteur-signature'

/** One block, so changing the sender swaps it whole instead of matching its HTML as a string
 *  (the editor rewrites that HTML). */
export const Signature = Node.create({
  name: 'signature',
  group: 'block',
  content: 'block+',
  defining: true,
  parseHTML: () => [{ tag: `div.${SIGNATURE_CLASS}`, priority: 60 }],
  renderHTML: () => ['div', { class: SIGNATURE_CLASS }, 0]
})

export const editorExtensions = [ResizableImage, Columns, Column, Signature, TextStyle, FontSize]

const FONT_SIZES = [11, 12, 14, 16, 18, 22, 28, 36]
const NORMAL_SIZE = 14

function currentFontSize(editor: Editor): number {
  const size = Number.parseFloat(String(editor.getAttributes('textStyle').fontSize ?? ''))
  return Number.isFinite(size) ? size : NORMAL_SIZE
}

export function stepFontSize(editor: Editor, direction: 1 | -1) {
  const current = currentFontSize(editor)
  const next = direction > 0 ? FONT_SIZES.find(s => s > current) : [...FONT_SIZES].reverse().find(s => s < current)
  const chain = editor.chain().focus()
  if (next === undefined) return chain
  return next === NORMAL_SIZE ? chain.unsetFontSize() : chain.setFontSize(`${next}px`)
}

export function canStepFontSize(editor: Editor, direction: 1 | -1): boolean {
  const current = currentFontSize(editor)
  return direction > 0 ? current < FONT_SIZES.at(-1)! : current > FONT_SIZES[0]!
}

export function currentColumn(editor: Editor) {
  const { $from } = editor.state.selection
  for (let d = $from.depth; d > 1; d--) {
    if ($from.node(d).type.name !== 'column') continue
    return { pos: $from.before(d), node: $from.node(d), layout: $from.node(d - 1), layoutPos: $from.before(d - 1), index: $from.index(d - 1) }
  }
  return null
}

export function hasColumns(editor: Editor): boolean {
  let found = false
  editor.state.doc.descendants((n) => {
    if (n.type.name === 'columns') found = true
    return !found
  })
  return found
}

/** Images (the logo) go in the first column, text in the second, an optional narrow third one
 *  stays empty. */
export function makeColumns(editor: Editor, count: 2 | 3) {
  return editor.chain().focus().command(({ tr, state }) => {
    const { schema } = state
    const blocks: (typeof state.doc)[] = []
    state.doc.forEach(n => blocks.push(n))
    const imageOnly = (n: typeof state.doc) => n.type.name === 'paragraph' && n.childCount > 0
      && n.content.content.every(c => c.type.name === 'image' || c.type.name === 'hardBreak' || !c.text?.trim())
    const empty = (n: typeof state.doc) => n.type.name === 'paragraph' && !n.textContent.trim() && !imageOnly(n)
    const column = (content: (typeof state.doc)[], attrs: Partial<ColumnAttrs> = {}) =>
      schema.nodes.column!.create(attrs, content.length ? content : [schema.nodes.paragraph!.create()])
    const columns = [column(blocks.filter(imageOnly)), column(blocks.filter(n => !imageOnly(n) && !empty(n)), { divider: true })]
    if (count === 3) columns.push(column([], { divider: true, width: 260 }))
    tr.replaceWith(0, state.doc.content.size, [schema.nodes.columns!.create(null, columns), schema.nodes.paragraph!.create()])
    return true
  })
}

export function addColumn(editor: Editor) {
  const col = currentColumn(editor)
  const at = col ? col.pos + col.node.nodeSize : 0
  return editor.chain().focus()
    .insertContentAt(at, { type: 'column', attrs: { divider: true }, content: [{ type: 'paragraph' }] })
    .setTextSelection(at + 2)
}

export function removeColumn(editor: Editor) {
  const col = currentColumn(editor)
  if (!col || col.layout.childCount === 1) return unwrapColumns(editor)
  return editor.chain().focus().command(({ tr }) => {
    tr.delete(col.pos, col.pos + col.node.nodeSize)
    const first = tr.doc.nodeAt(col.layoutPos + 1)
    if (col.index === 0 && first?.attrs.divider) tr.setNodeMarkup(col.layoutPos + 1, undefined, { ...first.attrs, divider: false })
    return true
  })
}

export function unwrapColumns(editor: Editor) {
  const col = currentColumn(editor)
  return editor.chain().focus().command(({ tr }) => {
    if (!col) return false
    const blocks: (typeof col.node)[] = []
    col.layout.forEach(c => c.forEach(b => blocks.push(b)))
    tr.replaceWith(col.layoutPos, col.layoutPos + col.layout.nodeSize, blocks)
    return true
  })
}

export function toggleDivider(editor: Editor) {
  const col = currentColumn(editor)
  return editor.chain().focus().command(({ tr }) => {
    if (!col) return false
    tr.setNodeMarkup(col.pos, undefined, { ...col.node.attrs, divider: !col.node.attrs.divider })
    return true
  })
}
