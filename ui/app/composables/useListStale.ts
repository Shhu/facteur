/** Bumped after an action that changes a list's content without going through it. The list then
 *  rereads every row it shows: its usual refresh rereads the first page only. */
export function useListStale() {
  const tick = useState('list-stale', () => 0)
  return { tick, bump: () => { tick.value++ } }
}
