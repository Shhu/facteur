import type { Mailbox, MessageSummary, Outgoing, ThreadSummary } from './types'

/** `threads` with the unread counts of those holding `ids` moved by them (marked read or
 *  unread); the same array when none changed. */
export function seenPatched(threads: ThreadSummary[], ids: number[], seen: boolean): ThreadSummary[] {
  const changed = new Set(ids)
  let touched = false
  const next = threads.map((t) => {
    const n = t.message_ids.filter(i => changed.has(i)).length
    if (!n) return t
    const unread = seen ? Math.max(0, t.unread - n) : Math.min(t.count, t.unread + n)
    if (unread === t.unread) return t
    touched = true
    return { ...t, unread }
  })
  return touched ? next : threads
}

/** The messages a "mark read" actually changes: those of the targets still unread. */
export function newlySeen(targets: { ids: number[], unread: boolean }[]): number[] {
  return targets.filter(t => t.unread).flatMap(t => t.ids)
}

interface Target { accountId: number, threadId: number, mailboxId: number, ids: number[] }

/** `threads` without the rows whose messages all leave with `targets` (a search hit is one
 *  message of its thread); `kept`: a row lost some of them only, its counts are stale. */
export function rowsLeft(threads: ThreadSummary[], targets: Target[]): { rows: ThreadSummary[], kept: boolean } {
  const leaving = new Map<string, Set<number>>()
  for (const t of targets) {
    const key = `${t.accountId}-${t.threadId}-${t.mailboxId}`
    const ids = leaving.get(key) ?? new Set<number>()
    t.ids.forEach(i => ids.add(i))
    leaving.set(key, ids)
  }
  let kept = false
  const rows = threads.filter((t) => {
    const ids = leaving.get(`${t.account_id}-${t.thread_id}-${t.mailbox_id}`)
    if (!ids) return true
    if (t.message_ids.every(i => ids.has(i))) return false
    kept = true
    return true
  })
  return { rows, kept }
}

/** `next`, reusing the objects of `old` that did not change (by id); `old` itself when none did,
 *  so what renders from it is left alone. */
export function keptById<T extends { id: number }>(old: T[], next: T[]): T[] {
  const before = new Map(old.map(o => [o.id, o]))
  let same = old.length === next.length
  const merged = next.map((n, i) => {
    const o = before.get(n.id)
    const kept = o && JSON.stringify(o) === JSON.stringify(n) ? o : n
    if (kept !== old[i]) same = false
    return kept
  })
  return same ? old : merged
}

/** The thread's latest message in the folder shown (not the reply sent from it), else its latest. */
export function latestIn(messages: MessageSummary[], mailboxId: number): MessageSummary | undefined {
  return messages.findLast(m => m.mailbox_id === mailboxId) ?? messages.at(-1)
}

/** The thread's messages saved in a Drafts folder: written, never sent. */
export function draftIds(messages: MessageSummary[], mailboxes: Mailbox[]): Set<number> {
  const drafts = new Set(mailboxes.filter(b => b.role === 'drafts').map(b => b.id))
  return new Set(messages.filter(m => m.flags.draft || drafts.has(m.mailbox_id)).map(m => m.id))
}

/** What reply and forward answer: the latest message, a draft aside. */
export function replyTargetIn(messages: MessageSummary[], drafts: Set<number>): MessageSummary | undefined {
  return messages.findLast(m => !drafts.has(m.id)) ?? messages.at(-1)
}

/** Messages already read whose content may have changed since: moved (a draft sent is filed in
 *  Sent under the same id), a draft sent (Gmail: the same row, relabelled), a new preview, or a
 *  draft (saved again at each pause, its preview the same once past its first lines). */
export function staleDetails(before: MessageSummary[], after: MessageSummary[], drafts: Set<number>): number[] {
  const old = new Map(before.map(m => [m.id, m]))
  return after
    .filter((m) => {
      const was = old.get(m.id)
      return !!was && (drafts.has(m.id) || was.flags.draft !== m.flags.draft || was.mailbox_id !== m.mailbox_id || was.preview !== m.preview)
    })
    .map(m => m.id)
}

export interface Leaving { item: Outgoing, sentAt: number | null }

/** How long a sent reply shows before its real message arrives. */
export const SENT_SHOWN = 60_000

/** The replies on their way: those still in the outbox (`items`), updated; those gone from it,
 *  sent, kept a minute, timed; new ones answering the thread (`ids`). `clock`: a sent one waits. */
export function reconcileLeaving(prev: Leaving[], items: Outgoing[], ids: Set<number>, now: number): { next: Leaving[], clock: boolean } {
  const present = new Map(items.map(o => [o.id, o]))
  const next: Leaving[] = []
  for (const l of prev) {
    const o = present.get(l.item.id)
    if (o) next.push({ item: o, sentAt: null })
    else if (l.sentAt === null || now - l.sentAt <= SENT_SHOWN) next.push({ item: l.item, sentAt: l.sentAt ?? now })
  }
  for (const o of items) {
    if (!o.scheduled && o.in_reply_to !== null && ids.has(o.in_reply_to) && !next.some(l => l.item.id === o.id)) next.push({ item: o, sentAt: null })
  }
  // Unchanged: the same array, so what shows it is not rendered again.
  const same = next.length === prev.length && next.every((l, i) => l.item === prev[i]!.item && l.sentAt === prev[i]!.sentAt)
  return { next: same ? prev : next, clock: next.some(l => l.sentAt !== null) }
}

/** An action that would navigate (close the open thread, go to the next one) while another
 *  screen covers the list: done once the list shows again, if the same thread is still open. */
export function whileCovered() {
  let pending: number | null = null
  return {
    ask(covered: boolean, thread: number | null, act: () => void) {
      if (covered) pending = thread
      else act()
    },
    uncovered(thread: number | null, act: () => void) {
      const asked = pending
      pending = null
      if (asked !== null && asked === thread) act()
    }
  }
}
