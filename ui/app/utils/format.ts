import { ref } from 'vue'
import type { Address, Role } from './types'

// Reactive so dates and labels computed from it follow a language change; set by plugins/00.language.
const language = ref<'fr' | 'en'>('en')
let translate: (key: string, params?: Record<string, unknown>, plural?: number) => string = key => key

export function setFormatLanguage(code: 'fr' | 'en', t: typeof translate) {
  language.value = code
  translate = t
}

/** Translation outside components. A numeric `n` param also picks the plural form. */
export function tr(key: string, params?: Record<string, unknown>): string {
  // Reactive dependency for the computeds that call tr().
  void language.value
  return translate(key, params, typeof params?.n === 'number' ? params.n : undefined)
}

export function intlLocale(): string {
  return language.value === 'en' ? 'en-US' : 'fr-FR'
}

export type HourCycle = 'auto' | 'h12' | 'h23'
const hourCycle = ref<HourCycle>('auto')
export function setHourCycle(cycle: HourCycle) {
  hourCycle.value = cycle
}

const formatters = new Map<string, Intl.DateTimeFormat>()
export function dateFormat(options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  // `hourCycle` rather than `hour12`: `hour12: false` gives "24:05" after midnight in en-US.
  const cycle = hourCycle.value
  if (cycle !== 'auto' && (options.hour !== undefined || options.timeStyle !== undefined)) options = { ...options, hourCycle: cycle }
  const key = intlLocale() + JSON.stringify(options)
  let f = formatters.get(key)
  if (!f) {
    f = new Intl.DateTimeFormat(intlLocale(), options)
    formatters.set(key, f)
  }
  return f
}

const time = { format: (d: Date) => dateFormat({ hour: '2-digit', minute: '2-digit' }).format(d) }
const dayMonth = { format: (d: Date) => dateFormat({ day: 'numeric', month: 'short' }).format(d) }
const full = { format: (d: Date) => dateFormat({ day: '2-digit', month: '2-digit', year: 'numeric' }).format(d) }
const long = { format: (d: Date) => dateFormat({ dateStyle: 'full', timeStyle: 'short' }).format(d) }

export function shortDate(ts: number): string {
  const d = new Date(ts * 1000)
  const now = new Date()
  if (d.toDateString() === now.toDateString()) return time.format(d)
  if (d.getFullYear() === now.getFullYear()) return dayMonth.format(d)
  return full.format(d)
}

const month = { format: (d: Date) => dateFormat({ month: 'long' }).format(d) }
const monthYear = { format: (d: Date) => dateFormat({ month: 'long', year: 'numeric' }).format(d) }

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
}

export function dateGroup(ts: number): string {
  const d = new Date(ts * 1000)
  const now = new Date()
  const days = Math.round((startOfDay(now) - startOfDay(d)) / 86_400_000)
  if (days <= 0) return tr('common.date.today')
  if (days === 1) return tr('common.date.yesterday')
  if (days < 7) return tr('common.date.thisWeek')
  const label = d.getFullYear() === now.getFullYear()
    ? (d.getMonth() === now.getMonth() ? tr('common.date.earlierThisMonth') : month.format(d))
    : monthYear.format(d)
  return label.charAt(0).toUpperCase() + label.slice(1)
}

export function longDate(ts: number): string {
  return long.format(new Date(ts * 1000))
}

export function addressLabel(a: Address | null | undefined): string {
  if (!a) return tr('common.unknown')
  return a.name || a.email
}

export function firstName(a: Address): string {
  const name = a.name?.trim()
  return name ? name.split(/\s+/)[0]! : a.email.split('@')[0]!
}

/** `own`: our addresses (account and aliases), lowercased, see useMail().ownSet. */
export function peopleLabel(people: Address[], own: ReadonlySet<string> = new Set()): string {
  const isMe = (a: Address) => own.has(a.email.toLowerCase())
  if (!people.length) return tr('common.unknown')
  if (people.length === 1) return isMe(people[0]!) ? tr('common.me') : addressLabel(people[0])
  const names = people.map(a => (isMe(a) ? tr('common.me') : firstName(a)))
  return names.length > 3 ? `${names.slice(0, 2).join(', ')} +${names.length - 2}` : names.join(', ')
}

export function initials(a: Address | string | null | undefined): string {
  if (!a) return '?'
  const text = typeof a === 'string' ? a.replace(/@.*/, '') : (a.name?.trim() || a.email.split('@')[0]!)
  const words = text.split(/[\s._-]+/).filter(Boolean)
  const letters = words.length > 1 ? words[0]![0]! + words[1]![0]! : (words[0] ?? '').slice(0, 2)
  return letters.toUpperCase() || '?'
}

export function avatarHue(a: Address | null | undefined): number {
  const key = a?.email.toLowerCase() ?? ''
  let h = 0
  for (const c of key) h = (h * 31 + c.charCodeAt(0)) >>> 0
  return h % 360
}

export function accountName(a: { label?: string | null, email: string }): string {
  return a.label?.trim() || a.email
}

/** Quotes names with special characters, like the engine's `format_address`: a name such as
 *  "Boss <attacker@x.com>" must not become the address. */
/** A recipient as written ("Name <address>", `"Last, First" <address>` or a bare address). */
export function parseRecipient(piece: string): Address {
  const m = /^(.*?)\s*<\s*([^<>\s]+)\s*>$/.exec(piece)
  if (!m) return { name: null, email: piece.trim() }
  let name = m[1]!.trim()
  if (name.startsWith('"') && name.endsWith('"') && name.length >= 2) name = name.slice(1, -1).replace(/\\(.)/g, '$1')
  return { name: name || null, email: m[2]! }
}

export function addressFull(a: Address): string {
  const name = a.name?.trim()
  if (!name) return a.email
  return /[()<>[\]:;@\\,."]/.test(name) ? `"${name.replace(/[\\"]/g, m => `\\${m}`)}" <${a.email}>` : `${name} <${a.email}>`
}

export function fileSize(bytes: number): string {
  if (bytes < 1024) return tr('common.size.b', { n: bytes })
  if (bytes < 1024 * 1024) return tr('common.size.kb', { n: Math.round(bytes / 1024) })
  if (bytes < 1024 ** 3) return tr('common.size.mb', { n: (bytes / 1024 / 1024).toLocaleString(intlLocale(), { maximumFractionDigits: 1 }) })
  return tr('common.size.gb', { n: (bytes / 1024 ** 3).toLocaleString(intlLocale(), { maximumFractionDigits: 1 }) })
}

export const roleIcons: Record<Role, string> = {
  inbox: 'i-lucide-inbox',
  sent: 'i-lucide-send',
  drafts: 'i-lucide-file-pen',
  trash: 'i-lucide-trash-2',
  junk: 'i-lucide-shield-alert',
  archive: 'i-lucide-archive',
  all: 'i-lucide-mails',
  flagged: 'i-lucide-pin',
  important: 'i-lucide-bookmark',
  snoozed: 'i-lucide-alarm-clock',
  screener: 'i-lucide-user-round-search',
  refused: 'i-lucide-user-round-x'
}

export const roleLabels = new Proxy({} as Record<Role, string>, {
  get: (_, role) => tr(`common.role.${String(role)}`)
})

export function escapeHtml(text: string): string {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')
}

/** Parsed outside the document, so no image is loaded. */
export function htmlText(html: string): string {
  return new DOMParser().parseFromString(html, 'text/html').body.textContent ?? ''
}

/** Signatures written before the rich editor are plain text; an emptied editor leaves an empty
 *  paragraph. */
export function signatureContent(signature: string | undefined): string {
  const s = signature?.trim() ?? ''
  if (!s) return ''
  if (!s.startsWith('<')) return textToHtml(s)
  return htmlText(s).trim() || /<img\b/i.test(s) ? s : ''
}

export function textToHtml(text: string): string {
  if (!text.trim()) return ''
  return text
    .split(/\n{2,}/)
    .map(p => `<p>${escapeHtml(p).replace(/\n/g, '<br>')}</p>`)
    .join('')
}

const weekdayTime = { format: (d: Date) => dateFormat({ weekday: 'long', hour: '2-digit', minute: '2-digit' }).format(d) }
const dayMonthTime = { format: (d: Date) => dateFormat({ day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' }).format(d) }

export function untilLabel(ts: number): string {
  const d = new Date(ts * 1000)
  const days = Math.round((startOfDay(d) - startOfDay(new Date())) / 86_400_000)
  if (days <= 0) return tr('common.until.today', { time: time.format(d) })
  if (days === 1) return tr('common.until.tomorrow', { time: time.format(d) })
  if (days < 7) return weekdayTime.format(d)
  return dayMonthTime.format(d)
}

export function ago(ts: number): string {
  const days = Math.floor((Date.now() / 1000 - ts) / 86_400)
  if (days <= 0) return tr('common.ago.today')
  if (days === 1) return tr('common.ago.yesterday')
  if (days < 7) return tr('common.ago.days', { n: days })
  const weeks = Math.floor(days / 7)
  return weeks === 1 ? tr('common.ago.week') : tr('common.ago.weeks', { n: weeks })
}

export interface WhenPreset { label: string, icon: string, at: number }

export function whenPresets(kind: 'snooze' | 'send', now = new Date()): WhenPreset[] {
  const at = (days: number, hour: number) => {
    const d = new Date(now.getFullYear(), now.getMonth(), now.getDate() + days, hour)
    return Math.floor(d.getTime() / 1000)
  }
  // Days until the next given weekday, never today.
  const until = (weekday: number) => ((weekday - now.getDay() + 7) % 7) || 7
  const weekend = now.getDay() === 6 || now.getDay() === 0
  const list: WhenPreset[] = kind === 'snooze'
    ? [
        { label: tr('common.when.in3h'), icon: 'i-lucide-hourglass', at: Math.floor(now.getTime() / 1000 / 60) * 60 + 3 * 3600 },
        ...(now.getHours() < 17 ? [{ label: tr('common.when.tonight'), icon: 'i-lucide-sunset', at: at(0, 18) }] : []),
        { label: tr('common.when.tomorrowMorning'), icon: 'i-lucide-sunrise', at: at(1, 8) },
        ...(weekend ? [] : [{ label: tr('common.when.weekend'), icon: 'i-lucide-sofa', at: at(until(6), 8) }]),
        { label: tr('common.when.nextWeek'), icon: 'i-lucide-calendar-arrow-up', at: at(until(1), 8) }
      ]
    : [
        { label: tr('common.when.tomorrowMorning'), icon: 'i-lucide-sunrise', at: at(1, 8) },
        { label: tr('common.when.tomorrowAfternoon'), icon: 'i-lucide-sun', at: at(1, 13) },
        { label: tr('common.when.mondayMorning'), icon: 'i-lucide-calendar-arrow-up', at: at(until(1), 8) }
      ]
  // "Tomorrow morning" and "Monday morning" coincide on Sundays.
  return list.filter((p, i) => list.findIndex(q => q.at === p.at) === i)
}
