// The released exe has no console: uncaught errors would vanish without the log file.
export default defineNuxtPlugin((nuxtApp) => {
  const describe = (e: unknown) => (e instanceof Error ? `${e.message}\n${e.stack ?? ''}` : String(e))
  nuxtApp.hook('vue:error', (e, _instance, info) => logUi('error', `vue (${info}): ${describe(e)}`))
  // A browser notice, not an error (layout settled over two frames).
  const benign = (message: string) => message.startsWith('ResizeObserver loop')
  window.addEventListener('error', (e) => {
    if (benign(e.message ?? '')) return
    logUi('error', `window: ${describe(e.error ?? e.message)} @ ${e.filename}:${e.lineno}`)
  })
  window.addEventListener('unhandledrejection', e => logUi('error', `promise: ${describe(e.reason)}`))
})
