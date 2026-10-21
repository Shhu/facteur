import type { FollowUp } from '~/utils/types'

/** An answer older than the one applied is dropped. */
let followUpsRequest = 0
let followUpsApplied = 0

export function useFollowUps() {
  const items = useState<FollowUp[]>('follow-ups', () => [])
  const showError = useErrorToast()

  async function refresh() {
    const mine = ++followUpsRequest
    const list = await api.followUps().catch(() => null)
    if (!list || mine < followUpsApplied) return
    followUpsApplied = mine
    items.value = list
  }

  async function dismiss(id: number) {
    items.value = items.value.filter(f => f.id !== id)
    await api.dismissFollowUp(id).catch(showError)
  }

  return { items, refresh, dismiss }
}
