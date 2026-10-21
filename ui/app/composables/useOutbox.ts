import type { Outgoing } from '~/utils/types'

/** Outbox reads, started and applied: an answer older than the one applied is dropped. */
let outboxRequest = 0
let outboxApplied = 0

export function useOutbox() {
  const items = useState<Outgoing[]>('outbox', () => [])
  /** Not the sends still in their few-second undo delay. */
  const visible = computed(() => items.value.filter(o => o.scheduled || o.state === 'failed'))
  const failed = computed(() => items.value.filter(o => o.state === 'failed').length)

  async function refresh() {
    const mine = ++outboxRequest
    const list = await api.outbox().catch(() => null)
    if (!list || mine < outboxApplied) return
    outboxApplied = mine
    items.value = list
  }

  return { items, visible, failed, refresh }
}

/** Longest quit wait for pending messages while none of them is being transferred. */
const QUIT_SEND_WAIT = 30_000

/** Messages still in their undo delay leave now rather than at the next launch, possibly days
 *  later, and a transfer is never cut short (it would come back as interrupted). Past
 *  QUIT_SEND_WAIT, what has not started stays in the outbox, but a transfer under way is still
 *  waited for: the engine's SMTP timeouts bound it, and closing again quits at once. */
export async function flushOutboxBeforeQuit(onWait?: (count: number) => void): Promise<void> {
  const leaving = (await api.outbox().catch(() => [] as Outgoing[])).filter(o => !o.scheduled && o.state !== 'failed')
  if (!leaving.length) return
  onWait?.(leaving.length)
  const ids = new Set(leaving.map(o => o.id))
  // One already being sent is refused: it only has to be waited for.
  await Promise.allSettled(leaving.filter(o => o.state === 'pending').map(o => api.sendNow(o.id)))
  const deadline = Date.now() + QUIT_SEND_WAIT
  for (;;) {
    await new Promise(resolve => setTimeout(resolve, 500))
    const items = await api.outbox().catch(() => null)
    if (!items) return
    const now = Date.now() / 1000
    const waited = items.filter(o => ids.has(o.id))
    // A transfer cut short would come back as "interrupted, may have gone": never give up on it.
    if (waited.some(o => o.state === 'sending')) continue
    // Sent (gone), failed, or set for a later try after a failure: nothing more to wait for.
    if (!waited.some(o => o.state === 'pending' && o.send_at <= now + 2)) return
    if (Date.now() >= deadline) return
  }
}
