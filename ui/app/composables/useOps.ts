import type { OpsStatus } from '~/utils/types'

/** An answer older than the one applied is dropped. */
let statusRequest = 0
let statusApplied = 0

export function useOps() {
  const status = useState<OpsStatus>('ops-status', () => ({ pending: 0, failed: [] }))
  const showError = useErrorToast()

  async function refresh() {
    const mine = ++statusRequest
    try {
      const next = await api.opsStatus()
      if (mine < statusApplied) return
      statusApplied = mine
      status.value = next
    } catch {
      // Indicator only: not worth an alert.
    }
  }

  async function retry(id: number) {
    try {
      await api.retryOp(id)
    } catch (e) {
      showError(e, tr('common.toast.stillRefused'))
    }
    await refresh()
  }

  async function discard(id: number) {
    try {
      await api.discardOp(id)
    } catch (e) {
      showError(e)
    }
    await refresh()
  }

  return { status, refresh, retry, discard }
}
