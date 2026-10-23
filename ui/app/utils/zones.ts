/** The computer's IANA zone. */
export function localZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
}

const formats = new Map<string, Intl.DateTimeFormat>()
function format(zone: string): Intl.DateTimeFormat {
  let f = formats.get(zone)
  if (!f) {
    f = new Intl.DateTimeFormat('en-US', { timeZone: zone, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' })
    formats.set(zone, f)
  }
  return f
}

/** The wall clock of `zone` at an instant (milliseconds), its fields as numbers. */
function wallParts(ms: number, zone: string) {
  const parts: Record<string, number> = {}
  for (const p of format(zone).formatToParts(new Date(ms))) {
    if (p.type !== 'literal') parts[p.type] = Number(p.value)
  }
  return { y: parts.year!, m: parts.month!, d: parts.day!, h: parts.hour! % 24, min: parts.minute!, s: parts.second! }
}

/** Milliseconds `zone` is ahead of UTC at an instant. */
function offset(ms: number, zone: string): number {
  const w = wallParts(ms, zone)
  return Date.UTC(w.y, w.m - 1, w.d, w.h, w.min, w.s) - Math.floor(ms / 1000) * 1000
}

const two = (n: number) => String(n).padStart(2, '0')

/** The wall clock of `zone` at an instant (Unix seconds), as the form's fields write it
 *  ("2026-10-20", "14:00"). Kept as figures: a local Date would shift a time the computer's own
 *  clock skips. */
export function wallFields(unix: number, zone: string): { date: string, time: string } {
  const w = wallParts(unix * 1000, zone)
  return { date: `${w.y}-${two(w.m)}-${two(w.d)}`, time: `${two(w.h)}:${two(w.min)}` }
}

/** The instant (Unix seconds) the form's fields show in `zone`; `null` when unreadable. A time
 *  skipped by a change of clocks is read as after it. */
export function instantAt(date: string, time: string, zone: string): number | null {
  const [y, m, d] = date.split('-').map(Number)
  const [h, min] = time.split(':').map(Number)
  if (!y || !m || !d || h === undefined || min === undefined || Number.isNaN(h) || Number.isNaN(min)) return null
  const asUtc = Date.UTC(y, m - 1, d, h, min)
  let ms = asUtc - offset(asUtc, zone)
  ms = asUtc - offset(ms, zone)
  return Math.round(ms / 1000)
}

/** "UTC+9", "UTC−3:30", "UTC" at an instant (Unix seconds). */
export function zoneOffsetLabel(zone: string, unix: number): string {
  const minutes = Math.round(offset(unix * 1000, zone) / 60_000)
  if (!minutes) return 'UTC'
  const sign = minutes < 0 ? '−' : '+'
  const a = Math.abs(minutes)
  return `UTC${sign}${Math.floor(a / 60)}${a % 60 ? `:${String(a % 60).padStart(2, '0')}` : ''}`
}

/** Every zone the system knows (IANA names). */
export function allZones(): string[] {
  try {
    const list = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf?.('timeZone')
    if (list?.length) return list
  } catch {
    // Older engines: the computer's zone only.
  }
  return [localZone()]
}
