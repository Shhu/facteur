<script setup lang="ts">
import type { LinkReport } from '~/utils/types'

/** `cautious`: untrustworthy message, every link asks first. `messageId`: a cached message,
 *  whose links show their card on hover (not those of a backup). */
// Two roots (link preview, frame): attributes such as `class` go to the frame's box.
defineOptions({ inheritAttrs: false })
/** `highlight`: words to mark in the text (an appointment the engine found there). */
/** `hosts`: approved domains whose remote content loads even while `allowRemote` is off. */
const props = defineProps<{ messageId?: number, html: string, plain: boolean, allowRemote: boolean, hosts?: string[], cautious?: boolean, darken?: boolean, highlight?: string | null }>()
const emit = defineEmits<{ mailto: [address: string] }>()

const frame = ref<HTMLIFrameElement>()
const height = ref(80)
/** Hidden until loaded, sized and darkened: before that it showed a short white box with a
 *  scrollbar, the light mail undarkened, for an instant. */
const ready = ref(false)
const slow = ref(false)
let slowTimer: ReturnType<typeof setTimeout> | undefined
/** Shown as soon as the message's own document is parsed: `load` also waits for every remote
 *  image, and one that never answers would keep the spinner up for good. */
let parsedPoll: ReturnType<typeof setInterval> | undefined
function waitForLoad() {
  ready.value = false
  slow.value = false
  clearTimeout(slowTimer)
  slowTimer = setTimeout(() => {
    slow.value = true
  }, 150)
  clearInterval(parsedPoll)
  parsedPoll = setInterval(() => {
    const doc = frame.value?.contentDocument
    if (doc && doc.location.href === 'about:srcdoc' && doc.readyState !== 'loading') prepare(doc)
  }, 100)
}
waitForLoad()
onBeforeUnmount(() => {
  clearTimeout(slowTimer)
  clearInterval(parsedPoll)
})
const colorMode = useColorMode()
const { t } = useI18n()

// The HTML is already sanitised in Rust. The iframe has no `allow-scripts`: no script can run.
// `allow-same-origin` only serves to measure the height and catch clicks from the parent page.
const srcdoc = computed(() => {
  const remote = remoteSources(props.allowRemote, props.hosts ?? [])
  const csp = `default-src 'none'; img-src data:${remote}; style-src 'unsafe-inline'${remote}; font-src data:${remote}`
  // Plain-text mails follow the theme; HTML mails keep the white background their author intended.
  const dark = props.plain && colorMode.value === 'dark'
  // A light HTML mail shown as is in dark mode sits in a white box: keep its content off the edges.
  const boxed = !props.plain && colorMode.value === 'dark'
  const body = props.plain ? markPlainSignature(collapsePlainQuotes(props.html)) : props.html
  // Quote toggle colours, written for a light page: the dark mode of HTML mails inverts them.
  const toggle = dark
    ? { border: '#27272a', bg: '#232327', hover: '#2c2c31', fg: '#a1a1aa' }
    : { border: '#e4e4e7', bg: '#f4f4f5', hover: '#e9e9ec', fg: '#71717a' }
  // The CSP does not cover DNS: on an http origin (Windows, Android), Chromium would resolve the
  // host of every link as it is parsed, telling a tracker the mail was opened. Off for good here,
  // before any link; the sanitiser strips the mail's own <meta>, so it cannot turn it back on.
  return `<!doctype html><html><head><meta http-equiv="x-dns-prefetch-control" content="off"><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<style>
  html { color-scheme: ${dark ? 'dark' : 'light'}; }
  body { margin: 0; padding: 0; font: 15px/1.6 system-ui, -apple-system, "Segoe UI", sans-serif;
         color: ${dark ? '#d4d4d8' : props.plain ? '#3f3f46' : '#18181b'}; background: ${dark ? 'transparent' : '#fff'};
         overflow-wrap: anywhere; }
  ${boxed ? 'html:not(.facteur-dark) body { padding: 12px; }' : ''}
  ${props.plain ? 'body { max-width: 620px; }' : ''}
  @media (pointer: coarse) { body { font-size: 16px; } }
  /* The signature, set apart and quieter: after "-- " in plain text, in the signature block mail
     clients mark in HTML. Opacity rather than a colour: an HTML mail keeps its own. */
  .facteur-signature, .gmail_signature, .moz-signature, div[data-smartmail="gmail_signature"] {
         margin-top: 12px; padding-top: 12px; border-top: 1px dashed ${dark ? '#3f3f46' : '#d4d4d8'}; font-size: 13.5px; opacity: .8; }
  mark.facteur-date { background: ${dark ? 'rgba(14, 165, 233, .16)' : '#e0f2fe'}; color: inherit; font-weight: 600; border-radius: 4px; padding: 0 3px; }
  /* Height set inline by the message (Outlook does) outweighs auto: shrunk to the width, the
     picture keeps its proportions inside the box rather than being squashed. */
  img { max-width: 100%; height: auto; object-fit: contain; }
  /* A link or image dragged out of the mail would be dropped on the app window and navigate it. */
  a, img { -webkit-user-drag: none; }
  a { color: ${dark ? '#5eead4' : '#0f766e'}; }
  blockquote { margin: 0 0 0 .5em; padding-left: .75em; border-left: 3px solid #d4d4d8; color: #71717a; }
  pre { white-space: pre-wrap; }
  /* Dark mode for HTML mails: colours inverted (hues kept), images and videos inverted back.
     A partial inversion turns white into the card's zinc-900 instead of pure black;
     contrast(1.232) undoes it on media (1 / (2 × 0.906 − 1)). */
  html.facteur-dark { filter: invert(0.906) hue-rotate(180deg); }
  html.facteur-dark img, html.facteur-dark video, html.facteur-dark picture, html.facteur-dark svg,
  html.facteur-dark [style*="background-image"], html.facteur-dark [background] { filter: invert(1) contrast(1.232) hue-rotate(180deg); }
  .facteur-quote { display: none; }
  html.facteur-quotes-open .facteur-quote { display: block; }
  .facteur-quote-toggle { all: unset; box-sizing: border-box; display: flex; width: max-content; max-width: 100%;
         align-items: center; gap: 8px; height: 30px; margin: 14px 0 0; padding: 0 12px; border-radius: 999px;
         border: 1px solid ${toggle.border}; background: ${toggle.bg}; color: ${toggle.fg};
         font: 550 12.5px/1 system-ui, -apple-system, "Segoe UI", sans-serif; white-space: nowrap; cursor: pointer; }
  .facteur-quote-toggle:hover { background: ${toggle.hover}; }
  .facteur-quote-toggle:focus-visible { outline: 2px solid ${dark ? '#5eead4' : '#0f766e'}; outline-offset: 2px; }
  .facteur-quote-toggle svg { flex: none; }
  html.facteur-dark .facteur-quote-toggle svg { filter: none; }
  html.facteur-quotes-open .facteur-quote-toggle { margin-bottom: 10px; }
  .facteur-quote-hide, html.facteur-quotes-open .facteur-quote-show { display: none; }
  html.facteur-quotes-open .facteur-quote-hide { display: inline; }
  @media (pointer: coarse) { .facteur-quote-toggle { height: 44px; padding: 0 16px; font-size: 13px; } }
</style></head><body>${body}</body></html>`
})

/** Quoted text is shown: kept across reloads of the frame (images allowed, theme change). */
const quotesOpen = ref(false)

const ELLIPSIS = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/></svg>'

function toggleMarkup(count: number) {
  return `<button type="button" class="facteur-quote-toggle">${ELLIPSIS}<span class="facteur-quote-show">${escapeHtml(t('mail.reader.showQuoted', count))}</span><span class="facteur-quote-hide">${escapeHtml(t('mail.reader.hideQuoted'))}</span></button>`
}

/** Line introducing a quote: "On …, Claire wrote:" and its translations. */
const ATTRIBUTION = /(?:a écrit|écrit|wrote|schrieb|escribió|ha scritto)\s*(?::\s*)?$/i
/** A forwarded message stays visible: it is the content, not a reminder of the thread. */
const FORWARDED = /^\s*-{3,}\s*(?:forwarded message|message transféré|weitergeleitete nachricht|mensaje reenviado|messaggio inoltrato)/i
/** Separators of earlier messages written as text (Outlook in plain text, some webmails). Two
 *  dashes each side are enough to match: `-{2,}` there backtracks quadratically on long runs. */
const TEXT_HEADERS = /-{2}\s*(?:original message|message d'origine|ursprüngliche nachricht|mensaje original)\s*-{2}/gi

function decodeText(html: string) {
  return html.replace(/<[^>]*>/g, '').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&#x?0*27;/g, "'").replace(/&amp;/g, '&')
}

/**
 * Plain-text mail (rendered by the engine as one pre-wrap div, one line per `\n`, quoted lines
 * in a span): the trailing block of `>` lines and its attribution line are folded.
 */
function collapsePlainQuotes(html: string): string {
  const open = html.indexOf('>') + 1
  const close = html.lastIndexOf('</div>')
  if (!html.startsWith('<div') || close < open) return html
  const lines = html.slice(open, close).split('\n')
  const text = lines.map(decodeText)
  const blank = (i: number) => !text[i]!.trim()
  const quoted = (i: number) => text[i]!.startsWith('>')

  // Trailing run of quoted or blank lines, starting at its first quoted line.
  let start = lines.length
  while (start > 0 && (blank(start - 1) || quoted(start - 1))) start--
  while (start < lines.length && !quoted(start)) start++
  if (start >= lines.length) return html

  // Attribution just above, possibly wrapped over two lines by the sender's client.
  let first = start
  let k = start - 1
  while (k >= 0 && blank(k)) k--
  if (k >= 0 && text[k]!.length < 300 && ATTRIBUTION.test(text[k]!.trim())) {
    first = k
    if (k > 0 && !blank(k - 1) && !/^(?:le|on|am|el|il)\s/i.test(text[k]!.trim()) && /^(?:le|on|am|el|il)\s/i.test(text[k - 1]!.trim())) first = k - 1
  }
  let last = first
  while (last > 0 && blank(last - 1)) last--
  // Nothing written above the quote: folding it would leave an empty message.
  if (last === 0) return html

  let depth = 1
  for (let i = start; i < lines.length; i++) {
    const prefix = text[i]!.match(/^(?:>\s?)+/)?.[0]
    if (prefix) depth = Math.max(depth, prefix.split('>').length - 1)
  }
  return `${html.slice(0, open)}${lines.slice(0, last).join('\n')}${toggleMarkup(depth)}<div class="facteur-quote">${lines.slice(first).join('\n')}</div></div>`
}

/** Plain text: from the "-- " line (the signature delimiter) to the end of what was written, the
 *  folded quote left where it is. The delimiter line itself is dropped, the border stands for it. */
function markPlainSignature(html: string): string {
  const open = html.indexOf('>') + 1
  const toggle = html.indexOf('<button type="button" class="facteur-quote-toggle">')
  const end = toggle >= 0 ? toggle : html.lastIndexOf('</div>')
  if (!html.startsWith('<div') || end < open) return html
  const lines = html.slice(open, end).split('\n')
  let at = -1
  for (let i = lines.length - 1; i > 0; i--) {
    if (/^--\s?$/.test(decodeText(lines[i]!))) {
      at = i
      break
    }
  }
  // Too long for a signature: likely a separator inside the text.
  if (at < 0 || lines.length - at > 15) return html
  const kept = lines.slice(0, at).join('\n').replace(/\n+$/, '')
  const signature = lines.slice(at + 1).join('\n').replace(/^\n+/, '').replace(/\n+$/, '')
  if (!signature.trim()) return html
  return `${html.slice(0, open)}${kept}<div class="facteur-signature">${signature}</div>${html.slice(end)}`
}

/** Explicit reply markers of mail clients: what follows them in their parent is the earlier thread. */
const QUOTE_MARKERS = [
  '.gmail_quote_container', '.gmail_quote', '.yahoo_quoted', '.protonmail_quote', '#appendonsend', '#divRplyFwdMsg',
  '.moz-cite-prefix', 'hr#zwchr', 'blockquote[type="cite"]',
  // Outlook desktop: the "From: … Sent: …" header box.
  'div[style*="border-top:solid #E1E1E1" i]', 'div[style*="border-top: solid #E1E1E1" i]',
  'div[style*="border-top:solid #B5C4DF" i]', 'div[style*="border-top: solid #B5C4DF" i]'
].join(', ')
/** Headers of earlier messages inside a quote, to count them. */
const QUOTE_HEADERS = '.gmail_attr, #divRplyFwdMsg, .moz-cite-prefix, div[style*="border-top:solid #E1E1E1" i], div[style*="border-top: solid #E1E1E1" i], div[style*="border-top:solid #B5C4DF" i], div[style*="border-top: solid #B5C4DF" i]'
/** Parents where a wrapping div would break the markup. */
const NO_WRAP = new Set(['TABLE', 'TBODY', 'THEAD', 'TFOOT', 'TR', 'UL', 'OL', 'DL', 'SELECT'])

/** Text and images between two points of the document (empty: nothing of value there). */
function hasContent(doc: Document, setup: (r: Range) => void) {
  const range = doc.createRange()
  setup(range)
  return !!range.toString().trim() || !!range.cloneContents().querySelector('img')
}

/**
 * HTML mail: folds the quoted earlier messages (Gmail, Outlook, Apple Mail, Thunderbird,
 * or a trailing blockquote with its "… wrote:" line) behind a toggle.
 */
function collapseHtmlQuotes(doc: Document) {
  const body = doc.body
  if (!body || body.querySelector('.facteur-quote')) return
  let start = body.querySelector(QUOTE_MARKERS)
  let explicit = !!start && !start.matches('blockquote')
  if (!start) {
    // Last top-level blockquote, only when nothing follows it (no inline answers below).
    start = [...body.querySelectorAll('blockquote')].filter(b => !b.parentElement?.closest('blockquote')).at(-1) ?? null
    explicit = false
  }
  // A marker alone in its wrapper (Outlook boxes its header in a bare div): the earlier
  // messages follow the wrapper, so the wrapper is what starts the quote.
  const blankNode = (n: Node) => (n.nodeType === Node.TEXT_NODE && !n.textContent?.trim()) || n.nodeName === 'BR'
  if (explicit) {
    while (start && start.parentElement && start.parentElement !== body && [...start.parentElement.childNodes].every(n => n === start || blankNode(n))) {
      start = start.parentElement
    }
  }
  const parent = start?.parentNode
  if (!start || !parent || NO_WRAP.has((parent as Element).tagName)) return
  if (!explicit && hasContent(doc, r => {
    r.setStartAfter(start!)
    r.setEnd(body, body.childNodes.length)
  })) return

  // Attribution line or Outlook rule just before the marker belongs to the quote.
  let first: Node = start
  let prev = start.previousSibling
  while (prev && blankNode(prev)) prev = prev.previousSibling
  const prevText = prev?.textContent?.trim() ?? ''
  if (prev && (prev.nodeName === 'HR' || (prevText.length < 300 && ATTRIBUTION.test(prevText)))) first = prev
  if (!hasContent(doc, r => {
    r.setStart(body, 0)
    r.setEndBefore(first)
  })) return

  const wrapper = doc.createElement('div')
  wrapper.className = 'facteur-quote'
  parent.insertBefore(wrapper, first)
  // Explicit marker: everything after it in its parent; a blockquote: itself and its attribution.
  const stop = explicit ? null : start.nextSibling
  for (let n = wrapper.nextSibling; n && n !== stop;) {
    const next = n.nextSibling
    wrapper.appendChild(n)
    n = next
  }
  const content = wrapper.textContent ?? ''
  if (FORWARDED.test(content)) {
    while (wrapper.firstChild) parent.insertBefore(wrapper.firstChild, wrapper)
    wrapper.remove()
    return
  }

  // Earlier messages: nesting depth of quotes, or headers found one after another.
  let depth = 0
  for (const b of wrapper.querySelectorAll('blockquote')) {
    let d = 0
    for (let e: Element | null = b; e && e !== wrapper; e = e.parentElement) if (e.tagName === 'BLOCKQUOTE') d++
    depth = Math.max(depth, d)
  }
  const headers = wrapper.querySelectorAll(QUOTE_HEADERS).length
  // A bounded sample: counting, not reading the whole quote.
  const sample = content.slice(0, 200_000)
  const written = (sample.match(/(?:a écrit|wrote|schrieb|escribió|ha scritto)\s*:/gi)?.length ?? 0) + (sample.match(TEXT_HEADERS)?.length ?? 0)
  const count = Math.max(1, depth, headers, written)
  wrapper.insertAdjacentHTML('beforebegin', toggleMarkup(count))
}

function setQuotes(doc: Document, open: boolean) {
  quotesOpen.value = open
  doc.documentElement.classList.toggle('facteur-quotes-open', open)
  fitHeight(doc)
}

/** Backstop against a hostile layout: no mail is this tall. */
const MAX_HEIGHT = 500_000

/**
 * Fits the frame to its content. The page never measures smaller than its frame, and content
 * sized from it (`height: 100%`, `100vh`) plus a padding would measure taller at each pass, the
 * frame growing forever. So the frame's viewport is emptied while measuring: a bottom padding as
 * tall as the frame keeps its outer size, and the page around it (and its scroll) does not move.
 */
function fitHeight(doc: Document) {
  const el = frame.value
  if (!el) return
  const root = doc.documentElement
  // A scrollbar shown in the empty viewport would narrow the text, and measure it taller.
  const scrollbar = root.style.getPropertyValue('scrollbar-width')
  root.style.setProperty('scrollbar-width', 'none')
  el.style.boxSizing = 'border-box'
  el.style.paddingBottom = `${height.value}px`
  el.style.height = `${height.value}px`
  const h = Math.min(root.scrollHeight, MAX_HEIGHT)
  el.style.paddingBottom = ''
  el.style.boxSizing = ''
  if (scrollbar) root.style.setProperty('scrollbar-width', scrollbar)
  else root.style.removeProperty('scrollbar-width')
  height.value = h
  el.style.height = `${h}px`
}

let observer: ResizeObserver | undefined

/** Lightness (0 to 1) of a computed CSS colour; `null` if transparent. */
function lightness(color: string): number | null {
  const m = color.match(/rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?/)
  if (!m || (m[4] !== undefined && Number(m[4]) < 0.5)) return null
  return (0.2126 * Number(m[1]) + 0.7152 * Number(m[2]) + 0.0722 * Number(m[3])) / 255
}

/** Marks the first place the words appear within one piece of text (spaces and line breaks as
 *  they come); across formatting (a part in bold), they are left unmarked. */
function markWords(doc: Document, words: string) {
  const pattern = new RegExp(words.trim().split(/\s+/).map(w => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('\\s+'))
  const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_TEXT)
  for (let node = walker.nextNode() as Text | null; node; node = walker.nextNode() as Text | null) {
    const found = pattern.exec(node.data)
    if (!found) continue
    const range = doc.createRange()
    range.setStart(node, found.index)
    range.setEnd(node, found.index + found[0].length)
    const mark = doc.createElement('mark')
    mark.className = 'facteur-date'
    range.surroundContents(mark)
    return
  }
}

/** Darkens the mail unless already dark (background at the top of the page, read where it shows). */
function applyDark() {
  const doc = frame.value?.contentDocument
  if (!doc || props.plain) return
  doc.documentElement.classList.remove('facteur-dark')
  if (!props.darken) return
  const win = doc.defaultView
  let el = doc.elementFromPoint((win?.innerWidth ?? 600) / 2, 12) as Element | null
  let light = 1
  while (el) {
    const l = lightness(win?.getComputedStyle(el).backgroundColor ?? '')
    if (l !== null) {
      light = l
      break
    }
    el = el.parentElement
  }
  if (light > 0.4) doc.documentElement.classList.add('facteur-dark')
}
watch(() => props.darken, applyDark)
// A new page loads in the frame: hidden again until `onLoad` has fitted it.
watch(srcdoc, waitForLoad)

/** The document set up once, whether its parsing or its full load comes first. */
let prepared: Document | null = null
function onLoad() {
  const doc = frame.value?.contentDocument
  if (!doc) return
  prepare(doc)
  // Images now in: the size and the background seen for the dark mode may have changed.
  fitHeight(doc)
  applyDark()
}

function prepare(doc: Document) {
  if (doc === prepared || !doc.body) return
  prepared = doc
  clearInterval(parsedPoll)
  observer?.disconnect()
  if (!props.plain) {
    collapseHtmlQuotes(doc)
    // The frame is as tall as its content: a page filling it (`html, body { height: 100% }`) would
    // only leave its body padding overflowing. Last in the body, so it wins over the mail's rules.
    doc.body?.insertAdjacentHTML('beforeend', '<style>html, body { height: auto !important; min-height: 0 !important; }</style>')
  }
  doc.documentElement.classList.toggle('facteur-quotes-open', quotesOpen.value)
  if (props.highlight) {
    try {
      markWords(doc, props.highlight)
    } catch {
      // A mark that cannot be placed never keeps the message from showing.
    }
  }
  observer = new ResizeObserver(() => fitHeight(doc))
  observer.observe(doc.documentElement)
  fitHeight(doc)
  blockBrowserContextMenu(doc)
  applyDark()
  ready.value = true
  clearTimeout(slowTimer)

  doc.addEventListener('click', (e) => {
    if ((e.target as Element | null)?.closest('.facteur-quote-toggle')) {
      e.preventDefault()
      setQuotes(doc, !quotesOpen.value)
      return
    }
    const link = (e.target as Element | null)?.closest('a')
    const href = link?.getAttribute('href')
    if (!href) return
    e.preventDefault()
    if (/^mailto:/i.test(href)) emit('mailto', href)
    else if (/^https?:/i.test(href) && link) {
      const { text, doubtful } = linkText(link)
      openLink(href, { text, cautious: (props.cautious || doubtful) && 'message' })
    }
  })

  // Once clicked into, the frame has the focus and its keys never reach the app's window: they are
  // forwarded there so the shortcuts keep working. Copy and select all stay with the frame.
  doc.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && /^[ac]$/i.test(e.key)) return
    // A field in the message (a form) keeps its keys. `instanceof` fails across documents.
    const target = e.target as Element | null
    if (target && (/^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName) || (target as HTMLElement).isContentEditable)) return
    const forwarded = new KeyboardEvent('keydown', {
      key: e.key,
      code: e.code,
      location: e.location,
      repeat: e.repeat,
      ctrlKey: e.ctrlKey,
      altKey: e.altKey,
      shiftKey: e.shiftKey,
      metaKey: e.metaKey,
      bubbles: true,
      cancelable: true
    })
    if (!window.dispatchEvent(forwarded)) e.preventDefault()
  })

  // Hovering a link: its card appears after a short delay.
  doc.addEventListener('mouseover', (e) => {
    const link = (e.target as Element | null)?.closest('a')
    const href = link?.getAttribute('href')
    if (props.messageId === undefined || !link || !href || !/^https?:/i.test(href)) return
    if (preview.value?.href === href && preview.value.anchor === link) {
      clearTimeout(hideTimer)
      return
    }
    clearTimeout(showTimer)
    showTimer = setTimeout(() => show(link, href), 350)
  })
  doc.addEventListener('mouseout', (e) => {
    const link = (e.target as Element | null)?.closest('a')
    if (!link) return
    clearTimeout(showTimer)
    scheduleHide()
  })
}

interface Preview {
  href: string
  anchor: Element
  x: number
  y: number
  above: boolean
  report: LinkReport | null
  error: string | null
}
const preview = ref<Preview | null>(null)
/** Analyses already made for this message, by link and displayed text. */
const reports = new Map<string, Promise<LinkReport>>()
let showTimer: ReturnType<typeof setTimeout> | undefined
let hideTimer: ReturnType<typeof setTimeout> | undefined

/** Expected card height: past the window bottom, it goes above the link. */
const PREVIEW_HEIGHT = 300
const PREVIEW_WIDTH = 352

function show(link: Element, href: string) {
  const frameRect = frame.value?.getBoundingClientRect()
  if (!frameRect) return
  const r = link.getBoundingClientRect()
  const above = frameRect.top + r.bottom + PREVIEW_HEIGHT > window.innerHeight
  const x = Math.max(8, Math.min(frameRect.left + r.left, window.innerWidth - PREVIEW_WIDTH - 8))
  const y = above ? frameRect.top + r.top - 6 : frameRect.top + r.bottom + 6
  const text = visibleLinkText(link)
  const current: Preview = { href, anchor: link, x, y, above, report: null, error: null }
  preview.value = current

  const key = `${href}\n${text}`
  if (!reports.has(key)) reports.set(key, api.inspectLink(props.messageId!, href, text))
  reports.get(key)!.then(
    (report) => {
      if (preview.value?.anchor === link) preview.value = { ...preview.value, report }
    },
    (e) => {
      reports.delete(key)
      if (preview.value?.anchor === link) preview.value = { ...preview.value, error: String(e) }
    }
  )
}

function keep() {
  clearTimeout(hideTimer)
}

function scheduleHide() {
  clearTimeout(hideTimer)
  hideTimer = setTimeout(() => {
    preview.value = null
  }, 300)
}

function hide() {
  clearTimeout(showTimer)
  clearTimeout(hideTimer)
  preview.value = null
}

// The card is placed in window coordinates: scrolling would offset it.
onMounted(() => window.addEventListener('scroll', hide, true))
onBeforeUnmount(() => {
  window.removeEventListener('scroll', hide, true)
  hide()
})

const openLink = useOpenLink()

onBeforeUnmount(() => observer?.disconnect())
</script>

<template>
  <LinkPreview
    v-if="preview"
    :href="preview.href"
    :report="preview.report"
    :error="preview.error"
    :x="preview.x"
    :y="preview.y"
    :above="preview.above"
    @enter="keep"
    @leave="scheduleHide"
    @close="hide"
  />
  <div v-bind="$attrs" class="relative">
    <!-- An HTML mail sees a light page: its own dark-mode rules (Google Calendar has some) would
         otherwise apply in a dark app, and facteur's inversion then turn their light text dark. -->
    <iframe
      ref="frame"
      sandbox="allow-same-origin"
      :srcdoc="srcdoc"
      :style="{ height: `${height}px`, colorScheme: plain ? undefined : 'light' }"
      class="block w-full rounded-md border-0"
      :class="[plain || darken ? '' : 'bg-white', ready ? '' : 'opacity-0']"
      :title="$t('mail.mailFrame.frameTitle')"
      @load="onLoad"
    />
    <div v-if="!ready && slow" class="pointer-events-none absolute inset-0 flex items-center justify-center" role="status">
      <UIcon name="i-lucide-loader-circle" class="size-5 animate-spin text-muted" />
      <span class="sr-only">{{ $t('mail.mailFrame.loading') }}</span>
    </div>
  </div>
</template>
