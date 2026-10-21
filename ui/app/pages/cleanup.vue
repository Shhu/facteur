<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import { useMediaQuery } from '@vueuse/core'
import type { MessageSummary, SimilarCriteria, SimilarFacets, SimilarResult } from '~/utils/types'
import type { SimilarAction } from '~/components/SimilarActionPicker.vue'
import { folderLabel } from '~/components/SimilarFolderMenu.vue'

const route = useRoute()
const router = useRouter()
const { mailboxes } = useMail()
const showError = useErrorToast()
const listStale = useListStale()
const { done } = useUndo()
const { t } = useI18n()
/** Desktop: criteria and results side by side; below `lg`, one column and a bottom bar. */
const desktop = useMediaQuery('(min-width: 1024px)')

const messageId = computed(() => Number(route.query.message))
const facets = ref<SimilarFacets | null>(null)
const reference = ref<MessageSummary | null>(null)

const accountId = ref<number | null>(null)
type FromMode = 'address' | 'domain'
const fromOn = ref(true)
const fromMode = ref<FromMode>('address')
const toOn = ref(false)
const toAddress = ref<string | undefined>()
const subjectOn = ref(false)
const subject = ref('')
const bodyOn = ref(false)
const body = ref('')
const linksOn = ref(false)
const domains = ref<string[]>([])
const listOn = ref(false)
const bulkOn = ref(false)
/** Also include the other messages of the matching conversations (replies to a sender). */
const threadsOn = ref(false)
/** Period in date-input format (YYYY-MM-DD); empty: unbounded. */
const dateFrom = ref('')
const dateTo = ref('')
/** Start of the day (min) or end of the day (max), local time. */
function day(value: string, end: boolean): number | null {
  if (!value) return null
  const [y, m, d] = value.split('-').map(Number)
  const t = end ? new Date(y!, m! - 1, d! + 1).getTime() - 1000 : new Date(y!, m! - 1, d!).getTime()
  return Number.isNaN(t) ? null : Math.floor(t / 1000)
}

const result = ref<SimilarResult | null>(null)
const loading = ref(false)
const failed = ref(false)

// Declared before this immediate watch, which resets them as soon as the page sets up.
watch(messageId, async (id) => {
  facets.value = null
  reference.value = null
  result.value = null
  failed.value = false
  if (!Number.isInteger(id) || id <= 0) {
    failed.value = true
    return
  }
  // Only for the date of the header band: silently left out if it fails.
  api.message(id).then((m) => {
    if (id === messageId.value) reference.value = m.summary
  }, () => {})
  try {
    const f = await api.similarFacets(id)
    if (id !== messageId.value) return
    accountId.value = f.account_id
    fromOn.value = !!f.from
    fromMode.value = 'address'
    toAddress.value = f.to[0]?.email
    toOn.value = false
    subject.value = f.subject_core
    subjectOn.value = false
    body.value = ''
    bodyOn.value = false
    domains.value = f.link_domains[0] ? [f.link_domains[0].domain] : []
    linksOn.value = false
    listOn.value = false
    bulkOn.value = false
    dateFrom.value = ''
    dateTo.value = ''
    facets.value = f
  } catch (e) {
    if (id !== messageId.value) return
    failed.value = true
    showError(e, t('tools.cleanup.notFound'))
  }
}, { immediate: true })

const fromItems = computed(() => {
  const f = facets.value
  if (!f?.from) return []
  return [
    { label: t('tools.cleanup.thisAddress', { email: f.from.email }), value: 'address' as const },
    ...(f.from_domain ? [{ label: t('tools.cleanup.thisDomain', { domain: f.from_domain }), value: 'domain' as const }] : [])
  ]
})
const toItems = computed(() => (facets.value?.to ?? []).map(a => ({ label: addressFull(a), value: a.email })))

function toggleDomain(d: string) {
  domains.value = domains.value.includes(d) ? domains.value.filter(x => x !== d) : [...domains.value, d]
  linksOn.value = domains.value.length > 0
}

const criteria = computed<SimilarCriteria | null>(() => {
  const f = facets.value
  if (!f || accountId.value === null) return null
  return {
    account_id: accountId.value,
    from_address: fromOn.value && fromMode.value === 'address' ? f.from?.email ?? null : null,
    from_domain: fromOn.value && fromMode.value === 'domain' ? f.from_domain : null,
    to_address: toOn.value ? toAddress.value ?? null : null,
    subject: subjectOn.value && subject.value.trim() ? subject.value.trim() : null,
    body: bodyOn.value && body.value.trim() ? body.value.trim() : null,
    link_domains: linksOn.value ? domains.value : [],
    list_id: listOn.value ? f.list_id : null,
    bulk_only: bulkOn.value,
    whole_threads: threadsOn.value,
    date_from: day(dateFrom.value, false),
    date_to: day(dateTo.value, true)
  }
})
const empty = computed(() => {
  const c = criteria.value
  return !c || (!c.from_address && !c.from_domain && !c.to_address && !c.subject && !c.body && !c.link_domains.length && !c.list_id && !c.bulk_only)
})

let timer: ReturnType<typeof setTimeout> | undefined
/** Last search requested: an older answer arriving after it is ignored. */
let searchRequest = 0
/** Criteria changed, search not started yet: the result shown is stale. */
const debouncing = ref(false)

async function search() {
  debouncing.value = false
  const request = ++searchRequest
  if (!criteria.value || empty.value) {
    result.value = null
    loading.value = false
    return
  }
  loading.value = true
  try {
    const found = await api.findSimilar(criteria.value)
    if (request === searchRequest) result.value = found
  } catch (e) {
    if (request === searchRequest) showError(e)
  } finally {
    if (request === searchRequest) loading.value = false
  }
}
watch(criteria, () => {
  clearTimeout(timer)
  // Invalidate the search in progress: it was for the old criteria.
  searchRequest++
  loading.value = false
  debouncing.value = true
  timer = setTimeout(search, 250)
}, { deep: true })

/** "Also search the server": while on, each new set of criteria also fetches the matching
 *  messages not downloaded yet, then the results are read again. */
const serverOn = ref(false)
const serverBusy = ref(false)
let serverTimer: ReturnType<typeof setTimeout> | undefined
/** Criteria the server was last searched for (only those it uses: not the period). */
let serverDone = ''
function serverKey(c: SimilarCriteria | null): string {
  if (!c) return ''
  const { date_from: _from, date_to: _to, ...rest } = c
  return JSON.stringify(rest)
}

async function fromServer() {
  const c = criteria.value
  if (serverBusy.value || !serverOn.value || !c || empty.value) return
  const key = serverKey(c)
  if (key === serverDone) return
  serverBusy.value = true
  try {
    await api.similarFromServer(c)
    serverDone = key
    if (key === serverKey(criteria.value)) await search()
  } catch (e) {
    showError(e, t('tools.cleanup.serverFailed'))
    serverOn.value = false
  } finally {
    serverBusy.value = false
  }
  // Criteria changed during the search: go again for the new ones.
  if (serverOn.value && serverKey(criteria.value) !== serverDone) fromServer()
}
watch([criteria, serverOn], () => {
  clearTimeout(serverTimer)
  if (serverOn.value) serverTimer = setTimeout(fromServer, 600)
}, { deep: true })
onBeforeUnmount(() => {
  clearTimeout(timer)
  clearTimeout(serverTimer)
})

/** Messages the user unchecked; everything starts checked. */
const unchecked = ref(new Set<number>())
/** Copies of each unchecked message, remembered when it was unchecked: a refreshed result may
 *  push it out of the list into the unlisted oldest, where it must stay excluded. */
const uncheckedCopies = new Map<number, number[]>()
/** Matches beyond the listed ones (the oldest): checked or not, as a whole. */
const restOn = ref(true)
watch(result, (r) => {
  if (!r) {
    unchecked.value = new Set()
    uncheckedCopies.clear()
    restOn.value = true
    return
  }
  // A refreshed result keeps what was unchecked and still matches, listed or not.
  r.messages.forEach((m, i) => {
    if (uncheckedCopies.has(m.id)) uncheckedCopies.set(m.id, r.copies[i] ?? [m.id])
  })
  const matching = new Set(r.ids)
  const kept = [...unchecked.value].filter(id => (uncheckedCopies.get(id) ?? [id]).some(c => matching.has(c)))
  for (const id of uncheckedCopies.keys()) if (!kept.includes(id)) uncheckedCopies.delete(id)
  unchecked.value = new Set(kept)
})
const rest = computed(() => (result.value ? Math.max(0, result.value.count - result.value.messages.length) : 0))
const listedUnchecked = computed(() => result.value?.messages.filter(m => unchecked.value.has(m.id)).length ?? 0)
const hiddenUnchecked = computed(() => unchecked.value.size - listedUnchecked.value)
const selCount = computed(() => {
  const r = result.value
  if (!r) return 0
  return r.messages.length - listedUnchecked.value + (restOn.value ? Math.max(0, rest.value - hiddenUnchecked.value) : 0)
})
const allOn = computed(() => !!result.value?.count && selCount.value === result.value.count)
function setAll(on: boolean) {
  const r = result.value
  uncheckedCopies.clear()
  if (!on && r) r.messages.forEach((m, i) => uncheckedCopies.set(m.id, r.copies[i] ?? [m.id]))
  unchecked.value = new Set(uncheckedCopies.keys())
  restOn.value = on
}
function setChecked(id: number, on: boolean) {
  const next = new Set(unchecked.value)
  if (on) {
    next.delete(id)
    uncheckedCopies.delete(id)
  } else {
    next.add(id)
    const r = result.value
    const i = r?.messages.findIndex(m => m.id === id) ?? -1
    uncheckedCopies.set(id, (i >= 0 ? r?.copies[i] : undefined) ?? [id])
  }
  unchecked.value = next
}

/** Ids the action applies to: all matches, less the copies of unchecked messages (or, when the
 *  unlisted oldest are unchecked, only the copies of the checked ones). The engine gives each
 *  listed message's copies (Gmail keeps one message in several folders). */
function selectedIds(r: SimilarResult): number[] {
  const copies = (i: number) => r.copies[i] ?? [r.messages[i]!.id]
  const everyMatch = restOn.value || !rest.value
  if (!unchecked.value.size && everyMatch) return r.ids
  if (everyMatch) {
    const drop = new Set(r.messages.flatMap((m, i) => (unchecked.value.has(m.id) ? copies(i) : [])))
    // Unchecked earlier, then pushed into the unlisted oldest by a refresh.
    for (const id of unchecked.value) for (const c of uncheckedCopies.get(id) ?? [id]) drop.add(c)
    return r.ids.filter(id => !drop.has(id))
  }
  return [...new Set(r.messages.flatMap((m, i) => (unchecked.value.has(m.id) ? [] : copies(i))))]
}

const action = ref<SimilarAction>('move')
const dest = ref<number | null>(null)
watch(accountId, () => {
  dest.value = null
})
const destFolder = computed(() => mailboxes.value.find(m => m.id === dest.value))
const running = ref(false)
/** The result is for older criteria while a search waits or runs. */
const stale = computed(() => loading.value || debouncing.value)
const canRun = computed(() => !empty.value && selCount.value > 0 && !stale.value && !running.value && (action.value === 'delete' || !!destFolder.value))

async function run() {
  const r = result.value
  if (!r || !canRun.value) return
  const n = selCount.value
  const count = fmt.value.format(n)
  running.value = true
  try {
    const ids = selectedIds(r)
    // The criteria changed meanwhile: this is no longer the selection on screen.
    if (result.value !== r || !ids.length) return
    if (action.value === 'delete') {
      const ok = await confirm(t('tools.cleanup.confirm', { n: count }, n), { title: t('tools.cleanup.deleteTitle'), kind: 'warning' })
      if (!ok) return
      done(await api.trash(ids), t('tools.cleanup.trashed', { n: count }, n), 'i-lucide-trash-2')
      listStale.bump()
    } else {
      const folder = destFolder.value
      if (!folder) return
      done(await api.moveTo(ids, folder.id), t('tools.cleanup.moved', { n: count, folder: folderLabel(folder) }, n), 'i-lucide-folder-input')
      listStale.bump()
    }
    router.back()
  } catch (e) {
    showError(e, action.value === 'delete' ? t('tools.shared.deleteFailed') : t('tools.cleanup.moveFailed'))
  } finally {
    running.value = false
  }
}

const folderOf = (id: number) => mailboxes.value.find(b => b.id === id)
const fmt = computed(() => new Intl.NumberFormat(intlLocale()))
const runLabel = computed(() => {
  const n = selCount.value
  return t(action.value === 'delete' ? 'tools.cleanup.deleteCount' : 'tools.cleanup.moveCount', { n: fmt.value.format(n) }, n)
})

type Sheet = 'from' | 'to' | 'subject' | 'body' | 'links' | 'period'
const sheet = ref<Sheet | null>(null)
const sheetOpen = computed({
  get: () => sheet.value !== null,
  set: (open: boolean) => {
    if (!open) sheet.value = null
  }
})
const sheetTitle = computed(() => {
  switch (sheet.value) {
    case 'from': return t('tools.cleanup.sender')
    case 'to': return t('tools.cleanup.recipient')
    case 'subject': return t('tools.cleanup.subjectContains')
    case 'body': return t('tools.cleanup.bodyContains')
    case 'links': return t('tools.cleanup.linksTo')
    case 'period': return t('tools.cleanup.period')
    default: return ''
  }
})

const fromValue = computed(() => fromItems.value.find(i => i.value === fromMode.value)?.label ?? '')
const toValue = computed(() => toItems.value.find(i => i.value === toAddress.value)?.label ?? '')
const periodValue = computed(() => t('tools.cleanup.periodValue', {
  from: dateFrom.value ? dateLabel(dateFrom.value) : t('tools.cleanup.since'),
  to: dateTo.value ? dateLabel(dateTo.value) : t('tools.cleanup.untilLower')
}))
function dateLabel(value: string): string {
  const [y, m, d] = value.split('-').map(Number)
  return dateFormat({ day: 'numeric', month: 'short', year: 'numeric' }).format(new Date(y!, m! - 1, d!))
}
const referenceMeta = computed(() => [
  facets.value?.from ? addressLabel(facets.value.from) : '',
  reference.value ? shortDate(reference.value.date) : ''
].filter(Boolean).join(' · '))
</script>

<template>
  <UDashboardPanel id="cleanup" :ui="{ body: 'bg-(--surface-read) p-0 sm:p-0' }">
    <template #header>
      <UDashboardNavbar :title="t('tools.cleanup.title')" :ui="{ title: 'max-lg:text-[17px]' }">
        <template #leading>
          <UButton
            icon="i-lucide-arrow-left"
            color="neutral"
            variant="ghost"
            class="max-lg:size-11 max-lg:justify-center"
            :aria-label="t('tools.shared.back')"
            @click="router.back()"
          />
        </template>
      </UDashboardNavbar>
    </template>

    <template #body>
      <UEmpty
        v-if="failed"
        icon="i-lucide-search-x"
        :title="t('tools.cleanup.notFound')"
        :description="t('tools.cleanup.notFoundHint')"
        :actions="[{ label: t('tools.shared.back'), icon: 'i-lucide-arrow-left', color: 'neutral', variant: 'subtle', onClick: () => router.back() }]"
        variant="naked"
        class="h-full"
      />
      <div v-else-if="!facets" class="flex h-full items-center justify-center text-sm text-muted">
        <UIcon name="i-lucide-loader-circle" class="me-2 size-4 animate-spin" /> {{ t('tools.cleanup.analyzing') }}
      </div>

      <div v-else-if="desktop" class="flex min-h-0 flex-1 justify-center gap-5 px-8 pt-6 pb-7">
        <aside
          :aria-label="t('tools.cleanup.criteria')"
          class="flex w-95 shrink-0 flex-col overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs xl:w-105 dark:shadow-none"
        >
          <div class="flex flex-col gap-2.5 border-b border-default bg-(--ui-color-neutral-50) px-5 py-4.5 dark:bg-(--ui-color-neutral-950)/40">
            <span class="text-xs font-bold uppercase tracking-[0.05em] text-muted">{{ t('tools.cleanup.similarTo') }}</span>
            <div class="flex items-center gap-3">
              <MailAvatar :address="facets.from" :account-id="facets.account_id" class="size-9.5!" />
              <div class="flex min-w-0 flex-col gap-0.5">
                <span class="truncate text-sm font-semibold text-highlighted">{{ facets.subject || t('common.noSubject') }}</span>
                <span v-if="referenceMeta" class="truncate text-[12.5px] text-muted">{{ referenceMeta }}</span>
              </div>
            </div>
          </div>

          <div class="min-h-0 flex-1 overflow-y-auto">
            <div class="flex flex-col gap-2 border-b border-default px-5 py-4.5">
              <span class="text-sm font-semibold text-highlighted">{{ t('tools.cleanup.account') }}</span>
              <AccountSelect v-model="accountId" class="w-full" />
              <span class="text-[12.5px] text-muted">{{ t('tools.cleanup.accountNote') }}</span>
            </div>

            <div class="flex flex-col gap-3.5 px-5 py-4">
              <p class="flex flex-wrap items-baseline gap-x-2">
                <span class="text-sm font-semibold text-highlighted">{{ t('tools.cleanup.criteria') }}</span>
                <span class="text-[12.5px] text-muted">{{ t('tools.cleanup.criteriaHint') }}</span>
              </p>

              <SimilarCriterion v-if="facets.from" v-model="fromOn" :label="t('tools.cleanup.sender')">
                <USelect
                  v-model="fromMode"
                  :items="fromItems"
                  size="lg"
                  class="w-full"
                  :ui="{ base: 'h-10 rounded-[10px]' }"
                  @update:model-value="fromOn = true"
                />
              </SimilarCriterion>

              <SimilarCriterion v-if="facets.to.length" v-model="toOn" :label="t('tools.cleanup.recipient')">
                <USelect
                  v-model="toAddress"
                  :items="toItems"
                  size="lg"
                  class="w-full"
                  :ui="{ base: 'h-10 rounded-[10px]' }"
                  @update:model-value="toOn = true"
                />
              </SimilarCriterion>

              <SimilarCriterion v-model="subjectOn" :label="t('tools.cleanup.subjectContains')">
                <UInput
                  v-model="subject"
                  size="lg"
                  class="w-full"
                  :ui="{ base: 'h-10 rounded-[10px]' }"
                  @update:model-value="(v) => { subjectOn = !!String(v).trim() }"
                />
              </SimilarCriterion>

              <SimilarCriterion v-model="bodyOn" :label="t('tools.cleanup.bodyContains')">
                <UInput
                  v-model="body"
                  size="lg"
                  class="w-full"
                  :placeholder="t('tools.cleanup.bodyPlaceholder')"
                  :ui="{ base: 'h-10 rounded-[10px]' }"
                  @update:model-value="(v) => { bodyOn = !!String(v).trim() }"
                />
              </SimilarCriterion>

              <SimilarCriterion v-if="facets.link_domains.length" v-model="linksOn" :label="t('tools.cleanup.linksTo')">
                <div class="flex flex-wrap gap-1.5">
                  <UButton
                    v-for="d in facets.link_domains"
                    :key="d.domain"
                    :label="d.domain"
                    size="sm"
                    class="rounded-full"
                    :color="domains.includes(d.domain) ? 'primary' : 'neutral'"
                    :variant="domains.includes(d.domain) ? 'soft' : 'outline'"
                    :icon="domains.includes(d.domain) ? 'i-lucide-check' : 'i-lucide-link'"
                    @click="toggleDomain(d.domain)"
                  >
                    <template #trailing>
                      <span class="text-dimmed">{{ d.links }}</span>
                    </template>
                  </UButton>
                </div>
              </SimilarCriterion>

              <SimilarCriterion v-if="facets.list_id" v-model="listOn" :label="t('tools.cleanup.sameList')">
                <div class="flex h-10 items-center gap-2 rounded-[10px] bg-elevated/60 px-3 text-[13px] text-highlighted ring-1 ring-inset ring-default">
                  <UIcon name="i-lucide-mails" class="size-4 shrink-0 text-muted" />
                  <span class="truncate">{{ facets.list_id }}</span>
                </div>
              </SimilarCriterion>

              <SimilarCriterion v-if="facets.bulk" v-model="bulkOn" :label="t('tools.cleanup.bulkOnly')" />

              <SimilarCriterion v-model="threadsOn" :label="t('tools.cleanup.wholeThreads')">
                <p class="text-xs text-muted">{{ t('tools.cleanup.wholeThreadsHint') }}</p>
              </SimilarCriterion>

              <div class="flex flex-col gap-1.5">
                <span class="flex min-h-7 items-center text-[13.5px] font-semibold text-highlighted">{{ t('tools.cleanup.period') }}</span>
                <div class="flex items-center gap-2">
                  <SimilarDateField v-model="dateFrom" :placeholder="t('tools.cleanup.since')" :label="t('tools.shared.from')" :max="dateTo || undefined" />
                  <span class="text-dimmed" aria-hidden="true">→</span>
                  <SimilarDateField v-model="dateTo" :placeholder="t('tools.cleanup.until')" :label="t('tools.shared.to')" :min="dateFrom || undefined" />
                </div>
              </div>
            </div>
          </div>

          <div class="flex items-center gap-3 border-t border-default bg-(--ui-color-neutral-50) px-5 py-3.5 dark:bg-(--ui-color-neutral-950)/40">
            <div class="flex min-w-0 flex-1 flex-col gap-0.5">
              <span class="text-[13.5px] font-semibold text-highlighted">{{ t('tools.cleanup.searchServer') }}</span>
              <span class="text-xs text-muted">{{ t('tools.cleanup.searchServerHint') }}</span>
            </div>
            <UIcon v-if="serverBusy" name="i-lucide-loader-circle" class="size-4 shrink-0 animate-spin text-muted" />
            <USwitch v-model="serverOn" size="lg" :aria-label="t('tools.cleanup.searchServer')" />
          </div>
        </aside>

        <section
          :aria-label="t('tools.cleanup.results')"
          class="flex min-w-0 max-w-215 flex-1 flex-col overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none"
        >
          <div class="flex h-14 shrink-0 items-center gap-3 border-b border-default px-4.5">
            <SimilarCheck
              v-if="result?.count"
              :model-value="allOn"
              :mixed="selCount > 0 && !allOn"
              :label="t('tools.cleanup.selectAll')"
              @update:model-value="setAll(!allOn)"
            />
            <p class="min-w-0 truncate text-[15px] text-highlighted">
              <span v-if="empty" class="text-sm text-muted">{{ t('tools.cleanup.checkOne') }}</span>
              <template v-else-if="result">
                <b class="font-bold">{{ t('tools.cleanup.messages', { n: fmt.format(result.count) }, result.count) }}</b>
                <span class="text-muted"> · {{ fileSize(result.bytes) }}</span>
              </template>
            </p>
            <UIcon v-if="loading || serverBusy" name="i-lucide-loader-circle" class="size-4 shrink-0 animate-spin text-muted" />
            <span v-if="serverBusy" class="truncate text-xs text-muted">{{ t('tools.cleanup.serverSearching') }}</span>
            <span
              v-if="result?.count"
              class="ms-auto shrink-0 rounded-full bg-primary/14 px-2.5 py-0.75 text-[12.5px] font-semibold text-primary"
            >{{ t('tools.cleanup.selected', { n: fmt.format(selCount) }, selCount) }}</span>
          </div>

          <div class="min-h-0 flex-1 overflow-y-auto px-2 py-1.5" :class="stale ? 'opacity-70' : ''">
            <template v-if="!empty && result">
              <SimilarRow
                v-for="m in result.messages"
                :key="m.id"
                :model-value="!unchecked.has(m.id)"
                :message="m"
                :folder="folderOf(m.mailbox_id)"
                @update:model-value="(v: boolean) => setChecked(m.id, v)"
              />
              <SimilarCheck
                v-if="rest"
                v-model="restOn"
                class="w-full px-2.5 py-2.5 text-[13px] text-muted"
                :class="restOn ? 'bg-primary/5 dark:bg-primary/6' : ''"
              >
                {{ t('tools.cleanup.more', { n: fmt.format(rest) }, rest) }}
              </SimilarCheck>
              <p v-if="!result.count" class="p-6 text-center text-sm text-muted">{{ t('tools.shared.noMatch') }}</p>
            </template>
          </div>

          <div class="flex shrink-0 flex-wrap items-center gap-3 border-t border-default bg-(--ui-color-neutral-50) px-4.5 py-3.5 dark:bg-(--ui-color-neutral-950)/40">
            <SimilarActionPicker v-model="action" />
            <template v-if="action === 'move'">
              <span class="text-[13.5px] text-muted">{{ t('tools.cleanup.to') }}</span>
              <SimilarFolderMenu v-if="accountId !== null" v-model="dest" :account-id="accountId" />
            </template>
            <span v-else class="min-w-0 text-[13px] text-muted">{{ t('tools.cleanup.deleteNote') }}</span>
            <UButton
              :label="runLabel"
              :icon="action === 'delete' ? 'i-lucide-trash-2' : undefined"
              :color="action === 'delete' ? 'error' : 'primary'"
              size="lg"
              class="ms-auto h-10.5 rounded-[11px] px-4.5 font-semibold"
              :class="{ 'btn-danger': action === 'delete' }"
              :disabled="!canRun"
              :loading="running"
              @click="run"
            />
          </div>
        </section>
      </div>

      <div v-else class="flex flex-col gap-3.5 px-4 pt-4 pb-6">
        <div class="flex items-center gap-3 rounded-2xl border border-default bg-(--surface-card) px-3.5 py-3 shadow-xs dark:shadow-none">
          <MailAvatar :address="facets.from" :account-id="facets.account_id" class="size-9.5!" />
          <div class="flex min-w-0 flex-col gap-0.5">
            <span class="text-xs text-muted">{{ t('tools.cleanup.similarTo') }}</span>
            <span class="truncate text-sm font-semibold text-highlighted">{{ facets.subject || t('common.noSubject') }}</span>
            <span v-if="referenceMeta" class="truncate text-xs text-muted">{{ referenceMeta }}</span>
          </div>
        </div>

        <div class="flex flex-col gap-1.5">
          <AccountSelect v-model="accountId" size="lg" :label="t('tools.cleanup.account')" />
          <span class="mx-1.5 text-xs text-muted">{{ t('tools.cleanup.accountNote') }}</span>
        </div>

        <section aria-labelledby="similar-criteria" class="overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none">
          <div class="flex flex-col gap-0.5 px-3.5 pt-3.5 pb-1.5">
            <h2 id="similar-criteria" class="text-[15px] font-semibold text-highlighted">{{ t('tools.cleanup.criteria') }}</h2>
            <span class="text-[12.5px] text-muted">{{ t('tools.cleanup.criteriaHintFull') }}</span>
          </div>
          <div class="divide-y divide-default border-t border-default">
            <SimilarListRow v-if="facets.from" v-model="fromOn" :label="t('tools.cleanup.sender')" :value="fromValue" :editable="fromItems.length > 1" @edit="sheet = 'from'" />
            <SimilarListRow v-if="facets.to.length" v-model="toOn" :label="t('tools.cleanup.recipient')" :value="toValue" :editable="toItems.length > 1" @edit="sheet = 'to'" />
            <SimilarListRow
              v-model="subjectOn"
              :label="t('tools.cleanup.subjectContains')"
              :value="subject.trim() || t('tools.cleanup.subjectPlaceholder')"
              editable
              @edit="sheet = 'subject'"
            />
            <SimilarListRow
              v-model="bodyOn"
              :label="t('tools.cleanup.bodyContains')"
              :value="body.trim() || t('tools.cleanup.bodyPlaceholder')"
              editable
              @edit="sheet = 'body'"
            />
            <SimilarListRow
              v-if="facets.link_domains.length"
              v-model="linksOn"
              :label="t('tools.cleanup.linksTo')"
              :value="domains.join(', ') || t('tools.cleanup.noDomain')"
              editable
              @edit="sheet = 'links'"
            />
            <SimilarListRow v-if="facets.list_id" v-model="listOn" :label="t('tools.cleanup.sameList')" :value="facets.list_id" />
            <SimilarListRow v-if="facets.bulk" v-model="bulkOn" :label="t('tools.cleanup.bulkOnly')" />
            <SimilarListRow v-model="threadsOn" :label="t('tools.cleanup.wholeThreads')" />
            <SimilarListRow :label="t('tools.cleanup.period')" :value="periodValue" icon="i-lucide-calendar" editable @edit="sheet = 'period'" />
            <div class="flex min-h-14 items-center gap-3 px-3.5 py-1.5">
              <span class="flex min-w-0 flex-1 flex-col gap-0.5">
                <span class="text-[14.5px] text-highlighted">{{ t('tools.cleanup.searchServer') }}</span>
                <span class="text-xs text-muted">{{ serverBusy ? t('tools.cleanup.serverSearching') : t('tools.cleanup.searchServerHint') }}</span>
              </span>
              <USwitch v-model="serverOn" size="lg" :loading="serverBusy" :aria-label="t('tools.cleanup.searchServer')" />
            </div>
          </div>
        </section>

        <section aria-labelledby="similar-results" class="overflow-hidden rounded-2xl border border-default bg-(--surface-card) shadow-xs dark:shadow-none">
          <div class="flex min-h-14 items-center gap-2.5 px-3.5 py-2">
            <h2 id="similar-results" class="min-w-0 flex-1 truncate text-[15px] text-highlighted">
              <span v-if="empty" class="text-sm text-muted">{{ t('tools.cleanup.checkOne') }}</span>
              <template v-else-if="result">
                <b class="font-bold">{{ t('tools.cleanup.messages', { n: fmt.format(result.count) }, result.count) }}</b>
                <span class="font-normal text-muted"> · {{ fileSize(result.bytes) }}</span>
              </template>
            </h2>
            <UIcon v-if="loading" name="i-lucide-loader-circle" class="size-4 shrink-0 animate-spin text-muted" />
            <UButton
              v-if="result?.count"
              :label="allOn ? t('tools.cleanup.uncheckAll') : t('tools.cleanup.checkAll')"
              variant="ghost"
              class="h-11 shrink-0 rounded-lg px-2.5 text-[13.5px] font-semibold"
              @click="setAll(!allOn)"
            />
          </div>
          <div v-if="!empty && result" class="divide-y divide-default border-t border-default" :class="stale ? 'opacity-70' : ''">
            <SimilarRow
              v-for="m in result.messages"
              :key="m.id"
              :model-value="!unchecked.has(m.id)"
              :message="m"
              :folder="folderOf(m.mailbox_id)"
              touch
              @update:model-value="(v: boolean) => setChecked(m.id, v)"
            />
            <SimilarCheck
              v-if="rest"
              v-model="restOn"
              touch
              class="min-h-15 w-full rounded-none px-3.5 py-2 text-[13px] text-muted"
              :class="restOn ? 'bg-primary/5 dark:bg-primary/6' : ''"
            >
              <span class="ps-0.5">{{ t('tools.cleanup.more', { n: fmt.format(rest) }, rest) }}</span>
            </SimilarCheck>
            <p v-if="!result.count" class="p-6 text-center text-sm text-muted">{{ t('tools.shared.noMatch') }}</p>
          </div>
        </section>

        <UDrawer v-model:open="sheetOpen" :title="sheetTitle" :ui="{ container: 'pb-[calc(1rem+env(safe-area-inset-bottom))]' }">
          <template #body>
            <div
              v-if="sheet === 'from' || sheet === 'to'"
              role="radiogroup"
              :aria-label="sheetTitle"
              class="divide-y divide-default overflow-hidden rounded-2xl border border-default"
            >
              <button
                v-for="o in (sheet === 'from' ? fromItems : toItems)"
                :key="o.value"
                type="button"
                role="radio"
                :aria-checked="(sheet === 'from' ? fromMode : toAddress) === o.value"
                class="flex min-h-13 w-full items-center gap-3 px-4 text-start text-[15px] active:bg-elevated/60"
                :class="(sheet === 'from' ? fromMode : toAddress) === o.value ? 'font-semibold text-highlighted' : 'text-default'"
                @click="() => {
                  if (sheet === 'from') { fromMode = o.value as 'address' | 'domain'; fromOn = true }
                  else { toAddress = o.value; toOn = true }
                  sheet = null
                }"
              >
                <span class="min-w-0 flex-1 break-words">{{ o.label }}</span>
                <UIcon v-if="(sheet === 'from' ? fromMode : toAddress) === o.value" name="i-lucide-check" class="size-5 shrink-0 text-primary" />
              </button>
            </div>

            <form v-else-if="sheet === 'subject' || sheet === 'body'" class="flex flex-col gap-3" @submit.prevent="sheet = null">
              <UInput
                v-if="sheet === 'subject'"
                v-model="subject"
                size="xl"
                autofocus
                class="w-full"
                :placeholder="t('tools.cleanup.subjectPlaceholder')"
                :ui="{ base: 'h-12 rounded-xl' }"
                @update:model-value="(v) => { subjectOn = !!String(v).trim() }"
              />
              <UInput
                v-else
                v-model="body"
                size="xl"
                autofocus
                class="w-full"
                :placeholder="t('tools.cleanup.bodyPlaceholder')"
                :ui="{ base: 'h-12 rounded-xl' }"
                @update:model-value="(v) => { bodyOn = !!String(v).trim() }"
              />
              <UButton type="submit" :label="t('tools.cleanup.done')" size="xl" block class="h-12 rounded-xl font-semibold" />
            </form>

            <div v-else-if="sheet === 'links'" class="divide-y divide-default overflow-hidden rounded-2xl border border-default">
              <button
                v-for="d in facets.link_domains"
                :key="d.domain"
                type="button"
                role="checkbox"
                :aria-checked="domains.includes(d.domain)"
                class="flex min-h-13 w-full items-center gap-3 px-4 text-start text-[15px] active:bg-elevated/60"
                @click="toggleDomain(d.domain)"
              >
                <UIcon name="i-lucide-link" class="size-5 shrink-0 text-muted" />
                <span class="min-w-0 flex-1 truncate text-highlighted">{{ d.domain }}</span>
                <span class="text-sm text-dimmed">{{ d.links }}</span>
                <UIcon v-if="domains.includes(d.domain)" name="i-lucide-check" class="size-5 shrink-0 text-primary" />
              </button>
            </div>

            <div v-else-if="sheet === 'period'" class="flex flex-col gap-3">
              <div class="flex flex-col gap-1.5">
                <span class="mx-1 text-xs text-muted">{{ t('tools.shared.from') }}</span>
                <SimilarDateField v-model="dateFrom" size="lg" :placeholder="t('tools.cleanup.since')" :label="t('tools.shared.from')" :max="dateTo || undefined" />
              </div>
              <div class="flex flex-col gap-1.5">
                <span class="mx-1 text-xs text-muted">{{ t('tools.shared.to') }}</span>
                <SimilarDateField v-model="dateTo" size="lg" :placeholder="t('tools.cleanup.until')" :label="t('tools.shared.to')" :min="dateFrom || undefined" />
              </div>
              <UButton :label="t('tools.cleanup.done')" size="xl" block class="mt-1 h-12 rounded-xl font-semibold" @click="sheet = null" />
            </div>
          </template>
        </UDrawer>
      </div>
    </template>

    <template v-if="facets && !desktop" #footer>
      <div class="flex shrink-0 flex-col gap-2.5 border-t border-default bg-default px-4 pt-3 pb-4 shadow-[0_-8px_24px_rgb(0_0_0/0.08)] dark:shadow-[0_-8px_24px_rgb(0_0_0/0.3)]">
        <SimilarActionPicker v-model="action" block />
        <template v-if="action === 'move'">
          <SimilarFolderMenu v-if="accountId !== null" v-model="dest" :account-id="accountId" size="lg" />
        </template>
        <span v-else class="text-center text-[13px] text-muted">{{ t('tools.cleanup.deleteNote') }}</span>
        <UButton
          :label="runLabel"
          :icon="action === 'delete' ? 'i-lucide-trash-2' : undefined"
          :color="action === 'delete' ? 'error' : 'primary'"
          size="xl"
          block
          class="h-12.5 justify-center rounded-[14px] text-[15px] font-semibold"
          :class="{ 'btn-danger': action === 'delete' }"
          :disabled="!canRun"
          :loading="running"
          @click="run"
        />
      </div>
    </template>
  </UDashboardPanel>
</template>
