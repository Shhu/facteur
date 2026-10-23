/** Bumped after an action that changes a list's content without going through it. The list then
 *  rereads every row it shows: its usual refresh rereads the first page only. Given the thread
 *  that left a folder (archived, deleted, moved from the open thread), the list drops its row there and merges
 *  its first page instead: rereading thousands of scrolled rows after each archive stalls triage. */
export function useListStale() {
  const tick = useState('list-stale', () => 0)
  const gone = useState<{ account_id: number, thread_id: number, mailbox_id: number } | null>('list-stale-gone', () => null)
  return {
    tick,
    gone,
    bump: (left?: { account_id: number, thread_id: number, mailbox_id: number }) => {
      gone.value = left ?? null
      tick.value++
    }
  }
}
