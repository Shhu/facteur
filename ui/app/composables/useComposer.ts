import type { Account, Address, Draft, MessageDetail } from '~/utils/types'
import { SIGNATURE_CLASS } from '~/utils/editorExtensions'

/** Separated by "-- ", the convention mail clients know to fold. It sits in a marked block so that
 *  a change of sender replaces it as a whole. */
export function signatureHtml(signature: string | undefined): string {
  const content = signatureContent(signature)
  return content ? `<div class="${SIGNATURE_CLASS}"><p>-- </p>${content}</div>` : ''
}

/** The alias's own signature if it has one, else the account's. */
export function senderSignature(account: Account | null, from?: string | null): string {
  const identity = from ? account?.identities.find(i => i.email.toLowerCase() === from.toLowerCase()) : undefined
  return signatureHtml(identity && signatureContent(identity.signature) ? identity.signature : account?.signature)
}

/** A top-level blockquote (with the "X wrote:" line right above it) or the forward header. */
function quotedStart(body: HTMLElement): Element | null {
  const children = Array.from(body.children)
  const i = children.findIndex(el => el.tagName === 'BLOCKQUOTE' || (el.tagName === 'P' && (el.textContent ?? '').trimStart().startsWith('----------')))
  if (i < 0) return null
  const el = children[i]!
  const prev = children[i - 1]
  if (el.tagName === 'BLOCKQUOTE' && prev?.tagName === 'P' && /:\s*$/.test(prev.textContent ?? '')) return prev
  return el
}

/** The first top-level one above the quoted or forwarded message, which may hold a signature of
 *  its own. */
function signatureBlock(body: HTMLElement): Element | null {
  const children = Array.from(body.children)
  const end = quotedStart(body)
  const limit = end ? children.indexOf(end) : children.length
  return children.slice(0, limit).find(el => el.matches(`div.${SIGNATURE_CLASS}`)) ?? null
}

/** A quoted or forwarded message's signature blocks lose their mark: changing the sender only
 *  ever swaps the draft's own signature. */
function unmarkSignatures(html: string): string {
  if (!html.includes(SIGNATURE_CLASS)) return html
  const doc = new DOMParser().parseFromString(html, 'text/html')
  doc.querySelectorAll(`.${SIGNATURE_CLASS}`).forEach(el => el.classList.remove(SIGNATURE_CLASS))
  return doc.body.innerHTML
}

/** Encoded size; the engine refuses larger. Most providers stop between 20 and 35 MB. */
export const MAX_MESSAGE_BYTES = 25 * 1024 * 1024

/** Rough encoded size: base64 with its line breaks. */
export function draftBytes(d: Draft): number {
  const attached = d.attachments.reduce((n, a) => n + a.data.length, 0)
  const forwarded = d.forwarded.reduce((n, f) => n + Math.ceil(f.size / 3) * 4, 0)
  const html = d.html?.length ?? 0
  return Math.round((attached + forwarded + html) * 78 / 76) + (d.html ? Math.round(html / 4) : d.body.length)
}

/** A legacy draft, written before the signature block, loses `legacy` if it contains it verbatim. */
export function withoutSignature(html: string, legacy = ''): string {
  const doc = new DOMParser().parseFromString(html, 'text/html')
  const block = signatureBlock(doc.body)
  if (block) {
    block.remove()
    return doc.body.innerHTML
  }
  return legacy ? html.replace(legacy, '') : html
}

/** Without a signature block, falls back to the previous signature's exact HTML; with no previous
 *  signature, the new one goes above the quoted message, else at the end (never above what was
 *  typed). */
export function swapSignature(html: string, before: string, after: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html')
  const block = signatureBlock(doc.body)
  if (block) {
    if (after) block.outerHTML = after
    else block.remove()
    return doc.body.innerHTML
  }
  if (before) return html.includes(before) ? html.replace(before, after) : html
  if (!after) return html
  const spot = quotedStart(doc.body)
  if (spot) spot.insertAdjacentHTML('beforebegin', after)
  else doc.body.insertAdjacentHTML('beforeend', after)
  return doc.body.innerHTML
}

/** Called before a new draft replaces the open one (the composer saves its own).
 *  Resolves to false when the open draft must stay (its save failed and the user keeps it). */
let beforeReplace: (() => Promise<boolean>) | null = null
let beforeQuit: (() => Promise<boolean>) | null = null
/** Latest replacement asked for: an older one still waiting for the save is dropped. */
let replacing = 0

/** Resolves to false when the save failed and the user keeps the draft: cancel the close. */
export function flushDraftBeforeQuit(): Promise<boolean> {
  return beforeQuit ? beforeQuit() : Promise.resolve(true)
}

export function useComposer() {
  const open = useState('composer-open', () => false)
  const draft = useState<Draft | null>('composer-draft', () => null)
  /** Bumped for every draft shown: the composer resets its per-draft state on each change. */
  const session = useState('composer-session', () => 0)
  /** The draft just shown exists nowhere else (taken back from the outbox): save it at once. */
  const unsaved = useState('composer-unsaved', () => false)

  function onBeforeReplace(fn: () => Promise<boolean>) {
    beforeReplace = fn
  }
  function onBeforeQuit(fn: () => Promise<boolean>) {
    beforeQuit = fn
  }
  async function show(next: Draft, options: { unsaved?: boolean } = {}) {
    const n = ++replacing
    if (open.value && draft.value && beforeReplace && !(await beforeReplace())) return
    if (n !== replacing) return
    unsaved.value = !!options.unsaved
    session.value++
    draft.value = next
    open.value = true
  }
  const { accountId, accountById, isMine } = useMail()
  const showError = useErrorToast()
  const toast = useToast()

  function blank(account = accountId.value!): Draft {
    return {
      account_id: account, to: [], cc: [], bcc: [], subject: '', body: '', html: '', in_reply_to: null,
      attachments: [], forwarded: [], draft_id: null
    }
  }

  const signature = (account: number, from?: string | null) => senderSignature(accountById(account), from)

  /** The alias the message was addressed to, so the reply goes out from it. Our own message: the
   *  alias it was sent from. */
  function aliasFor(m: MessageDetail): string | null {
    const identities = accountById(m.summary.account_id)?.identities ?? []
    const from = m.summary.from?.email.toLowerCase()
    if (from && isMine(from, m.summary.account_id)) {
      return identities.find(i => i.email.toLowerCase() === from)?.email ?? null
    }
    const to = [...m.summary.to, ...m.cc].map(a => a.email.toLowerCase())
    return identities.find(i => to.includes(i.email.toLowerCase()))?.email ?? null
  }

  /** Without sender approval, remote images are removed: the editor would load them and tell the
   *  sender you are replying (tracking pixels). */
  function original(m: MessageDetail): string {
    if (m.is_plain) return textToHtml(m.text ?? '')
    if (m.images_allowed || !m.has_remote_content) return unmarkSignatures(m.html)
    return unmarkSignatures(withoutRemoteImages(m.html))
  }

  function quote(m: MessageDetail): string {
    const who = escapeHtml(addressLabel(m.summary.from))
    return `<p>${escapeHtml(tr('common.compose.wrote', { date: longDate(m.summary.date), who: '\u0000' })).replace('\u0000', who)}</p><blockquote>${original(m)}</blockquote>`
  }

  function prefix(p: string, subject: string) {
    return new RegExp(`^${p}\\s*:`, 'i').test(subject) ? subject : `${p}: ${subject}`
  }

  function compose(to: string[] = []) {
    show({ ...blank(), to, html: `<p></p>${signature(accountId.value!)}` })
  }

  /** `body`: text to start the reply with (a quick reply), above the signature. */
  function reply(m: MessageDetail, all = false, body = '') {
    const account = m.summary.account_id
    const from = aliasFor(m)
    // Each address once across To and Cc; ours (any account, any alias) are left out.
    const seen = new Set<string>()
    const pick = (list: (Address | null)[], dropMine: boolean) => list.filter((a): a is Address => {
      const key = a?.email?.trim().toLowerCase()
      if (!key || seen.has(key) || (dropMine && isMine(key))) return false
      seen.add(key)
      return true
    })
    // Our own message (a reply we sent): the answer goes to the people it was written to, not to us.
    const ownMessage = isMine(m.summary.from?.email, account)
    let to: Address[]
    if (ownMessage) {
      to = pick(m.summary.to, true)
      // Written to ourselves only: back to the same address.
      if (!to.length) to = pick(m.summary.to.length ? m.summary.to : [m.summary.from], false)
    } else {
      to = pick(m.reply_to.length ? m.reply_to : [m.summary.from], false)
    }
    const cc = all ? pick(ownMessage ? m.cc : [...m.summary.to, ...m.cc], true) : []
    show({
      ...blank(account),
      to: to.map(addressFull),
      cc: cc.map(addressFull),
      subject: prefix('Re', m.summary.subject),
      // Signature above the quote, like Spark and Gmail.
      html: `${body || '<p></p>'}${signature(account, from)}${quote(m)}`,
      in_reply_to: m.summary.id,
      from
    })
  }

  function forward(m: MessageDetail) {
    const account = m.summary.account_id
    const header = [
      tr('common.compose.forwarded'),
      tr('common.compose.from', { v: m.summary.from ? addressFull(m.summary.from) : '' }),
      tr('common.compose.date', { v: longDate(m.summary.date) }),
      tr('common.compose.subject', { v: m.summary.subject }),
      tr('common.compose.to', { v: m.summary.to.map(addressFull).join(', ') })
    ].map(escapeHtml).join('<br>')
    const from = aliasFor(m)
    show({
      ...blank(account),
      from,
      subject: prefix('Fwd', m.summary.subject),
      html: `<p></p>${signature(account, from)}<p>${header}</p>${original(m)}`,
      forwarded: m.attachments.map(a => ({
        message_id: m.summary.id, index: a.index, filename: a.filename, mime: a.mime, size: a.size
      }))
    })
  }

  function composeMailto(link: string) {
    const f = parseMailto(link)
    const account = accountId.value!
    show({
      ...blank(account),
      to: f.to,
      cc: f.cc,
      bcc: f.bcc,
      subject: f.subject,
      html: `${textToHtml(f.body) || '<p></p>'}${signature(account)}`
    })
  }

  async function resume(messageId: number) {
    try {
      await reopen(await api.openDraft(messageId))
    } catch (e) {
      showError(e, tr('common.toast.draftUnreadable'))
    }
  }

  /** Deletes a saved draft without opening it; not the one the composer holds, which saves itself
   *  again (it is discarded from there). Resolves to false when nothing was deleted. */
  async function discardSaved(messageId: number): Promise<boolean> {
    try {
      const d = await api.openDraft(messageId)
      if (!d.draft_id) return false
      if (open.value && draft.value?.draft_id === d.draft_id) {
        toast.add({ title: tr('common.toast.draftOpen'), icon: 'i-lucide-file-pen' })
        return false
      }
      await api.discardDraft(d.account_id, d.draft_id)
      return true
    } catch (e) {
      showError(e, tr('common.toast.draftNotDiscarded'))
      return false
    }
  }

  /** `unsaved`: the message exists nowhere else any more, the composer saves it as a draft right
   *  away. */
  function reopen(d: Draft, unsaved = false) {
    return show({ ...d, html: d.html ?? textToHtml(d.body) }, { unsaved })
  }

  async function editOutgoing(id: number) {
    try {
      // Taken out of the outbox: until it is saved again, this window holds the only copy.
      await reopen(await api.cancelSend(id), true)
    } catch (e) {
      showError(e, tr('common.toast.cannotReopen'))
    }
  }

  return { open, draft, session, unsaved, compose, composeMailto, reply, forward, resume, discardSaved, reopen, editOutgoing, onBeforeReplace, onBeforeQuit }
}

/** Strips every remote resource the editor would load (tracking pixels): non-embedded `img`
 *  sources, `srcset`s, `background`s and CSS `url(...)`. */
export function withoutRemoteImages(html: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html')
  const embedded = (src: string | null) => /^(data|blob|cid):/i.test((src ?? '').replace(/[\s\u0000-\u001f]/g, ''))
  doc.querySelectorAll('img').forEach((img) => {
    if (!embedded(img.getAttribute('src'))) img.remove()
    else img.removeAttribute('srcset')
  })
  doc.querySelectorAll('picture source, video, audio, svg image, input[type=image], link').forEach(el => el.remove())
  doc.querySelectorAll('[srcset], [background], [poster]').forEach((el) => {
    el.removeAttribute('srcset')
    el.removeAttribute('background')
    el.removeAttribute('poster')
  })
  // CSS escapes (`u\72l(`) could hide a url( from the test: such styles go as well.
  const remoteCss = /url\s*\(|image-set\s*\(|@import|\\/i
  doc.querySelectorAll('[style]').forEach((el) => {
    if (remoteCss.test(el.getAttribute('style') ?? '')) el.removeAttribute('style')
  })
  doc.querySelectorAll('style').forEach((style) => {
    const css = style.textContent ?? ''
    if (/\\/.test(css)) style.remove()
    else style.textContent = css.replace(/@import[^;]*;?/gi, '').replace(/(url|image-set)\s*\([^)]*\)/gi, 'none')
  })
  return doc.body.innerHTML
}
