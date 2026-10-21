/** The webview's menu (Back, Reload, Save as…) makes no sense in an app; it is kept only where it
 *  helps: editable fields and selected text. */
export function blockBrowserContextMenu(doc: Document) {
  doc.addEventListener('contextmenu', (e) => {
    const target = e.target as HTMLElement | null
    const editable = target?.closest('input, textarea, [contenteditable=""], [contenteditable="true"]')
    const selected = doc.getSelection()?.toString().trim()
    if (!editable && !selected) e.preventDefault()
  })
}
