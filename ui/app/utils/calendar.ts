import { ref } from 'vue'
import { dateFormat, intlLocale, tr } from './format'
import { meetingLink } from './invites'
import type { CalendarEvent } from './types'

// Everything is in local time: days start at local midnight, and a day may last 23 or 25 hours
// when the clock changes.

const DAY = 86_400

export function dayStart(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate())
}

/** Same wall-clock time, not n × 24 h. */
export function addDays(d: Date, n: number): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate() + n, d.getHours(), d.getMinutes())
}

export const sameDay = (a: Date, b: Date) => a.toDateString() === b.toDateString()

export const unix = (d: Date) => Math.floor(d.getTime() / 1000)

export function dayKey(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

const chosenWeekStart = ref<number | null>(null)
export function setWeekStart(day: number | null) {
  chosenWeekStart.value = day
}

/** As `Date.getDay()`: the settings' choice, else Monday in French, else the locale's own when
 *  the browser knows it. */
export function weekStartDay(): number {
  if (chosenWeekStart.value !== null) return chosenWeekStart.value
  const locale = intlLocale()
  if (locale.startsWith('fr')) return 1
  try {
    const l = new Intl.Locale(locale) as Intl.Locale & { getWeekInfo?: () => { firstDay: number }, weekInfo?: { firstDay: number } }
    const first = l.getWeekInfo?.().firstDay ?? l.weekInfo?.firstDay
    if (first) return first % 7
  } catch {
    // Engines without Intl week info fall back to Monday.
  }
  return 1
}

export function weekStart(d: Date, first = weekStartDay()): Date {
  const day = dayStart(d)
  return addDays(day, -((day.getDay() - first + 7) % 7))
}

export function monthGrid(d: Date, first = weekStartDay()): Date[] {
  const start = weekStart(new Date(d.getFullYear(), d.getMonth(), 1), first)
  return Array.from({ length: 42 }, (_, i) => addDays(start, i))
}

/** End is exclusive; a zero-length event counts on the day it happens. */
export function onDay(e: CalendarEvent, day: Date): boolean {
  const from = unix(day)
  const to = unix(addDays(day, 1))
  return e.start < to && (e.end > from || (e.end === e.start && e.start >= from))
}

/** Stable across syncs, unlike the row id. */
export const eventKey = (e: CalendarEvent) => `${e.calendar_id}\u0000${e.uid}\u0000${e.instance}`

/** Shown in the all-day band: all-day events, and timed ones lasting a full day or more. */
export const inBand = (e: CalendarEvent) => e.all_day || e.end - e.start >= DAY

export interface TimedBlock {
  event: CalendarEvent
  top: number
  bottom: number
  col: number
  cols: number
  span: number
  continuesBefore: boolean
  continuesAfter: boolean
}

/** Wall-clock minutes, clipped to 0..1440: the grid shows 24 hours even on a 23- or 25-hour day,
 *  and 15:00 stays on the 15:00 line. */
export function wallMinutes(t: number, from: number, to: number): number {
  if (t <= from) return 0
  if (t >= to) return 1440
  const d = new Date(t * 1000)
  return Math.min(1440, Math.max(0, d.getHours() * 60 + d.getMinutes() + d.getSeconds() / 60))
}

/** Overlapping events sit side by side; an event widens over free columns to its right. */
export function layoutDay(events: CalendarEvent[], day: Date, minHeight = 20): TimedBlock[] {
  const from = unix(day)
  const to = unix(addDays(day, 1))
  const minutes = (t: number) => wallMinutes(t, from, to)
  const blocks: TimedBlock[] = events
    .filter(e => !inBand(e) && onDay(e, day))
    .map(e => ({
      event: e,
      top: minutes(e.start),
      // Short events still take room for their title.
      bottom: Math.max(minutes(e.end), minutes(e.start) + minHeight),
      col: 0,
      cols: 1,
      span: 1,
      continuesBefore: e.start < from,
      continuesAfter: e.end > to
    }))
    .sort((a, b) => a.top - b.top || b.bottom - a.bottom)

  // Groups of transitively overlapping blocks share a column count.
  let group: TimedBlock[] = []
  let groupEnd = -1
  const columns: TimedBlock[][] = []
  const close = () => {
    const cols = columns.length
    for (const b of group) {
      b.cols = cols
      let span = 1
      for (let c = b.col + 1; c < cols; c++) {
        if (columns[c]!.some(o => o.top < b.bottom && o.bottom > b.top)) break
        span++
      }
      b.span = span
    }
    group = []
    columns.length = 0
  }
  for (const b of blocks) {
    if (group.length && b.top >= groupEnd) close()
    let c = columns.findIndex(col => col.at(-1)!.bottom <= b.top)
    if (c === -1) {
      c = columns.length
      columns.push([])
    }
    columns[c]!.push(b)
    b.col = c
    group.push(b)
    groupEnd = group.length === 1 ? b.bottom : Math.max(groupEnd, b.bottom)
  }
  if (group.length) close()
  return blocks
}

export interface BandBlock {
  event: CalendarEvent
  from: number
  to: number
  lane: number
  continuesBefore: boolean
  continuesAfter: boolean
}

export function layoutBand(events: CalendarEvent[], days: Date[]): BandBlock[] {
  const lanes: number[] = []
  const out: BandBlock[] = []
  const list = events.filter(inBand).sort((a, b) => a.start - b.start || b.end - a.end)
  for (const e of list) {
    const cols = days.map((d, i) => (onDay(e, d) ? i : -1)).filter(i => i >= 0)
    if (!cols.length) continue
    const from = cols[0]!
    const to = cols.at(-1)!
    let lane = lanes.findIndex(end => end < from)
    if (lane === -1) {
      lane = lanes.length
      lanes.push(to)
    } else lanes[lane] = to
    out.push({
      event: e,
      from,
      to,
      lane,
      continuesBefore: e.start < unix(days[0]!),
      continuesAfter: e.end > unix(addDays(days.at(-1)!, 1))
    })
  }
  return out
}

export type TextPart = { text: string, href?: undefined } | { text: string, href: string }

/** Links as parts so the template renders them without v-html. */
export function linkify(text: string): TextPart[] {
  const parts: TextPart[] = []
  const re = /\bhttps?:\/\/[^\s<>"')\]]+[^\s<>"')\].,;:!?]/gi
  let last = 0
  for (const m of text.matchAll(re)) {
    if (m.index > last) parts.push({ text: text.slice(last, m.index) })
    parts.push({ text: m[0], href: m[0] })
    last = m.index + m[0].length
  }
  if (last < text.length) parts.push({ text: text.slice(last) })
  return parts
}

export const timeOf = (t: number) => dateFormat({ hour: '2-digit', minute: '2-digit' }).format(new Date(t * 1000))
const dayLong = (d: Date) => dateFormat({ weekday: 'long', day: 'numeric', month: 'long' }).format(d)
const dayShort = (d: Date) => dateFormat({ day: 'numeric', month: 'short' }).format(d)

/** All-day ends are exclusive midnights. */
function lastDay(e: CalendarEvent): Date {
  return dayStart(new Date((e.end > e.start ? e.end - 1 : e.start) * 1000))
}

export function eventTime(e: CalendarEvent): string {
  const s = new Date(e.start * 1000)
  const last = lastDay(e)
  if (e.all_day) return sameDay(s, last) ? tr('tools.calendar.allDay') : `${dayShort(s)} – ${dayShort(last)}`
  if (sameDay(s, new Date(e.end * 1000)) || e.end === e.start) return e.end > e.start ? `${timeOf(e.start)} – ${timeOf(e.end)}` : timeOf(e.start)
  return `${dayShort(s)} ${timeOf(e.start)} – ${dayShort(new Date(e.end * 1000))} ${timeOf(e.end)}`
}

export function eventWhen(e: CalendarEvent): string {
  const s = new Date(e.start * 1000)
  const last = lastDay(e)
  const first = dayLong(s)
  if (e.all_day) return sameDay(s, last) ? `${first} · ${tr('tools.calendar.allDay')}` : `${first} – ${dayLong(last)}`
  const end = new Date(e.end * 1000)
  if (sameDay(s, end) || e.end <= e.start) return `${first} · ${e.end > e.start ? `${timeOf(e.start)} – ${timeOf(e.end)}` : timeOf(e.start)}`
  return `${first} ${timeOf(e.start)} – ${dayLong(end)} ${timeOf(e.end)}`
}

export function dayHeading(d: Date, today = new Date()): string {
  if (sameDay(d, today)) return tr('common.date.today')
  if (sameDay(d, addDays(dayStart(today), 1))) return tr('tools.calendar.tomorrow')
  const label = dayLong(d)
  return label.charAt(0).toUpperCase() + label.slice(1)
}

export function isoWeek(d: Date): number {
  const t = new Date(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()))
  const day = t.getUTCDay() || 7
  t.setUTCDate(t.getUTCDate() + 4 - day)
  const year = new Date(Date.UTC(t.getUTCFullYear(), 0, 1))
  return Math.ceil(((t.getTime() - year.getTime()) / 86_400_000 + 1) / 7)
}

export function eventPlace(e: CalendarEvent): string | null {
  const l = e.location?.trim()
  if (!l) return null
  const meeting = meetingLink({ location: l, description: null })
  return meeting && l.includes(meeting.url) ? meeting.provider : l
}
