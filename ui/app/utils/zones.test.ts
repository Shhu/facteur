import { describe, expect, it } from 'vitest'
import { instantAt, wallFields, zoneOffsetLabel } from './zones'

// Run with the computer in Europe/Paris (see the `test` script): the night of 2027-03-28 has no
// 02:30 there.
describe('an event set in another zone', () => {
  it('keeps a time the computer\'s clock skips', () => {
    // 02:30 in Tokyo that night, 18:30 UTC the day before.
    const at = Date.UTC(2027, 2, 27, 17, 30) / 1000
    const fields = wallFields(at, 'Asia/Tokyo')
    expect(fields).toEqual({ date: '2027-03-28', time: '02:30' })
    // Saved again unchanged: the same instant, not an hour later.
    expect(instantAt(fields.date, fields.time, 'Asia/Tokyo')).toBe(at)
  })

  it('reads a time its own zone skips as after it', () => {
    expect(instantAt('2027-03-28', '02:30', 'Europe/Paris')).toBe(Date.UTC(2027, 2, 28, 1, 30) / 1000)
  })

  it('goes both ways in any zone', () => {
    const at = Date.UTC(2026, 9, 20, 12, 0) / 1000
    for (const zone of ['America/Los_Angeles', 'Asia/Kolkata', 'Europe/Paris', 'UTC']) {
      const f = wallFields(at, zone)
      expect(instantAt(f.date, f.time, zone)).toBe(at)
    }
    expect(instantAt('2026-13-40', 'xx', 'UTC')).toBeNull()
    expect(zoneOffsetLabel('Asia/Kolkata', at)).toBe('UTC+5:30')
  })
})
