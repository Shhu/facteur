import { openUrl } from '@tauri-apps/plugin-opener'
import { confirm } from '~/composables/useConfirm'
import { generatedText, invisibleColor } from '~/utils/linkStyle'
import type { LinkFacts } from '~/utils/types'

/** Below that, text reads as a dot or nothing. */
const UNREADABLE_PX = 6

/** The link's text with what style hides left out, so a lookalike domain cannot be pushed out
 *  of the leading position by words nobody sees. `doubtful`: text the check cannot read (written
 *  by a style), so the link is opened only once asked. */
export function linkText(link: Element): { text: string, doubtful: boolean } {
  const doc = link.ownerDocument
  const view = doc.defaultView
  if (!view) return { text: link.textContent ?? '', doubtful: false }
  const width = doc.documentElement.scrollWidth
  const height = doc.documentElement.scrollHeight
  const hidden = new Map<Element, boolean>()
  const isHidden = (el: Element | null): boolean => {
    if (!el) return false
    const known = hidden.get(el)
    if (known !== undefined) return known
    const s = view.getComputedStyle(el)
    let own = s.display === 'none' || s.visibility === 'hidden' || s.visibility === 'collapse'
      || Number.parseFloat(s.opacity) === 0 || Number.parseFloat(s.fontSize) < UNREADABLE_PX || invisibleColor(s.color)
    if (!own) {
      // Out of the page, or in a box too small to show it.
      const r = el.getBoundingClientRect()
      const left = r.left + view.scrollX
      const top = r.top + view.scrollY
      const laidOut = r.width > 0 || r.height > 0 || el.getClientRects().length > 0
      own = laidOut && (left + r.width <= 0 || top + r.height <= 0 || left >= width || top >= height
        || (s.overflow !== 'visible' && (r.width < 1 || r.height < 1)))
    }
    const result = own || (el !== link && isHidden(el.parentElement))
    hidden.set(el, result)
    return result
  }
  const walker = doc.createTreeWalker(link, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT)
  let text = ''
  let alts = ''
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (n.nodeType === Node.TEXT_NODE) {
      if (!isHidden(n.parentElement)) text += n.nodeValue ?? ''
    } else if ((n as Element).tagName === 'IMG' && !isHidden(n as Element)) {
      // A blocked image shows its alt text in its place; one shown hides it. After the text:
      // never ahead of a domain the user reads.
      const img = n as HTMLImageElement
      if (!img.getAttribute('src') || (img.complete && img.naturalWidth === 0)) alts += ` ${img.getAttribute('alt') ?? ''}`
    }
  }
  text += alts
  // Text a style writes, or shows in another order than written (`<bdo>`, bidi overrides):
  // what is read is not what is seen. A right-to-left mail's direction alone is no reordering.
  const doubtful = [link, ...link.querySelectorAll('*')].some((el) => {
    const s = view.getComputedStyle(el)
    return el.tagName === 'BDO' || /override|plaintext/.test(s.unicodeBidi)
      || generatedText(view.getComputedStyle(el, '::before').content) || generatedText(view.getComputedStyle(el, '::after').content)
  })
  return { text: text.trim(), doubtful }
}

export function visibleLinkText(link: Element): string {
  return linkText(link).text
}

/** A calendar event counts: anyone can put one in your calendar by inviting you. */
export type LinkCaution = 'message' | 'event'

/** Asks first when the link text shows another domain than the real destination (the engine's
 *  `links::analyze`, as in the hover card), or when the source calls for caution. */
export function useOpenLink() {
  const { t } = useI18n()
  const showError = useErrorToast()

  return async function openLink(href: string, options: { text?: string, cautious?: LinkCaution | false } = {}): Promise<boolean> {
    let facts: LinkFacts | null
    try {
      facts = await api.checkLink(href, options.text ?? '')
    } catch (e) {
      showError(e, t('mail.mailFrame.openFailed'))
      return false
    }
    if (!facts) return false
    // Punycode, as the browser resolves it, not as a lookalike might spell it.
    let host: string
    try {
      host = new URL(facts.url).hostname.replace(/^www\./, '')
    } catch {
      return false
    }
    const shown = facts.shown_host
    if (facts.deceptive) {
      const ok = await confirm(t('mail.mailFrame.deceptive', { shown, host }), { title: t('mail.mailFrame.deceptiveTitle'), kind: 'warning' })
      if (!ok) return false
    } else if (options.cautious === 'message') {
      const ok = await confirm(t('mail.mailFrame.cautious', { host }), { title: t('mail.mailFrame.cautiousTitle'), kind: 'warning' })
      if (!ok) return false
    } else if (options.cautious === 'event') {
      const ok = await confirm(t('tools.calendar.linkConfirm', { host }), { title: t('tools.calendar.linkConfirmTitle'), kind: 'warning' })
      if (!ok) return false
    }
    openUrl(facts.url).catch(e => showError(e, t('mail.mailFrame.openFailed')))
    return true
  }
}
