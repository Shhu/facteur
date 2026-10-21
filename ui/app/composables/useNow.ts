/** Refreshed every minute so labels relative to today recompute after midnight instead of
 *  staying stale. */
const now = ref(Date.now())
let started = false

function start() {
  if (started || typeof window === 'undefined') return
  started = true
  const update = () => {
    now.value = Date.now()
  }
  // Aligned on the minute, then every minute; also on return from sleep or a hidden window.
  setTimeout(() => {
    update()
    setInterval(update, 60_000)
  }, 60_000 - (Date.now() % 60_000))
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') update()
  })
}

export function useNow(): Readonly<Ref<number>> {
  start()
  return readonly(now)
}

export function useStartOfDay(): ComputedRef<number> {
  const current = useNow()
  return computed(() => {
    const d = new Date(current.value)
    return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 1000
  })
}

/** Reactive to the minute: call it from a computed or the template. */
export function useSinceLabel(): (at: number | null) => string {
  const { t } = useI18n()
  const current = useNow()
  return (at) => {
    if (at === null) return t('common.neverSynced')
    const seconds = Math.max(0, current.value / 1000 - at)
    const rtf = new Intl.RelativeTimeFormat(intlLocale(), { numeric: 'auto', style: 'short' })
    if (seconds < 60) return t('common.since.now')
    if (seconds < 3600) return rtf.format(-Math.floor(seconds / 60), 'minute')
    if (seconds < 86_400) return rtf.format(-Math.floor(seconds / 3600), 'hour')
    if (seconds < 7 * 86_400) return rtf.format(-Math.floor(seconds / 86_400), 'day')
    return shortDate(at)
  }
}

/** Reactive to the day: call it from a computed or the template. */
export function useSyncedLabel(): (at: number | null) => string {
  const { t } = useI18n()
  const current = useNow()
  return (at) => {
    if (at === null) return t('common.neverSynced')
    const d = new Date(at * 1000)
    if (d.toDateString() !== new Date(current.value).toDateString()) return t('tools.home.synced', { date: shortDate(at) })
    return t('tools.home.syncedAt', { time: dateFormat({ hour: '2-digit', minute: '2-digit' }).format(d) })
  }
}
