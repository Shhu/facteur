<script setup lang="ts">
import type { Account, Dashboard, FollowUp, ThreadSummary } from '~/utils/types'

const { accounts, accountById, accountColor, mailboxes, isSyncing, unifiedUnread, tick, mailboxByRole, syncAll, ownSet } = useMail()
const { enabled: perfEnabled, open: perfOpen } = usePerf()
const showError = useErrorToast()
const { t } = useI18n()

watchEffect(() => {
  if (!accounts.value.length) navigateTo('/setup', { replace: true })
})

const unread = ref<ThreadSummary[]>([])
const data = ref<Dashboard | null>(null)

const now = useNow()
const startOfDay = useStartOfDay()
/** Last load requested: an older answer arriving after it is ignored. */
let loadRequest = 0
async function load() {
  const request = ++loadRequest
  try {
    const answer = await Promise.all([
      api.unifiedThreads('unread', 8, 0),
      api.dashboard(startOfDay.value)
    ])
    if (request === loadRequest) [unread.value, data.value] = answer
  } catch (e) {
    if (request === loadRequest) showError(e)
  }
}
// The cache changes several times a second during a sync: throttle reloads.
// They also run at midnight, which resets "received today".
const reloader = useReloader(load)
reloader.now()
watch(tick, reloader.schedule)
watch(startOfDay, () => reloader.now())

const today = computed(() => dateFormat({ weekday: 'long', day: 'numeric', month: 'long' }).format(new Date(now.value)))
const compact = computed(() => new Intl.NumberFormat(intlLocale(), { notation: 'compact', maximumFractionDigits: 1 }))

const syncedLabel = useSyncedLabel()

/** The least recently synced account gives the time for all. */
const lastSync = computed(() => {
  const times = accounts.value.map(a => a.last_sync)
  return syncedLabel(times.length && !times.includes(null) ? Math.min(...(times as number[])) : null)
})

const query = ref('')
function search() {
  const q = query.value.trim()
  if (q) navigateTo({ path: '/mail/unified', query: { q } })
}

function accountUnread(a: Account) {
  return mailboxByRole('inbox', a.id)?.unread ?? 0
}

function accountInbox(a: Account) {
  const inbox = mailboxByRole('inbox', a.id)
  return inbox ? `/mail/${inbox.id}` : '/mail/unified'
}

// Counted as the sidebar counts (conversations or messages, as set), so that the numbers agree.
const { totalOf } = useFolderCounts()
const inboxTotal = computed(() => mailboxes.value.filter(m => m.role === 'inbox').reduce((n, m) => n + totalOf(m).n, 0))
const heldTotal = computed(() => mailboxes.value.filter(m => m.role === 'screener').reduce((n, m) => n + totalOf(m).n, 0))

const tiles = computed(() => {
  const received = data.value?.received_today ?? 0
  return [
    {
      label: t('tools.home.receivedToday'),
      value: received,
      hint: received ? t('tools.home.receivedHint') : t('tools.home.receivedHintNone'),
      icon: 'i-lucide-mail',
      tone: 'bg-blue-100 text-blue-600 dark:bg-blue-500/15 dark:text-blue-400',
      to: '/mail/unified'
    },
    {
      label: t('tools.shared.flagged'),
      value: data.value?.flagged_count ?? 0,
      hint: t('tools.home.pinnedHint'),
      icon: 'i-lucide-pin',
      tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400',
      // Search engine operator: never translated.
      to: { path: '/mail/unified', query: { q: 'is:pinned' } }
    },
    {
      label: t('tools.home.inInbox'),
      value: inboxTotal.value,
      hint: t('tools.home.inboxHint'),
      icon: 'i-lucide-inbox',
      tone: 'bg-(--ui-color-primary-100) text-primary dark:bg-primary/15',
      to: '/mail/unified'
    },
    {
      label: t('tools.home.screening'),
      value: heldTotal.value,
      hint: t('tools.home.screeningHint'),
      icon: 'i-lucide-user-round-search',
      tone: 'bg-violet-100 text-violet-700 dark:bg-violet-500/15 dark:text-violet-400',
      to: '/screener'
    }
  ]
})

const NuxtLink = resolveComponent('NuxtLink')
const { items: followUps, dismiss: dismissFollowUp } = useFollowUps()
const several = computed(() => accounts.value.length > 1)
/** The row is a link only when the follow-up's thread and folder are known. */
const followUpRoute = (f: FollowUp) =>
  f.thread_id !== null && f.mailbox_id !== null
    ? threadRoute({ thread_id: f.thread_id, account_id: f.account_id, mailbox_id: f.mailbox_id })
    : undefined

const { hasCalendars, eventColor } = useCalendar()
// Today and the next 7 days; the range only moves at midnight, the past is filtered out here.
const { events: upcomingRange } = useCalendarEvents(() => (hasCalendars.value ? { from: startOfDay.value, to: startOfDay.value + 8 * 86_400 } : null))
const upcoming = computed(() => (upcomingRange.value ?? [])
  .filter(e => e.end * 1000 > now.value && e.status !== 'cancelled' && e.start < now.value / 1000 + 7 * 86_400)
  .slice(0, 5))
const isToday = (e: { start: number }) => sameDay(new Date(e.start * 1000), new Date(now.value))
function upcomingWhen(e: { start: number }) {
  const d = new Date(e.start * 1000)
  const today = new Date(now.value)
  const day = sameDay(d, today) || sameDay(d, addDays(dayStart(today), 1))
    ? dayHeading(d, today)
    : dateFormat({ weekday: 'short', day: 'numeric', month: 'short' }).format(d)
  return day
}

const card = 'overflow-hidden rounded-2xl border border-default bg-default shadow-xs dark:shadow-none'
const row = 'flex gap-3 border-t border-default px-4 hover:bg-elevated/50 sm:px-5'
</script>

<template>
  <UDashboardPanel id="home" :ui="{ body: 'bg-(--ui-color-neutral-100) dark:bg-(--ui-color-neutral-950) max-lg:p-0' }">
    <template #header>
      <UDashboardNavbar :title="t('tools.home.title')" class="max-lg:hidden">
        <template #right>
          <SearchField
            v-model="query"
            :placeholder="t('tools.home.searchPlaceholder')"
            class="w-72 xl:w-85"
            @submit="search"
          >
            <template #trailing="{ empty }">
              <template v-if="empty">
                <UKbd value="meta" />
                <UKbd value="k" />
              </template>
            </template>
          </SearchField>
          <UButton :label="lastSync" color="neutral" variant="outline" :loading="isSyncing" @click="syncAll">
            <template #leading>
              <span v-if="!isSyncing" class="size-2 rounded-full bg-success" />
            </template>
            <template #trailing>
              <UIcon name="i-lucide-refresh-cw" class="size-4 text-muted" />
            </template>
          </UButton>
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <div class="mx-auto flex w-full max-w-270 flex-col gap-4 px-4 pt-6 pb-8 sm:gap-5 lg:px-0 lg:pt-2">
        <header class="flex items-end gap-2 lg:hidden">
          <div class="min-w-0 flex-1">
            <p class="text-xs font-semibold uppercase tracking-wide text-muted">{{ today }}</p>
            <h1 class="text-3xl font-bold tracking-tight text-highlighted">{{ t('tools.home.title') }}</h1>
          </div>
          <!-- Developer tools on: the diagnostics panel, by the sync button. -->
          <UButton
            v-if="perfEnabled"
            icon="i-lucide-activity"
            color="neutral"
            variant="outline"
            size="xl"
            square
            :aria-label="t('app.diagnostics.open')"
            class="rounded-xl"
            @click="perfOpen = true"
          />
          <UButton
            icon="i-lucide-refresh-cw"
            color="neutral"
            variant="outline"
            size="xl"
            square
            :loading="isSyncing"
            :aria-label="t('tools.home.sync')"
            class="relative rounded-xl"
            @click="syncAll"
          >
            <span v-if="!isSyncing" class="absolute top-2 right-2 size-2 rounded-full bg-success ring-2 ring-(--ui-bg)" />
          </UButton>
        </header>

        <section
          class="flex flex-wrap items-center gap-5 rounded-[20px] border border-(--ui-color-primary-200) bg-(--ui-color-primary-100) p-5 sm:gap-8 sm:px-8 sm:py-7 dark:border-(--ui-color-primary-900) dark:bg-(--ui-color-primary-950)"
          :aria-label="t('tools.home.title')"
        >
          <div class="flex min-w-0 flex-[1_1_26rem] flex-col gap-4">
            <p class="text-[13px] font-semibold uppercase tracking-wide text-(--ui-color-primary-700) max-lg:hidden dark:text-(--ui-color-primary-200)">
              {{ today }}
            </p>
            <NuxtLink to="/mail/unified" class="flex items-center gap-4 sm:gap-5">
              <span class="text-6xl font-bold leading-[0.9] tracking-tighter text-(--ui-color-primary-900) sm:text-7xl dark:text-(--ui-color-primary-50)">
                {{ compact.format(unifiedUnread) }}
              </span>
              <span class="flex flex-col gap-0.5">
                <span class="text-lg font-semibold text-(--ui-color-primary-900) sm:text-[22px] dark:text-(--ui-color-primary-50)">
                  {{ t('tools.home.unreadMessages', unifiedUnread) }}
                </span>
                <span class="text-[13px] text-(--ui-color-primary-700) sm:text-sm dark:text-(--ui-color-primary-200)">
                  {{ t('tools.home.inUnified') }}<span v-if="several" class="max-sm:hidden">, {{ t('tools.home.acrossAccounts', { n: accounts.length }, accounts.length) }}</span>
                </span>
              </span>
            </NuxtLink>
            <div class="flex flex-wrap gap-1.5 sm:gap-2">
              <NuxtLink
                v-for="a in accounts"
                :key="a.id"
                :to="accountInbox(a)"
                class="inline-flex items-center gap-2 rounded-full bg-white/65 py-1 ps-1 pe-3 text-xs font-medium text-(--ui-color-primary-900) sm:text-[13px] dark:bg-white/8 dark:text-(--ui-color-primary-50)"
              >
                <UAvatar
                  :src="a.avatar ?? undefined"
                  :text="initials(accountName(a))"
                  :alt="accountName(a)"
                  size="2xs"
                  class="ring-2"
                  :class="[accountColor(a.id).solid, accountColor(a.id).ring]"
                  :ui="{ fallback: 'text-inherit font-bold' }"
                />
                {{ accountName(a) }}
                <span v-if="accountUnread(a)" class="font-bold">{{ accountUnread(a) }}</span>
                <UIcon v-else name="i-lucide-check" class="size-3.5 text-(--ui-color-primary-700) dark:text-(--ui-color-primary-200)" :aria-label="t('tools.home.upToDate')" />
              </NuxtLink>
            </div>
          </div>
        </section>

        <div class="grid grid-cols-2 gap-2.5 sm:gap-4 lg:grid-cols-4">
          <NuxtLink
            v-for="tile in tiles"
            :key="tile.label"
            :to="tile.to"
            class="flex min-w-0 flex-col gap-2.5 px-3 py-3.5 hover:bg-elevated/40 md:flex-row md:items-center md:gap-4 md:px-5 md:py-4.5"
            :class="card"
          >
            <span class="flex size-9 shrink-0 items-center justify-center rounded-[11px] md:size-11.5 md:rounded-[14px]" :class="tile.tone">
              <UIcon :name="tile.icon" class="size-4.5 md:size-5.5" />
            </span>
            <span class="flex min-w-0 flex-col md:gap-0.5">
              <span class="order-2 text-xs leading-snug text-muted md:order-1 md:text-[13px]">{{ tile.label }}</span>
              <span class="order-1 text-[22px] font-bold leading-tight text-highlighted md:order-2 md:text-[26px]">{{ compact.format(tile.value) }}</span>
              <span class="order-3 truncate text-xs text-dimmed max-md:hidden">{{ tile.hint }}</span>
            </span>
          </NuxtLink>
        </div>

        <div class="grid items-start gap-4 sm:gap-5 lg:grid-cols-[minmax(0,1.65fr)_minmax(0,1fr)]">
          <div class="flex min-w-0 flex-col gap-4 sm:gap-5">
            <section :class="card" aria-labelledby="home-to-read">
              <div class="flex min-h-11 items-center gap-2.5 px-4 py-2 sm:px-5 sm:py-3">
                <h2 id="home-to-read" class="font-semibold text-highlighted">{{ t('tools.home.toRead') }}</h2>
                <UBadge v-if="unifiedUnread" :label="compact.format(unifiedUnread)" variant="soft" size="sm" class="rounded-full font-bold" />
                <UButton
                  to="/mail/unified"
                  color="primary"
                  variant="link"
                  trailing-icon="i-lucide-arrow-right"
                  class="ms-auto min-h-11 px-0 font-medium"
                >
                  <span class="sm:hidden">{{ t('tools.home.seeAll') }}</span>
                  <span class="max-sm:hidden">{{ t('tools.home.unified') }}</span>
                </UButton>
              </div>
              <div v-if="!unread.length" class="flex flex-col items-center gap-2 border-t border-default py-10 text-sm text-muted">
                <UIcon name="i-lucide-check-check" class="size-7" />
                {{ t('tools.home.nothingToRead') }}
              </div>
              <NuxtLink
                v-for="th in unread"
                :key="th.thread_id"
                :to="threadRoute(th, '/mail/unified')"
                class="items-start py-3.5"
                :class="row"
              >
                <span class="relative shrink-0">
                  <MailAvatar :address="th.from" />
                  <span class="absolute -top-0.5 -left-0.5 size-2.5 rounded-full bg-primary ring-2 ring-(--ui-bg)" />
                </span>
                <span class="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span class="flex items-baseline gap-2">
                    <span class="truncate text-[15px] font-semibold text-highlighted sm:text-sm">{{ peopleLabel(th.participants, ownSet(th.account_id)) }}</span>
                    <span v-if="th.count > 1" class="text-xs text-muted">{{ th.count }}</span>
                    <span class="ms-auto shrink-0 text-xs font-semibold text-primary">{{ shortDate(th.date) }}</span>
                  </span>
                  <span class="line-clamp-2 text-sm text-default sm:line-clamp-1">{{ th.subject || t('common.noSubject') }}</span>
                  <span v-if="several && accountById(th.account_id)" class="mt-0.5 inline-flex items-center gap-1.5 text-xs text-muted">
                    <span class="size-2 rounded-full" :class="accountColor(th.account_id).dot" />
                    {{ accountName(accountById(th.account_id)!) }}
                  </span>
                </span>
              </NuxtLink>
            </section>

            <section :class="card" aria-labelledby="home-pinned">
              <div class="flex min-h-11 items-center gap-2.5 px-4 py-2 sm:px-5 sm:py-3">
                <h2 id="home-pinned" class="font-semibold text-highlighted">{{ t('tools.home.pinned') }}</h2>
                <span
                  v-if="data?.flagged_count"
                  class="rounded-full bg-amber-100 px-2 py-0.5 text-xs font-bold text-amber-700 dark:bg-amber-500/15 dark:text-amber-400"
                >{{ compact.format(data.flagged_count) }}</span>
              </div>
              <i18n-t
                v-if="!data?.flagged.length"
                keypath="tools.home.noPinned"
                tag="p"
                scope="global"
                class="border-t border-default px-4 py-4 text-sm text-muted sm:px-5"
              >
                <template #key>
                  <UKbd value="s" />
                </template>
              </i18n-t>
              <NuxtLink
                v-for="m in data?.flagged ?? []"
                :key="m.id"
                :to="threadRoute(m)"
                class="min-h-11 items-center py-3"
                :class="row"
              >
                <span class="flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400">
                  <UIcon name="i-lucide-pin" class="size-4" />
                </span>
                <span class="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span class="truncate text-sm font-medium text-highlighted">{{ m.subject || t('common.noSubject') }}</span>
                  <span class="truncate text-xs text-muted">{{ addressLabel(m.from) }} · {{ shortDate(m.date) }}</span>
                </span>
                <UIcon name="i-lucide-chevron-right" class="size-4 shrink-0 text-dimmed max-sm:hidden" />
              </NuxtLink>
            </section>
          </div>

          <div class="flex min-w-0 flex-col gap-4 sm:gap-5">
            <section v-if="hasCalendars" :class="card" aria-labelledby="home-upcoming">
              <div class="flex min-h-11 items-center gap-2.5 px-4 py-2 sm:px-5 sm:py-3">
                <h2 id="home-upcoming" class="font-semibold text-highlighted">{{ t('tools.home.upcoming') }}</h2>
                <UButton
                  to="/calendar"
                  color="primary"
                  variant="link"
                  trailing-icon="i-lucide-arrow-right"
                  class="ms-auto min-h-11 px-0 font-medium"
                  :label="t('tools.home.calendar')"
                />
              </div>
              <p v-if="upcomingRange && !upcoming.length" class="border-t border-default px-4 py-4 text-sm text-muted sm:px-5">
                {{ t('tools.home.noUpcoming') }}
              </p>
              <NuxtLink
                v-for="e in upcoming"
                :key="`${e.id}-${e.start}`"
                to="/calendar"
                class="min-h-11 items-stretch py-3"
                :class="row"
              >
                <span class="w-1 shrink-0 rounded-full" :style="{ background: eventColor(e) }" />
                <span class="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span class="truncate text-sm font-medium text-highlighted">{{ e.summary || t('tools.calendar.untitled') }}</span>
                  <span class="truncate text-xs text-muted">
                    <span class="font-semibold" :class="isToday(e) ? 'text-primary' : 'text-toned'">{{ upcomingWhen(e) }}</span> · {{ eventTime(e) }}<template v-if="eventPlace(e)"> · {{ eventPlace(e) }}</template>
                  </span>
                </span>
              </NuxtLink>
            </section>

            <section v-if="followUps.length" :class="card" aria-labelledby="home-no-reply">
              <h2 id="home-no-reply" class="flex min-h-11 items-center px-4 py-2 font-semibold text-highlighted sm:px-5 sm:py-3">
                {{ t('tools.home.noReply') }}
              </h2>
              <component
                :is="followUpRoute(f) ? NuxtLink : 'div'"
                v-for="f in followUps"
                :key="f.id"
                :to="followUpRoute(f)"
                class="items-center py-1.5"
                :class="row"
              >
                <UIcon name="i-lucide-bell-ring" class="size-4 shrink-0 text-primary" />
                <span class="flex min-w-0 flex-1 flex-col">
                  <span class="truncate text-sm text-highlighted">{{ f.subject || t('common.noSubject') }}</span>
                  <span class="truncate text-xs text-muted">{{ t('common.sentTo', { to: f.to.join(', ') || '…', ago: ago(f.sent_at) }) }}</span>
                </span>
                <UButton
                  icon="i-lucide-x"
                  color="neutral"
                  variant="ghost"
                  class="size-11 justify-center"
                  :aria-label="t('tools.home.dismiss')"
                  @click.prevent.stop="dismissFollowUp(f.id)"
                />
              </component>
            </section>
          </div>
        </div>
      </div>
    </template>
  </UDashboardPanel>
</template>
