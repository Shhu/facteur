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

/** The wall clock of `zone` at an instant (Unix seconds), as a local Date showing those figures:
 *  for the form's date and time fields. */
export function wallDate(unix: number, zone: string): Date {
  const w = wallParts(unix * 1000, zone)
  return new Date(w.y, w.m - 1, w.d, w.h, w.min)
}

/** The instant (Unix seconds) a wall clock (a local Date's figures) shows in `zone`. A time
 *  skipped by a change of clocks is read as after it. */
export function instantOf(wall: Date, zone: string): number {
  const asUtc = Date.UTC(wall.getFullYear(), wall.getMonth(), wall.getDate(), wall.getHours(), wall.getMinutes())
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
