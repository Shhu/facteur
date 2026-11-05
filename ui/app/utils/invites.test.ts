import { describe, expect, it } from 'vitest'
import { inviteFromSubject, inviteNotes, seriesUid } from './invites'

describe('inviteFromSubject', () => {
  it('reads Google subjects', () => {
    expect(inviteFromSubject('Invitation: Point projet @ Fri Oct 23, 2026 8am - 11am (UTC+2) (me@x.com)'))
      .toEqual({ kind: 'invitation', title: 'Point projet' })
    expect(inviteFromSubject('Invitation: lunch')).toBeNull()
  })

  it('stays fast on a crafted subject', () => {
    const started = performance.now()
    // Ending as Google writes them: the right answer is an invitation, found after the crafted part.
    const found = inviteFromSubject('Invitation: x' + ' @ 1('.repeat(4000) + ' @ Fri Oct 23, 2026 8am - 11am (UTC+2) (me@x.com)')
    expect(performance.now() - started).toBeLessThan(500)
    // Still read: quick, not given up.
    expect(found?.kind).toBe('invitation')
    expect(found?.title.startsWith('x')).toBe(true)
  })
})

describe('inviteNotes', () => {
  it('stays fast on a crafted description', () => {
    const started = performance.now()
    // No tag in it: the text as it is.
    expect(inviteNotes('<a'.repeat(50_000))).toBe('<a'.repeat(50_000))
    expect(performance.now() - started).toBeLessThan(500)
  })
})

describe('seriesUid', () => {
  it('tells an Outlook occurrence by its series', () => {
    const series = '040000008200E00074C5B7101A82E00800000000D0B0A1C2E3F4D501000000000000000010000000ABCDEF'
    const occurrence = '040000008200e00074c5b7101a82e00807EA0B05D0B0A1C2E3F4D501000000000000000010000000abcdef'
    expect(seriesUid(occurrence)).toBe(seriesUid(series))
    expect(seriesUid('abc@google.com')).toBe('abc@google.com')
  })
})
