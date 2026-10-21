import type { AiStatus } from '~/utils/types'

/** The local AI's state, shared; read again on the engine's `ai_changed`. */
export function useAi() {
  const status = useState<AiStatus | null>('ai-status', () => null)
  async function refresh() {
    try {
      status.value = await api.aiStatus()
    } catch {
      // Older engine: no local AI.
    }
  }
  /** On, its model there: the features show. */
  const ready = computed(() => !!status.value?.model && status.value.ready)
  return { status, ready, refresh }
}
