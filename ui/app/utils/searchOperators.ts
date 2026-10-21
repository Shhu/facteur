/** Search operators, parsed on the UI side exactly like the engine (crates/core/src/search.rs),
 *  to show what a query will filter on. The query sent to the engine is never rewritten. */

import { escapeHtml } from './format'
import type { Role } from './types'

export type SearchOperator = 'from' | 'to' | 'with' | 'subject' | 'in' | 'before' | 'after'
export type SearchFlag = 'attachment' | 'unread' | 'read' | 'pinned' | 'labelled' | 'unlabelled'

export type SearchChip =
  | { kind: 'operator', operator: SearchOperator, value: string }
  | { kind: 'flag', flag: SearchFlag }
  | { kind: 'word', value: string }

// Null prototype: a typed word is looked up as a key, and `constructor` or `__proto__` must not
// resolve to Object's own members (the engine reads them as plain words).
const OPERATORS: Record<string, SearchOperator> = Object.assign(Object.create(null), {
  'de': 'from', 'from': 'from',
  'à': 'to', 'a': 'to', 'to': 'to', 'pour': 'to',
  'avec': 'with', 'with': 'with',
  'objet': 'subject', 'sujet': 'subject', 'subject': 'subject',
  'dans': 'in', 'in': 'in', 'label': 'in', 'libellé': 'in', 'libelle': 'in',
  'avant': 'before', 'before': 'before',
  'après': 'after', 'apres': 'after', 'after': 'after', 'depuis': 'after', 'since': 'after'
})

const FLAGS: Record<string, SearchFlag> = Object.assign(Object.create(null), {
  'pj': 'attachment', 'has:attachment': 'attachment', 'avec:pj': 'attachment', 'has:pj': 'attachment',
  'non-lu': 'unread', 'nonlu': 'unread', 'is:unread': 'unread',
  'lu': 'read', 'is:read': 'read',
  'épinglé': 'pinned', 'epingle': 'pinned', 'épinglés': 'pinned', 'is:pinned': 'pinned', 'is:starred': 'pinned', 'is:flagged': 'pinned',
  'étiqueté': 'labelled', 'etiquete': 'labelled', 'has:userlabels': 'labelled',
  'sans-étiquette': 'unlabelled', 'sans-etiquette': 'unlabelled', 'has:nouserlabels': 'unlabelled'
})

/** Quotes kept, so the token can be edited back as typed. */
interface RawToken {
  raw: string
  start: number
  end: number
}

/** Like the engine's `tokens()`: quoted text stays together (`subject:"march invoice"`). */
function rawTokens(input: string): RawToken[] {
  const out: RawToken[] = []
  let start = -1
  let quoted = false
  for (let i = 0; i < input.length; i++) {
    const c = input[i]!
    if (/\s/.test(c) && !quoted) {
      if (start >= 0) out.push({ raw: input.slice(start, i), start, end: i })
      start = -1
    } else {
      if (start < 0) start = i
      if (c === '"') quoted = !quoted
    }
  }
  if (start >= 0) out.push({ raw: input.slice(start), start, end: input.length })
  return out
}

const unquote = (raw: string) => raw.replaceAll('"', '')

export function tokens(input: string): string[] {
  return rawTokens(input).map(t => unquote(t.raw)).filter(t => t.length > 0)
}

/** Mirrors the engine's `day()`: `YYYY-MM-DD`, `D/M/YYYY` or `D/M/YY` (chrono is lenient on
 *  padding), a real calendar day (no 31/04), from 1900 on (`after:26-03-01` is dropped). */
export function readableDay(value: string): boolean {
  const iso = /^\+?(\d{1,4})-(\d{1,2})-(\d{1,2})$/.exec(value)
  const dmy = iso ? null : /^(\d{1,2})\/(\d{1,2})\/(\d{1,4})$/.exec(value)
  const [y, m, d] = iso ? [iso[1], iso[2], iso[3]] : dmy ? [dmy[3], dmy[2], dmy[1]] : []
  if (!y || !m || !d) return false
  // A D/M year of one or two digits goes through chrono's `%y` (1969–2068); any other is read as is.
  const short = dmy && y.length <= 2 ? Number(y) : null
  const year = short === null ? Number(y) : short < 69 ? 2000 + short : 1900 + short
  if (year < 1900) return false
  const month = Number(m), date = Number(d)
  // setUTCFullYear, not Date.UTC, which would turn a two-digit year into 19xx.
  const probe = new Date(0)
  probe.setUTCFullYear(year, month - 1, date)
  return probe.getUTCFullYear() === year && probe.getUTCMonth() === month - 1 && probe.getUTCDate() === date
}

/** A date the engine cannot read (`before:yesterday`, `before:03/31/2026`) is dropped by it, so
 *  it shows as a word, not a block. */
export function chipOf(token: string): SearchChip {
  const flag = FLAGS[token.toLowerCase()]
  if (flag) return { kind: 'flag', flag }
  const at = token.indexOf(':')
  const value = at >= 0 ? token.slice(at + 1) : ''
  const operator = at >= 0 && value ? OPERATORS[token.slice(0, at).toLowerCase()] : undefined
  if (operator && (operator === 'before' || operator === 'after') && !readableDay(value)) return { kind: 'word', value: token }
  return operator ? { kind: 'operator', operator, value } : { kind: 'word', value: token }
}

export interface ParsedSearch {
  chips: SearchChip[]
  filtered: boolean
  /** Text to highlight in results: free words and `subject:` values. */
  terms: string[]
}

export function parseSearch(input: string): ParsedSearch {
  const chips: SearchChip[] = []
  const terms: string[] = []
  for (const token of tokens(input)) {
    const chip = chipOf(token)
    chips.push(chip)
    if (chip.kind === 'word' || (chip.kind === 'operator' && chip.operator === 'subject')) terms.push(chip.value)
  }
  return { chips, filtered: chips.some(c => c.kind !== 'word'), terms }
}

/** `short`: the desktop footer's subset. */
export function operatorShortcuts(language: string, short = false): string[] {
  const all = language === 'fr'
    ? ['de:', 'à:', 'objet:', 'dans:', 'pj', 'non-lu', 'épinglé']
    : ['from:', 'to:', 'subject:', 'in:', 'has:attachment', 'is:unread', 'is:pinned']
  return short ? [all[0]!, all[2]!, all[3]!, all[4]!, all[5]!] : all
}

/** A flag is appended only once. */
export function appendOperator(query: string, operator: string): string {
  const base = query.trimEnd()
  if (!operator.endsWith(':') && tokens(base).some(t => t.toLowerCase() === operator)) return query
  return `${base ? `${base} ` : ''}${operator}${operator.endsWith(':') ? '' : ' '}`
}

/** `text` as HTML, every occurrence of `terms` wrapped in `<mark>` (case-insensitive). The text
 *  is escaped piece by piece: nothing from the message is ever injected as markup. */
export function highlightHtml(text: string, terms: string[], markClass = ''): string {
  const words = [...new Set(terms.map(t => t.trim()).filter(t => t.length > 0))]
  if (!words.length) return escapeHtml(text)
  words.sort((a, b) => b.length - a.length)
  const pattern = new RegExp(`(${words.map(w => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|')})`, 'giu')
  const open = markClass ? `<mark class="${escapeHtml(markClass)}">` : '<mark>'
  return text.split(pattern).map((part, i) => (i % 2 ? `${open}${escapeHtml(part)}</mark>` : escapeHtml(part))).join('')
}

const CATALOG: { tokens: [fr: string, en: string], key: string }[] = [
  { tokens: ['de:', 'from:'], key: 'from' },
  { tokens: ['à:', 'to:'], key: 'to' },
  { tokens: ['avec:', 'with:'], key: 'with' },
  { tokens: ['objet:', 'subject:'], key: 'subject' },
  { tokens: ['dans:', 'in:'], key: 'in' },
  { tokens: ['après:', 'after:'], key: 'after' },
  { tokens: ['avant:', 'before:'], key: 'before' },
  { tokens: ['pj', 'has:attachment'], key: 'attachment' },
  { tokens: ['non-lu', 'is:unread'], key: 'unread' },
  { tokens: ['lu', 'is:read'], key: 'read' },
  { tokens: ['épinglé', 'is:pinned'], key: 'pinned' },
  { tokens: ['étiqueté', 'has:userlabels'], key: 'labelled' },
  { tokens: ['sans-étiquette', 'has:nouserlabels'], key: 'unlabelled' }
]

const fold = (s: string) => s.toLowerCase().normalize('NFD').replace(/\p{Diacritic}/gu, '')

export interface OperatorSuggestion {
  insert: string
  /** i18n key of its description, under `mail.searchSuggest`. */
  key: string
}

/** The token reaching the end of `query`, as typed (none after a space, unless inside quotes). */
function typedToken(query: string): string {
  const last = rawTokens(query).at(-1)
  return last && last.end === query.length ? last.raw : ''
}

export function lastToken(query: string): string {
  return unquote(typedToken(query))
}

/** The interface language's spelling first, the other one only when it alone matches. */
export function suggestOperators(word: string, language: string): OperatorSuggestion[] {
  const typed = fold(word)
  if (!typed || (typed.includes(':') && !typed.endsWith(':') && !typed.startsWith('is:') && !typed.startsWith('has:'))) return []
  const mine = language === 'fr' ? 0 : 1
  const out: OperatorSuggestion[] = []
  for (const entry of CATALOG) {
    const [fr, en] = entry.tokens
    const order = mine === 0 ? [fr, en] : [en, fr]
    const match = order.find(t => fold(t).startsWith(typed) && fold(t) !== typed)
    if (match) out.push({ insert: match, key: entry.key })
  }
  return out
}

/** `from:` without its value, `is:p`, `has`: searching now would only be a slow lookup of a word
 *  nobody means. */
export function incompleteOperator(query: string): boolean {
  const word = fold(lastToken(query))
  if (!word) return false
  if (word.endsWith(':')) return true
  if (FLAGS[word]) return false
  const known = [...CATALOG.flatMap(e => e.tokens), ...Object.keys(FLAGS)].map(fold)
  // With its colon: the start of a flag (`is:un`, `has:att`). A finished but unknown one
  // (`has:attachments`, `is:important`) is no start of anything: searched, the engine just skips
  // it, where waiting would leave the previous query's results on screen. Before the colon: only
  // `is` and `has`, which are nothing else (`de` or `to` may well be words).
  if (word.includes(':')) return known.some(t => t.startsWith(word) && t !== word)
  return ['is:', 'has:'].some(t => t.startsWith(word) && word.length > 1)
}

export function completeOperator(query: string, insert: string): string {
  const word = typedToken(query)
  const base = query.slice(0, query.length - word.length)
  return `${base}${insert}${insert.endsWith(':') ? '' : ' '}`
}

/** `from:mar` → operator `from`, prefix `from:` as typed, value `mar`. */
export interface ValueContext {
  operator: SearchOperator
  prefix: string
  value: string
}

export function valueContext(word: string): ValueContext | null {
  const at = word.indexOf(':')
  const operator = at > 0 ? OPERATORS[word.slice(0, at).toLowerCase()] : undefined
  if (!operator || FLAGS[word.toLowerCase()]) return null
  return { operator, prefix: word.slice(0, at + 1), value: word.slice(at + 1) }
}

/** The engine has no escape for a quote inside a quoted value, so quotes are dropped. */
export function quoteValue(value: string): string {
  const v = unquote(value)
  return /\s/.test(v) ? `"${v}"` : v
}

/** Words naming a folder role, like `folder_role()` in search.rs; the first of each language is
 *  the one inserted. */
const ROLE_WORDS: Partial<Record<Role, { fr: string, en: string, also?: string[] }>> = {
  inbox: { fr: 'réception', en: 'inbox', also: ['boîte', 'boite', 'reception'] },
  sent: { fr: 'envoyés', en: 'sent', also: ['envoyes'] },
  drafts: { fr: 'brouillons', en: 'drafts' },
  trash: { fr: 'corbeille', en: 'trash' },
  junk: { fr: 'indésirables', en: 'junk', also: ['spam', 'indesirables'] },
  archive: { fr: 'archives', en: 'archive' },
  all: { fr: 'tous', en: 'all' },
  snoozed: { fr: 'plus-tard', en: 'snoozed' }
}

export function folderRole(value: string): Role | null {
  const v = value.toLowerCase()
  for (const [role, w] of Object.entries(ROLE_WORDS)) {
    if (w.fr === v || w.en === v || w.also?.includes(v)) return role as Role
  }
  return null
}

/** A role word matches that folder in every account; otherwise the engine matches part of the name. */
export function folderValue(folder: { role: Role | null, display_name: string }, language: string): string {
  const words = folder.role ? ROLE_WORDS[folder.role] : undefined
  if (words) return language === 'fr' ? words.fr : words.en
  return quoteValue(folder.display_name)
}

export function isoDay(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

export function datePresets(now = new Date()): { key: string, value: string }[] {
  // A month back from the 31st is the last day of the shorter month, not the 1st after it.
  const day = (days: number, months = 0) => {
    const last = new Date(now.getFullYear(), now.getMonth() - months + 1, 0).getDate()
    return isoDay(new Date(now.getFullYear(), now.getMonth() - months, Math.min(now.getDate(), last) - days))
  }
  return [
    { key: 'today', value: day(0) },
    { key: 'yesterday', value: day(1) },
    { key: 'week', value: day(7) },
    { key: 'month', value: day(0, 1) },
    { key: 'quarter', value: day(0, 3) },
    { key: 'year', value: day(0, 12) }
  ]
}

/** A finished token of the field: an operator or a flag (shown as a block), or a word. `end`:
 *  where the next token starts (its spaces belong to it). Offsets are in the text split. */
export interface QueryPiece {
  raw: string
  chip: SearchChip
  start: number
  end: number
}

export function queryPieces(text: string, offset = 0): QueryPiece[] {
  const raw = rawTokens(text)
  return raw
    .map((t, i) => ({ raw: t.raw, chip: chipOf(unquote(t.raw)), start: offset + t.start, end: offset + (raw[i + 1]?.start ?? text.length) }))
    .filter(p => unquote(p.raw).length > 0)
}

/** `text` cut after its last finished operator or flag (and the spaces after it): `head` shows as
 *  blocks, `draft` stays in the input. A token is finished once a space follows it; `final`: the
 *  last one too (the field left). `head + draft` is always `text`. */
export function splitQuery(text: string, final = false): { head: string, draft: string } {
  let cut = 0
  for (const p of queryPieces(text)) {
    const finished = final || p.start + p.raw.length < text.length
    const closed = p.raw.split('"').length % 2 === 1
    if (finished && closed && p.chip.kind !== 'word') cut = p.end
  }
  return { head: text.slice(0, cut), draft: text.slice(cut) }
}

export function joinQuery(...parts: string[]): string {
  return parts.reduce((out, part) => (!part ? out : out && !/\s$/.test(out) && !/^\s/.test(part) ? `${out} ${part}` : out + part), '')
}
