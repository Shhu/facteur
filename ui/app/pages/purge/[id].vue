<script setup lang="ts">
import { useMediaQuery } from '@vueuse/core'
import type { CountedAddress, FolderSummary, MessageSummary } from '~/utils/types'

/** Rows of the compact list: beyond that the remaining count is enough, and rendering stays light. */
const LIST = 200
/** While downloading, the summary (costly to compute) is reread only every N batches. */
const SUMMARY_EVERY = 5
const PREVIEW = 6
const route = useRoute()
const { accountById, accountColor, mailboxes, mailboxByRole } = useMail()
const showError = useErrorToast()
const listStale = useListStale()
const toast = useToast()
const { t } = useI18n()
const desktop = useMediaQuery('(min-width: 1024px)')
const back = useScreenBack()

const mailboxId = computed(() => Number(route.params.id))
const summary = ref<FolderSummary | null>(null)
/** The folder could not be read at all (deleted, invalid id): only when nothing is shown yet. */
const failed = ref(false)
const filter = ref<CountedAddress | null>(null)
const senderMessages = ref<MessageSummary[] | null>(null)
const expanded = ref(false)

/** Last read requested: an older answer arriving after it is ignored. */
let loadRequest = 0
async function load() {
  const request = ++loadRequest
  try {
    const s = await api.folderSummary(mailboxId.value, LIST)
    if (request !== loadRequest) return
    summary.value = s
    if (filter.value) loadSender()
  } catch (e) {
    if (request !== loadRequest) return
    if (!summary.value) failed.value = true
    showError(e, summary.value ? undefined : t('tools.folder.notFound'))
  }
}
watch(mailboxId, (id) => {
  summary.value = null
  filter.value = null
  expanded.value = false
  failed.value = !Number.isInteger(id)
  if (!failed.value) load()
}, { immediate: true })

/** Known before the summary arrives (the folder list), so the title is right from the start. */
const role = computed(() => summary.value?.mailbox.role ?? mailboxes.value.find(m => m.id === mailboxId.value)?.role)
const isTrash = computed(() => role.value !== 'junk')
const title = computed(() => t(isTrash.value ? 'common.purge.titleTrash' : 'common.purge.titleJunk'))
const folderName = computed(() => roleLabels[isTrash.value ? 'trash' : 'junk'])
const folderIcon = computed(() => roleIcons[isTrash.value ? 'trash' : 'junk'])
const account = computed(() => (summary.value ? accountById(summary.value.mailbox.account_id) : null))
const color = computed(() => accountColor(summary.value?.mailbox.account_id ?? 0))
const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
/** Messages on the server when known, else those in the cache. */
const total = computed(() => summary.value?.server_count ?? summary.value?.local ?? 0)
const missing = computed(() => {
  const s = summary.value
  return s && s.server_count !== null ? Math.max(0, s.server_count - s.local) : 0
})

const longDay = (ts: number) => dateFormat({ day: 'numeric', month: 'long', year: 'numeric' }).format(new Date(ts * 1000))
const isToday = (ts: number) => new Date(ts * 1000).toDateString() === new Date().toDateString()

const period = computed(() => {
  const s = summary.value
  if (!s?.oldest || !s.newest) return ''
  if (isToday(s.newest)) {
    const time = dateFormat({ hour: '2-digit', minute: '2-digit' }).format(new Date(s.newest * 1000))
    return t('common.purge.periodToday', { from: longDay(s.oldest), time })
  }
  return t('common.purge.period', { from: longDay(s.oldest), to: longDay(s.newest) })
})

const provider = computed(() => {
  const a = account.value
  if (summary.value?.gmail || (a?.auth.kind === 'oauth2' && a.auth.provider === 'google')) return 'Gmail'
  if (!a) return ''
  if (a.auth.kind === 'oauth2' && a.auth.provider === 'microsoft') return 'Outlook'
  return a.imap.host.replace(/^(imaps?|mail)\./i, '')
})
const warningDetail = computed(() => (provider.value
  ? t('common.purge.warningDetail', { n: fmt.value.format(total.value), provider: provider.value }, total.value)
  : t('common.purge.warningShort')))

/** Same tones as the statistics tiles; the space is what emptying gains on the server. */
const tiles = computed(() => {
  const s = summary.value
  if (!s) return []
  return [
    { key: 'unread', value: fmt.value.format(s.unread), label: t('common.purge.unread', s.unread), icon: 'i-lucide-mail', tone: 'bg-teal-100 text-teal-700 dark:bg-teal-400/14 dark:text-teal-400' },
    { key: 'attachments', value: fmt.value.format(s.with_attachments), label: t('common.purge.withAttachments', s.with_attachments), icon: 'i-lucide-paperclip', tone: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-400' },
    { key: 'space', value: fileSize(s.bytes), label: t(desktop.value ? 'common.purge.freed' : 'common.purge.freedShort'), icon: 'i-lucide-hard-drive', tone: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-400' }
  ]
})

const senders = computed(() => summary.value?.top_senders ?? [])
const senderMax = computed(() => Math.max(1, ...senders.value.map(s => s.count)))
/** Bars only when the counts differ: equal ones would all fill the width for nothing. */
const senderBars = computed(() => new Set(senders.value.map(s => s.count)).size > 1)
const filterName = computed(() => (filter.value ? filter.value.name || filter.value.email : ''))
const isActive = (s: CountedAddress) => filter.value?.email === s.email

let senderRequest = 0
async function loadSender() {
  const request = ++senderRequest
  const sender = filter.value
  if (!sender) return
  try {
    const list = await api.folderMessagesFrom(mailboxId.value, sender.email, LIST)
    if (request === senderRequest) senderMessages.value = list
  } catch (e) {
    if (request !== senderRequest) return
    showError(e)
    filter.value = null
  }
}
watch(() => filter.value?.email, () => {
  senderMessages.value = null
  senderRequest++
  if (filter.value) loadSender()
})

function toggleSender(s: CountedAddress) {
  filter.value = isActive(s) ? null : s
}

const listSection = useTemplateRef<HTMLElement>('listSection')
async function openSender(s: CountedAddress) {
  filter.value = s
  expanded.value = true
  await nextTick()
  listSection.value?.scrollIntoView({ block: 'start', behavior: 'smooth' })
}

const shown = computed(() => (filter.value ? senderMessages.value : summary.value?.recent) ?? [])
const more = computed(() => (filter.value ? filter.value.count : summary.value?.local ?? 0) - shown.value.length)
const countLabel = computed(() => (filter.value
  ? t('common.purge.countSender', { n: fmt.value.format(filter.value.count) }, filter.value.count)
  : t('common.purge.countAll', { n: fmt.value.format(total.value) }, total.value)))

interface Row { key: number, message: MessageSummary, n: number, attachments: boolean }
interface Day { key: string, label: string, rows: Row[] }

const dayKey = (ts: number) => new Date(ts * 1000).toDateString()
const senderKey = (m: MessageSummary) => m.from?.email.toLowerCase() ?? ''

/** Consecutive messages of a day from the same sender with the same subject share a row. */
const rows = computed(() => {
  const out: Row[] = []
  for (const m of shown.value) {
    const last = out.at(-1)
    if (last && senderKey(last.message) === senderKey(m) && last.message.subject === m.subject && dayKey(last.message.date) === dayKey(m.date)) {
      last.n++
      last.attachments ||= m.has_attachments
    } else out.push({ key: m.id, message: m, n: 1, attachments: m.has_attachments })
  }
  return out
})

function dayLabel(ts: number): string {
  const d = new Date(ts * 1000)
  if (isToday(ts)) return t('common.date.today')
  const sameYear = d.getFullYear() === new Date().getFullYear()
  const label = dateFormat({ weekday: 'long', day: 'numeric', month: 'long', year: sameYear ? undefined : 'numeric' }).format(d)
  return label.charAt(0).toUpperCase() + label.slice(1)
}

const days = computed(() => {
  const out: Day[] = []
  for (const row of rows.value) {
    const key = dayKey(row.message.date)
    const last = out.at(-1)
    if (last?.key === key) last.rows.push(row)
    else out.push({ key, label: dayLabel(row.message.date), rows: [row] })
  }
  return out
})

const downloading = ref(false)
let stopped = false
async function downloadMissing() {
  downloading.value = true
  stopped = false
  try {
    let batches = 0
    while (!stopped && await api.loadOlder(mailboxId.value) > 0) {
      if (++batches % SUMMARY_EVERY === 0) await load()
    }
  } catch (e) {
    showError(e, t('common.purge.downloadFailed'))
  } finally {
    downloading.value = false
    await load()
  }
}
function stopDownload() {
  stopped = true
}
onBeforeUnmount(stopDownload)

const emptying = ref(false)
async function empty() {
  const s = summary.value
  if (!s || emptying.value) return
  emptying.value = true
  stopped = true
  try {
    await api.emptyFolder(s.mailbox.id)
    listStale.bump()
    toast.add({ title: t('common.purge.done', { n: total.value }, total.value), icon: 'i-lucide-trash-2' })
    const inbox = mailboxByRole('inbox', s.mailbox.account_id)
    await navigateTo(inbox ? `/mail/${inbox.id}` : '/', { replace: true })
  } catch (e) {
    showError(e, t('common.purge.failed'))
  } finally {
    emptying.value = false
  }
}

const card = 'border border-default bg-(--surface-card) shadow-xs dark:shadow-none'
const dangerWash = 'border-red-200 bg-red-50 dark:border-red-500/30 dark:bg-red-500/7'
const dangerIcon = 'bg-red-100 text-red-700 dark:bg-red-500/16 dark:text-red-300'
/** With `btn-danger` (main.css): solid red in both themes. */
const confirmButton = computed(() => ({
  label: t('common.purge.confirm', { n: fmt.value.format(total.value) }, total.value),
  icon: 'i-lucide-trash-2',
  color: 'error' as const,
  loading: emptying.value,
  disabled: !total.value || downloading.value
}))
</script>

<template>
  <UDashboardPanel id="purge" :ui="{ body: 'bg-(--surface-read) p-0 sm:p-0' }">
    <template #header>
      <UDashboardNavbar :title="title" class="max-lg:hidden">
        <template #leading>
          <UButton icon="i-lucide-arrow-left" color="neutral" variant="ghost" :aria-label="t('tools.shared.back')" @click="back" />
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
        <h1 class="text-[17px] font-semibold text-highlighted">{{ title }}</h1>
      </div>
    </template>

    <template #body>
      <UEmpty
        v-if="failed"
        icon="i-lucide-folder-x"
        :title="t('tools.folder.notFound')"
        :description="t('tools.shared.unavailableHint')"
        :actions="[{ label: t('tools.shared.back'), icon: 'i-lucide-arrow-left', color: 'neutral', variant: 'subtle', onClick: back }]"
        variant="naked"
        class="h-full"
      />
      <div v-else-if="!summary" class="flex h-full items-center justify-center text-sm text-muted">
        <UIcon name="i-lucide-loader-circle" class="me-2 size-4 animate-spin" /> {{ t('common.purge.loading') }}
      </div>

      <div v-else-if="desktop" class="mx-auto flex h-full w-full max-w-265 flex-col gap-4.5 px-8 pt-6 pb-4.5 xl:px-0">
        <section :aria-label="title" class="flex items-center gap-4.5 rounded-[18px] border px-6 py-5" :class="dangerWash">
          <span class="flex size-14 shrink-0 items-center justify-center rounded-2xl" :class="dangerIcon">
            <UIcon :name="folderIcon" class="size-6.5" />
          </span>
          <div class="flex min-w-0 flex-1 flex-col gap-1.5">
            <div class="flex min-w-0 items-center gap-2.5">
              <span class="text-lg font-bold text-highlighted">{{ folderName }}</span>
              <span v-if="account" class="inline-flex min-w-0 items-center gap-1.5 rounded-full py-0.5 ps-0.75 pe-2.5 text-[12.5px] font-semibold" :class="color.soft">
                <img v-if="account.avatar" :src="account.avatar" alt="" class="size-5 shrink-0 rounded-full object-cover">
                <span v-else class="flex size-5 shrink-0 items-center justify-center rounded-full text-[9px] font-bold" :class="color.solid">{{ initials(accountName(account)) }}</span>
                <span class="truncate">{{ accountName(account) }}</span>
              </span>
            </div>
            <span v-if="period" class="text-[13px] text-muted">{{ period }}</span>
          </div>
          <p class="flex shrink-0 flex-col items-end">
            <span class="text-[56px] leading-[0.95] font-bold tracking-[-0.03em] tabular-nums text-highlighted">{{ fmt.format(total) }}</span>
            <span class="text-[13px] text-muted">{{ t('common.purge.messages', total) }}</span>
          </p>
        </section>

        <UAlert
          v-if="missing > 0"
          color="warning"
          variant="subtle"
          icon="i-lucide-cloud-download"
          class="shrink-0"
          :title="t('common.purge.missingTitle', { n: fmt.format(missing) }, missing)"
          :description="downloading
            ? t('common.purge.downloading', { local: fmt.format(summary.local), server: fmt.format(summary.server_count ?? 0) })
            : t('common.purge.missing', { server: fmt.format(summary.server_count ?? 0), local: fmt.format(summary.local) })"
          :actions="downloading
            ? [{ label: t('common.purge.stop'), color: 'neutral', variant: 'outline', onClick: stopDownload }]
            : [{ label: t('common.purge.download', { n: fmt.format(missing) }, missing), color: 'warning', variant: 'solid', icon: 'i-lucide-download', onClick: downloadMissing }]"
        />

        <template v-if="summary.local">
          <section class="grid shrink-0 grid-cols-3 gap-3">
            <div v-for="tile in tiles" :key="tile.key" :class="card" class="flex min-w-0 items-center gap-3.5 rounded-2xl px-4 py-3.5">
              <span class="flex size-9.5 shrink-0 items-center justify-center rounded-[11px]" :class="tile.tone">
                <UIcon :name="tile.icon" class="size-4.5" />
              </span>
              <div class="flex min-w-0 flex-col">
                <span class="text-[22px] leading-[1.15] font-bold tabular-nums text-highlighted">{{ tile.value }}</span>
                <span class="truncate text-[12.5px] text-muted">{{ tile.label }}</span>
              </div>
            </div>
          </section>

          <div class="flex min-h-72 flex-1 gap-4">
            <section aria-labelledby="purge-senders" :class="card" class="flex w-85 shrink-0 flex-col overflow-hidden rounded-2xl">
              <div class="flex flex-col gap-0.5 px-4.5 pt-4 pb-2">
                <h2 id="purge-senders" class="text-[15px] font-semibold text-highlighted">{{ t('common.purge.topSenders') }}</h2>
                <span class="text-xs text-muted">{{ t('common.purge.sendersHint') }}</span>
              </div>
              <div class="flex min-h-0 flex-col gap-0.5 overflow-y-auto px-2 pt-1 pb-2.5">
                <button
                  v-for="s in senders"
                  :key="s.email"
                  type="button"
                  :aria-pressed="isActive(s)"
                  :title="s.email"
                  class="flex w-full items-center gap-3 rounded-xl px-2.5 py-2.25 text-start transition-colors"
                  :class="isActive(s) ? 'bg-primary/9 ring-1 ring-inset ring-primary/35' : 'hover:bg-elevated/60'"
                  @click="toggleSender(s)"
                >
                  <MailAvatar :address="{ name: s.name, email: s.email }" :account-id="summary.mailbox.account_id" class="size-8.5! text-[11.5px]!" />
                  <span class="flex min-w-0 flex-1 flex-col gap-1.25">
                    <span class="flex min-w-0 items-baseline gap-2">
                      <span class="min-w-0 flex-1 truncate text-[13.5px] font-semibold text-highlighted">{{ s.name || s.email }}</span>
                      <span class="shrink-0 text-[13px] font-bold tabular-nums text-highlighted">{{ fmt.format(s.count) }}</span>
                    </span>
                    <span v-if="senderBars" class="block h-1.5 overflow-hidden rounded-full bg-elevated" aria-hidden="true">
                      <span class="block h-full rounded-full bg-(--viz-series-1)" :style="{ width: `${(s.count / senderMax) * 100}%`, minWidth: '4px' }" />
                    </span>
                  </span>
                </button>
              </div>
            </section>

            <section aria-labelledby="purge-list" :class="card" class="flex min-w-0 flex-1 flex-col overflow-hidden rounded-2xl">
              <div class="flex min-w-0 items-center gap-2.5 px-4.5 pt-3.5 pb-2.5">
                <h2 id="purge-list" class="text-[15px] font-semibold text-highlighted">{{ t('common.purge.list') }}</h2>
                <button
                  v-if="filter"
                  type="button"
                  class="inline-flex h-6.5 min-w-0 items-center gap-1.5 rounded-full bg-primary/14 ps-2.5 pe-1.5 text-[12.5px] font-semibold text-primary"
                  :aria-label="t('common.purge.clearFilter', { name: filterName })"
                  @click="filter = null"
                >
                  <span class="truncate">{{ filterName }}</span>
                  <UIcon name="i-lucide-x" class="size-3.25 shrink-0" />
                </button>
                <span class="ms-auto shrink-0 text-[12.5px] text-muted">{{ countLabel }}</span>
              </div>
              <div class="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
                <div v-if="filter && !senderMessages" class="flex items-center justify-center py-8 text-sm text-muted">
                  <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" />
                </div>
                <template v-for="day in days" :key="day.key">
                  <h3 class="px-2.5 pt-2.5 pb-1 text-[11px] font-bold tracking-[0.06em] text-muted uppercase">{{ day.label }}</h3>
                  <div v-for="row in day.rows" :key="row.key" class="flex items-center gap-3 rounded-[10px] px-2.5 py-1.75">
                    <MailAvatar :address="row.message.from" :account-id="row.message.account_id" class="size-7.5! text-[10.5px]!" />
                    <span class="w-47.5 shrink-0 truncate text-[13.5px] font-medium text-highlighted">{{ addressLabel(row.message.from) }}</span>
                    <span class="min-w-0 flex-1 truncate text-[13.5px] text-default">{{ row.message.subject || t('common.noSubject') }}</span>
                    <span v-if="row.n > 1" class="shrink-0 rounded-full bg-elevated px-1.75 py-px text-[11.5px] font-bold text-muted">
                      <span aria-hidden="true">×{{ row.n }}</span><span class="sr-only">{{ t('common.purge.grouped', { n: row.n }, row.n) }}</span>
                    </span>
                    <UIcon v-if="row.attachments" name="i-lucide-paperclip" class="size-3.5 shrink-0 text-dimmed" />
                    <span class="min-w-12 shrink-0 text-end text-xs tabular-nums text-muted">{{ shortDate(row.message.date) }}</span>
                  </div>
                </template>
                <p v-if="more > 0 && (!filter || senderMessages)" class="px-2.5 py-1.5 text-xs text-muted">
                  {{ t('common.purge.more', { n: fmt.format(more) }, more) }}
                </p>
              </div>
            </section>
          </div>
        </template>
        <p v-else-if="!total" :class="card" class="rounded-2xl p-6 text-center text-sm text-muted">{{ t('common.purge.empty') }}</p>
      </div>

      <div v-else class="flex flex-col gap-3.5 px-4 pt-4 pb-6">
        <section :aria-label="title" class="flex flex-col gap-3 rounded-[18px] border p-4.5" :class="dangerWash">
          <div class="flex items-center gap-3">
            <span class="flex size-11 shrink-0 items-center justify-center rounded-[13px]" :class="dangerIcon">
              <UIcon :name="folderIcon" class="size-5.5" />
            </span>
            <div class="flex min-w-0 flex-1 flex-col gap-1">
              <span class="flex min-w-0 items-center gap-2 text-base font-bold text-highlighted">
                {{ folderName }}
                <span v-if="account" class="inline-flex min-w-0 items-center gap-1.25 rounded-full py-px ps-0.75 pe-2 text-xs font-semibold" :class="color.soft">
                  <img v-if="account.avatar" :src="account.avatar" alt="" class="size-4.5 shrink-0 rounded-full object-cover">
                  <span v-else class="flex size-4.5 shrink-0 items-center justify-center rounded-full text-[8px] font-bold" :class="color.solid">{{ initials(accountName(account)) }}</span>
                  <span class="truncate">{{ accountName(account) }}</span>
                </span>
              </span>
              <span v-if="summary.oldest" class="text-[12.5px] text-muted">{{ t('common.purge.since', { from: longDay(summary.oldest) }) }}</span>
            </div>
          </div>
          <p class="flex items-baseline gap-2.5 text-highlighted">
            <span class="text-[52px] leading-[0.95] font-bold tracking-[-0.03em] tabular-nums">{{ fmt.format(total) }}</span>
            <span class="text-base font-semibold">{{ t('common.purge.messages', total) }}</span>
          </p>
        </section>

        <UAlert
          v-if="missing > 0"
          color="warning"
          variant="subtle"
          icon="i-lucide-cloud-download"
          :title="t('common.purge.missingTitle', { n: fmt.format(missing) }, missing)"
          :description="downloading
            ? t('common.purge.downloading', { local: fmt.format(summary.local), server: fmt.format(summary.server_count ?? 0) })
            : t('common.purge.missing', { server: fmt.format(summary.server_count ?? 0), local: fmt.format(summary.local) })"
          :actions="downloading
            ? [{ label: t('common.purge.stop'), color: 'neutral', variant: 'outline', onClick: stopDownload }]
            : [{ label: t('common.purge.download', { n: fmt.format(missing) }, missing), color: 'warning', variant: 'solid', icon: 'i-lucide-download', onClick: downloadMissing }]"
        />

        <template v-if="summary.local">
          <section class="grid grid-cols-3 gap-2">
            <div v-for="tile in tiles" :key="tile.key" :class="card" class="flex min-w-0 flex-col gap-2 rounded-[14px] p-3">
              <span class="flex size-7.5 items-center justify-center rounded-[9px]" :class="tile.tone">
                <UIcon :name="tile.icon" class="size-4" />
              </span>
              <span class="flex min-w-0 flex-col">
                <span class="text-lg leading-[1.15] font-bold tabular-nums text-highlighted">{{ tile.value }}</span>
                <span class="text-[11.5px] leading-[1.3] text-muted">{{ tile.label }}</span>
              </span>
            </div>
          </section>

          <section v-if="senders.length" aria-labelledby="purge-senders-m" :class="card" class="overflow-hidden rounded-2xl">
            <h2 id="purge-senders-m" class="px-3.5 pt-3.5 pb-2 text-[15px] font-semibold text-highlighted">{{ t('common.purge.topSenders') }}</h2>
            <ul>
              <li v-for="s in senders" :key="s.email" class="border-t border-default">
                <button type="button" class="flex min-h-13 w-full items-center gap-3 px-3.5 py-1.5 text-start active:bg-elevated/60" @click="openSender(s)">
                  <MailAvatar :address="{ name: s.name, email: s.email }" :account-id="summary.mailbox.account_id" class="size-8! text-[11px]!" />
                  <span class="flex min-w-0 flex-1 flex-col gap-1.25">
                    <span class="truncate text-sm font-semibold text-highlighted">{{ s.name || s.email }}</span>
                    <span v-if="senderBars" class="block h-1.25 overflow-hidden rounded-full bg-elevated" aria-hidden="true">
                      <span class="block h-full rounded-full bg-(--viz-series-1)" :style="{ width: `${(s.count / senderMax) * 100}%`, minWidth: '4px' }" />
                    </span>
                  </span>
                  <span class="shrink-0 text-sm font-bold tabular-nums text-highlighted">{{ fmt.format(s.count) }}</span>
                  <UIcon name="i-lucide-chevron-right" class="size-4 shrink-0 text-dimmed" />
                </button>
              </li>
            </ul>
          </section>

          <section ref="listSection" aria-labelledby="purge-list-m" :class="card" class="scroll-mt-4 overflow-hidden rounded-2xl">
            <div class="flex min-w-0 items-center gap-2 px-3.5 pt-3.5 pb-2">
              <h2 id="purge-list-m" class="shrink-0 text-[15px] font-semibold text-highlighted">
                {{ t('common.purge.list') }}<span v-if="!expanded" class="font-normal text-muted"> · {{ t('common.purge.recent') }}</span>
              </h2>
              <button
                v-if="filter"
                type="button"
                class="inline-flex h-8 min-w-0 items-center gap-1.5 rounded-full bg-primary/14 ps-3 pe-2 text-[13px] font-semibold text-primary"
                :aria-label="t('common.purge.clearFilter', { name: filterName })"
                @click="filter = null"
              >
                <span class="truncate">{{ filterName }}</span>
                <UIcon name="i-lucide-x" class="size-3.5 shrink-0" />
              </button>
            </div>
            <p v-if="expanded" class="px-3.5 pb-2 text-[12.5px] text-muted">{{ countLabel }}</p>
            <div v-if="filter && !senderMessages" class="flex items-center justify-center border-t border-default py-8 text-sm text-muted">
              <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" />
            </div>
            <template v-for="day in (expanded ? days : [{ key: 'preview', label: '', rows: rows.slice(0, PREVIEW) }])" :key="day.key">
              <h3 v-if="day.label" class="border-t border-default px-3.5 pt-2.5 pb-1 text-[11px] font-bold tracking-[0.06em] text-muted uppercase">{{ day.label }}</h3>
              <div v-for="row in day.rows" :key="row.key" class="flex min-h-14 items-center gap-3 border-t border-default px-3.5 py-1.5">
                <MailAvatar :address="row.message.from" :account-id="row.message.account_id" class="size-8! text-[11px]!" />
                <div class="flex min-w-0 flex-1 flex-col gap-px">
                  <div class="flex min-w-0 items-baseline gap-2">
                    <span class="min-w-0 flex-1 truncate text-[13.5px] font-semibold text-highlighted">{{ addressLabel(row.message.from) }}</span>
                    <span class="shrink-0 text-[11.5px] tabular-nums text-muted">{{ shortDate(row.message.date) }}</span>
                  </div>
                  <div class="flex min-w-0 items-center gap-1.5">
                    <span class="min-w-0 flex-1 truncate text-[13px] text-default">{{ row.message.subject || t('common.noSubject') }}</span>
                    <UIcon v-if="row.attachments" name="i-lucide-paperclip" class="size-3.5 shrink-0 text-dimmed" />
                    <span v-if="row.n > 1" class="shrink-0 rounded-full bg-elevated px-1.5 text-[11px] font-bold text-muted">
                      <span aria-hidden="true">×{{ row.n }}</span><span class="sr-only">{{ t('common.purge.grouped', { n: row.n }, row.n) }}</span>
                    </span>
                  </div>
                </div>
              </div>
            </template>
            <button
              v-if="!expanded && (rows.length > PREVIEW || more > 0)"
              type="button"
              class="h-12 w-full border-t border-default text-sm font-semibold text-primary"
              @click="expanded = true"
            >
              {{ t('common.purge.seeAll', { n: fmt.format(filter ? filter.count : total) }, filter ? filter.count : total) }}
            </button>
            <p v-else-if="expanded && more > 0 && (!filter || senderMessages)" class="border-t border-default px-3.5 py-2.5 text-xs text-muted">
              {{ t('common.purge.more', { n: fmt.format(more) }, more) }}
            </p>
          </section>
        </template>
        <p v-else-if="!total" :class="card" class="rounded-2xl p-6 text-center text-sm text-muted">{{ t('common.purge.empty') }}</p>
      </div>
    </template>

    <template #footer>
      <template v-if="summary">
        <div v-if="desktop" class="bg-(--surface-read) px-8 pb-6 xl:px-0">
          <div role="alert" class="mx-auto flex w-full max-w-265 items-center gap-3.5 rounded-2xl border py-3.5 ps-4.5 pe-4" :class="dangerWash">
            <span class="flex size-9 shrink-0 items-center justify-center rounded-[10px]" :class="dangerIcon">
              <UIcon name="i-lucide-triangle-alert" class="size-4.5" />
            </span>
            <div class="flex min-w-0 flex-1 flex-col gap-px">
              <span class="text-sm font-semibold text-highlighted">{{ t('common.purge.warningTitle') }}</span>
              <span class="text-[12.5px] text-muted">{{ warningDetail }}</span>
            </div>
            <UButton
              :label="t('common.cancel')"
              color="neutral"
              variant="outline"
              class="h-10.5 rounded-[11px] px-4 text-sm"
              @click="back"
            />
            <UButton
              v-bind="confirmButton"
              class="h-10.5 rounded-[11px] px-4.5 text-sm font-semibold btn-danger"
              @click="empty"
            />
          </div>
        </div>
        <div
          v-else
          role="alert"
          class="flex flex-col gap-2.5 border-t border-red-200 bg-default px-4 pt-3.5 pb-[calc(1.25rem+env(safe-area-inset-bottom))] shadow-[0_-8px_24px_rgba(0,0,0,0.12)] dark:border-red-500/30 dark:bg-(--surface-side)"
        >
          <p class="flex items-start gap-2.5 text-[13px] leading-[1.4] text-default">
            <UIcon name="i-lucide-triangle-alert" class="mt-px size-4.5 shrink-0 text-red-700 dark:text-red-300" />
            <span><b class="font-semibold text-highlighted">{{ t('common.purge.warningTitle') }}.</b> {{ t('common.purge.warningShort') }}</span>
          </p>
          <div class="flex gap-2">
            <UButton
              :label="t('common.cancel')"
              color="neutral"
              variant="outline"
              class="h-12.5 rounded-[14px] px-4.5 text-[15px]"
              @click="back"
            />
            <UButton
              v-bind="confirmButton"
              block
              class="h-12.5 flex-1 rounded-[14px] text-[15px] font-semibold btn-danger"
              @click="empty"
            />
          </div>
        </div>
      </template>
    </template>
  </UDashboardPanel>
</template>
