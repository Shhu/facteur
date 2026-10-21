/** Account and mailbox go in the URL so the thread can be shown even when it is not among the
 *  loaded rows. */
export function threadRoute(
  t: { thread_id: number, account_id: number, mailbox_id: number },
  base = `/mail/${t.mailbox_id}`
): { path: string, query: Record<string, string> } {
  return {
    path: base,
    query: { thread: String(t.thread_id), account: String(t.account_id), box: String(t.mailbox_id) }
  }
}
