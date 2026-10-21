import { openUrl } from '@tauri-apps/plugin-opener'
import { confirm } from '~/composables/useConfirm'
import type { LinkFacts } from '~/utils/types'

/** Leaves out text hidden by style, so a lookalike domain cannot be pushed out of the leading
 *  position by words nobody sees. */
export function visibleLinkText(link: Element): string {
  const doc = link.ownerDocument
  const view = doc.defaultView
  if (!view) return link.textContent ?? ''
  const hidden = new Map<Element, boolean>()
  const isHidden = (el: Element | null): boolean => {
    if (!el) return false
    const known = hidden.get(el)
    if (known !== undefined) return known
    const s = view.getComputedStyle(el)
    const own = s.display === 'none' || s.visibility === 'hidden' || s.visibility === 'collapse'
      || Number.parseFloat(s.opacity) === 0 || Number.parseFloat(s.fontSize) < 1
    const result = own || (el !== link && isHidden(el.parentElement))
    hidden.set(el, result)
    return result
  }
  const walker = doc.createTreeWalker(link, NodeFilter.SHOW_TEXT)
  let text = ''
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (!isHidden(n.parentElement)) text += n.nodeValue ?? ''
  }
  return text.trim()
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
