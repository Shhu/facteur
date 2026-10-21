// Shared by every page: the language switch, the download links filled from the latest release,
// the release notes, and the feature tabs. download/*.json are written by the website workflow.

const body = document.body

// Links to the sections of the former one-page site lead to their pages now.
if (body.dataset.page === 'home') {
  const moved = { '#fonctions': 'features.html', '#nouveautes': 'news.html', '#installer': 'download.html', '#telecharger': 'download.html', '#linux': 'download.html#linux', '#android': 'download.html' }[location.hash]
  if (moved) location.replace(moved)
}

// ── Language ──────────────────────────────────────────────────────────────

const langButton = document.getElementById('lang')
function setLang(lang) {
  body.dataset.lang = lang
  document.documentElement.lang = lang
  langButton.querySelector('.long').textContent = lang === 'fr' ? 'English' : 'Français'
  langButton.querySelector('.short').textContent = lang === 'fr' ? 'EN' : 'FR'
  langButton.setAttribute('aria-label', lang === 'fr' ? 'Switch to English' : 'Passer en français')
  for (const img of document.querySelectorAll('img[data-alt-fr]')) img.alt = img.dataset[lang === 'fr' ? 'altFr' : 'altEn']
  const title = document.querySelector('meta[name="title-' + lang + '"]')
  if (title) document.title = title.content
  try { localStorage.setItem('facteur-lang', lang) } catch {}
}
let savedLang = null
try { savedLang = localStorage.getItem('facteur-lang') } catch {}
setLang(savedLang || ((navigator.language || 'fr').toLowerCase().startsWith('fr') ? 'fr' : 'en'))
langButton.addEventListener('click', () => setLang(body.dataset.lang === 'fr' ? 'en' : 'fr'))

// ── Latest release: version, sizes, download links ────────────────────────

const bilingual = (fr, en) => `<span lang="fr">${fr}</span><span lang="en">${en}</span>`
const mb = (n) => {
  const x = n / 1048576
  return { fr: `${x.toLocaleString('fr', { maximumFractionDigits: 1 })} Mo`, en: `${x.toFixed(1)} MB` }
}
const longDate = (iso, lang) => new Date(iso).toLocaleDateString(lang, { dateStyle: 'long' })

fetch('download/release.json', { cache: 'no-store' })
  .then(r => (r.ok ? r.json() : Promise.reject()))
  .then((release) => {
    const version = release.tag.replace(/^v/, '')
    for (const el of document.querySelectorAll('.version')) {
      el.innerHTML = bilingual(`Version ${version} · ${longDate(release.published, 'fr')}`, `Version ${version} · ${longDate(release.published, 'en')}`)
    }
    // facteur.exe is the download alias; facteur-X.Y.Z.exe, the same file, is for the updater.
    const exe = release.assets.find(a => a.name === 'facteur.exe') ?? release.assets.find(a => /\.exe$/.test(a.name))
    const flatpak = release.assets.find(a => a.name === 'facteur.flatpak') ?? release.assets.find(a => /\.flatpak$/.test(a.name))
    const apk = release.assets.find(a => /\.apk$/.test(a.name))
    if (exe) {
      const size = mb(exe.size)
      for (const link of document.querySelectorAll('a.win')) {
        link.href = `download/${exe.name}`
        link.removeAttribute('aria-disabled')
      }
      for (const meta of document.querySelectorAll('.win-meta')) meta.innerHTML = bilingual(`Windows 10 et 11 · ${size.fr}`, `Windows 10 and 11 · ${size.en}`)
    } else {
      for (const note of document.querySelectorAll('.none')) note.hidden = false
    }
    if (flatpak) {
      const size = mb(flatpak.size)
      for (const link of document.querySelectorAll('a.linux')) {
        link.href = `download/${flatpak.name}`
        link.removeAttribute('aria-disabled')
        link.hidden = false
      }
      for (const meta of document.querySelectorAll('.linux-meta')) meta.innerHTML = bilingual(`Flatpak · ${size.fr}`, `Flatpak · ${size.en}`)
    }
    if (apk) {
      const size = mb(apk.size)
      for (const link of document.querySelectorAll('a.apk')) {
        link.href = `download/${apk.name}`
        link.removeAttribute('aria-disabled')
        link.hidden = false
      }
      for (const meta of document.querySelectorAll('.apk-meta')) meta.innerHTML = bilingual(`Aperçu · ${size.fr}`, `Preview · ${size.en}`)
    }
  })
  .catch(() => { for (const note of document.querySelectorAll('.none')) note.hidden = false })

// ── Release notes ─────────────────────────────────────────────────────────

// Into every [data-releases] list: the notes of the latest releases, newest first, as many as its
// data-limit says. Shown as text only (never as HTML); a line starting with "- " is a list item.
const releaseLists = document.querySelectorAll('[data-releases]')
if (releaseLists.length) {
  fetch('download/notes.json', { cache: 'no-store' })
    .then(r => (r.ok ? r.json() : Promise.reject()))
    .then((releases) => {
      const el = (tag, text) => {
        const e = document.createElement(tag)
        if (text !== undefined) e.textContent = text
        return e
      }
      const notes = releases.filter(r => (r.body || '').trim())
      for (const list of releaseLists) {
        const limit = Number(list.dataset.limit) || notes.length
        notes.slice(0, limit).forEach((release, i) => {
          const card = el('article')
          card.className = list.dataset.plain === undefined ? 'card release' : 'release'
          const title = el('h3', `Version ${release.tag.replace(/^v/, '')}`)
          if (i === 0) {
            const tag = el('span')
            tag.className = 'tag'
            tag.innerHTML = bilingual('Dernière', 'Latest')
            title.append(tag)
          }
          const when = el('small')
          when.innerHTML = '<span lang="fr"></span><span lang="en"></span>'
          when.children[0].textContent = longDate(release.published, 'fr')
          when.children[1].textContent = longDate(release.published, 'en')
          title.append(when)
          card.append(title)
          let ul = null
          for (const raw of release.body.split(/\r?\n/)) {
            const line = raw.trim()
            if (!line) continue
            const item = line.match(/^[-*]\s+(.*)$/)
            if (item) {
              if (!ul) card.append(ul = el('ul'))
              ul.append(el('li', item[1]))
            } else {
              ul = null
              card.append(el('p', line))
            }
          }
          list.append(card)
        })
        if (list.children.length) {
          for (const hidden of document.querySelectorAll(`[data-when-releases="${list.id}"]`)) hidden.hidden = false
          for (const empty of document.querySelectorAll(`[data-no-releases="${list.id}"]`)) empty.hidden = true
        }
      }
    })
    .catch(() => {})
}

// ── Feature tabs ──────────────────────────────────────────────────────────

const tablist = document.querySelector('.tour [role="tablist"]')
if (tablist) {
  const tabs = [...tablist.querySelectorAll('[role="tab"]')]
  const select = (tab, focus) => {
    for (const t of tabs) {
      const on = t === tab
      t.setAttribute('aria-selected', String(on))
      t.tabIndex = on ? 0 : -1
      document.getElementById(t.getAttribute('aria-controls')).hidden = !on
    }
    if (focus) tab.focus()
    // On a phone the tab row scrolls: keep the chosen one in view.
    tab.scrollIntoView({ block: 'nearest', inline: 'nearest' })
    history.replaceState(null, '', '#' + tab.dataset.slug)
  }
  for (const tab of tabs) tab.addEventListener('click', () => select(tab))
  tablist.addEventListener('keydown', (e) => {
    const i = tabs.indexOf(document.activeElement)
    if (i < 0) return
    const step = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[e.key]
    if (step) {
      e.preventDefault()
      select(tabs[(i + step + tabs.length) % tabs.length], true)
    } else if (e.key === 'Home' || e.key === 'End') {
      e.preventDefault()
      select(tabs[e.key === 'Home' ? 0 : tabs.length - 1], true)
    }
  })
  // A link to features.html#agenda opens that tab.
  const fromHash = tabs.find(t => t.dataset.slug === location.hash.slice(1))
  if (fromHash) select(fromHash)
}
