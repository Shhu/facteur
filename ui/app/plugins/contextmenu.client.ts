// Browser reflexes make no sense in an app. A reload restarts only the UI while the engine keeps
// running, which desynchronised the display.
export default defineNuxtPlugin(() => {
  blockBrowserContextMenu(document)
  if (import.meta.dev) return
  window.addEventListener('keydown', (e) => {
    // The keydown sent by the browser's autofill has no key.
    const key = (e.key as string | undefined)?.toLowerCase() ?? ''
    const reload = key === 'f5' || ((e.ctrlKey || e.metaKey) && key === 'r')
    const history = e.altKey && (key === 'arrowleft' || key === 'arrowright')
    const browserKeys = key === 'browserback' || key === 'browserforward' || key === 'browserrefresh'
    if (reload || history || browserKeys) e.preventDefault()
  }, { capture: true })
})
