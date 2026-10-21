import { invoke, timeCommands } from '~/utils/ipc'

// `perf_enabled` is false in a release. Never in the way of the app: any failure is only logged.
export default defineNuxtPlugin(async () => {
  const on = await invoke<boolean>('perf_enabled').catch(() => false)
  if (!on) return
  try {
    const { enabled, open, commands, freezes, samples, push, SAMPLES_KEPT } = usePerf()
    enabled.value = true
    // The panel's own polling stays out of the measures.
    const quiet = new Set(['perf_events', 'log_ui', 'diagnostics', 'read_log'])
    timeCommands((command, ms, ok) => {
      if (quiet.has(command)) return
      push(commands, { at: Date.now(), command, ms, ok })
      if (ms > 500) logUi('warn', `perf: ${command} took ${Math.round(ms)} ms`)
    })
    try {
      new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) push(freezes, { at: Date.now(), ms: entry.duration })
      }).observe({ type: 'longtask', buffered: true })
    } catch {}
    let expected = performance.now() + 1000
    setInterval(() => {
      const now = performance.now()
      const lag = Math.max(0, now - expected)
      expected = now + 1000
      // Over 10 s the page was not running (the machine asleep, a hidden window's throttled
      // timers): not a freeze of the interface, so not a sample.
      if (lag > 10_000) return
      const memory = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory
      push(samples, { at: Date.now(), lag, heap: memory ? memory.usedJSHeapSize : null }, SAMPLES_KEPT)
    }, 1000)
    window.addEventListener('keydown', (e) => {
      if (e.ctrlKey && e.shiftKey && (e.key as string | undefined)?.toLowerCase() === 'p') {
        e.preventDefault()
        open.value = !open.value
      }
    })
  } catch (e) {
    console.warn('perf tools unavailable', e)
  }
})
