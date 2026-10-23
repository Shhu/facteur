<script setup lang="ts">
import type { DropdownMenuItem } from '@nuxt/ui'
import { useEventListener, useIntersectionObserver, useMediaQuery, watchDebounced } from '@vueuse/core'
import type { CategoryUnread, FollowUp, ListItem, MessageSummary, ThreadFilter, ThreadSummary } from '~/utils/types'
import type { ListAction } from '~/components/ThreadList.vue'

const PAGE = 100

const route = useRoute()
const router = useRouter()
const { accounts, accountId, accountById, accountColor, accountNameOf, accountCount, mailboxes, tick, syncing, unifiedUnread, ownSet } = useMail()
const showError = useErrorToast()
const { t } = useI18n()
/** Another screen is open over this list (layouts/default.vue): its shortcuts stand down. */
const { covered } = useMailOverlay()

/** "Done" view (as in Spark): what left the inboxes, across all accounts. */
const doneView = computed(() => route.params.mailbox === 'done')
const unified = computed(() => route.params.mailbox === 'unified' || doneView.value)
const view = computed(() => String(route.params.mailbox))
const mailboxId = computed(() => (unified.value ? null : Number(route.params.mailbox)))
const threadId = computed(() => (route.query.thread ? Number(route.query.thread) : null))
const mailbox = computed(() => mailboxes.value.find(m => m.id === mailboxId.value))
const title = computed(() => {
  if (doneView.value) return t('mail.page.done')
  if (unified.value) return t('mail.page.unified')
  const m = mailbox.value
  return m ? (m.role ? roleLabels[m.role] : m.display_name) : ''
})
const { unreadOf } = useFolderCounts()
const unread = computed(() => (doneView.value ? 0 : unified.value ? unifiedUnread.value : (mailbox.value ? unreadOf(mailbox.value) : 0)))
const syncingHere = computed(() =>
  unified.value ? syncing.value.length > 0 : !!mailbox.value && syncing.value.includes(mailbox.value.account_id)
)

watch(mailbox, (m) => {
  if (m) accountId.value = m.account_id
}, { immediate: true })
/** In Sent and Drafts, rows show the recipients rather than "me". */
const outgoing = computed(() => mailbox.value?.role === 'sent' || mailbox.value?.role === 'drafts')

// Replaced wholesale on each load: no need for deep reactivity.
const threads = shallowRef<ThreadSummary[]>([])
const limit = ref(PAGE)
/** The whole local list is loaded (a read returned fewer rows than asked for). */
const complete = ref(false)
const filter = ref<ThreadFilter>('all')
const query = ref('')
const searchBins = ref(false)
const results = ref<MessageSummary[] | null>(null)
/** Server hits (body included, beyond the cache): kept until the search changes. */
const serverHits = ref<MessageSummary[] | null>(null)
const searchingServer = ref(false)
const searchInput = useTemplateRef('searchInput')
const { resume, open: composing } = useComposer()
const { ask } = useWhenPicker()
const { done } = useUndo()

const searchOpen = ref(false)
const showSearch = computed(() => searchOpen.value || !!query.value)
function focusSearch() {
  searchOpen.value = true
  nextTick(() => searchInput.value?.focus())
}
function onSearchEscape() {
  if (query.value) query.value = ''
  else setSearchOpen(false)
}
function setSearchOpen(open: boolean) {
  if (open) focusSearch()
  else {
    searchOpen.value = false
    query.value = ''
  }
}

/** Last read requested: an older answer arriving after it is ignored. */
let loadRequest = 0
/** View (folder, filter, tab, length) whose rows `threads` holds. */
let loadedFor = ''
const loadKey = () => `${view.value}|${filter.value}|${smartView.value ? smart.category.value : ''}|${limit.value}`
let inflight: string | null = null
/** The read in flight is for a longer list (a page more, another view): scrolling meanwhile asks for nothing. */
let inflightGrows = false
/** The bottom was reached during a refresh: the page asked for is added once that read lands
 *  (at the bottom, no further scroll event comes to ask again). */
let wantMore = false
/** A read asked for while another one of the same view was in flight: it runs once that one lands. */
let queued: 'full' | 'merge' | null = null
/** The next refresh rereads the whole list: a pin or unpin moves threads past the first page. */
let fullNext = false
// An action elsewhere changed which messages the list holds: every row shown is read again, or
// only the first page once the thread that left is dropped.
const listStale = useListStale()
watch(listStale.tick, () => {
  const left = listStale.gone.value
  listStale.gone.value = null
  // Only the row of the folder it left: another of its folders' rows stays. A read in flight,
  // from before the action, would bring the row back: the next read is a full one.
  if (left) threads.value = threads.value.filter(t => t.account_id !== left.account_id || t.thread_id !== left.thread_id || t.mailbox_id !== left.mailbox_id)
  if (!left || inflight) fullNext = true
  refresh()
  // A search shown: its results are read again too (filed elsewhere, a filter may drop them).
  refreshSearch()
})
/** The view changed and its first read hasn't landed yet: no "empty folder" meanwhile. */
const listLoading = ref(true)
/** Above this many earlier rows missing from the fresh first page, the merge rereads everything
 *  (they may have moved further down rather than left the list). */
const MERGE_MISSING = 5

function fetchThreads(count: number) {
  const cat = smartView.value && smart.category.value !== 'all' ? smart.category.value : null
  return doneView.value
    ? api.doneThreads(filter.value, count, 0)
    : unified.value
      ? api.unifiedThreads(filter.value, count, 0, cat)
      : api.threads(mailboxId.value!, filter.value, count, 0, cat)
}

/** Rereads the whole list (`limit` rows): view, filter or tab change, next page. */
const load = () => read(true)
/** After an engine change: only the first page is reread, then merged with the rest already
 *  loaded (rereading hundreds of threads on every flag change would be costly). */
const refresh = () => read(false)

/** One read at a time per view: a tick during a slow read doesn't cancel it (a sync ticking every
 *  few hundred ms would starve it), it runs again once the read lands. A read for another view
 *  supersedes the one in flight. */
async function read(full: boolean) {
  const key = loadKey()
  if (inflight === key) {
    queued = full ? 'full' : (queued ?? 'merge')
    return
  }
  const request = ++loadRequest
  inflight = key
  inflightGrows = loadedFor !== key
  queued = null
  if (fullNext) {
    fullNext = false
    full = true
  }
  try {
    // Another view's list: nothing to merge. Older messages loaded at the end ask for a full read
    // themselves (`fullNext`): a refresh only merges the first page, even into a long list.
    if (full || loadedFor !== key) await readAll(request, key)
    else await merge(request, key)
  } catch (e) {
    if (request === loadRequest) showError(e)
  } finally {
    if (request === loadRequest) {
      inflight = null
      listLoading.value = false
      const again = queued
      queued = null
      const grow = wantMore && !complete.value && !results.value
      wantMore = false
      // The limit watcher rereads the whole list, which covers a queued read too.
      if (grow) limit.value += PAGE
      else if (again) read(again === 'full')
    }
  }
}

async function readAll(request: number, key: string) {
  const list = await fetchThreads(limit.value)
  if (request !== loadRequest) return
  threads.value = list
  complete.value = list.length < limit.value
  loadedFor = key
}

async function merge(request: number, key: string) {
  const page = await fetchThreads(PAGE)
  if (request !== loadRequest) return
  if (page.length < PAGE) {
    threads.value = page
    complete.value = true
    return
  }
  const old = threads.value
  const id = (t: ThreadSummary) => `${t.account_id}-${t.thread_id}`
  // Rows after the page's last row stay valid; earlier rows missing from the page left the list
  // (archived, read under "unread"...).
  const last = id(page.at(-1)!)
  const at = old.findIndex(t => id(t) === last)
  // Unknown row: merging would be unreliable, reread everything.
  if (at === -1) return readAll(request, key)
  const fresh = new Set(page.map(id))
  if (old.slice(0, at).filter(t => !fresh.has(id(t))).length > MERGE_MISSING) return readAll(request, key)
  // Rows unchanged keep their object, and an unchanged list is not assigned: every row, its date
  // group and its labels would be built again for nothing at each sync tick.
  const before = new Map(old.map(t => [id(t), t]))
  const kept = page.map(t => {
    const was = before.get(id(t))
    return was && rowSignature(was) === rowSignature(t) ? was : t
  })
  const next = [...kept, ...old.slice(at + 1).filter(t => !fresh.has(id(t)))]
  if (next.length !== old.length || next.some((t, i) => t !== old[i])) threads.value = next
}

/** What a row shows of its thread. */
function rowSignature(t: ThreadSummary): string {
  return [t.latest_id, t.activity, t.unread, t.flagged, t.count, t.snoozed_until, t.woken, t.category, t.has_attachments, t.subject, t.preview, t.labels?.join(), t.message_ids.join()].join('|')
}

watch(view, () => {
  query.value = ''
  filter.value = 'all'
  searchOpen.value = false
})

const smart = useSmartInbox()
const smartView = computed(() => smart.enabled.value && !doneView.value && (unified.value || mailbox.value?.role === 'inbox'))
const categoryUnread = ref<CategoryUnread>({ people: 0, notification: 0, newsletter: 0 })
let categoryRequest = 0
async function loadCategoryUnread() {
  if (!smartView.value) return
  const request = ++categoryRequest
  const counts = await api.categoryUnread(unified.value ? null : mailboxId.value).catch(() => null)
  if (counts && request === categoryRequest) categoryUnread.value = counts
}
watch([view, smartView], loadCategoryUnread, { immediate: true })
const categoryItems = computed(() => [
  { label: t('mail.page.category.people'), value: 'people', icon: 'i-lucide-users', badge: categoryUnread.value.people || undefined },
  { label: t('mail.page.category.notification'), value: 'notification', icon: 'i-lucide-bell', badge: categoryUnread.value.notification || undefined },
  { label: t('mail.page.category.newsletter'), value: 'newsletter', icon: 'i-lucide-newspaper', badge: categoryUnread.value.newsletter || undefined },
  { label: t('mail.page.category.all'), value: 'all', icon: 'i-lucide-layers' }
])
// Another view: back to the first page, and the old rows go at once (actions must not target them).
watch([view, filter, () => smart.category.value], () => {
  limit.value = PAGE
  selected.value = new Set()
  threads.value = []
  complete.value = false
  loadedFor = ''
  wantMore = false
  listLoading.value = true
})

watch([view, filter, limit, () => smart.category.value, smartView], load, { immediate: true })
/** A sync ticks a few times a second, and each tick would aggregate the whole folder (tab
 *  counts) and reread the list: at most one round a second, one at a time. */
const tickReloader = useReloader(() => Promise.all([loadCategoryUnread(), refresh(), refreshSearch()]), 1000)
// Under another screen (stats, settings…) the list is read once, when it shows again.
let staleWhileCovered = false
watch(tick, () => {
  if (covered.value) staleWhileCovered = true
  else tickReloader.schedule()
})
watch(covered, (now) => {
  if (!now && staleWhileCovered) {
    staleWhileCovered = false
    tickReloader.schedule()
  }
})
// Saved search opened from the sidebar (`?q=`). Declared after the view reset so it runs last
// when both change together.
watch(() => route.query.q, (q, before) => {
  // The same search, give or take spaces (the address keeps it trimmed): left as typed, or a
  // space typed after an operator would go.
  if (typeof q === 'string' && q) {
    if (q.trim() !== query.value.trim()) query.value = q
  } else if (before && query.value.trim()) query.value = ''
}, { immediate: true })
// The other way round: the typed search goes to the address too (replaced, no new history entry),
// so a link to the same search (the home's pinned tile) counts as a change once this one was
// edited or cleared. Not while another screen covers the list: its route is not current.
// The debounce outlives the page: a folder change within 400 ms remounts it while this instance's
// `route` stays on the old folder, so the write is dropped once the page is gone or no longer owns
// the address, and the path is explicit so it can never land on another folder.
let unmounted = false
onBeforeUnmount(() => { unmounted = true })
watchDebounced(query, (q) => {
  if (unmounted || covered.value || router.currentRoute.value.path !== route.path) return
  const current = typeof route.query.q === 'string' ? route.query.q : ''
  if (q.trim() === current.trim()) return
  router.replace({ path: route.path, query: { ...route.query, q: q.trim() || undefined } })
}, { debounce: 400 })

let timer: ReturnType<typeof setTimeout> | undefined
/** Current search: any change makes pending answers (local and server) stale. */
let searchRequest = 0
/** Hits acted on (archived, moved, snoozed…) in this search: an archived or moved message keeps
 *  its id and still matches, so the re-reads leave them out until the search changes. */
let departed = new Set<number>()

/** `quiet`: a reread after an engine change, errors not shown. */
async function runSearch(request: number, quiet: boolean) {
  const q = query.value
  const targets = unified.value ? accounts.value.map(a => a.id) : accountId.value !== null ? [accountId.value] : []
  try {
    const lists = await Promise.all(targets.map(id => api.search(id, q, searchBins.value)))
    if (request === searchRequest) results.value = lists.flat().filter(m => !departed.has(m.id)).sort((a, b) => b.date - a.date)
  } catch (e) {
    if (request === searchRequest && !quiet) showError(e)
  }
}

// Immediate: a search from the URL (`?q=`) is set above, before this watcher exists.
watch([query, searchBins], ([q]) => {
  clearTimeout(timer)
  const request = ++searchRequest
  departed = new Set()
  serverHits.value = null
  // A server search still running is for the previous query: a new one may start.
  searchingServer.value = false
  if (!q.trim() || accountId.value === null) {
    results.value = null
    return
  }
  // `is:p`, `de:`…: an operator still being typed is not searched (the results stay as they are).
  if (incompleteOperator(q)) return
  timer = setTimeout(() => runSearch(request, false), 300)
}, { immediate: true })

/** After a change in the engine, the results are read again (archived hits leave, flags follow),
 *  one read at a time. The server hits stay: local copies of the same messages take precedence. */
let searchRefreshing = false
let searchStale = false
async function refreshSearch() {
  if (!results.value || !query.value.trim() || incompleteOperator(query.value)) return
  if (searchRefreshing) {
    searchStale = true
    return
  }
  searchRefreshing = true
  try {
    await runSearch(searchRequest, true)
  } finally {
    searchRefreshing = false
  }
  if (searchStale) {
    searchStale = false
    refreshSearch()
  }
}
onBeforeUnmount(() => clearTimeout(timer))

const accountLabel = (id: number) => (unified.value && accountCount.value > 1 ? accountNameOf(id) : null)

const searchList = computed(() => {
  if (!results.value) return null
  const seen = new Set<number>()
  return [...results.value, ...(serverHits.value ?? [])]
    .filter(m => !seen.has(m.id) && seen.add(m.id))
    .sort((a, b) => b.date - a.date)
})

const items = computed<ListItem[]>(() => {
  if (searchList.value) {
    return searchList.value.map(m => ({
      key: `m${m.id}`,
      accountId: m.account_id,
      mailboxId: m.mailbox_id,
      accountLabel: accountLabel(m.account_id),
      accountDot: accountColor(m.account_id).dot,
      threadId: m.thread_id,
      latestId: m.id,
      ids: [m.id],
      avatar: m.from,
      who: m.from ? peopleLabel([m.from], ownSet(m.account_id)) : t('common.unknown'),
      subject: m.subject,
      preview: m.preview,
      date: m.date,
      count: 1,
      unread: !m.flags.seen,
      flagged: m.flags.flagged,
      attachments: m.has_attachments,
      activity: m.date,
      snoozedUntil: null,
      woken: false,
      labels: m.labels?.map(id => labelNames.value.get(id)).filter((l): l is string => !!l)
    }))
  }
  return threads.value.map(t => ({
    key: `t${t.thread_id}`,
    accountId: t.account_id,
    mailboxId: t.mailbox_id,
    accountLabel: accountLabel(t.account_id),
    accountDot: accountColor(t.account_id).dot,
    threadId: t.thread_id,
    latestId: t.latest_id,
    ids: t.message_ids,
    avatar: outgoing.value ? (t.to[0] ?? t.from) : t.from,
    who: outgoing.value ? tr('mail.page.to', { who: peopleLabel(t.to, ownSet(t.account_id)) }) : peopleLabel(t.participants, ownSet(t.account_id)),
    subject: t.subject,
    preview: t.preview,
    date: t.date,
    count: t.count,
    unread: t.unread > 0,
    flagged: t.flagged,
    attachments: t.has_attachments,
    activity: t.activity,
    snoozedUntil: t.snoozed_until,
    woken: t.woken,
    labels: t.labels?.map(id => labelNames.value.get(id)).filter((l): l is string => !!l)
  }))
})

/** Label names by folder id, built once per folder list rather than looked up per thread. */
/** Kept while the names stay the same: the folder list is read again often during a sync. */
let names = new Map<number, string>()
const labelNames = computed(() => {
  const fresh = new Map(mailboxes.value.map(m => [m.id, m.role ? roleLabels[m.role] : m.display_name] as [number, string]))
  if (fresh.size !== names.size || [...fresh].some(([id, name]) => names.get(id) !== name)) names = fresh
  return names
})

const selected = ref(new Set<string>())
let anchor: string | null = null
const selectedItems = computed(() => items.value.filter(i => selected.value.has(i.key)))
const allSelected = computed(() => items.value.length > 0 && selected.value.size === items.value.length)

watch(items, (list) => {
  const keys = new Set(list.map(i => i.key))
  if ([...selected.value].some(k => !keys.has(k))) selected.value = new Set([...selected.value].filter(k => keys.has(k)))
})

function toggle(item: ListItem, range: boolean) {
  const next = new Set(selected.value)
  const keys = items.value.map(i => i.key)
  if (range && anchor && keys.includes(anchor)) {
    const from = keys.indexOf(anchor)
    const to = keys.indexOf(item.key)
    keys.slice(Math.min(from, to), Math.max(from, to) + 1).forEach(k => next.add(k))
  } else {
    if (next.has(item.key)) next.delete(item.key)
    else next.add(item.key)
    anchor = item.key
  }
  selected.value = next
}

function selectAll(value: boolean | 'indeterminate') {
  selected.value = value === true ? new Set(items.value.map(i => i.key)) : new Set()
}

const desktop = useMediaQuery('(min-width: 1024px)')
/** Mobile: list entry under a thread opened with a push (the Back button returns to the list). */
let pushedFrom: string | null = null

function listQuery(): Record<string, string | string[]> {
  const { thread: _thread, account: _account, box: _box, q: _q, ...rest } = route.query
  const out = Object.fromEntries(Object.entries(rest).filter(([, v]) => v != null)) as Record<string, string | string[]>
  if (query.value.trim()) out.q = query.value
  return out
}

/** The thread's account and folder go in the URL, so it shows even when not among the loaded
 *  rows (home link, filter change). */
function open(item: Pick<ListItem, 'threadId' | 'accountId' | 'mailboxId' | 'latestId'> | null) {
  // In Drafts, a click resumes writing instead of showing the message.
  if (item && mailbox.value?.role === 'drafts') {
    resume(item.latestId)
    return
  }
  // The path too: the route read here is this list's, not necessarily the current one.
  const list = { path: route.path, query: listQuery() }
  if (!item) {
    // Mobile: the thread was pushed over the list; going back keeps the history as it was.
    const back = pushedFrom
    pushedFrom = null
    if (back && window.history.state?.back === back) router.back()
    else router.replace(list)
    return
  }
  const target = { path: route.path, query: { ...list.query, ...threadRoute({ thread_id: item.threadId, account_id: item.accountId, mailbox_id: item.mailboxId }).query } }
  // Mobile, from the list: a new history entry, so the Back button returns to the list instead
  // of leaving the folder. From a thread already open (next, j/k), or on desktop: replaced.
  if (!desktop.value && threadId.value === null) {
    pushedFrom = router.resolve(list).fullPath
    router.push(target)
  } else router.replace(target)
}

/** The open thread left this folder (ThreadView) while another screen covers the list: closing
 *  it now would navigate away from that screen. Done once the list shows again, if still open. */
let pendingClose: number | null = null
function onThreadClose() {
  if (covered.value) pendingClose = threadId.value
  else open(null)
}
watch(covered, (now) => {
  if (now || pendingClose === null) return
  const id = pendingClose
  pendingClose = null
  if (threadId.value === id) open(null)
})

/** Follow-up (Sent only): opened in its own folder, not the one shown. */
function openFollowUp(f: FollowUp) {
  if (f.thread_id === null || f.mailbox_id === null) return
  navigateTo(threadRoute({ thread_id: f.thread_id, account_id: f.account_id, mailbox_id: f.mailbox_id }))
}

function more() {
  if (results.value || complete.value) return
  // A read in flight: the scroll handler fires on every event, one page at a time. A refresh in
  // flight keeps the request for when it lands.
  if (inflight === null) limit.value += PAGE
  else if (!inflightGrows) wantMore = true
}

/** The whole local window is shown: older messages can be asked from the server. */
const atLocalEnd = computed(() => !results.value && !unified.value && filter.value === 'all' && complete.value)
const loadingOlder = ref(false)
const historyComplete = ref(false)
/** The last attempt failed: wait for a click rather than retrying in a loop. */
const olderFailed = ref(false)
watch(mailboxId, () => {
  historyComplete.value = false
  olderFailed.value = false
})

async function loadOlder() {
  if (mailboxId.value === null || loadingOlder.value) return
  loadingOlder.value = true
  olderFailed.value = false
  try {
    const added = await api.loadOlder(mailboxId.value)
    if (!added) historyComplete.value = true
    // The list grew at its end, past what a merge rereads.
    else fullNext = true
  } catch (e) {
    olderFailed.value = true
    showError(e, t('mail.page.historyUnavailable'))
  } finally {
    loadingOlder.value = false
  }
}

/** Bottom of the list visible: older messages load by themselves, batch after batch while it
 *  stays visible (a batch too short to fill the screen calls the next). */
const olderSentinel = useTemplateRef<HTMLElement>('olderSentinel')
const sentinelVisible = ref(false)
useIntersectionObserver(olderSentinel, ([entry]) => {
  sentinelVisible.value = entry?.isIntersecting ?? false
}, { rootMargin: '200px' })
watch([sentinelVisible, atLocalEnd, loadingOlder], ([visible, atEnd, loading]) => {
  if (visible && atEnd && !loading && !historyComplete.value && !olderFailed.value) loadOlder()
})

async function searchOnServer() {
  const q = query.value.trim()
  if (!q || searchingServer.value) return
  const targets = unified.value ? accounts.value.map(a => a.id) : accountId.value !== null ? [accountId.value] : []
  const request = searchRequest
  searchingServer.value = true
  // Each account's hits show as soon as they come: a slow server (one folder after another)
  // holds back neither the others nor their results, and one failing hides nothing. Added to the
  // hits shown, not to a list of their own: those acted on meanwhile stay out.
  const outcomes = await Promise.allSettled(targets.map(id => api.searchServer(id, q, searchBins.value).then((hits) => {
    if (request !== searchRequest) return
    serverHits.value = [...(serverHits.value ?? []), ...hits.filter(m => !departed.has(m.id))]
  })))
  if (request !== searchRequest) return
  serverHits.value ??= []
  searchingServer.value = false
  const failed = outcomes.find(o => o.status === 'rejected')
  if (failed) showError(failed.reason, t('mail.page.serverSearchFailed'))
}

/** After archiving or deleting the open thread, move to the next one, like Gmail. Search results
 *  may hold several messages of one thread: its other entries are skipped, as are `skip`. */
function next(step = 1, skip: Set<number> = new Set()) {
  const list = items.value
  const current = openedItem.value
  const at = current ? list.indexOf(current) : -1
  const usable = (t: ListItem) => t.threadId !== threadId.value && !skip.has(t.threadId)
  if (at === -1) {
    if (threadId.value !== null && openedAt >= 0) {
      // The open thread left the list (a filter, archived elsewhere): step from where its row
      // was, the rows after it having moved up into its place.
      const from = Math.min(openedAt, list.length)
      const after = () => list.slice(from).find(usable) ?? null
      const before = () => list.slice(0, from).findLast(usable) ?? null
      open(step > 0 ? (after() ?? before()) : (before() ?? after()))
      return
    }
    open(list.find(usable) ?? null)
    return
  }
  const pick = (dir: number) => {
    for (let j = at + dir; j >= 0 && j < list.length; j += dir) if (usable(list[j]!)) return list[j]!
    return null
  }
  open(pick(step) ?? pick(-step))
}

/** Inbox (or unified): pinned threads first, as the engine sorts them (whatever the filter). */
const pinnedFirst = computed(() => !results.value && !doneView.value && (unified.value || mailbox.value?.role === 'inbox'))

/** If the open thread leaves the list, move to the next one, like Gmail.
 *  Returns `restore`, to call when the action fails: the hits of the given targets (all of them
 *  by default) come back into the search they were removed from. */
function leave(targets: ListItem[]): (failed?: ListItem[]) => void {
  const leaving = new Set(targets.map(t => t.threadId))
  if (threadId.value !== null && leaving.has(threadId.value)) next(1, leaving)
  // Search results leave at once too, and stay out of this search's re-reads: an archived,
  // moved or snoozed message still matches it.
  if (!results.value) return () => {}
  const request = searchRequest
  const before = { results: results.value, serverHits: serverHits.value }
  const gone = new Set(targets.flatMap(t => t.ids))
  for (const id of gone) departed.add(id)
  results.value = results.value.filter(m => !gone.has(m.id))
  if (serverHits.value) serverHits.value = serverHits.value.filter(m => !gone.has(m.id))
  return (failed = targets) => {
    // Another search since: its results are not these.
    if (request !== searchRequest || !results.value) return
    const back = new Set(failed.flatMap(t => t.ids))
    for (const id of back) departed.delete(id)
    // Put back only what is missing: a re-read since then may already have brought hits back.
    const missing = (now: MessageSummary[], then: MessageSummary[]) => {
      const present = new Set(now.map(m => m.id))
      return then.filter(m => back.has(m.id) && !present.has(m.id))
    }
    results.value = [...results.value, ...missing(results.value, before.results)]
    if (serverHits.value && before.serverHits) serverHits.value = [...serverHits.value, ...missing(serverHits.value, before.serverHits)]
  }
}

/** Sets a flag, then on the search hits shown: the server hits are never read again, so without
 *  this they would keep their old flags (and the next toggle would go the same way again). */
async function setFlag(ids: number[], flag: 'seen' | 'flagged', value: boolean) {
  await api.setFlag(ids, flag, value)
  const sent = new Set(ids)
  const patch = (list: MessageSummary[]) =>
    list.map(m => (sent.has(m.id) ? { ...m, flags: { ...m.flags, [flag]: value } } : m))
  if (results.value) results.value = patch(results.value)
  if (serverHits.value) serverHits.value = patch(serverHits.value)
}

/** Once the action is undone: its hits come back into the search as they were, then are read again. */
const undone = (restore: () => void) => () => {
  restore()
  refreshSearch()
}

async function act(kind: ListAction, targets: ListItem[]) {
  if (!targets.length) return
  const ids = targets.flatMap(t => t.ids)
  // Set by `leave`: on failure, the search hits removed up front come back.
  let restore: ((failed?: ListItem[]) => void) | undefined
  // Threads leave the list, maybe from below its first page, which the usual refresh rereads
  // alone: their rows go at once, then the first page is merged (rereading every row of a long
  // scrolled list after each action stalls triage). Undone, the list is read again in full.
  const left = () => {
    const gone = new Set(targets.map(t => `${t.accountId}-${t.threadId}-${t.mailboxId}`))
    threads.value = threads.value.filter(t => !gone.has(`${t.account_id}-${t.thread_id}-${t.mailbox_id}`))
    // A read in flight, from before the action, would bring the rows back.
    if (inflight) fullNext = true
    refresh()
  }
  try {
    switch (kind) {
      case 'snooze': {
        const until = await ask({ kind: 'snooze', title: targets.length > 1 ? t('mail.page.snoozeMany', { n: targets.length }) : t('mail.page.snoozeUntil') })
        if (until === null) return
        const back = leave(targets)
        restore = back
        done(await api.snooze(ids, until), t('mail.page.backOn', { when: untilLabel(until) }), 'i-lucide-alarm-clock', undone(back))
        left()
        selected.value = new Set()
        break
      }
      case 'unsnooze': {
        const back = leave(targets)
        // One thread at a time: when one fails, it and those after it were not unsnoozed.
        for (const [i, t] of targets.entries()) {
          restore = () => back(targets.slice(i))
          await api.unsnooze(t.accountId, t.threadId)
        }
        restore = undefined
        left()
        selected.value = new Set()
        break
      }
      case 'archive':
      case 'trash': {
        const back = leave(targets)
        restore = back
        const n = targets.length
        if (kind === 'archive' && doneView.value) {
          done(await api.toInbox(ids), t('mail.page.toInboxDone', n), 'i-lucide-inbox', undone(back))
        } else if (kind === 'archive') done(await api.archive(ids), t(unified.value ? 'mail.page.doneDone' : 'mail.page.archived', n), unified.value ? 'i-lucide-check' : 'i-lucide-archive', undone(back))
        else done(await api.trash(ids, mailbox.value?.role === 'trash'), t('mail.page.deleted', n), 'i-lucide-trash-2', undone(back))
        left()
        selected.value = new Set()
        break
      }
      case 'read':
        // Any unread in the batch: mark all read. Otherwise the latest message becomes unread again.
        if (targets.some(t => t.unread)) await setFlag(ids, 'seen', true)
        else await setFlag(targets.map(t => t.latestId), 'seen', false)
        break
      case 'flag':
        // Pinned threads sit first: the thread moves, possibly past the first page.
        fullNext = true
        if (targets.some(t => !t.flagged)) await setFlag(targets.map(t => t.latestId), 'flagged', true)
        else await setFlag(ids, 'flagged', false)
        break
    }
  } catch (e) {
    restore?.()
    showError(e)
  }
}

/** Row of the open thread. Search results may list several messages of the thread, from several
 *  folders: the one opened (its folder is in the URL) is preferred. */
const openedItem = computed(() => {
  if (threadId.value === null) return undefined
  const box = route.query.box ? Number(route.query.box) : null
  const same = items.value.filter(i => i.threadId === threadId.value)
  return same.find(i => i.mailboxId === box) ?? same[0]
})
/** Last index of the open thread's row, kept once it leaves the list (see `next`). */
let openedAt = -1
watch([openedItem, items], ([item, list]) => {
  if (threadId.value === null) openedAt = -1
  else if (item) openedAt = list.indexOf(item)
})
/** Account and folder of the open thread: from the URL (set on open), else the loaded row, else
 *  the folder shown. */
const opened = computed(() => {
  if (route.query.account && route.query.box) {
    return { accountId: Number(route.query.account), mailboxId: Number(route.query.box) }
  }
  if (openedItem.value) return { accountId: openedItem.value.accountId, mailboxId: openedItem.value.mailboxId }
  if (mailboxId.value !== null && accountId.value !== null) return { accountId: accountId.value, mailboxId: mailboxId.value }
  return null
})

/** Moving only makes sense to a folder of the same account. */
const selectionAccount = computed(() => {
  const ids = new Set(selectedItems.value.map(i => i.accountId))
  return ids.size === 1 ? [...ids][0]! : null
})

const moveItems = computed<DropdownMenuItem[]>(() =>
  mailboxes.value
    .filter(m => m.selectable && m.account_id === selectionAccount.value && m.id !== mailboxId.value)
    .map(m => ({
      label: m.role ? roleLabels[m.role] : m.display_name,
      icon: m.role ? roleIcons[m.role] : 'i-lucide-folder',
      onSelect: async () => {
        const targets = selectedItems.value
        if (!targets.length) return
        const restore = leave(targets)
        try {
          done(await api.moveTo(targets.flatMap(t => t.ids), m.id, mailboxId.value), t('mail.page.movedTo', { folder: m.role ? roleLabels[m.role] : m.display_name }), 'i-lucide-folder-input', undone(restore))
          selected.value = new Set()
        } catch (e) {
          restore()
          showError(e)
        }
      }
    }))
)

/** Gmail: the selection's account has labels of its own to put on the messages. */
const selectionLabels = computed(() => selectionAccount.value !== null && mailboxes.value.some(m => m.account_id === selectionAccount.value && m.user_label))
function labelsApplied(token: number | null, title: string) {
  if (typeof token === 'number') done(token, title, 'i-lucide-tag')
  useListStale().bump()
}

/** Search covers the whole account (not just the folder), or every account in the unified view. */
const searchPlaceholder = computed(() => {
  if (unified.value) return accounts.value.length > 1 ? t('mail.page.searchAll') : t('mail.page.search')
  const a = mailbox.value ? accountById(mailbox.value.account_id) : null
  return accounts.value.length > 1 && a ? t('mail.page.searchIn', { account: accountName(a) }) : t('mail.page.searchAccount')
})

/** Pinned or unpinned from the reading pane: the thread moves in the list. */
function onPinned() {
  fullNext = true
}

const free = () => !composing.value && !overlayOpen()
const bulk = (kind: ListAction) => () => free() && selected.value.size > 0 && act(kind, selectedItems.value)

defineShortcuts(computed(() => (covered.value ? {} : {
  j: () => free() && next(1),
  k: () => free() && next(-1),
  x: () => {
    const item = openedItem.value
    if (item && free()) toggle(item, false)
  },
  '/': () => free() && focusSearch(),
  // With a selection, these keys act on it rather than on the open thread.
  e: bulk('archive'),
  delete: bulk('trash'),
  s: bulk('flag'),
  b: bulk('snooze'),
  u: bulk('read')
})))
const typingIn = (t: EventTarget | null) => t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))
// Escape clears the selection. Not through defineShortcuts: its preventDefault would stop a
// dialog's or menu's own Escape (Reka skips a prevented one), and an Escape closing one of them
// must leave the selection alone.
// Captured, and prevented once used: the open thread's own Escape (close) skips a prevented one.
useEventListener('keydown', (e: KeyboardEvent) => {
  if (e.key !== 'Escape' || e.defaultPrevented || covered.value || overlayOpen() || typingIn(e.target)) return
  if (selected.value.size > 0) {
    selected.value = new Set()
    e.preventDefault()
  }
}, { capture: true })
// `#` deletes the selection. Not through defineShortcuts: on AZERTY under Windows it is typed with
// AltGr, reported as Ctrl+Alt, which a plain `#` shortcut never matches.
useEventListener('keydown', (e: KeyboardEvent) => {
  if (!isHashKey(e) || e.defaultPrevented || covered.value || typingIn(e.target)) return
  if (free() && selected.value.size > 0) {
    e.preventDefault()
    act('trash', selectedItems.value)
  }
})

/** Starts resizing the list with the panel's own handlers (read under both spellings Vue may hand
 *  them). The grip captures the pointer: over the message being read, an iframe, the window would
 *  otherwise stop hearing the mouse and the resize would stall. */
function resizeStart(e: PointerEvent, grip: Record<string, unknown>) {
  if (e.pointerType === 'touch' || e.button !== 0) return
  const start = grip.onMouseDown ?? grip['on-mouse-down']
  if (typeof start !== 'function') return
  const handle = e.currentTarget as HTMLElement
  handle.setPointerCapture(e.pointerId)
  handle.addEventListener('pointerup', () => handle.releasePointerCapture(e.pointerId), { once: true })
  // Nuxt UI follows the mouse events that come after; cancelling this pointer event (it calls
  // preventDefault on what it is given) would suppress them all.
  start({ clientX: e.clientX, preventDefault: () => {}, stopPropagation: () => e.stopPropagation() })
}

function resizeTouch(e: TouchEvent, grip: Record<string, unknown>) {
  const start = grip.onTouchStart ?? grip['on-touch-start']
  if (typeof start === 'function') start(e)
}
</script>

<template>
  <!-- `--list-bg`: list background, shared with the rows (cut-out around the unread dot) and the
       sticky date header. The list column tone on desktop; on mobile, the page tone of the mockups. -->
  <UDashboardPanel
    id="threads"
    :default-size="440"
    :min-size="300"
    :max-size="900"
    resizable
    class="bg-(--list-bg) [--list-bg:var(--surface-list)] max-lg:[--list-bg:var(--ui-color-neutral-100)] max-lg:dark:[--list-bg:var(--ui-color-neutral-950)]"
    :class="{ 'hidden lg:flex': threadId !== null }"
    :ui="{ body: 'gap-0 p-0 sm:gap-0 sm:p-0' }"
  >
    <!-- The grip reaches over the reading pane rather than the list: over the list it lies on
         its scrollbar, which then resized the panel (and reset it on a double click). -->
    <template #resize-handle="grip">
      <UDashboardResizeHandle
        aria-controls="threads"
        class="before:-left-0.5 before:-right-2"
        @pointerdown="resizeStart($event, grip as Record<string, unknown>)"
        @touchstart="resizeTouch($event, grip as Record<string, unknown>)"
      />
    </template>
    <template #header>
      <div class="flex flex-col gap-3 px-4 pt-4 pb-3 lg:border-b lg:border-default lg:pt-3.5">
        <MailListHeader
          v-model:filter="filter"
          :search-open="showSearch"
          :title="title"
          :unread="unread"
          :account-id="unified ? null : (mailbox?.account_id ?? null)"
          :unified="unified"
          :done-view="doneView"
          :show-filter="!results"
          @update:search-open="setSearchOpen"
        />

        <div v-if="selected.size" class="flex h-[38px] items-center gap-1">
          <UCheckbox
            :model-value="allSelected ? true : 'indeterminate'"
            :aria-label="$t('mail.page.selectAll')"
            @update:model-value="selectAll"
          />
          <span class="ms-2 truncate text-sm font-medium text-highlighted">{{ $t('mail.page.selectedCount', selected.size) }}</span>
          <div class="ms-auto flex items-center">
            <UTooltip :text="doneView ? $t('mail.page.toInbox') : unified ? $t('mail.page.markDone') : $t('mail.page.archive')" :kbds="['e']">
              <UButton
                :icon="doneView ? 'i-lucide-inbox' : unified ? 'i-lucide-check' : 'i-lucide-archive'"
                color="neutral"
                variant="ghost"
                :aria-label="doneView ? $t('mail.page.toInbox') : unified ? $t('mail.page.markDone') : $t('mail.page.archive')"
                @click="act('archive', selectedItems)"
              />
            </UTooltip>
            <UTooltip :text="$t('mail.page.trash')" :kbds="['#']">
              <UButton icon="i-lucide-trash-2" color="neutral" variant="ghost" :aria-label="$t('mail.page.trash')" @click="act('trash', selectedItems)" />
            </UTooltip>
            <UTooltip :text="$t('mail.page.readToggle')" :kbds="['u']">
              <UButton icon="i-lucide-mail-open" color="neutral" variant="ghost" :aria-label="$t('mail.page.readToggleAria')" @click="act('read', selectedItems)" />
            </UTooltip>
            <UTooltip :text="$t('mail.page.later')" :kbds="['b']">
              <UButton icon="i-lucide-alarm-clock" color="neutral" variant="ghost" :aria-label="$t('mail.page.later')" @click="act('snooze', selectedItems)" />
            </UTooltip>
            <UTooltip :text="$t('mail.page.pin')" :kbds="['s']">
              <UButton icon="i-lucide-pin" color="neutral" variant="ghost" :aria-label="$t('mail.page.pin')" @click="act('flag', selectedItems)" />
            </UTooltip>
            <UDropdownMenu v-if="selectionAccount !== null" :items="moveItems">
              <UButton icon="i-lucide-folder-input" color="neutral" variant="ghost" :aria-label="$t('mail.page.move')" />
            </UDropdownMenu>
            <LabelPicker v-if="selectionLabels" align="end" :account-id="selectionAccount!" :ids="selectedItems.flatMap(t => t.ids)" @applied="labelsApplied">
              <UTooltip :text="$t('mail.labels.title')">
                <UButton icon="i-lucide-tag" color="neutral" variant="ghost" :aria-label="$t('mail.labels.title')" />
              </UTooltip>
            </LabelPicker>
            <UTooltip :text="$t('mail.page.clearSelection')" :kbds="['escape']">
              <UButton icon="i-lucide-x" color="neutral" variant="ghost" :aria-label="$t('mail.page.clearSelection')" @click="selected = new Set()" />
            </UTooltip>
          </div>
        </div>
        <template v-else>
          <SearchField
            v-show="showSearch"
            ref="searchInput"
            v-model="query"
            :placeholder="searchPlaceholder"
            variant="soft"
            :account-id="unified ? null : (mailbox?.account_id ?? null)"
            class="w-full lg:block!"
            @escape="onSearchEscape"
          >
            <template #trailing="{ empty }">
              <UKbd v-if="empty" value="/" variant="outline" class="max-lg:hidden" />
            </template>
          </SearchField>
          <UTabs
            v-if="smartView && !query.trim()"
            v-model="smart.category.value"
            :items="categoryItems"
            :content="false"
            size="xs"
            variant="link"
            class="-mb-3"
            :ui="{ list: 'w-full', trigger: 'flex-1 justify-center max-lg:min-h-11' }"
          />
          <USwitch
            v-if="query.trim()"
            v-model="searchBins"
            size="xs"
            :label="$t('mail.page.includeBins')"
            :ui="{ label: 'text-xs text-muted' }"
          />
        </template>
      </div>
      <AccountAlerts />
      <FollowUpsBanner
        v-if="pinnedFirst && filter === 'all'"
        :account-id="unified ? null : (mailbox?.account_id ?? null)"
        @open="openFollowUp"
      />
      <ScreenerBanner
        v-if="(pinnedFirst || (mailbox?.role === 'screener' && !results)) && filter === 'all'"
        :account-id="unified ? null : (mailbox?.account_id ?? null)"
      />
    </template>

    <template #body>
      <!-- First read of a new view: blank rather than a wrong "empty" message. -->
      <div v-if="!items.length && listLoading && !results" class="h-full" />
      <div v-else-if="!items.length" class="flex h-full flex-col items-center justify-center gap-2 p-4 text-sm text-muted">
        <UIcon :name="results ? 'i-lucide-search-x' : filter === 'all' && !unified ? 'i-lucide-inbox' : 'i-lucide-check-check'" class="size-8" />
        <UButton
          v-if="results && (!serverHits || searchingServer)"
          :label="$t('mail.page.searchServer')"
          icon="i-lucide-cloud"
          size="sm"
          color="neutral"
          variant="outline"
          :loading="searchingServer"
          class="order-last"
          @click="searchOnServer"
        />
        <p>
          {{ $t(results ? (serverHits && !searchingServer ? 'mail.page.empty.noResultsServer' : 'mail.page.empty.noResultsCache')
            : syncingHere ? 'common.syncing'
              : filter === 'unread' ? 'mail.page.empty.allRead'
                : filter === 'flagged' ? 'mail.page.empty.noPinned'
                  : doneView ? 'mail.page.empty.noDone'
                  : unified ? 'mail.page.empty.allDone'
                    : mailbox?.role === 'snoozed' ? 'mail.page.empty.nothingSnoozed'
                    : mailbox?.role === 'screener' ? 'mail.page.empty.noNewSenders'
                    : mailbox?.role === 'inbox' ? 'mail.page.empty.inboxEmpty' : 'mail.page.empty.noMessages') }}
        </p>
      </div>
      <ThreadList
        v-else
        :items="items"
        :opened="threadId"
        :selected="selected"
        :done-mode="unified"
        :restore-mode="doneView"
        :pinned-first="pinnedFirst"
        :label="title"
        @step="(dir) => next(dir)"
        @open="open"
        @toggle="toggle"
        @action="(kind, item) => act(kind, [item])"
        @more="more"
      >
        <template #footer>
          <div v-if="results" class="flex justify-center p-4">
            <UButton
              v-if="!serverHits || searchingServer"
              :label="$t('mail.page.searchServerToo')"
              icon="i-lucide-cloud"
              size="sm"
              color="neutral"
              variant="outline"
              :loading="searchingServer"
              @click="searchOnServer"
            />
            <p v-else class="text-xs text-muted">{{ $t('mail.page.serverSearched', serverHits.length) }}</p>
          </div>
          <div v-else-if="atLocalEnd" ref="olderSentinel" class="flex justify-center p-4">
            <p v-if="historyComplete" class="text-xs text-muted">{{ $t('mail.page.historyComplete') }}</p>
            <p v-else-if="!olderFailed" class="flex items-center gap-2 text-xs text-muted">
              <UIcon name="i-lucide-loader-circle" class="size-3.5 animate-spin" /> {{ $t('mail.page.loadingOlder') }}
            </p>
            <UButton
              v-else
              :label="$t('mail.page.retryOlder')"
              icon="i-lucide-history"
              size="sm"
              color="neutral"
              variant="outline"
              :loading="loadingOlder"
              @click="loadOlder"
            />
          </div>
        </template>
      </ThreadList>
    </template>
  </UDashboardPanel>

  <ThreadView
    v-if="threadId !== null && opened"
    :key="`${opened.accountId}-${view}`"
    :account-id="opened.accountId"
    :thread-id="threadId"
    :mailbox-id="opened.mailboxId"
    :done-mode="unified"
    :restore-mode="doneView"
    :shortcuts="selected.size === 0 && !covered"
    @close="onThreadClose"
    @done="next()"
    @pinned="onPinned"
  />
  <UDashboardPanel v-else id="empty" class="hidden lg:flex">
    <template #body>
      <div class="flex h-full flex-col items-center justify-center gap-2 text-dimmed">
        <UIcon name="i-lucide-mail-open" class="size-10" />
        <p class="text-sm">{{ $t('mail.page.selectConversation') }}</p>
        <p class="text-xs"><UKbd value="j" /> <UKbd value="k" /> {{ $t('mail.page.hint.navigate') }} · <UKbd value="x" /> {{ $t('mail.page.hint.select') }} · <UKbd value="c" /> {{ $t('mail.page.hint.compose') }}</p>
      </div>
    </template>
  </UDashboardPanel>
</template>
