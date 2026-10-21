<script setup lang="ts">
import { open as pickFolder } from '@tauri-apps/plugin-dialog'
import { revealItemInDir } from '@tauri-apps/plugin-opener'
import { useMediaQuery } from '@vueuse/core'
import type { Neutral } from '~/composables/useAppearance'
import type { FolderCountMode, FolderCountUnit } from '~/composables/useFolderCounts'
import type { WeekStart } from '~/composables/useDateTimePrefs'
import type { ThreadOrder } from '~/composables/useThreadOrder'
import type { HourCycle } from '~/utils/format'
import type { BackupStatus, Coverage, MailClientStatus, NotificationMode } from '~/utils/types'

const { accounts, tick, accountById, accountColor } = useMail()
const showError = useErrorToast()
const { t } = useI18n()
/** Desktop: section menu and cards; below `lg`, grouped lists and the account screen. */
const desktop = useMediaQuery('(min-width: 1024px)')

const coverages = ref<Coverage[]>([])

async function readCoverage() {
  const c = await api.coverage().catch(() => null)
  if (c) coverages.value = c
}
// Counting every account's messages is not free, and the cache changes several times a second
// during a sync: throttle reloads.
const coverageReloader = useReloader(readCoverage)
const loadCoverage = () => coverageReloader.now()
loadCoverage()
watch(tick, coverageReloader.schedule)
const coverageOf = (id: number) => coverages.value.find(c => c.account_id === id)

// Legacy `?account=<id>` link: redirect to that account's page.
const route = useRoute()
const askedAccount = Number(route.query.account)
if (accounts.value.some(a => a.id === askedAccount)) navigateTo(`/settings/account/${askedAccount}`, { replace: true })

const notificationItems = computed(() => [
  { label: t('settings.mail.people'), value: 'people' as const },
  { label: t('settings.mail.all'), value: 'all' as const },
  { label: t('settings.mail.none'), value: 'none' as const }
])
const notificationMode = ref<NotificationMode>('people')
const mailClient = ref<MailClientStatus | null>(null)
const blocked = ref<string[]>([])
const senderLogos = ref(true)
const notifyNewSenders = ref(false)
const eventReminders = ref(true)
const trusted = ref<string[]>([])
const trustedEntry = ref('')

async function addTrusted() {
  try {
    const key = await api.addTrustedImages(trustedEntry.value)
    if (!trusted.value.includes(key)) trusted.value = [...trusted.value, key].sort()
    trustedEntry.value = ''
  } catch (e) {
    showError(e)
  }
}

async function removeTrusted(key: string) {
  try {
    await api.removeTrustedImages(key)
    trusted.value = trusted.value.filter(k => k !== key)
  } catch (e) {
    showError(e)
  }
}
const logos = useSenderLogos()

onMounted(async () => {
  notificationMode.value = (await api.getSetting('notifications').catch(() => null) as NotificationMode | null) ?? 'people'
  mailClient.value = await api.mailClientStatus().catch(() => null)
  blocked.value = await api.blockedSenders().catch(() => [])
  trusted.value = await api.trustedImages().catch(() => [])
  senderLogos.value = (await api.getSetting('sender_logos').catch(() => null)) !== 'off'
  notifyNewSenders.value = (await api.getSetting('notify_new_senders').catch(() => null)) !== 'off'
  watch(notifyNewSenders, on => api.setSetting('notify_new_senders', on ? 'on' : 'off').catch(showError))
  eventReminders.value = (await api.getSetting('event_reminders').catch(() => null)) !== 'off'
  watch(eventReminders, on => api.setSetting('event_reminders', on ? 'on' : 'off').catch(showError))
  watch(senderLogos, async (on) => {
    await api.setSetting('sender_logos', on ? 'on' : 'off').catch(showError)
    logos.reset()
  })
})
watch(notificationMode, (mode, before) => {
  if (before !== undefined) api.setSetting('notifications', mode).catch(showError)
})

const onWindows = typeof navigator !== 'undefined' && /Windows/i.test(navigator.userAgent)
const notifyDuration = ref<string>()
const notifyDurationItems = computed(() => [
  { label: t('settings.mail.notifyDurationSystem'), value: 'system' },
  { label: t('settings.mail.notifyDurationLong'), value: 'long' },
  { label: t('settings.mail.notifyDurationSticky'), value: 'sticky' }
])
onMounted(async () => {
  notifyDuration.value = (await api.getSetting('notify_duration').catch(() => null)) ?? 'long'
})
watch(notifyDuration, (value, before) => {
  if (before !== undefined && value !== undefined) api.setSetting('notify_duration', value).catch(showError)
})

const undoSend = ref<string>()
const undoSendItems = computed(() => [
  { label: t('settings.mail.undoNone'), value: '0' },
  ...['5', '10', '20', '30'].map(n => ({ label: t('settings.mail.seconds', { n }), value: n }))
])
onMounted(async () => {
  undoSend.value = (await api.getSetting('undo_send').catch(() => null)) ?? '10'
})
watch(undoSend, (value, before) => {
  if (before !== undefined && value !== undefined) api.setSetting('undo_send', value).catch(showError)
})

/** Windows asks for confirmation in its Settings; the state is reread when facteur regains focus. */
async function makeDefaultClient() {
  try {
    await api.registerMailClient()
    const refresh = async () => {
      mailClient.value = await api.mailClientStatus().catch(() => mailClient.value)
      window.removeEventListener('focus', refresh)
    }
    window.addEventListener('focus', refresh)
  } catch (e) {
    showError(e)
  }
}

async function unblock(email: string) {
  try {
    await api.unblockSender(email)
    blocked.value = blocked.value.filter(e => e !== email)
  } catch (e) {
    showError(e)
  }
}

const backup = useBackup()
const { settings: backupSettings, status: backupStatus, running: backupRunning, busy: backupBusy, progress: backupProgress, autoFailure: backupFailure } = backup
onMounted(async () => {
  await backup.dest()
  backup.refresh()
})

async function chooseBackupFolder() {
  const picked = await pickFolder({ directory: true, defaultPath: backupSettings.value.dest ?? undefined })
  if (typeof picked === 'string') {
    backupSettings.value.dest = picked
    backup.refresh()
  }
}

async function showBackupFolder() {
  try {
    await revealItemInDir(await backup.dest())
  } catch (e) {
    showError(e)
  }
}

const numberFormat = computed(() => new Intl.NumberFormat(intlLocale()))
const num = (n: number) => numberFormat.value.format(n)

/** The account whose backup was asked for here, until it ends. */
const backingUp = ref<number | null>(null)
async function backUpAccount(id: number) {
  backingUp.value = id
  try {
    await backup.backupAll([id])
  } finally {
    backingUp.value = null
  }
}
/** That account is being backed up now (asked here, or the automatic backup reached it). */
function backingUpNow(id: number) {
  const p = backupProgress.value
  return backingUp.value === id || backupBusy.value.includes(id) || (p?.kind === 'backup' && p.accountId === id)
}

function backupName(s: BackupStatus) {
  const account = accountById(s.account_id)
  return account ? accountName(account) : s.email
}

function backupLine(s: BackupStatus) {
  const m = s.manifest
  if (!m) return t('settings.backup.never')
  const line = t('settings.backup.line', { date: longDate(m.last_backup), n: num(m.messages), size: fileSize(m.bytes) }, m.messages)
  // Kept on disk with the manifest: still shown after a restart, until a complete backup.
  return m.failed ? `${t('common.backup.incomplete')} · ${line}` : line
}

const progressLabel = computed(() => {
  const p = backupProgress.value
  if (!p) return backupRunning.value ? t('settings.backup.connecting') : ''
  const email = accounts.value.find(a => a.id === p.accountId)?.email ?? ''
  return t(p.kind === 'backup' ? 'settings.backup.backingUp' : 'settings.backup.restoring', { email, mailbox: p.mailbox, done: p.done, total: p.total })
})

const backedUp = computed(() => backupStatus.value.filter(s => s.manifest))
const lastBackup = computed(() => Math.max(0, ...backedUp.value.map(s => s.manifest!.last_backup)))
const backupIncomplete = computed(() => backedUp.value.some(s => s.manifest!.failed))

const { accent, neutral } = useAppearance()
const darkMail = useDarkMail()
const smartInbox = useSmartInbox()
const androidListening = useAndroidListening()
const { mode: folderCounts, unit: countUnit } = useFolderCounts()
const folderCountItems = computed(() => [
  { label: t('settings.mail.folderCountsUnread'), value: 'unread' as const },
  { label: t('settings.mail.folderCountsTotal'), value: 'total' as const }
])
const countUnitItems = computed(() => [
  { label: t('settings.mail.countUnitConversations'), value: 'conversations' as const },
  { label: t('settings.mail.countUnitMessages'), value: 'messages' as const }
])
const { order: threadOrder } = useThreadOrder()
const threadOrderItems = computed(() => [
  { label: t('settings.mail.threadOrderOldest'), value: 'oldest' as const },
  { label: t('settings.mail.threadOrderNewest'), value: 'newest' as const }
])
const colorMode = useColorMode()
const modeItems = computed(() => [
  { label: t('settings.appearance.light'), value: 'light', icon: 'i-lucide-sun' },
  { label: t('settings.appearance.dark'), value: 'dark', icon: 'i-lucide-moon' },
  { label: t('settings.appearance.system'), value: 'system', icon: 'i-lucide-monitor' }
])
const colorName = (c: string) => t(`settings.appearance.colors.${c}`)

const LIGHT = { page: 'bg-(--ui-color-neutral-100)', side: 'bg-white', card: 'bg-white', line: 'bg-(--ui-color-neutral-300)' }
const DARK = { page: 'bg-(--ui-color-neutral-950)', side: 'bg-(--ui-color-neutral-900)', card: 'bg-(--ui-color-neutral-800)', line: 'bg-(--ui-color-neutral-700)' }
const themePreviews: Record<string, { a: typeof LIGHT, b: typeof LIGHT }> = {
  light: { a: LIGHT, b: LIGHT },
  dark: { a: DARK, b: DARK },
  system: { a: LIGHT, b: DARK }
}

const { language, set: setLanguage } = useLanguage()
const languageItems = computed(() => (['fr', 'en'] as const).map(code => ({ label: t(`common.language.${code}`), value: code })))

const { hourCycle, weekStart } = useDateTimePrefs()
const hourCycleItems = computed(() => [
  { label: t('settings.appearance.auto'), value: 'auto' as const },
  { label: t('settings.appearance.hours12'), value: 'h12' as const },
  { label: t('settings.appearance.hours24'), value: 'h23' as const }
])
/** Day names in the UI language, from a known Monday, Saturday and Sunday of 2026. */
const weekStartItems = computed(() => [
  { label: t('settings.appearance.auto'), value: 'auto' as const },
  ...([['1', 5], ['6', 10], ['0', 4]] as const).map(([value, date]) => {
    const name = dateFormat({ weekday: 'long' }).format(new Date(2026, 0, date))
    return { label: name.charAt(0).toUpperCase() + name.slice(1), value }
  })
])

const { query: search, found, showsSection } = provideSettingsSearch()
const searchField = useTemplateRef<{ inputRef?: HTMLInputElement }>('searchField')
defineShortcuts({ meta_f: { usingInput: true, handler: () => searchField.value?.inputRef?.focus() } })
watch(search, () => {
  if (scroller) scroller.scrollTop = 0
})

const sections = computed(() => [
  { id: 'settings-appearance', label: t('settings.appearance.title'), icon: 'i-lucide-palette', tone: 'bg-violet-100 text-violet-700 dark:bg-violet-500/20 dark:text-violet-300' },
  { id: 'settings-accounts', label: t('settings.accounts.title'), icon: 'i-lucide-users', tone: 'bg-blue-100 text-blue-600 dark:bg-blue-500/15 dark:text-blue-400' },
  { id: 'settings-mail', label: t('settings.mail.title'), icon: 'i-lucide-bell', tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400' },
  { id: 'settings-backup', label: t('settings.backup.title'), icon: 'i-lucide-hard-drive-download', tone: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-400' },
  { id: 'settings-ai', label: t('settings.ai.section'), icon: 'i-lucide-sparkles', tone: 'bg-fuchsia-100 text-fuchsia-700 dark:bg-fuchsia-500/15 dark:text-fuchsia-300' },
  { id: 'settings-about', label: t('settings.about.title'), icon: 'i-lucide-info', tone: 'bg-zinc-100 text-zinc-700 dark:bg-zinc-500/15 dark:text-zinc-300' }
])
const tone = (id: string) => sections.value.find(s => s.id === id)!.tone

const root = useTemplateRef<HTMLElement>('root')
const activeSection = ref('settings-appearance')
let scroller: HTMLElement | null = null
/** Section clicked in the menu: kept highlighted until the smooth scroll ends. */
let pinned: string | null = null
let pinTimer: ReturnType<typeof setTimeout> | undefined

function onScroll() {
  if (!scroller || pinned) return
  const top = scroller.getBoundingClientRect().top
  let current = sections.value[0]!.id
  for (const s of sections.value) {
    const el = document.getElementById(s.id)
    if (el && el.getBoundingClientRect().top - top <= 120) current = s.id
  }
  // At the very bottom, the last section is the one being read even if it is short.
  if (scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 4) current = sections.value.at(-1)!.id
  activeSection.value = current
}
function unpin() {
  pinned = null
  clearTimeout(pinTimer)
}

watch(root, (el) => {
  scroller?.removeEventListener('scroll', onScroll)
  scroller?.removeEventListener('scrollend', unpin)
  scroller = el?.closest<HTMLElement>('[data-slot=body]') ?? null
  scroller?.addEventListener('scroll', onScroll, { passive: true })
  scroller?.addEventListener('scrollend', unpin)
  restoreScroll()
  onScroll()
}, { flush: 'post' })

/** Scroll position when leaving for a sub-screen (account, templates, rules): restored on the way
 *  back so the list need not be scrolled down again. */
const leftAt = useState<{ path: string, top: number } | null>('settings-left-at', () => null)
onBeforeRouteLeave((to) => {
  leftAt.value = scroller ? { path: to.fullPath, top: scroller.scrollTop } : null
})
function restoreScroll() {
  const saved = leftAt.value
  leftAt.value = null
  const el = scroller
  if (!saved || !el || window.history.state?.forward !== saved.path) return
  // Some cards fill in after the first frame (coverage, backups): tried until the page is tall enough.
  let frames = 0
  const apply = () => {
    el.scrollTop = saved.top
    if (Math.abs(el.scrollTop - saved.top) > 1 && ++frames < 30) requestAnimationFrame(apply)
  }
  apply()
}
onBeforeUnmount(() => {
  scroller?.removeEventListener('scroll', onScroll)
  scroller?.removeEventListener('scrollend', unpin)
  clearTimeout(pinTimer)
})

function goTo(id: string) {
  activeSection.value = id
  pinned = id
  clearTimeout(pinTimer)
  // Fallback when `scrollend` does not fire (nothing to scroll).
  pinTimer = setTimeout(unpin, 1000)
  document.getElementById(id)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

const cardUi = { root: 'rounded-2xl bg-(--surface-card) shadow-xs dark:shadow-none', body: 'p-0 sm:p-0 divide-y divide-default' }

type Sheet = 'theme' | 'language' | 'hours' | 'weekStart' | 'accent' | 'neutral' | 'notifications' | 'notifyDuration' | 'undo' | 'counts' | 'countUnit' | 'order' | 'trusted' | 'blocked' | 'backup'
const sheet = ref<Sheet | null>(null)
const sheetOpen = computed({
  get: () => sheet.value !== null,
  set: (open: boolean) => {
    if (!open) sheet.value = null
  }
})
const sheetTitle = computed(() => ({
  theme: t('settings.appearance.theme'),
  language: t('common.language.label'),
  hours: t('settings.appearance.timeFormat'),
  weekStart: t('settings.appearance.weekStart'),
  accent: t('settings.appearance.accent'),
  neutral: t('settings.appearance.neutral'),
  notifications: t('settings.mail.notifications'),
  notifyDuration: t('settings.mail.notifyDuration'),
  undo: t('settings.mail.undoSend'),
  counts: t('settings.mail.folderCounts'),
  countUnit: t('settings.mail.countUnit'),
  order: t('settings.mail.threadOrder'),
  trusted: t('settings.mail.trusted'),
  blocked: t('settings.mail.blocked'),
  backup: t('settings.backup.title')
})[sheet.value ?? 'theme'])

interface Choice { label: string, value: string, icon?: string, dot?: string }
const choices = computed<{ items: Choice[], value: string | undefined, set: (v: string) => void } | null>(() => {
  switch (sheet.value) {
    case 'theme': return { items: modeItems.value, value: colorMode.preference, set: (v) => { colorMode.preference = v } }
    case 'language': return { items: languageItems.value, value: language.value, set: v => setLanguage(v as 'fr' | 'en') }
    case 'hours': return { items: hourCycleItems.value, value: hourCycle.value, set: (v) => { hourCycle.value = v as HourCycle } }
    case 'weekStart': return { items: weekStartItems.value, value: weekStart.value, set: (v) => { weekStart.value = v as WeekStart } }
    case 'neutral': return {
      items: neutralColors.map(c => ({ label: colorName(c), value: c, dot: swatch(c) })),
      value: neutral.value,
      set: (v) => { neutral.value = v as Neutral }
    }
    case 'notifications': return { items: notificationItems.value, value: notificationMode.value, set: (v) => { notificationMode.value = v as NotificationMode } }
    case 'notifyDuration': return { items: notifyDurationItems.value, value: notifyDuration.value, set: (v) => { notifyDuration.value = v } }
    case 'undo': return { items: undoSendItems.value, value: undoSend.value, set: (v) => { undoSend.value = v } }
    case 'counts': return { items: folderCountItems.value, value: folderCounts.value, set: (v) => { folderCounts.value = v as FolderCountMode } }
    case 'countUnit': return { items: countUnitItems.value, value: countUnit.value, set: (v) => { countUnit.value = v as FolderCountUnit } }
    case 'order': return { items: threadOrderItems.value, value: threadOrder.value, set: (v) => { threadOrder.value = v as ThreadOrder } }
    default: return null
  }
})
function choose(v: string) {
  choices.value?.set(v)
  sheet.value = null
}
const labelOf = (items: { label: string, value: string }[], value: string | undefined) => items.find(o => o.value === value)?.label ?? ''
</script>

<template>
  <UDashboardPanel id="settings" :ui="{ body: 'bg-(--surface-read) p-0 sm:p-0' }">
    <template #header>
      <UDashboardNavbar :title="$t('settings.title')" class="max-lg:hidden">
      </UDashboardNavbar>
    </template>

    <template #body>
      <div v-if="desktop" ref="root" class="mx-auto flex w-full max-w-265 items-start gap-10 px-8 py-10 xl:px-0">
        <nav :aria-label="$t('settings.nav')" class="sticky top-10 flex w-55 shrink-0 flex-col gap-1">
          <UInput
            ref="searchField"
            v-model="search"
            type="search"
            icon="i-lucide-search"
            :placeholder="$t('settings.search')"
            :aria-label="$t('settings.search')"
            :kbds="search ? undefined : ['meta', 'f']"
            class="mb-3"
            :ui="{ base: 'rounded-xl bg-(--surface-card)' }"
            @keydown.esc="search = ''"
          />
          <a
            v-for="s in sections"
            v-show="!search || showsSection(s.id)"
            :key="s.id"
            :href="`#${s.id}`"
            class="flex h-11 items-center gap-3 rounded-xl px-2.5 text-sm transition-colors"
            :class="activeSection === s.id
              ? 'bg-(--surface-card) font-semibold text-highlighted ring-1 ring-inset ring-default'
              : 'font-medium text-default hover:bg-elevated/50'"
            :aria-current="activeSection === s.id ? 'true' : undefined"
            @click.prevent="goTo(s.id)"
          >
            <span class="flex size-7 shrink-0 items-center justify-center rounded-lg" :class="s.tone">
              <UIcon :name="s.icon" class="size-3.75" />
            </span>
            {{ s.label }}
          </a>
        </nav>

        <div class="flex min-w-0 flex-1 flex-col gap-12">
          <div v-if="search && !found" class="flex flex-col items-center gap-3 rounded-2xl border border-dashed border-default py-16 text-sm text-muted">
            <UIcon name="i-lucide-search-x" class="size-7" />
            {{ $t('settings.searchEmpty', { q: search.trim() }) }}
          </div>
          <SettingsSection
            id="settings-appearance"
            :title="$t('settings.appearance.title')"
            :description="$t('settings.appearance.description')"
            icon="i-lucide-palette"
            :tone="tone('settings-appearance')"
          >
            <UCard :ui="cardUi">
              <SettingsMatch :text="`${$t('settings.appearance.theme')} ${modeItems.map(m => m.label).join(' ')}`" class="flex flex-col gap-3.5 px-5.5 py-5">
                <span class="text-sm font-semibold text-highlighted">{{ $t('settings.appearance.theme') }}</span>
                <div role="radiogroup" :aria-label="$t('settings.appearance.theme')" class="grid grid-cols-3 gap-3">
                  <button
                    v-for="m in modeItems"
                    :key="m.value"
                    type="button"
                    role="radio"
                    :aria-checked="colorMode.preference === m.value"
                    class="flex flex-col gap-2.5 rounded-[14px] bg-(--surface-side) p-2 pb-2.5 text-start ring-inset transition-shadow"
                    :class="colorMode.preference === m.value ? 'ring-2 ring-primary' : 'ring-1 ring-default hover:ring-accented'"
                    @click="colorMode.preference = m.value"
                  >
                    <span class="flex h-21 overflow-hidden rounded-[9px] border border-default" :class="themePreviews[m.value]!.a.page">
                      <span class="flex w-[26%] flex-col gap-1.25 px-1.5 py-2" :class="themePreviews[m.value]!.a.side">
                        <span class="h-1.5 rounded-full bg-primary" />
                        <span class="h-1 rounded-full" :class="themePreviews[m.value]!.a.line" />
                        <span class="h-1 rounded-full" :class="themePreviews[m.value]!.a.line" />
                      </span>
                      <span class="flex flex-1 flex-col gap-1.5 p-2.5" :class="themePreviews[m.value]!.b.page">
                        <span class="h-4.5 rounded-[5px]" :class="themePreviews[m.value]!.b.card" />
                        <span class="h-1.25 w-[70%] rounded-full" :class="themePreviews[m.value]!.b.line" />
                        <span class="h-1.25 w-1/2 rounded-full" :class="themePreviews[m.value]!.b.line" />
                      </span>
                    </span>
                    <span
                      class="flex items-center gap-2 px-1 text-[13.5px]"
                      :class="colorMode.preference === m.value ? 'font-semibold text-highlighted' : 'font-medium text-muted'"
                    >
                      <span
                        class="size-4 shrink-0 rounded-full"
                        :class="colorMode.preference === m.value ? 'border-[5px] border-primary bg-(--surface-card)' : 'border-[1.5px] border-(--ui-text-dimmed)'"
                      />
                      {{ m.label }}
                    </span>
                  </button>
                </div>
              </SettingsMatch>

              <SettingsRow :label="$t('common.language.label')" :description="$t('settings.appearance.languageHelp')">
                <SettingsSegmented
                  :model-value="language"
                  :items="languageItems"
                  :label="$t('common.language.label')"
                  @update:model-value="(v) => setLanguage(v)"
                />
              </SettingsRow>

              <SettingsRow :label="$t('settings.appearance.timeFormat')" :description="$t('settings.appearance.timeFormatHelp')">
                <SettingsSegmented v-model="hourCycle" :items="hourCycleItems" :label="$t('settings.appearance.timeFormat')" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.appearance.weekStart')" :description="$t('settings.appearance.weekStartHelp')">
                <SettingsSegmented v-model="weekStart" :items="weekStartItems" :label="$t('settings.appearance.weekStart')" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.appearance.darkMail')" :description="$t('settings.appearance.darkMailHelp')">
                <USwitch :model-value="darkMail.enabled.value" size="lg" :aria-label="$t('settings.appearance.darkMail')" @update:model-value="darkMail.set" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.appearance.accent')">
                <template #hint>
                  <span class="text-[13px] font-normal text-muted">{{ colorName(accent) }}</span>
                </template>
                <template #below>
                  <div role="radiogroup" :aria-label="$t('settings.appearance.accent')" class="flex flex-wrap gap-2.5">
                    <UTooltip v-for="c in accentColors" :key="c" :text="colorName(c)">
                      <button
                        type="button"
                        role="radio"
                        class="flex size-8 items-center justify-center rounded-full text-white transition-transform hover:scale-110"
                        :style="{
                          backgroundColor: swatch(c),
                          boxShadow: accent === c ? `0 0 0 2px var(--surface-card), 0 0 0 4px ${swatch(c)}` : undefined
                        }"
                        :aria-label="colorName(c)"
                        :aria-checked="accent === c"
                        @click="accent = c"
                      >
                        <UIcon v-if="accent === c" name="i-lucide-check" class="size-4" />
                      </button>
                    </UTooltip>
                  </div>
                </template>
              </SettingsRow>

              <SettingsRow :label="$t('settings.appearance.neutral')">
                <template #below>
                  <div role="radiogroup" :aria-label="$t('settings.appearance.neutral')" class="flex flex-wrap gap-2">
                    <button
                      v-for="c in neutralColors"
                      :key="c"
                      type="button"
                      role="radio"
                      :aria-checked="neutral === c"
                      class="inline-flex h-9 items-center gap-2 rounded-full ps-2.5 pe-3.5 text-[13px] ring-inset transition-colors"
                      :class="neutral === c
                        ? 'bg-(--ui-color-primary-100) font-semibold text-primary ring-[1.5px] ring-primary dark:bg-primary/15 dark:text-(--ui-color-primary-300)'
                        : 'bg-(--surface-side) font-medium text-default ring-1 ring-default hover:ring-accented'"
                      @click="neutral = c"
                    >
                      <span class="size-3.5 rounded-full" :style="{ backgroundColor: swatch(c) }" />
                      {{ colorName(c) }}
                    </button>
                  </div>
                </template>
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.folderCounts')" :description="$t('settings.mail.folderCountsHelp')">
                <SettingsSegmented v-model="folderCounts" :items="folderCountItems" :label="$t('settings.mail.folderCounts')" />
              </SettingsRow>
              <SettingsRow :label="$t('settings.mail.countUnit')" :description="$t('settings.mail.countUnitHelp')">
                <SettingsSegmented v-model="countUnit" :items="countUnitItems" :label="$t('settings.mail.countUnit')" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.threadOrder')" :description="$t('settings.mail.threadOrderHelp')">
                <SettingsSegmented v-model="threadOrder" :items="threadOrderItems" :label="$t('settings.mail.threadOrder')" />
              </SettingsRow>
            </UCard>
          </SettingsSection>

          <SettingsSection
            id="settings-accounts"
            :title="$t('settings.accounts.title')"
            :description="$t('settings.accounts.description', { n: accounts.length }, accounts.length)"
            icon="i-lucide-users"
            :tone="tone('settings-accounts')"
          >
            <template #actions>
              <UButton icon="i-lucide-plus" :label="$t('settings.accounts.add')" to="/setup" size="lg" class="h-10 rounded-[11px] text-[13.5px] font-semibold" />
            </template>
            <UCard :ui="cardUi">
              <SettingsAccount
                v-for="a in accounts"
                :key="a.id"
                :account="a"
                layout="row"
                :coverage="coverageOf(a.id)"
              />
            </UCard>
          </SettingsSection>

          <SettingsSection
            id="settings-mail"
            :title="$t('settings.mail.title')"
            :description="$t('settings.mail.description')"
            icon="i-lucide-bell"
            :tone="tone('settings-mail')"
          >
            <UCard :ui="cardUi">
              <SettingsRow :label="$t('settings.mail.notifications')" :description="$t('settings.mail.notificationsHelp')">
                <SettingsSegmented v-model="notificationMode" :items="notificationItems" :label="$t('settings.mail.notifications')" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.notifyNewSenders')" :description="$t('settings.mail.notifyNewSendersHelp')">
                <USwitch v-model="notifyNewSenders" size="lg" :disabled="notificationMode === 'none'" :aria-label="$t('settings.mail.notifyNewSenders')" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.eventReminders')" :description="$t('settings.mail.eventRemindersHelp')">
                <USwitch v-model="eventReminders" size="lg" :disabled="notificationMode === 'none'" :aria-label="$t('settings.mail.eventReminders')" />
              </SettingsRow>

              <SettingsRow v-if="onWindows" :label="$t('settings.mail.notifyDuration')" :description="$t('settings.mail.notifyDurationHelp')">
                <SettingsSegmented
                  v-if="notifyDuration !== undefined"
                  v-model="notifyDuration"
                  :items="notifyDurationItems"
                  :label="$t('settings.mail.notifyDuration')"
                />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.undoSend')" :description="$t('settings.mail.undoSendHelp')">
                <SettingsSegmented v-if="undoSend !== undefined" v-model="undoSend" :items="undoSendItems" :label="$t('settings.mail.undoSend')" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.smartInbox')" :description="$t('settings.mail.smartInboxHelp')">
                <USwitch :model-value="smartInbox.enabled.value" size="lg" :aria-label="$t('settings.mail.smartInbox')" @update:model-value="smartInbox.set" />
              </SettingsRow>

              <SettingsRow v-if="androidListening.supported" :label="$t('settings.mail.backgroundListening')" :description="$t('settings.mail.backgroundListeningHelp')">
                <USwitch :model-value="androidListening.enabled.value" size="lg" :aria-label="$t('settings.mail.backgroundListening')" @update:model-value="androidListening.set" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.rules')" :description="$t('settings.mail.rulesHelp')">
                <UButton
                  :label="$t('settings.mail.manageRules')"
                  trailing-icon="i-lucide-chevron-right"
                  color="neutral"
                  variant="outline"
                  class="h-9 rounded-[10px]"
                  :ui="{ trailingIcon: 'size-3.5 text-dimmed' }"
                  to="/rules"
                />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.templates')" :description="$t('settings.mail.templatesHelp')">
                <UButton
                  :label="$t('settings.mail.manageTemplates')"
                  trailing-icon="i-lucide-chevron-right"
                  color="neutral"
                  variant="outline"
                  class="h-9 rounded-[10px]"
                  :ui="{ trailingIcon: 'size-3.5 text-dimmed' }"
                  to="/templates"
                />
              </SettingsRow>

              <SettingsRow
                :label="$t('settings.mail.defaultClient')"
                :description="!mailClient?.supported ? $t('settings.mail.windowsOnly')
                  : mailClient.default ? $t('settings.mail.isDefault')
                    : $t('settings.mail.notDefault')"
              >
                <UBadge v-if="mailClient?.default" :label="$t('settings.mail.enabled')" color="success" variant="subtle" icon="i-lucide-check" />
                <UButton
                  v-else-if="mailClient?.supported"
                  :label="$t('settings.mail.makeDefault')"
                  trailing-icon="i-lucide-chevron-right"
                  color="neutral"
                  variant="outline"
                  class="h-9 rounded-[10px]"
                  :ui="{ trailingIcon: 'size-3.5 text-dimmed' }"
                  @click="makeDefaultClient"
                />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.senderLogos')" :description="$t('settings.mail.senderLogosHelp')">
                <USwitch v-model="senderLogos" size="lg" :aria-label="$t('settings.mail.senderLogos')" />
              </SettingsRow>

              <SettingsRow :label="$t('settings.mail.trusted')" :description="$t('settings.mail.trustedHelp')">
                <template #below>
                  <form class="flex gap-2" @submit.prevent="addTrusted">
                    <UInput
                      v-model="trustedEntry"
                      :placeholder="$t('settings.mail.trustedPlaceholder')"
                      :aria-label="$t('settings.mail.trustedLabel')"
                      class="flex-1"
                      :ui="{ base: 'h-9.5 rounded-[10px]' }"
                    />
                    <UButton
                      type="submit"
                      :label="$t('settings.mail.add')"
                      variant="soft"
                      class="h-9.5 rounded-[10px] px-4 font-semibold"
                      :disabled="!trustedEntry.trim()"
                    />
                  </form>
                  <ul v-if="trusted.length" class="flex flex-wrap gap-2">
                    <li
                      v-for="key in trusted"
                      :key="key"
                      class="inline-flex h-8 max-w-full items-center gap-2 rounded-full border border-default bg-(--surface-side) ps-2.5 pe-1 text-[13px] text-default"
                    >
                      <UIcon
                        :name="key.includes('@') ? 'i-lucide-at-sign' : 'i-lucide-globe'"
                        class="size-3.5 shrink-0"
                        :class="key.includes('@') ? 'text-pink-700 dark:text-pink-300' : 'text-blue-600 dark:text-blue-400'"
                      />
                      <span class="truncate">{{ key }}</span>
                      <button
                        type="button"
                        class="flex size-6 shrink-0 items-center justify-center rounded-full text-dimmed hover:bg-elevated hover:text-default"
                        :aria-label="$t('settings.mail.removeKey', { key })"
                        @click="removeTrusted(key)"
                      >
                        <UIcon name="i-lucide-x" class="size-3.25" />
                      </button>
                    </li>
                  </ul>
                </template>
              </SettingsRow>

              <SettingsRow
                :label="$t('settings.mail.blocked')"
                :description="blocked.length ? $t('settings.mail.blockedCount', { n: blocked.length }, blocked.length) : $t('settings.mail.blockedEmpty')"
              >
                <template #leading>
                  <span class="flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-(--surface-side) text-dimmed">
                    <UIcon name="i-lucide-ban" class="size-4.25" />
                  </span>
                </template>
                <template v-if="blocked.length" #below>
                  <ul class="divide-y divide-default rounded-xl border border-default">
                    <li v-for="email in blocked" :key="email" class="flex items-center gap-2 py-1.5 ps-3.5 pe-1.5 text-sm">
                      <span class="min-w-0 flex-1 truncate">{{ email }}</span>
                      <UButton :label="$t('settings.mail.unblock')" size="sm" color="neutral" variant="ghost" @click="unblock(email)" />
                    </li>
                  </ul>
                </template>
              </SettingsRow>
            </UCard>
          </SettingsSection>

          <SettingsSection
            id="settings-backup"
            :title="$t('settings.backup.title')"
            :description="$t('settings.backup.intro')"
            icon="i-lucide-hard-drive-download"
            :tone="tone('settings-backup')"
          >
            <UCard :ui="cardUi">
              <div class="flex items-center gap-3 px-5.5 py-4.5">
                <span
                  class="flex h-10 min-w-0 flex-1 items-center gap-2.5 rounded-[10px] border border-default bg-(--surface-side) px-3 font-mono text-[12.5px] text-default"
                  :title="backupSettings.dest ?? ''"
                >
                  <UIcon name="i-lucide-folder" class="size-4 shrink-0 text-emerald-700 dark:text-emerald-400" />
                  <span class="truncate">{{ backupSettings.dest }}</span>
                </span>
                <UButton :label="$t('settings.backup.change')" color="neutral" variant="outline" class="h-10 rounded-[10px] px-3.5" :disabled="backupRunning" @click="chooseBackupFolder" />
                <UButton :label="$t('settings.backup.open')" icon="i-lucide-external-link" color="neutral" variant="outline" class="h-10 rounded-[10px] px-3.5" @click="showBackupFolder" />
              </div>

              <SettingsRow :label="$t('settings.backup.auto')" :description="$t('settings.backup.autoHelp')">
                <USwitch v-model="backupSettings.auto" size="lg" :aria-label="$t('settings.backup.auto')" />
              </SettingsRow>

              <!-- Stays until a backup succeeds: the hourly attempts no longer toast it. -->
              <div v-if="backupFailure" class="px-5.5 py-3.5">
                <UAlert
                  color="error"
                  variant="subtle"
                  icon="i-lucide-triangle-alert"
                  :title="$t('settings.backup.autoFailed', { date: longDate(backupFailure.at) })"
                  :description="backupFailure.message"
                  :ui="{ description: 'whitespace-pre-line break-words' }"
                />
              </div>

              <div v-for="s in backupStatus" :key="s.account_id" class="flex items-center gap-3.5 px-5.5 py-3.5">
                <span class="relative shrink-0">
                  <UAvatar
                    :src="accountById(s.account_id)?.avatar ?? undefined"
                    :alt="s.email"
                    :text="initials(accountById(s.account_id) ? accountName(accountById(s.account_id)!) : s.email)"
                    class="size-9.5 text-[13px]"
                    :class="accountById(s.account_id) ? accountColor(s.account_id).solid : 'bg-elevated text-muted'"
                    :ui="{ fallback: 'text-inherit font-bold' }"
                  />
                  <span
                    v-if="s.manifest"
                    class="absolute -end-0.75 -bottom-0.75 flex size-4.5 items-center justify-center rounded-full text-white ring-2 ring-(--surface-card)"
                    :class="s.manifest.failed ? 'bg-amber-600 dark:bg-amber-500' : 'bg-green-600 dark:bg-green-500'"
                  >
                    <UIcon :name="s.manifest.failed ? 'i-lucide-exclamation' : 'i-lucide-check'" class="size-2.75" />
                  </span>
                </span>
                <div class="min-w-0 flex-1">
                  <p class="truncate text-sm font-semibold text-highlighted" :title="s.email">{{ backupName(s) }}</p>
                  <p class="truncate text-[12.5px] text-muted">{{ backupLine(s) }}</p>
                </div>
                <UButton
                  :label="$t('settings.backup.now')"
                  icon="i-lucide-hard-drive-download"
                  size="sm"
                  color="neutral"
                  variant="ghost"
                  class="text-muted"
                  :loading="backingUpNow(s.account_id)"
                  :disabled="backupRunning"
                  @click="backUpAccount(s.account_id)"
                />
                <UButton
                  :label="$t('settings.backup.restore')"
                  icon="i-lucide-archive-restore"
                  size="sm"
                  color="neutral"
                  variant="ghost"
                  class="text-muted"
                  :disabled="!s.manifest || backupRunning"
                  :to="`/restore?source=${encodeURIComponent(s.manifest?.dir ?? s.email)}`"
                />
              </div>

              <div v-if="backupRunning" class="space-y-2 px-5.5 py-3.5">
                <p class="truncate text-sm text-muted">{{ progressLabel }}</p>
                <UProgress :model-value="backupProgress?.total ? backupProgress.done : null" :max="backupProgress?.total || 100" size="sm" />
              </div>

            </UCard>
          </SettingsSection>

          <SettingsSection
            id="settings-ai"
            :title="$t('settings.ai.section')"
            :keywords="$t('settings.ai.keywords')"
            :description="$t('settings.ai.intro')"
            icon="i-lucide-sparkles"
            :tone="tone('settings-ai')"
          >
            <UCard :ui="cardUi">
              <SettingsAi />
            </UCard>
          </SettingsSection>

          <SettingsSection
            id="settings-about"
            :title="$t('settings.about.title')"
            :keywords="$t('settings.about.keywords')"
            :description="$t('settings.about.intro')"
            icon="i-lucide-info"
            :tone="tone('settings-about')"
          >
            <UCard :ui="cardUi">
              <SettingsAbout layout="card" />
            </UCard>
          </SettingsSection>
        </div>
      </div>

      <div v-else ref="root" class="flex flex-col gap-5.5 px-4 pt-7 pb-8">
        <UInput
          v-model="search"
          type="search"
          icon="i-lucide-search"
          size="xl"
          :placeholder="$t('settings.search')"
          :aria-label="$t('settings.search')"
          :ui="{ base: 'rounded-xl bg-(--surface-card)' }"
        />
        <div v-if="search && !found" class="flex flex-col items-center gap-3 py-12 text-sm text-muted">
          <UIcon name="i-lucide-search-x" class="size-7" />
          {{ $t('settings.searchEmpty', { q: search.trim() }) }}
        </div>
        <h1 class="px-1 text-[30px] font-bold tracking-tight text-highlighted">{{ $t('settings.title') }}</h1>

        <SettingsGroup :title="$t('settings.appearance.title')">
          <SettingsListRow
            :label="$t('settings.appearance.theme')"
            icon="i-lucide-sun-moon"
            tone="bg-indigo-500"
            :value="labelOf(modeItems, colorMode.preference)"
            button
            @click="sheet = 'theme'"
          />
          <SettingsListRow
            :label="$t('common.language.label')"
            icon="i-lucide-languages"
            tone="bg-blue-500"
            :value="labelOf(languageItems, language)"
            button
            @click="sheet = 'language'"
          />
          <SettingsListRow
            :label="$t('settings.appearance.timeFormat')"
            icon="i-lucide-clock"
            tone="bg-indigo-500"
            :value="labelOf(hourCycleItems, hourCycle)"
            button
            @click="sheet = 'hours'"
          />
          <SettingsListRow
            :label="$t('settings.appearance.weekStart')"
            icon="i-lucide-calendar-range"
            tone="bg-rose-500"
            :value="labelOf(weekStartItems, weekStart)"
            button
            @click="sheet = 'weekStart'"
          />
          <SettingsListRow
            :label="$t('settings.appearance.accent')"
            icon="i-lucide-palette"
            :tone-style="{ backgroundColor: swatch(accent) }"
            button
            @click="sheet = 'accent'"
          >
            <template #value>
              <span class="size-3.5 shrink-0 rounded-full" :style="{ backgroundColor: swatch(accent) }" />
              <span class="truncate">{{ colorName(accent) }}</span>
            </template>
          </SettingsListRow>
          <SettingsListRow
            :label="$t('settings.appearance.neutral')"
            icon="i-lucide-swatch-book"
            :tone-style="{ backgroundColor: swatch(neutral) }"
            :value="colorName(neutral)"
            button
            @click="sheet = 'neutral'"
          />
          <SettingsListRow :label="$t('settings.appearance.darkMail')" icon="i-lucide-moon" tone="bg-violet-500">
            <USwitch :model-value="darkMail.enabled.value" size="lg" :aria-label="$t('settings.appearance.darkMail')" @update:model-value="darkMail.set" />
          </SettingsListRow>
          <SettingsListRow
            :label="$t('settings.mail.folderCountsShort')"
            icon="i-lucide-hash"
            tone="bg-sky-500"
            :value="labelOf(folderCountItems, folderCounts)"
            button
            @click="sheet = 'counts'"
          />
          <SettingsListRow
            :label="$t('settings.mail.countUnit')"
            icon="i-lucide-messages-square"
            tone="bg-sky-500"
            :value="labelOf(countUnitItems, countUnit)"
            button
            @click="sheet = 'countUnit'"
          />
          <SettingsListRow
            :label="$t('settings.mail.threadOrder')"
            icon="i-lucide-arrow-down-wide-narrow"
            tone="bg-teal-500"
            :value="labelOf(threadOrderItems, threadOrder)"
            button
            @click="sheet = 'order'"
          />
        </SettingsGroup>

        <SettingsGroup :title="$t('settings.accounts.title')">
          <SettingsListRow
            v-for="a in accounts"
            :key="a.id"
            :label="accountName(a)"
            :sublabel="a.email"
            :keywords="$t('settings.accounts.keywords')"
            :to="`/settings/account/${a.id}`"
            size="lg"
          >
            <template #leading>
              <UAvatar
                :src="a.avatar ?? undefined"
                :alt="accountName(a)"
                :text="initials(accountName(a))"
                class="size-9.5 shrink-0 text-[13px]"
                :class="accountColor(a.id).solid"
                :ui="{ fallback: 'text-inherit font-bold' }"
              />
            </template>
          </SettingsListRow>
          <NuxtLink to="/setup" class="flex min-h-13 items-center gap-3 px-3.5 py-2 text-[15px] font-semibold text-primary active:bg-elevated/60">
            <span class="flex size-9.5 shrink-0 items-center justify-center rounded-full border-[1.5px] border-dashed border-primary">
              <UIcon name="i-lucide-plus" class="size-4.5" />
            </span>
            {{ $t('settings.accounts.add') }}
          </NuxtLink>
        </SettingsGroup>

        <SettingsGroup :title="$t('settings.mail.title')">
          <SettingsListRow
            :label="$t('settings.mail.notificationsShort')"
            icon="i-lucide-bell"
            tone="bg-red-500"
            :value="labelOf(notificationItems, notificationMode)"
            button
            @click="sheet = 'notifications'"
          />
          <SettingsListRow :label="$t('settings.mail.notifyNewSendersShort')" icon="i-lucide-user-round-search" tone="bg-sky-500">
            <USwitch v-model="notifyNewSenders" size="lg" :disabled="notificationMode === 'none'" :aria-label="$t('settings.mail.notifyNewSenders')" />
          </SettingsListRow>
          <SettingsListRow :label="$t('settings.mail.eventRemindersShort')" icon="i-lucide-bell" tone="bg-orange-500">
            <USwitch v-model="eventReminders" size="lg" :disabled="notificationMode === 'none'" :aria-label="$t('settings.mail.eventReminders')" />
          </SettingsListRow>
          <SettingsListRow
            v-if="onWindows"
            :label="$t('settings.mail.notifyDuration')"
            icon="i-lucide-timer"
            tone="bg-violet-500"
            :value="labelOf(notifyDurationItems, notifyDuration)"
            :keywords="$t('settings.mail.notifyDurationHelp')"
            button
            @click="sheet = 'notifyDuration'"
          />
          <SettingsListRow
            :label="$t('settings.mail.undoSendShort')"
            icon="i-lucide-undo-2"
            tone="bg-orange-500"
            :value="labelOf(undoSendItems, undoSend)"
            button
            @click="sheet = 'undo'"
          />
          <SettingsListRow :label="$t('settings.mail.smartInbox')" icon="i-lucide-sparkles" tone="bg-pink-500">
            <USwitch :model-value="smartInbox.enabled.value" size="lg" :aria-label="$t('settings.mail.smartInbox')" @update:model-value="smartInbox.set" />
          </SettingsListRow>
          <SettingsListRow v-if="androidListening.supported" :label="$t('settings.mail.backgroundListening')" icon="i-lucide-radio-tower" tone="bg-teal-500">
            <USwitch :model-value="androidListening.enabled.value" size="lg" :aria-label="$t('settings.mail.backgroundListening')" @update:model-value="androidListening.set" />
          </SettingsListRow>
          <SettingsListRow :label="$t('settings.mail.rules')" icon="i-lucide-workflow" tone="bg-indigo-500" to="/rules" />
          <SettingsListRow :label="$t('settings.mail.templates')" icon="i-lucide-file-text" tone="bg-cyan-500" to="/templates" />
          <SettingsListRow :label="$t('settings.mail.senderLogos')" icon="i-lucide-store" tone="bg-amber-500">
            <USwitch v-model="senderLogos" size="lg" :aria-label="$t('settings.mail.senderLogos')" />
          </SettingsListRow>
          <SettingsListRow
            :label="$t('settings.mail.trustedShort')"
            icon="i-lucide-image"
            tone="bg-emerald-500"
            :value="$t('settings.mail.trustedCount', { n: trusted.length }, trusted.length)"
            button
            @click="sheet = 'trusted'"
          />
          <SettingsListRow
            :label="$t('settings.mail.blocked')"
            icon="i-lucide-ban"
            tone="bg-(--ui-color-neutral-500)"
            :value="$t('settings.mail.blockedShort', { n: blocked.length }, blocked.length)"
            button
            @click="sheet = 'blocked'"
          />
          <SettingsListRow
            v-if="mailClient?.supported"
            :label="$t('settings.mail.defaultClient')"
            icon="i-lucide-mail-check"
            tone="bg-cyan-500"
            :value="mailClient.default ? $t('settings.mail.enabled') : undefined"
            :button="!mailClient.default"
            @click="makeDefaultClient"
          />
        </SettingsGroup>

        <SettingsGroup :title="$t('settings.backup.title')">
          <SettingsListRow :label="$t('settings.backup.autoShort')" icon="i-lucide-hard-drive-download" tone="bg-emerald-500">
            <USwitch v-model="backupSettings.auto" size="lg" :aria-label="$t('settings.backup.auto')" />
          </SettingsListRow>
          <div v-if="backupFailure" class="px-3.5 py-3">
            <UAlert
              color="error"
              variant="subtle"
              icon="i-lucide-triangle-alert"
              :title="$t('settings.backup.autoFailed', { date: longDate(backupFailure.at) })"
              :description="backupFailure.message"
              :ui="{ description: 'whitespace-pre-line break-words' }"
            />
          </div>
          <SettingsListRow
            :label="$t('settings.backup.summary', { n: backedUp.length }, backedUp.length)"
            :sublabel="lastBackup ? $t('settings.backup.last', { date: longDate(lastBackup) }) : $t('settings.backup.never')"
            button
            @click="sheet = 'backup'"
          >
            <template #leading>
              <span
                class="flex size-7.5 shrink-0 items-center justify-center rounded-[9px]"
                :class="backupIncomplete ? 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300' : 'bg-green-100 text-green-700 dark:bg-green-500/15 dark:text-green-300'"
              >
                <UIcon :name="backupIncomplete ? 'i-lucide-triangle-alert' : backedUp.length ? 'i-lucide-check' : 'i-lucide-circle-dashed'" class="size-4.5" />
              </span>
            </template>
          </SettingsListRow>
          <div v-if="backupRunning" class="space-y-2 px-3.5 py-3">
            <p class="truncate text-sm text-muted">{{ progressLabel }}</p>
            <UProgress :model-value="backupProgress?.total ? backupProgress.done : null" :max="backupProgress?.total || 100" size="sm" />
          </div>
        </SettingsGroup>

        <SettingsGroup :title="$t('settings.ai.section')" :keywords="$t('settings.ai.keywords')">
          <SettingsAi touch />
        </SettingsGroup>

        <SettingsGroup :title="$t('settings.about.title')" :keywords="$t('settings.about.keywords')">
          <SettingsAbout layout="list" />
        </SettingsGroup>

        <UDrawer v-model:open="sheetOpen" :title="sheetTitle" :ui="{ container: 'pb-[calc(1rem+env(safe-area-inset-bottom))]' }">
          <template #body>
            <div v-if="choices" role="radiogroup" :aria-label="sheetTitle" class="divide-y divide-default overflow-hidden rounded-2xl border border-default">
              <button
                v-for="o in choices.items"
                :key="o.value"
                type="button"
                role="radio"
                :aria-checked="choices.value === o.value"
                class="flex min-h-13 w-full items-center gap-3 px-4 text-start text-[15px] active:bg-elevated/60"
                :class="choices.value === o.value ? 'font-semibold text-highlighted' : 'text-default'"
                @click="choose(o.value)"
              >
                <UIcon v-if="o.icon" :name="o.icon" class="size-5 shrink-0 text-muted" />
                <span v-if="o.dot" class="size-4 shrink-0 rounded-full" :style="{ backgroundColor: o.dot }" />
                <span class="flex-1">{{ o.label }}</span>
                <UIcon v-if="choices.value === o.value" name="i-lucide-check" class="size-5 shrink-0 text-primary" />
              </button>
            </div>

            <div v-else-if="sheet === 'accent'" role="radiogroup" :aria-label="sheetTitle" class="grid grid-cols-6 justify-items-center gap-3 py-1">
              <button
                v-for="c in accentColors"
                :key="c"
                type="button"
                role="radio"
                class="flex size-11 items-center justify-center rounded-full text-white"
                :style="{
                  backgroundColor: swatch(c),
                  boxShadow: accent === c ? `0 0 0 3px var(--ui-bg), 0 0 0 5px ${swatch(c)}` : undefined
                }"
                :aria-label="colorName(c)"
                :aria-checked="accent === c"
                @click="accent = c"
              >
                <UIcon v-if="accent === c" name="i-lucide-check" class="size-5" />
              </button>
            </div>

            <div v-else-if="sheet === 'trusted'" class="flex flex-col gap-3">
              <p class="text-[13px] text-muted">{{ $t('settings.mail.trustedHelp') }}</p>
              <form class="flex gap-2" @submit.prevent="addTrusted">
                <UInput
                  v-model="trustedEntry"
                  :placeholder="$t('settings.mail.trustedPlaceholder')"
                  :aria-label="$t('settings.mail.trustedLabel')"
                  size="lg"
                  class="flex-1"
                  :ui="{ base: 'h-11 rounded-xl' }"
                />
                <UButton type="submit" :label="$t('settings.mail.add')" variant="soft" size="lg" class="h-11 rounded-xl px-4 font-semibold" :disabled="!trustedEntry.trim()" />
              </form>
              <ul v-if="trusted.length" class="divide-y divide-default overflow-hidden rounded-2xl border border-default">
                <li v-for="key in trusted" :key="key" class="flex min-h-13 items-center gap-3 ps-4 pe-1.5 text-[15px]">
                  <UIcon
                    :name="key.includes('@') ? 'i-lucide-at-sign' : 'i-lucide-globe'"
                    class="size-4.5 shrink-0"
                    :class="key.includes('@') ? 'text-pink-700 dark:text-pink-300' : 'text-blue-600 dark:text-blue-400'"
                  />
                  <span class="min-w-0 flex-1 truncate">{{ key }}</span>
                  <UButton
                    icon="i-lucide-x"
                    color="neutral"
                    variant="ghost"
                    size="lg"
                    class="size-11 justify-center"
                    :aria-label="$t('settings.mail.removeKey', { key })"
                    @click="removeTrusted(key)"
                  />
                </li>
              </ul>
            </div>

            <div v-else-if="sheet === 'blocked'">
              <p v-if="!blocked.length" class="text-sm text-muted">{{ $t('settings.mail.blockedEmpty') }}</p>
              <ul v-else class="divide-y divide-default overflow-hidden rounded-2xl border border-default">
                <li v-for="email in blocked" :key="email" class="flex min-h-13 items-center gap-2 ps-4 pe-1.5 text-[15px]">
                  <span class="min-w-0 flex-1 truncate">{{ email }}</span>
                  <UButton :label="$t('settings.mail.unblock')" color="neutral" variant="ghost" size="lg" class="min-h-11" @click="unblock(email)" />
                </li>
              </ul>
            </div>

            <div v-else-if="sheet === 'backup'" class="flex flex-col gap-4">
              <p class="text-[13px] text-muted">{{ $t('settings.backup.intro') }}</p>
              <div class="flex flex-col gap-2">
                <span
                  class="flex min-h-11 min-w-0 items-center gap-2.5 rounded-xl border border-default bg-(--surface-side) px-3 py-2 font-mono text-[12.5px] text-default"
                >
                  <UIcon name="i-lucide-folder" class="size-4 shrink-0 text-emerald-700 dark:text-emerald-400" />
                  <span class="break-all">{{ backupSettings.dest }}</span>
                </span>
                <div class="grid grid-cols-2 gap-2">
                  <UButton :label="$t('settings.backup.change')" color="neutral" variant="outline" size="lg" block class="min-h-11 rounded-xl" :disabled="backupRunning" @click="chooseBackupFolder" />
                  <UButton :label="$t('settings.backup.open')" icon="i-lucide-external-link" color="neutral" variant="outline" size="lg" block class="min-h-11 rounded-xl" @click="showBackupFolder" />
                </div>
              </div>
              <ul class="divide-y divide-default overflow-hidden rounded-2xl border border-default">
                <li v-for="s in backupStatus" :key="s.account_id" class="flex min-h-15 items-center gap-3 ps-4 pe-1.5 py-2">
                  <div class="min-w-0 flex-1">
                    <p class="truncate text-[15px] text-highlighted">{{ backupName(s) }}</p>
                    <p class="truncate text-[12.5px] text-muted">{{ backupLine(s) }}</p>
                  </div>
                  <UButton
                    icon="i-lucide-hard-drive-download"
                    color="neutral"
                    variant="ghost"
                    size="lg"
                    class="size-11 justify-center"
                    :aria-label="$t('settings.backup.now')"
                    :loading="backingUpNow(s.account_id)"
                    :disabled="backupRunning"
                    @click="backUpAccount(s.account_id)"
                  />
                  <UButton
                    icon="i-lucide-archive-restore"
                    color="neutral"
                    variant="ghost"
                    size="lg"
                    class="size-11 justify-center"
                    :aria-label="$t('settings.backup.restore')"
                    :disabled="!s.manifest || backupRunning"
                    :to="`/restore?source=${encodeURIComponent(s.manifest?.dir ?? s.email)}`"
                  />
                </li>
              </ul>
            </div>
          </template>
        </UDrawer>
      </div>
    </template>
  </UDashboardPanel>
</template>
