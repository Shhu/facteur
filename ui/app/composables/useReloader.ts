/** `tick` fires up to a few times a second during a sync: one load at a time, at most one every
 *  `interval` ms, requests made meanwhile folded into one. `now()` loads at once, so the screen
 *  must ignore stale answers itself (request counter). */
export function useReloader(load: () => Promise<unknown>, interval = 2000) {
  /** Loads in flight (`now()` may start one while another runs). */
  let running = 0
  let pending = false
  let last = 0
  let timer: ReturnType<typeof setTimeout> | undefined
  /** The screen is gone: a load finishing after it schedules nothing more. */
  let disposed = false

  async function run() {
    clearTimeout(timer)
    timer = undefined
    pending = false
    running++
    last = Date.now()
    try {
      await load()
    } finally {
      running--
      if (pending && !running && !disposed) schedule()
    }
  }

  function schedule() {
    if (disposed) return
    if (running) {
      pending = true
      return
    }
    if (timer) return
    timer = setTimeout(run, Math.max(0, last + interval - Date.now()))
  }

  onScopeDispose(() => {
    disposed = true
    clearTimeout(timer)
  })
  return { schedule, now: run }
}
