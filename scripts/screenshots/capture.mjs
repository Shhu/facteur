#!/usr/bin/env node
// Screenshots of the UI filled with demo data, for the website (site/screens/*.webp).
//
//   node scripts/screenshots/capture.mjs [baseUrl] [name…]
//
// baseUrl: a running UI dev server (`npx -y pnpm@11.1.1 --dir ui dev --port 3017`), by default
// http://127.0.0.1:3017. Names: only these shots (see SHOTS below), by default all of them.
// The engine is replaced by mock.js, fed by data.js; the page clock starts at the demo's "now".
// Playwright: the one resolvable from here, or playwright-core installed once into
// ~/.cache/facteur-screenshots, at the version matching the browsers in ~/.cache/ms-playwright
// (PLAYWRIGHT_VERSION overrides it). Images are converted with ffmpeg (libwebp), or ImageMagick.

import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, statSync } from 'node:fs'
import { createRequire } from 'node:module'
import { homedir, tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const out = join(here, '../../site/screens')
const args = process.argv.slice(2)
const base = (args[0]?.startsWith('http') ? args.shift() : 'http://127.0.0.1:3017').replace(/\/$/, '')
const only = new Set(args)

/** The instant the demo data is written for (data.js): Tuesday 13 October 2026, 10:24 in Paris. */
const NOW = new Date(Date.UTC(2026, 9, 13, 8, 24))
/** Largest file accepted; above it the quality goes down until it fits. */
const MAX_BYTES = 250 * 1024

async function loadPlaywright() {
  for (const name of ['playwright', 'playwright-core']) {
    try {
      return (await import(name)).chromium
    } catch {}
  }
  const version = process.env.PLAYWRIGHT_VERSION || '1.58.2'
  const dir = join(homedir(), '.cache/facteur-screenshots')
  const require = createRequire(join(dir, 'package.json'))
  try {
    const pkg = require('playwright-core/package.json')
    if (pkg.version === version) return require('playwright-core').chromium
  } catch {}
  console.log(`Installing playwright-core@${version} into ${dir}…`)
  mkdirSync(dir, { recursive: true })
  execFileSync('npm', ['install', '--no-audit', '--no-fund', '--silent', '--prefix', dir, `playwright-core@${version}`], { stdio: 'inherit' })
  return require('playwright-core').chromium
}

// ── Shots ───────────────────────────────────────────────────────────────────

const DESKTOP = { width: 1440, height: 900 }
const MOBILE = { width: 390, height: 844 }
const thread = (id, account, box = account * 100 + 1) => `/mail/unified?thread=${id}&account=${account}&box=${box}`

const tab = label => async page => {
  await page.getByRole('tab', { name: new RegExp(`^${label}`) }).first().click()
}

/** The thread summed up by the local AI. */
async function summarize(page) {
  await page.getByRole('button', { name: 'Résumer le fil' }).first().click()
  const card = page.getByText('Résumé (IA locale)').first()
  await card.waitFor({ timeout: 5000 })
  await card.scrollIntoViewIfNeeded()
  await page.waitForTimeout(300)
}

/** An event's card, opened from the week. */
const openEvent = title => async (page) => {
  await page.getByText(title, { exact: false }).first().click()
  await page.getByText('Rappel 30 minutes avant').first().waitFor({ timeout: 5000 })
}

/** `width`: of the webp, in pixels (shots are taken at twice the viewport). */
const SHOTS = [
  { name: 'inbox-dark', path: thread(1, 2), scheme: 'dark', steps: [tab('Tout')], width: 2160 },
  { name: 'inbox-light', path: thread(1, 2), scheme: 'light', steps: [tab('Tout')], width: 2160 },
  { name: 'home-dark', path: '/', scheme: 'dark', width: 1800 },
  { name: 'home-light', path: '/', scheme: 'light', width: 1800 },
  { name: 'screener-dark', path: '/screener', scheme: 'dark', width: 1800 },
  { name: 'smart-dark', path: thread(4, 2), scheme: 'dark', steps: [tab('Personnes')], width: 1800 },
  { name: 'search-dark', path: `/mail/unified?q=${encodeURIComponent('Maison Brune')}&thread=1&account=2&box=201`, scheme: 'dark', width: 1800 },
  { name: 'safety-dark', path: thread(6, 1), scheme: 'dark', steps: [tab('Newsletters')], width: 1800 },
  { name: 'compose-dark', path: '/mail/301', scheme: 'dark', steps: [compose], width: 1800 },
  { name: 'backup-dark', path: '/settings', scheme: 'dark', steps: [async page => { await page.getByRole('link', { name: 'Sauvegarde' }).first().click().catch(() => page.getByText('Sauvegarde', { exact: true }).first().click()) }], width: 1800 },
  { name: 'stats-dark', path: '/stats/all', scheme: 'dark', width: 1800 },
  { name: 'calendar-dark', path: '/calendar', scheme: 'dark', width: 1800 },
  { name: 'calendar-light', path: '/calendar', scheme: 'light', width: 1800 },
  { name: 'ai-dark', path: thread(1, 2), scheme: 'dark', steps: [tab('Tout'), summarize], width: 1800 },
  { name: 'event-dark', path: '/calendar', scheme: 'dark', steps: [openEvent('Présentation identité visuelle Lumen')], width: 1800 },
  { name: 'mobile-inbox-dark', path: '/mail/unified', viewport: MOBILE, scheme: 'dark', steps: [tab('Tout')], width: 780 },
  { name: 'mobile-message-dark', path: thread(4, 2), viewport: MOBILE, scheme: 'dark', width: 780 },
  { name: 'mobile-inbox-light', path: '/mail/unified', viewport: MOBILE, scheme: 'light', steps: [tab('Tout')], width: 780 }
]

async function compose(page) {
  await page.getByRole('button', { name: 'Nouveau message' }).first().click()
  await page.waitForTimeout(600)
  await page.getByPlaceholder('Nom ou adresse').first().click()
  await page.keyboard.type('Inès', { delay: 40 })
  await page.getByRole('option', { name: /Inès Garnier/ }).first().click({ timeout: 5000 })
  await page.getByPlaceholder('De quoi parle ce message ?').first().fill('Assemblée générale du 7 novembre')
  await page.locator('.ProseMirror').first().click()
  await page.keyboard.press('Control+Home')
  const lines = [
    'Bonjour Inès,',
    'Je t\'envoie le bilan financier de la saison pour l\'assemblée générale : les cotisations couvrent la licence et l\'assurance, et il nous reste de quoi financer les nouveaux maillots.',
    'On le relit ensemble jeudi après la sortie ?'
  ]
  for (const [i, line] of lines.entries()) {
    await page.keyboard.type(line)
    if (i < lines.length - 1) await page.keyboard.press('Enter')
  }
  const input = page.locator('input[type=file]').first()
  if (await input.count()) {
    await input.setInputFiles({ name: 'bilan-saison-2025-2026.pdf', mimeType: 'application/pdf', buffer: Buffer.alloc(318 * 1024, 32) })
  }
}

// ── Capture ─────────────────────────────────────────────────────────────────

function toWebp(png, webp, width) {
  for (const quality of [82, 74, 66, 58, 50]) {
    if (hasFfmpeg) {
      execFileSync('ffmpeg', ['-v', 'error', '-y', '-i', png, '-vf', `scale=${width}:-1:flags=lanczos`, '-c:v', 'libwebp', '-quality', String(quality), '-compression_level', '6', webp])
    } else {
      execFileSync('convert', [png, '-resize', `${width}x`, '-quality', String(quality), '-define', 'webp:method=6', webp])
    }
    const size = statSync(webp).size
    if (size <= MAX_BYTES) return { size, quality }
  }
  return { size: statSync(webp).size, quality: 50 }
}

let hasFfmpeg = true
try {
  execFileSync('ffmpeg', ['-hide_banner', '-encoders'], { stdio: 'pipe' })
} catch {
  hasFfmpeg = false
}

const chromium = await loadPlaywright()
mkdirSync(out, { recursive: true })
const work = mkdtempSync(join(tmpdir(), 'facteur-shots-'))
/** The headless shell this Playwright expects, else the newest one installed (browsers in
 *  ~/.cache/ms-playwright from another Playwright version still drive these pages). */
async function launch() {
  try {
    return await chromium.launch()
  } catch (e) {
    const root = join(homedir(), '.cache/ms-playwright')
    const shells = existsSync(root) ? readdirSync(root).filter(d => d.startsWith('chromium_headless_shell-')).sort() : []
    const exe = shells.reverse().map(d => join(root, d, 'chrome-headless-shell-linux64/chrome-headless-shell')).find(p => existsSync(p))
    if (!exe) throw e
    console.log(`Using ${exe}`)
    return chromium.launch({ executablePath: exe })
  }
}
const browser = await launch()
try {
  for (const shot of SHOTS.filter(s => !only.size || only.has(s.name))) {
    const viewport = shot.viewport ?? DESKTOP
    const mobile = viewport === MOBILE
    const context = await browser.newContext({
      viewport,
      deviceScaleFactor: 2,
      colorScheme: shot.scheme,
      locale: 'fr-FR',
      timezoneId: 'Europe/Paris',
      isMobile: mobile,
      hasTouch: mobile
    })
    const page = await context.newPage()
    const problems = []
    page.on('pageerror', e => problems.push(e.message))
    // The clock starts at the demo's "now" and runs from there (a frozen clock stalls some timers).
    await page.clock.install({ time: NOW })
    await page.clock.resume()
    // Panel widths (Nuxt UI keeps them in localStorage): a little more room for the list.
    await page.addInitScript(() => {
      try {
        localStorage.setItem('facteur-sidebar-nav', JSON.stringify({ size: 260, collapsed: false }))
        localStorage.setItem('facteur:panel-unit', 'px')
        localStorage.setItem('facteur-panel-threads', JSON.stringify({ size: 520, collapsed: false }))
      } catch {}
    })
    await page.addInitScript({ path: join(here, 'data.js') })
    await page.addInitScript({ path: join(here, 'mock.js') })
    await page.goto(base + shot.path, { waitUntil: 'load', timeout: 120_000 })
    await page.waitForLoadState('networkidle').catch(() => {})
    await page.waitForTimeout(2500)
    for (const step of shot.steps ?? []) {
      await step(page)
      await page.waitForTimeout(700)
    }
    // Nothing hovered, nothing spinning, fonts and icons in.
    if (!mobile) await page.mouse.move(viewport.width - 2, viewport.height - 2)
    await page.evaluate(() => document.fonts.ready)
    await page.waitForFunction(() => !document.querySelector('.animate-spin'), null, { timeout: 10_000 }).catch(() => problems.push('a spinner is still visible'))
    await page.waitForTimeout(800)
    const png = join(work, `${shot.name}.png`)
    await page.screenshot({ path: png })
    const webp = join(out, `${shot.name}.webp`)
    const { size, quality } = toWebp(png, webp, shot.width)
    console.log(`${shot.name}.webp  ${Math.round(size / 1024)} KB  (quality ${quality})${problems.length ? `  ⚠ ${problems.join(' | ')}` : ''}`)
    if (process.env.KEEP_PNG) execFileSync('cp', [png, join(process.env.KEEP_PNG, `${shot.name}.png`)])
    await context.close()
  }
} finally {
  await browser.close()
  rmSync(work, { recursive: true, force: true })
}
if (!existsSync(out)) process.exitCode = 1
