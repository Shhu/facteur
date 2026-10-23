import type { Invite } from './types'

export type InviteKind = 'invitation' | 'update' | 'cancelled' | 'reply'

// List rows don't carry the ICS: the subject is all there is.
/** Prefixes only calendar services write: always an invitation. Google (fr/en) and Outlook forms. */
const STRONG_PREFIXES: [RegExp, InviteKind][] = [
  [/^(?:invitation mise à jour|updated invitation)(?: avec note| with note)?\s*[:：]\s*/i, 'update'],
  [/^(?:événement annulé|evenement annule|canceled event|cancelled event|invitation annulée)(?: avec note| with note)?\s*[:：]\s*/i, 'cancelled'],
  [/^(?:invitation updated|updated event|événement mis à jour)\s*[:：]\s*/i, 'update']
]
/** Prefixes ordinary mail may also start with: trusted only with another hint of an invitation. */
const WEAK_PREFIXES: [RegExp, InviteKind][] = [
  [/^invitation\s*[:：]\s*/i, 'invitation'],
  [/^(?:mis à jour|mise à jour|updated)\s*[:：]\s*/i, 'update'],
  [/^(?:annulé|annulée|canceled|cancelled)\s*[:：]\s*/i, 'cancelled']
]
/** Google's tail: "@ Fri Oct 23, 2026 8am - 11am (UTC+2) (me@x.com)". */
const GOOGLE_TAIL = /\s+@\s+(?=[^@]*\d).*\(.*\)\s*$/
/** The tail is looked for in the subject's last characters only: on a whole crafted subject
 *  ("@ 1( @ 1( …") the pattern backtracks for minutes. */
const TAIL_MAX = 200

/** Where Google's tail starts in `text`, or -1. */
function googleTail(text: string): number {
  const from = Math.max(0, text.length - TAIL_MAX)
  const m = GOOGLE_TAIL.exec(text.slice(from))
  return m ? from + m.index : -1
}

/** Conservative: a weak prefix ("Invitation:", "Cancelled:") counts only with Google's date tail
 *  or an attachment (the .ics), never on its own. */
export function inviteFromSubject(subject: string, hasAttachments = false): { kind: InviteKind, title: string } | null {
  const s = subject.trim()
  for (const [re, kind] of STRONG_PREFIXES) {
    if (re.test(s)) return { kind, title: clean(s.replace(re, '')) || s }
  }
  for (const [re, kind] of WEAK_PREFIXES) {
    if (!re.test(s)) continue
    const rest = s.replace(re, '')
    if (googleTail(rest) < 0 && !hasAttachments) return null
    return { kind, title: clean(rest) || s }
  }
  return null
}

function clean(title: string): string {
  const tail = googleTail(title)
  return (tail < 0 ? title : title.slice(0, tail)).trim()
}

/** Google's change label, in French and English ("Changed: time"). */
const CHANGE_LABEL = /(?:modifications?\s*\(s\)|modifications?|changed|changes)\s*[:：]\s*/i
/** Fields Google names in an update, to cut them out of a one-line preview. */
const CHANGE_FIELDS = /^(?:(?:fuseau horaire|time zone|video call|titre|heure|date|lieu|description|invités|participants|visioconférence|récurrence|title|time|location|guests|attendees|conferencing|recurrence)(?:\s*(?:,|et|and|&)\s*|\s*$|(?=\s)))+/i

export function changeFromText(text: string | null | undefined): string | null {
  if (!text) return null
  const m = CHANGE_LABEL.exec(text)
  if (!m) return null
  const rest = text.slice(m.index + m[0].length)
  const line = rest.split('\n')[0]!.split(/[.·|]/)[0]!.trim()
  // A long line is not a field list: better nothing than a wrong label.
  if (line && line.length <= 40) return line
  return fieldsAt(rest)
}

/** In a one-line preview the title follows the fields without a break. */
export function changeFromPreview(preview: string): string | null {
  const m = CHANGE_LABEL.exec(preview)
  return m ? fieldsAt(preview.slice(m.index + m[0].length)) : null
}

function fieldsAt(text: string): string | null {
  const fields = CHANGE_FIELDS.exec(text.trimStart())?.[0].trim().replace(/\s*(?:,|et|and|&)$/i, '').trim()
  return fields || null
}

export function htmlLines(html: string): string {
  const marked = html.replace(/<(?:br|\/p|\/div|\/tr|\/td|\/th|\/li|\/h[1-6]|\/table)\b[^>]*>/gi, '$&\n')
  const text = new DOMParser().parseFromString(marked, 'text/html').body.textContent ?? ''
  return text.replace(/[ \t ]+/g, ' ').replace(/ *\n */g, '\n').replace(/\n{2,}/g, '\n').trim()
}

export function inviteState(invite: Invite, html?: string): { kind: InviteKind, change: string | null } {
  if (invite.cancelled) return { kind: 'cancelled', change: null }
  if (invite.method === 'REPLY') return { kind: 'reply', change: null }
  const change = changeFromText(invite.description) ?? (html ? changeFromText(htmlLines(html)) : null)
  const update = invite.method === 'REQUEST' && (invite.sequence > 0 || change !== null)
  return { kind: update ? 'update' : 'invitation', change: update ? change : null }
}

const MEETING_LINKS: { provider: string, re: RegExp }[] = [
  { provider: 'Google Meet', re: /https:\/\/meet\.google\.com\/[a-z0-9-]+(?:\?[^\s<>"')\]]*)?/i },
  { provider: 'Zoom', re: /https:\/\/(?:[\w-]+\.)*zoom\.us\/(?:j|my|w|s)\/[^\s<>"')\]]+/i },
  { provider: 'Microsoft Teams', re: /https:\/\/teams\.(?:microsoft|live)\.com\/(?:l\/meetup-join|meet)\/[^\s<>"')\]]+/i }
]

/** `meeting`: the link the provider gave (an event's), tried first. */
export function meetingLink(invite: Pick<Invite, 'location' | 'description'> & { meeting?: string | null }): { provider: string, url: string, display: string } | null {
  for (const text of [invite.meeting, invite.location, invite.description]) {
    if (!text) continue
    for (const { provider, re } of MEETING_LINKS) {
      const url = re.exec(text)?.[0]
      if (!url) continue
      const u = new URL(url)
      // Teams paths are long opaque tokens: the host says enough.
      const display = provider === 'Microsoft Teams' ? u.host : `${u.host}${u.pathname}`
      return { provider, url, display }
    }
  }
  // Another service the engine knows (Webex, Whereby, Jitsi…): named by its host.
  if (invite.meeting && /^https:\/\//i.test(invite.meeting)) {
    const u = new URL(invite.meeting)
    return { provider: u.host.replace(/^www\./, ''), url: invite.meeting, display: `${u.host}${u.pathname}` }
  }
  return null
}

/** Strips the boilerplate calendar services append: Google's "-::~:~::~" block, Outlook's line
 *  of underscores before the Teams block, and link-only lines. */
export function inviteNotes(description: string | null): string | null {
  if (!description) return null
  // Bounded: "<a<a<a…" without a ">" made the open pattern quadratic on a long description.
  let text = /<[a-z][^<>]{0,500}>/i.test(description.slice(0, 20_000)) ? htmlLines(description) : description
  text = text.split(/-::~[:~]*/)[0]!.split(/\n?_{10,}/)[0]!
  const lines = text.split('\n').filter(l => !/^\s*<?https?:\/\/\S+>?\s*$/.test(l) && !MEETING_LINKS.some(({ re }) => re.test(l)))
  const notes = lines.join('\n').replace(/\n{3,}/g, '\n\n').trim()
  return notes || null
}

export function inviteProvider(invite: Invite): 'google' | 'outlook' | null {
  if (/@google\.com$/i.test(invite.uid)) return 'google'
  if (/^040000008200E00074C5B7101A82E008/i.test(invite.uid)) return 'outlook'
  return null
}

/** Outlook writes Windows zone names, which Intl rejects. */
export function validZone(tz: string | null | undefined): string | null {
  if (!tz) return null
  try {
    new Intl.DateTimeFormat('en', { timeZone: tz })
    return tz
  } catch {
    return null
  }
}

export function zoneCity(tz: string): string {
  return (tz.split('/').at(-1) ?? tz).replace(/_/g, ' ')
}
