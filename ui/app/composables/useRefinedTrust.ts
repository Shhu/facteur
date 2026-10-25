import type { Trust } from '~/utils/types'

const KEPT = 200

/** Trust re-weighed after the engine checked DKIM. Kept, not only applied: the refinement may
 *  arrive before the message has finished loading. */
export function useRefinedTrust() {
  const refined = useState<Record<number, Trust>>('refined-trust', () => ({}))
  function note(messageId: number, trust: Trust) {
    // Every arrival checked is noted, most never shown: only the latest are kept, or the store
    // would grow (and be copied whole at each arrival) for as long as facteur runs.
    const kept = Object.entries(refined.value).slice(-(KEPT - 1))
    refined.value = { ...Object.fromEntries(kept), [messageId]: trust }
  }
  /** Before the message is read again: the engine's answer will be fresher. */
  function forget(messageId: number) {
    if (!(messageId in refined.value)) return
    const { [messageId]: _stale, ...rest } = refined.value
    refined.value = rest
  }
  return { refined, note, forget }
}
