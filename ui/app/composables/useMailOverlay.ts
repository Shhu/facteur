import type { RouteLocationNormalizedLoaded } from 'vue-router'

// The last mail list stays mounted under every other screen, so closing that screen finds the
// list exactly as it was (scroll, loaded pages, selection, search, open thread).

export const isMailRoute = (r: { path: string }) => r.path.startsWith('/mail/')

const mailRoute = shallowRef<RouteLocationNormalizedLoaded | null>(null)
const pageRoute = shallowRef<RouteLocationNormalizedLoaded | null>(null)
/** History position of the mail entry, while it is still behind the current entry. */
let mailPosition: number | null = null

function historyPosition(): number | null {
  const position = window.history.state?.position
  return typeof position === 'number' ? position : null
}

export function useMailOverlay() {
  const router = useRouter()
  const covered = computed(() => mailRoute.value !== null && !isMailRoute(router.currentRoute.value))

  /** Through the history when the list's entry is behind (a natural back), else by opening it again. */
  function close() {
    const target = mailRoute.value
    if (!target || !covered.value) return
    const position = historyPosition()
    if (mailPosition !== null && position !== null && position > mailPosition) router.go(mailPosition - position)
    else router.push(target.fullPath)
  }

  /** For a deleted folder or account. */
  function forget() {
    mailRoute.value = null
    mailPosition = null
  }

  return { mailRoute, pageRoute, covered, close, forget }
}

export function trackMailOverlay() {
  const router = useRouter()
  // Synchronous: both routes are up to date before the layout renders the new route.
  watch(router.currentRoute, (r) => {
    const position = historyPosition()
    if (isMailRoute(r)) {
      mailRoute.value = r
      mailPosition = position
    } else {
      pageRoute.value = r
      // A screen pushed at or before the list's position has replaced its history entry.
      if (mailPosition !== null && (position === null || position <= mailPosition)) mailPosition = null
    }
  }, { immediate: true, flush: 'sync' })

  // A plain link to the covered list (sidebar, folder menu) returns to it as it was, open thread
  // included, instead of resetting it. Not its search: a link to the folder means the folder, not
  // a search left open in it (a saved search opened earlier would come back).
  const removeGuard = router.beforeEach((to, from) => {
    const target = mailRoute.value
    if (!target || isMailRoute(from) || to.path !== target.path || Object.keys(to.query).length || to.hash) return
    const { q: _q, ...query } = target.query
    const restored = router.resolve({ path: target.path, query, hash: target.hash }).fullPath
    if (restored !== to.fullPath) return restored
  })
  onBeforeUnmount(removeGuard)
}
