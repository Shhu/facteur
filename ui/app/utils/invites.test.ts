import { describe, expect, it } from 'vitest'
import { inviteFromSubject, inviteNotes } from './invites'

describe('inviteFromSubject', () => {
  it('reads Google subjects', () => {
    expect(inviteFromSubject('Invitation: Point projet @ Fri Oct 23, 2026 8am - 11am (UTC+2) (me@x.com)'))
      .toEqual({ kind: 'invitation', title: 'Point projet' })
    expect(inviteFromSubject('Invitation: lunch')).toBeNull()
  })

  it('stays fast on a crafted subject', () => {
    const started = performance.now()
    inviteFromSubject('Invitation: x' + ' @ 1('.repeat(4000))
    expect(performance.now() - started).toBeLessThan(500)
  })
})

describe('inviteNotes', () => {
  it('stays fast on a crafted description', () => {
    const started = performance.now()
    expect(inviteNotes('<a'.repeat(50_000))).not.toBeNull()
    expect(performance.now() - started).toBeLessThan(500)
  })
})
