<script setup lang="ts">
import { useMediaQuery } from '@vueuse/core'
import type { Account, AccountStats, CountedAddress, Role } from '~/utils/types'

const route = useRoute()
const router = useRouter()
const { accounts, accountById, tick } = useMail()
const showError = useErrorToast()
const { t } = useI18n()
const desktop = useMediaQuery('(min-width: 1024px)')
const NuxtLink = resolveComponent('NuxtLink')

/** The page follows the URL: `/stats/<id>`, or `/stats/all` for every account at once. */
const scope = computed<number | null>(() => (route.params.account === 'all' ? null : Number(route.params.account)))
const selectedAccount = computed({
  get: () => scope.value,
  set: (id: number | null) => navigateTo(`/stats/${id ?? 'all'}`, { replace: true })
})

function back() {
  if (window.history.state?.back) router.back()
  else navigateTo('/', { replace: true })
}

const parts = ref<{ account: Account, stats: AccountStats }[] | null>(null)
/** Scope the shown statistics are for (they stay on screen while another scope loads). */
const partsScope = ref<number | null | undefined>(undefined)
const failed = ref<'unknown' | 'error' | null>(null)
let request = 0

async function load() {
  const id = scope.value
  const targets = id === null ? accounts.value : accounts.value.filter(a => a.id === id)
  const mine = ++request
  if (!targets.length) {
    failed.value = 'unknown'
    return
  }
  if (partsScope.value !== id) failed.value = null
  try {
    const list = await Promise.all(targets.map(async a => ({ account: a, stats: await api.accountStats(a.id) })))
    // A newer request (another account, a sync) has started meanwhile: its answer wins.
    if (mine !== request) return
    parts.value = list
    partsScope.value = id
    failed.value = null
  } catch (e) {
    if (mine !== request) return
    // Statistics of this scope already on screen: they stay, the error is only reported.
    if (partsScope.value !== id) failed.value = 'error'
    showError(e)
  }
}
// The cache changes several times a second during a sync: throttle reloads.
const reloader = useReloader(load)
watch([scope, () => accounts.value.map(a => a.id).join()], () => reloader.now(), { immediate: true })
watch(tick, reloader.schedule)

/** Top addresses of several accounts: counts added up per address, the longest list's length kept. */
function mergeAddresses(lists: CountedAddress[][]): CountedAddress[] {
  const byEmail = new Map<string, CountedAddress>()
  for (const a of lists.flat()) {
    const key = a.email.toLowerCase()
    const seen = byEmail.get(key)
    if (seen) {
      seen.count += a.count
      seen.name ||= a.name
    } else byEmail.set(key, { ...a })
  }
  return [...byEmail.values()].sort((a, b) => b.count - a.count).slice(0, Math.max(0, ...lists.map(l => l.length)))
}

function merge(list: AccountStats[]): AccountStats {
  const sum = (pick: (s: AccountStats) => number) => list.reduce((n, s) => n + pick(s), 0)
  const add = (pick: (s: AccountStats) => number[], length: number) =>
    Array.from({ length }, (_, i) => list.reduce((n, s) => n + (pick(s)[i] ?? 0), 0))
  const months = new Map<string, { month: string, received: number, sent: number }>()
  for (const m of list.flatMap(s => s.months)) {
    const seen = months.get(m.month)
    if (seen) {
      seen.received += m.received
      seen.sent += m.sent
    } else months.set(m.month, { ...m })
  }
  const oldest = list.flatMap(s => (s.oldest === null ? [] : [s.oldest]))
  const newest = list.flatMap(s => (s.newest === null ? [] : [s.newest]))
  return {
    total: sum(s => s.total),
    received: sum(s => s.received),
    sent: sum(s => s.sent),
    unread: sum(s => s.unread),
    flagged: sum(s => s.flagged),
    with_attachments: sum(s => s.with_attachments),
    bulk: sum(s => s.bulk),
    bytes: sum(s => s.bytes),
    received_30d: sum(s => s.received_30d),
    sent_30d: sum(s => s.sent_30d),
    oldest: oldest.length ? Math.min(...oldest) : null,
    newest: newest.length ? Math.max(...newest) : null,
    months: [...months.values()].sort((a, b) => a.month.localeCompare(b.month)).slice(-12),
    weekdays: add(s => s.weekdays, 7),
    hours: add(s => s.hours, 24),
    top_senders: mergeAddresses(list.map(s => s.top_senders)),
    top_recipients: mergeAddresses(list.map(s => s.top_recipients)),
    folders: list.flatMap(s => s.folders)
  }
}

const stats = computed(() => {
  if (failed.value) return null
  const p = parts.value
  if (!p?.length) return null
  return p.length === 1 ? p[0]!.stats : merge(p.map(x => x.stats))
})

const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
const compact = computed(() => new Intl.NumberFormat(intlLocale(), { notation: 'compact', maximumFractionDigits: 1 }))
const percent = (part: number, whole: number) => (whole ? t('tools.stats.percent', { n: Math.round((part / whole) * 100) }) : '—')

const split = computed(() => {
  const s = stats.value
  if (!s) return []
  const whole = s.received + s.sent
  return [
    { color: 'var(--viz-series-1)', value: s.received },
    { color: 'var(--viz-series-2)', value: s.sent }
  ].filter(p => p.value > 0).map(p => ({ ...p, width: `${(p.value / whole) * 100}%` }))
})

const tiles = computed(() => {
  const s = stats.value
  if (!s) return []
  return [
    { label: t('tools.stats.received30d'), value: fmt.value.format(s.received_30d), icon: 'i-lucide-arrow-down-to-line', tone: 'bg-(--viz-series-1)/15 text-(--viz-series-1)' },
    { label: t('tools.stats.sent30d'), value: fmt.value.format(s.sent_30d), icon: 'i-lucide-send', tone: 'bg-(--viz-series-2)/15 text-(--viz-series-2)' },
    { label: t('tools.shared.unread'), value: fmt.value.format(s.unread), icon: 'i-lucide-mail', tone: 'bg-primary/10 text-primary' },
    { label: t('tools.stats.bulk'), value: percent(s.bulk, s.received), icon: 'i-lucide-newspaper', tone: 'bg-violet-100 text-violet-700 dark:bg-violet-500/20 dark:text-violet-300' },
    { label: t('tools.shared.withAttachments'), value: fmt.value.format(s.with_attachments), icon: 'i-lucide-paperclip', tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400' },
    { label: t('tools.stats.storage'), value: fileSize(s.bytes), icon: 'i-lucide-hard-drive', tone: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-400' }
  ]
})

const monthDate = (m: string) => {
  const [y, mo] = m.split('-').map(Number)
  return new Date(y!, mo! - 1, 1)
}
const months = computed(() => stats.value?.months ?? [])
const monthLabels = computed(() => months.value.map(m => dateFormat({ month: desktop.value ? 'short' : 'narrow' }).format(monthDate(m.month))))
const monthTitles = computed(() => months.value.map(m => dateFormat({ month: 'long', year: 'numeric' }).format(monthDate(m.month))))
const series = computed(() => ({
  received: { name: t('tools.stats.received'), color: 'var(--viz-series-1)' },
  sent: { name: t('tools.stats.sent'), color: 'var(--viz-series-2)' }
}))
const monthSeries = computed(() => [
  { ...series.value.received, values: months.value.map(m => m.received) },
  { ...series.value.sent, values: months.value.map(m => m.sent) }
])
const receivedSeries = (values: number[]) => [{ ...series.value.received, values }]

// Weekdays from Monday to Sunday (January 1st 2024 is a Monday).
const mondays = Array.from({ length: 7 }, (_, i) => new Date(2024, 0, 1 + i))
const weekdays = computed(() => mondays.map(d => dateFormat({ weekday: desktop.value ? 'short' : 'narrow' }).format(d)))
const weekdayNames = computed(() => mondays.map(d => dateFormat({ weekday: 'long' }).format(d)))
const hours = computed(() => Array.from({ length: 24 }, (_, h) => t('tools.stats.hour', { h })))
const hourTitles = computed(() => Array.from({ length: 24 }, (_, h) => t('tools.stats.hourRange', { from: h, to: h + 1 })))

/** Table under each chart: the values stay readable without hovering. */
const showTable = reactive({ months: false, weekdays: false, hours: false })

interface FolderRow { key: string, name: string, detail?: string, role: Role | null, to?: string, total: number, unread: number }

/** One account: its folders. All accounts: special folders added up per role (the inbox opens
 *  the unified inbox), then each account's own folders with the account's name. */
const folders = computed<FolderRow[]>(() => {
  const p = parts.value ?? []
  if (p.length === 1) {
    return p[0]!.stats.folders.map(f => ({ key: String(f.id), name: f.role ? roleLabels[f.role] : f.name, role: f.role, to: `/mail/${f.id}`, total: f.total, unread: f.unread }))
  }
  const roles = new Map<Role, FolderRow & { ids: number[] }>()
  const own: FolderRow[] = []
  for (const { account, stats: s } of p) {
    for (const f of s.folders) {
      if (!f.role) {
        own.push({ key: String(f.id), name: f.name, detail: accountName(account), role: null, to: `/mail/${f.id}`, total: f.total, unread: f.unread })
        continue
      }
      const seen = roles.get(f.role)
      if (seen) {
        seen.total += f.total
        seen.unread += f.unread
        seen.ids.push(f.id)
      } else roles.set(f.role, { key: f.role, name: roleLabels[f.role], role: f.role, total: f.total, unread: f.unread, ids: [f.id] })
    }
  }
  const merged = [...roles.values()].map(({ ids, ...row }) => ({
    ...row,
    to: ids.length === 1 ? `/mail/${ids[0]}` : row.role === 'inbox' ? '/mail/unified' : undefined
  }))
  return [...merged, ...own]
})
const folderMax = computed(() => Math.max(1, ...folders.value.map(f => f.total)))
const folderIcon = (f: FolderRow) => (f.role ? roleIcons[f.role] : 'i-lucide-folder')
const folderTone = (f: FolderRow) =>
  f.role === 'inbox'
    ? 'bg-primary/10 text-primary'
    : f.role === 'flagged'
      ? 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400'
      : 'bg-elevated text-default'

const card = 'rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none'
const tableButton = (open: boolean) => ({
  label: open ? t('tools.stats.hide') : t('tools.stats.table'),
  icon: 'i-lucide-table',
  color: 'neutral' as const,
  variant: 'outline' as const,
  size: 'sm' as const,
  class: 'rounded-lg text-muted max-lg:h-11',
  'aria-pressed': open
})
</script>

<template>
  <UDashboardPanel id="stats" :ui="{ body: 'bg-(--surface-read) p-0 sm:p-0' }">
    <template #header>
      <UDashboardNavbar :title="t('tools.stats.title')" class="max-lg:hidden">
        <template #right>
          <AccountSelect v-model="selectedAccount" :allow-all="accounts.length > 1" />
        </template>
      </UDashboardNavbar>
      <div class="flex shrink-0 items-center gap-1 border-b border-default bg-default px-2 py-2.5 lg:hidden dark:bg-(--surface-side)">
        <UButton
          icon="i-lucide-arrow-left"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl text-highlighted"
          :aria-label="t('tools.shared.back')"
          @click="back"
        />
        <h1 class="text-[17px] font-semibold text-highlighted">{{ t('tools.stats.title') }}</h1>
      </div>
    </template>

    <template #body>
      <div class="viz-root mx-auto flex w-full max-w-260 flex-col gap-4 px-4 pt-3.5 pb-8 lg:gap-6 lg:px-12 lg:pt-8 lg:pb-12 xl:px-0">
        <div class="lg:hidden">
          <AccountSelect v-model="selectedAccount" :allow-all="accounts.length > 1" size="lg" />
        </div>

        <template v-if="stats">
          <section :aria-label="t('tools.stats.title')" class="flex flex-wrap items-end gap-x-8 gap-y-4 max-lg:pt-1">
            <div class="flex min-w-0 flex-[1_1_26rem] flex-col gap-2.5">
              <p class="flex flex-wrap items-baseline gap-x-3.5 gap-y-1 text-highlighted">
                <span class="text-6xl leading-[0.9] font-bold tracking-[-0.03em] tabular-nums lg:text-7xl">
                  {{ stats.total >= 1_000_000 ? compact.format(stats.total) : fmt.format(stats.total) }}
                </span>
                <span class="text-lg font-semibold lg:text-xl">{{ t('tools.stats.messagesWord', stats.total) }}</span>
              </p>
              <p class="flex flex-wrap gap-x-4 gap-y-1 text-sm text-default">
                <span class="inline-flex items-center gap-1.75">
                  <span class="size-2.5 rounded-[3px] bg-(--viz-series-1)" />
                  {{ t('tools.stats.receivedCount', { n: fmt.format(stats.received) }, stats.received) }}
                </span>
                <span class="inline-flex items-center gap-1.75">
                  <span class="size-2.5 rounded-[3px] bg-(--viz-series-2)" />
                  {{ t('tools.stats.sentCount', { n: fmt.format(stats.sent) }, stats.sent) }}
                </span>
              </p>
              <div class="flex h-2 w-full gap-0.5 lg:w-80" aria-hidden="true">
                <span v-if="!split.length" class="h-full w-full rounded-full bg-elevated" />
                <span
                  v-for="(p, i) in split"
                  :key="p.color"
                  class="h-full"
                  :class="[i === 0 && 'rounded-s-full', i === split.length - 1 && 'rounded-e-full']"
                  :style="{ width: p.width, background: p.color }"
                />
              </div>
              <p v-if="stats.oldest" class="text-[12.5px] text-muted lg:hidden">
                {{ t('tools.stats.coverageShort', { from: shortDate(stats.oldest), to: shortDate(stats.newest!) }) }}
              </p>
            </div>
            <p
              v-if="stats.oldest"
              class="flex max-w-95 gap-2.5 rounded-xl border border-default bg-(--ui-color-neutral-50) px-3.5 py-3 text-[12.5px] leading-normal text-muted max-lg:hidden dark:bg-(--ui-color-neutral-900)/60"
            >
              <UIcon name="i-lucide-info" class="mt-px size-4 shrink-0 text-dimmed" />
              <span>{{ t('tools.stats.coverage', { from: shortDate(stats.oldest), to: shortDate(stats.newest!) }) }}</span>
            </p>
          </section>

          <section class="grid grid-cols-2 gap-2.5 sm:grid-cols-3 lg:grid-cols-6 lg:gap-3">
            <div
              v-for="tile in tiles"
              :key="tile.label"
              :class="card"
              class="flex min-w-0 items-center gap-3 px-3 py-3.5 lg:flex-col lg:items-start lg:p-4"
            >
              <span class="flex size-8.5 shrink-0 items-center justify-center rounded-[10px]" :class="tile.tone">
                <UIcon :name="tile.icon" class="size-4.25" />
              </span>
              <div class="flex min-w-0 flex-col gap-0.5">
                <span class="text-xl leading-[1.15] font-bold tabular-nums text-highlighted lg:text-2xl lg:leading-[1.1]">{{ tile.value }}</span>
                <span class="truncate text-xs text-muted lg:text-[12.5px]">{{ tile.label }}</span>
              </div>
            </div>
          </section>

          <StatsInsight :weekdays="stats.weekdays" :hours="stats.hours" class="lg:hidden" />

          <section :class="card" class="flex flex-col gap-3 px-3.5 pt-4 pb-3.5 lg:gap-3.5 lg:px-5.5 lg:pt-5 lg:pb-4.5" aria-labelledby="stats-month">
            <div class="flex flex-wrap items-center gap-x-4 gap-y-3">
              <h2 id="stats-month" class="text-[15px] font-semibold text-highlighted max-lg:flex-1 lg:text-base">{{ t('tools.stats.perMonth') }}</h2>
              <div class="flex gap-3.5 text-[12.5px] text-default max-lg:order-last max-lg:w-full">
                <span v-for="s in monthSeries" :key="s.name" class="inline-flex items-center gap-1.5">
                  <span class="size-2.5 rounded-[3px]" :style="{ background: s.color }" />
                  {{ s.name }}
                </span>
              </div>
              <div class="flex-1 max-lg:hidden" />
              <UButton v-bind="tableButton(showTable.months)" @click="showTable.months = !showTable.months" />
            </div>
            <StatsColumnChart
              :categories="monthLabels"
              :titles="monthTitles"
              :series="monthSeries"
              :height="desktop ? 250 : 180"
              y-axis
              :summary="t('tools.stats.perMonthSummary')"
            />
            <table v-if="showTable.months" class="w-full text-sm">
              <thead class="text-xs text-muted">
                <tr><th class="py-1 text-left font-medium">{{ t('tools.stats.month') }}</th><th class="text-right font-medium">{{ t('tools.stats.received') }}</th><th class="text-right font-medium">{{ t('tools.stats.sent') }}</th></tr>
              </thead>
              <tbody class="tabular-nums">
                <tr v-for="(m, i) in months" :key="m.month" class="border-t border-default">
                  <td class="py-1.5 first-letter:uppercase">{{ monthTitles[i] }}</td>
                  <td class="text-right">{{ fmt.format(m.received) }}</td>
                  <td class="text-right">{{ fmt.format(m.sent) }}</td>
                </tr>
              </tbody>
            </table>
          </section>

          <StatsInsight :weekdays="stats.weekdays" :hours="stats.hours" class="max-lg:hidden" />

          <div class="grid grid-cols-1 gap-4 lg:grid-cols-2 lg:gap-5">
            <section :class="card" class="flex flex-col gap-3 px-3.5 pt-4 pb-3.5 lg:gap-3.5 lg:px-5.5 lg:pt-5 lg:pb-4.5" aria-labelledby="stats-weekday">
              <div class="flex items-center gap-3">
                <h2 id="stats-weekday" class="flex-1 text-[15px] font-semibold text-highlighted lg:text-base">{{ t('tools.stats.perWeekday') }}</h2>
                <UButton v-bind="tableButton(showTable.weekdays)" @click="showTable.weekdays = !showTable.weekdays" />
              </div>
              <StatsColumnChart
                :categories="weekdays"
                :titles="weekdayNames"
                :series="receivedSeries(stats.weekdays)"
                :height="desktop ? 192 : 152"
                :bar-max="desktop ? 28 : 24"
                peak
                :summary="t('tools.stats.perWeekdaySummary')"
              />
              <table v-if="showTable.weekdays" class="w-full text-sm tabular-nums">
                <tbody>
                  <tr v-for="(n, i) in stats.weekdays" :key="i" class="border-t border-default">
                    <td class="py-1.5 first-letter:uppercase">{{ weekdayNames[i] }}</td><td class="text-right">{{ fmt.format(n) }}</td>
                  </tr>
                </tbody>
              </table>
            </section>

            <section :class="card" class="flex flex-col gap-3 px-3.5 pt-4 pb-3.5 lg:gap-3.5 lg:px-5.5 lg:pt-5 lg:pb-4.5" aria-labelledby="stats-hour">
              <div class="flex items-center gap-3">
                <h2 id="stats-hour" class="flex-1 text-[15px] font-semibold text-highlighted lg:text-base">{{ t('tools.stats.perHour') }}</h2>
                <UButton v-bind="tableButton(showTable.hours)" @click="showTable.hours = !showTable.hours" />
              </div>
              <StatsColumnChart
                :categories="hours"
                :titles="hourTitles"
                :series="receivedSeries(stats.hours)"
                :height="desktop ? 192 : 132"
                :label-every="6"
                label-last
                :bar-max="40"
                :fill="1"
                :summary="t('tools.stats.perHourSummary')"
              />
              <table v-if="showTable.hours" class="w-full text-sm tabular-nums">
                <tbody>
                  <tr v-for="(n, i) in stats.hours" :key="i" class="border-t border-default">
                    <td class="py-1.5">{{ hourTitles[i] }}</td><td class="text-right">{{ fmt.format(n) }}</td>
                  </tr>
                </tbody>
              </table>
            </section>
          </div>

          <div class="grid grid-cols-1 gap-4 lg:grid-cols-2 lg:gap-5">
            <section :class="card" class="flex flex-col gap-3.5 px-3.5 py-4 lg:gap-4 lg:px-5.5 lg:py-5" aria-labelledby="stats-from">
              <h2 id="stats-from" class="text-[15px] font-semibold text-highlighted lg:text-base">{{ t('tools.stats.topSenders') }}</h2>
              <p v-if="!stats.top_senders.length" class="text-sm text-muted">{{ t('tools.stats.noneReceived') }}</p>
              <StatsRankList v-else :items="stats.top_senders" color="var(--viz-series-1)" />
            </section>
            <section :class="card" class="flex flex-col gap-3.5 px-3.5 py-4 lg:gap-4 lg:px-5.5 lg:py-5" aria-labelledby="stats-to">
              <h2 id="stats-to" class="text-[15px] font-semibold text-highlighted lg:text-base">{{ t('tools.stats.topRecipients') }}</h2>
              <p v-if="!stats.top_recipients.length" class="text-sm text-muted">{{ t('tools.stats.noneSent') }}</p>
              <StatsRankList v-else :items="stats.top_recipients" color="var(--viz-series-2)" />
            </section>
          </div>

          <section :class="card" class="overflow-hidden" aria-labelledby="stats-folders">
            <h2 id="stats-folders" class="px-3.5 pt-4 pb-2.5 text-[15px] font-semibold text-highlighted lg:px-5.5 lg:pt-4.5 lg:pb-3 lg:text-base">{{ t('tools.stats.folders') }}</h2>

            <table class="w-full text-sm max-lg:hidden">
              <thead>
                <tr class="text-left text-xs text-muted">
                  <th scope="col" class="border-t border-default px-5.5 py-2 font-semibold">{{ t('tools.stats.folder') }}</th>
                  <th scope="col" class="w-2/5 border-t border-default px-3 py-2 font-semibold">{{ t('tools.stats.share') }}</th>
                  <th scope="col" class="border-t border-default px-3 py-2 text-right font-semibold">{{ t('tools.stats.messages') }}</th>
                  <th scope="col" class="border-t border-default px-5.5 py-2 text-right font-semibold">{{ t('tools.shared.unread') }}</th>
                </tr>
              </thead>
              <tbody class="tabular-nums">
                <tr v-for="f in folders" :key="f.key">
                  <td class="border-t border-default px-5.5 py-2.5">
                    <component
                      :is="f.to ? NuxtLink : 'span'"
                      :to="f.to"
                      class="inline-flex min-w-0 items-center gap-2.5 font-medium text-highlighted"
                      :class="f.to && 'hover:underline'"
                    >
                      <span class="flex size-7 shrink-0 items-center justify-center rounded-lg" :class="folderTone(f)">
                        <UIcon :name="folderIcon(f)" class="size-3.75" />
                      </span>
                      <span class="truncate">{{ f.name }}</span>
                      <span v-if="f.detail" class="truncate text-xs font-normal text-muted">{{ f.detail }}</span>
                    </component>
                  </td>
                  <td class="border-t border-default px-3 py-2.5">
                    <span class="block h-1.5 overflow-hidden rounded-full bg-elevated" aria-hidden="true">
                      <span class="block h-full rounded-full bg-(--ui-color-neutral-400) dark:bg-(--ui-color-neutral-500)" :style="{ width: `${(f.total / folderMax) * 100}%` }" />
                    </span>
                  </td>
                  <td class="border-t border-default px-3 py-2.5 text-right font-semibold text-highlighted">{{ fmt.format(f.total) }}</td>
                  <td class="border-t border-default px-5.5 py-2.5 text-right text-muted">{{ f.unread ? fmt.format(f.unread) : '—' }}</td>
                </tr>
              </tbody>
            </table>

            <ul class="lg:hidden">
              <li v-for="f in folders" :key="f.key" class="border-t border-default">
                <component :is="f.to ? NuxtLink : 'div'" :to="f.to" class="flex min-h-13 items-center gap-3 px-3.5 py-1.5">
                  <span class="flex size-7.5 shrink-0 items-center justify-center rounded-[9px]" :class="folderTone(f)">
                    <UIcon :name="folderIcon(f)" class="size-4" />
                  </span>
                  <span class="flex min-w-0 flex-1 flex-col leading-tight">
                    <span class="truncate text-[15px] text-highlighted">{{ f.name }}</span>
                    <span v-if="f.detail || f.unread" class="truncate text-xs text-muted">
                      {{ [f.detail, f.unread ? t('tools.stats.unreadCount', { n: fmt.format(f.unread) }, f.unread) : ''].filter(Boolean).join(' · ') }}
                    </span>
                  </span>
                  <span class="text-sm font-semibold tabular-nums text-highlighted">{{ fmt.format(f.total) }}</span>
                </component>
              </li>
            </ul>
          </section>
        </template>
        <UEmpty
          v-else-if="failed"
          :icon="failed === 'unknown' ? 'i-lucide-user-x' : 'i-lucide-circle-alert'"
          :title="failed === 'unknown' ? t('tools.stats.unknownAccount') : t('tools.stats.loadFailed')"
          :description="failed === 'unknown' ? t('tools.stats.unknownAccountHint') : undefined"
          :actions="[
            ...(failed === 'error' ? [{ label: t('tools.stats.retry'), icon: 'i-lucide-rotate-cw', color: 'neutral' as const, variant: 'subtle' as const, onClick: () => { reloader.now() } }] : []),
            { label: t('tools.shared.back'), icon: 'i-lucide-arrow-left', color: 'neutral' as const, variant: 'ghost' as const, onClick: back }
          ]"
          variant="naked"
          class="min-h-60"
        />
        <div v-else class="flex min-h-60 items-center justify-center text-sm text-muted">
          <UIcon name="i-lucide-loader-circle" class="me-2 size-4 animate-spin" /> {{ t('tools.stats.loading') }}
        </div>
      </div>
    </template>
  </UDashboardPanel>
</template>
