import { describe, expect, it } from 'vitest'
import { keptById, latestIn, newlySeen, reconcileLeaving, rowsLeft, seenPatched, whileCovered } from './listRows'
import type { MessageSummary, Outgoing, ThreadSummary } from './types'

const thread = (id: number, ids: number[], unread: number) => ({ thread_id: id, message_ids: ids, unread, count: ids.length }) as unknown as ThreadSummary

describe('seenPatched', () => {
  it('marks the rows past the first page read or unread', () => {
    const rows = [thread(1, [10, 11], 2), thread(2, [20], 0)]
    const read = seenPatched(rows, [10, 11], true)
    expect(read[0]!.unread).toBe(0)
    expect(read[1]).toBe(rows[1])
    const unread = seenPatched(read, [20], false)
    expect(unread[1]!.unread).toBe(1)
  })
  it('keeps the same array when nothing changes', () => {
    const rows = [thread(1, [10], 0)]
    expect(seenPatched(rows, [10], true)).toBe(rows)
    expect(seenPatched(rows, [99], false)).toBe(rows)
  })
})

describe('newlySeen', () => {
  it('leaves out messages already read', () => {
    const rows = [thread(1, [1, 2, 3], 2)]
    const changed = newlySeen([{ ids: [1], unread: true }, { ids: [2], unread: false }])
    expect(seenPatched(rows, changed, true)[0]!.unread).toBe(1)
  })
})

describe('rowsLeft', () => {
  const row = { account_id: 1, thread_id: 1, mailbox_id: 5, message_ids: [1, 2, 3] } as unknown as ThreadSummary
  const target = (ids: number[]) => ({ accountId: 1, threadId: 1, mailboxId: 5, ids })
  it('keeps a row whose other messages stay', () => {
    const left = rowsLeft([row], [target([2])])
    expect(left.rows).toEqual([row])
    expect(left.kept).toBe(true)
  })
  it('drops a row whose messages all leave', () => {
    expect(rowsLeft([row], [target([1, 2, 3])])).toEqual({ rows: [], kept: false })
    expect(rowsLeft([row], [target([1]), target([2, 3])]).rows).toEqual([])
  })
})

describe('keptById', () => {
  const box = (id: number, unread: number) => ({ id, name: `f${id}`, unread })
  it('keeps unchanged objects and the same array', () => {
    const old = [box(1, 0), box(2, 3)]
    expect(keptById(old, old.map(b => ({ ...b })))).toBe(old)
    const next = [box(1, 0), box(2, 4)]
    const kept = keptById(old, next)
    expect(kept[0]).toBe(old[0])
    expect(kept[1]).toBe(next[1])
  })
})

describe('latestIn', () => {
  const msg = (id: number, mailbox_id: number) => ({ id, mailbox_id }) as unknown as MessageSummary
  it('takes the folder\'s latest message, not the reply sent', () => {
    expect(latestIn([msg(1, 5), msg(2, 9)], 5)?.id).toBe(1)
    expect(latestIn([msg(1, 7), msg(2, 9)], 5)?.id).toBe(2)
    expect(latestIn([], 5)).toBeUndefined()
  })
})

describe('reconcileLeaving', () => {
  const item = (id: number) => ({ id, scheduled: false, in_reply_to: 1 }) as unknown as Outgoing
  it('forgets a sent reply after a minute and stops the clock', () => {
    const now = 100_000
    expect(reconcileLeaving([{ item: item(3), sentAt: now - 61_000 }], [], new Set([1]), now)).toEqual({ next: [], clock: false })
  })
  it('times a reply gone from the outbox', () => {
    const { next, clock } = reconcileLeaving([{ item: item(3), sentAt: null }], [], new Set([1]), 5)
    expect(next).toEqual([{ item: item(3), sentAt: 5 }])
    expect(clock).toBe(true)
  })
  it('picks up a reply to the thread', () => {
    expect(reconcileLeaving([], [item(4)], new Set([1]), 5).next).toEqual([{ item: item(4), sentAt: null }])
  })
})

describe('reconcileLeaving unchanged', () => {
  const item = (id: number) => ({ id, scheduled: false, in_reply_to: 1 }) as unknown as Outgoing
  it('keeps the same array when nothing changed', () => {
    const prev = [{ item: item(3), sentAt: 5 }]
    expect(reconcileLeaving(prev, [], new Set([1]), 10).next).toBe(prev)
  })
})

describe('whileCovered', () => {
  it('acts at once, or once the list shows again on the same thread', () => {
    const later = whileCovered()
    let acted = 0
    later.ask(false, 1, () => acted++)
    expect(acted).toBe(1)
    later.ask(true, 1, () => acted++)
    expect(acted).toBe(1)
    later.uncovered(1, () => acted++)
    expect(acted).toBe(2)
    later.uncovered(1, () => acted++)
    expect(acted).toBe(2)
    later.ask(true, 1, () => acted++)
    later.uncovered(2, () => acted++)
    expect(acted).toBe(2)
  })
})
