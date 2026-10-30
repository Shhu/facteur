<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import { createReusableTemplate, useEventListener, useMediaQuery } from '@vueuse/core'
import type { DropdownMenuItem } from '@nuxt/ui'
import type { FoundEvent, Mailbox, MessageDetail, MessageSummary, Outgoing } from '~/utils/types'
import type { Leaving } from '~/utils/listRows'

const props = withDefaults(
  defineProps<{ accountId: number, threadId: number, mailboxId: number, shortcuts?: boolean, doneMode?: boolean, restoreMode?: boolean }>(),
  { shortcuts: true, doneMode: false, restoreMode: false }
)
/** `done`: the thread left this folder (archived, deleted, moved). `pinned`: pinned or unpinned
 *  (it moves in the list). */
const emit = defineEmits<{ close: [], done: [], pinned: [] }>()

const { tick, mailboxes, isMine, accountById, accountColor } = useMail()
const { reply, forward, open: composing } = useComposer()
const showError = useErrorToast()
const listStale = useListStale()
const { t } = useI18n()

const messages = ref<MessageSummary[]>([])
const details = reactive(new Map<number, MessageDetail>())
const loadingIds = reactive(new Set<number>())
const expanded = reactive(new Set<number>())

/** Read once per loaded message: its whole HTML is parsed, and the thread reloads at each sync. */
const invites = new WeakMap<MessageDetail, ReturnType<typeof inviteState>>()
function inviteOf(d: MessageDetail & { invite: NonNullable<MessageDetail['invite']> }) {
  let state = invites.get(d)
  if (!state) {
    state = inviteState(d.invite, d.html)
    invites.set(d, state)
  }
  return state
}

/** Invitation thread: from the latest loaded ICS, else guessed from the subject until it loads. */
const invitation = computed(() => {
  for (const m of [...messages.value].reverse()) {
    const d = details.get(m.id)
    if (d?.invite) return { ...inviteOf(d as MessageDetail & { invite: NonNullable<MessageDetail['invite']> }), title: d.invite.summary }
  }
  const first = messages.value[0]
  const guess = first ? inviteFromSubject(first.subject, first.has_attachments) : null
  return guess ? { kind: guess.kind, change: null, title: guess.title } : null
})
const subject = computed(() => invitation.value?.title || messages.value[0]?.subject || t('common.noSubject'))
const invitationLabel = computed(() => {
  switch (invitation.value?.kind) {
    case 'update': return t('mail.invite.header.updated')
    case 'cancelled': return t('mail.invite.header.cancelled')
    case 'reply': return t('mail.invite.reply')
    case 'invitation': return t('mail.invite.invitation')
    default: return null
  }
})
const latest = computed(() => messages.value.at(-1))
/** The latest of the folder's copies: marked unread or pinned, it is what the list row shows
 *  (the reply sent from here sits in Sent). */
const latestHere = computed(() => latestIn(messages.value, props.mailboxId))

const { order } = useThreadOrder()
/** Unread when the thread was opened: they stay unfolded once marked read. */
const unreadAtOpen = reactive(new Set<number>())
const unfolded = ref(false)
type Row = { kind: 'message', message: MessageSummary } | { kind: 'folded', count: number }
/** What the thread shows: the first message, the last two, the open and the unread ones; runs of
 *  at least 3 others between them fold into one row. Newest first, the same rows reversed. */
const rows = computed<Row[]>(() => {
  const list = messages.value
  const out: Row[] = []
  if (unfolded.value) out.push(...list.map(message => ({ kind: 'message' as const, message })))
  else {
    const keep = new Set<number>([list[0]?.id, ...list.slice(-2).map(m => m.id)].filter((id): id is number => id !== undefined))
    for (const m of list) if (expanded.has(m.id) || unreadAtOpen.has(m.id)) keep.add(m.id)
    let run: MessageSummary[] = []
    const flush = () => {
      if (run.length >= 3) out.push({ kind: 'folded', count: run.length })
      else out.push(...run.map(message => ({ kind: 'message' as const, message })))
      run = []
    }
    for (const m of list) {
      if (keep.has(m.id)) {
        flush()
        out.push({ kind: 'message', message: m })
      } else run.push(m)
    }
    flush()
  }
  return order.value === 'newest' ? out.reverse() : out
})

const body = useTemplateRef<HTMLElement>('body')
function scrollTo(id: number | undefined) {
  if (id === undefined) return
  nextTick(() => requestAnimationFrame(() => {
    body.value?.querySelector(`[data-message-id="${id}"]`)?.scrollIntoView({ block: 'start' })
  }))
}
/** Message answered by reply, reply all and forward: the latest one, a draft aside (a draft
 *  saved in the thread is not what one replies to). */
const drafts = computed(() => draftIds(messages.value, mailboxes.value))
const replyTarget = computed(() => replyTargetIn(messages.value, drafts.value))
/** Another thread was asked for and its messages aren't in yet: `messages` still holds the
 *  previous one, which no action (button or shortcut) may target meanwhile. */
const resetPending = ref(false)
/** Actions apply to the thread's messages present in the folder shown. */
const here = computed(() => resetPending.value ? [] : messages.value.filter(m => m.mailbox_id === props.mailboxId).map(m => m.id))
const flagged = computed(() => !resetPending.value && messages.value.some(m => m.flags.flagged))

/** Bumped on every thread change: makes responses in flight stale. */
let generation = 0
/** An action of ours takes the thread out of this folder: the page moves to the next thread, the
 *  reload that sees it gone must not close the pane meanwhile. */
let leaving = false
/** Latest thread read requested: an older response arriving later is ignored. */
let threadRequest = 0

// The engine may weigh a message's trust again once it is open (its own DKIM check).
const { refined: refinedTrust, forget: forgetRefinedTrust } = useRefinedTrust()
function applyRefinedTrust(detail: MessageDetail) {
  const trust = refinedTrust.value[detail.summary.id]
  if (!trust) return
  detail.trust = trust
  // Remote images are never loaded for an untrustworthy message.
  if (trust.level === 'low') detail.images_allowed = false
}
watch(refinedTrust, () => details.forEach(applyRefinedTrust))

/** A message read before, read again in place (no spinner meanwhile); dropped if not open. */
async function refreshDetail(id: number) {
  if (!details.has(id)) return
  if (!expanded.has(id)) {
    details.delete(id)
    return
  }
  const gen = generation
  try {
    const detail = await api.message(id)
    applyRefinedTrust(detail)
    if (gen === generation) details.set(id, detail)
  } catch {
    // Gone meanwhile (a draft sent and deleted): the next reload drops its card.
  }
}

async function loadDetail(id: number) {
  if (details.has(id) || loadingIds.has(id)) return
  const gen = generation
  loadingIds.add(id)
  forgetRefinedTrust(id)
  try {
    const detail = await api.message(id)
    applyRefinedTrust(detail)
    // Arrived after a thread change: no longer belongs to what is shown.
    if (gen === generation) details.set(id, detail)
  } catch (e) {
    if (gen === generation) showError(e, t('mail.threadView.openFailed'))
  } finally {
    if (gen === generation) loadingIds.delete(id)
  }
}

function toggle(id: number) {
  if (expanded.has(id)) expanded.delete(id)
  else {
    expanded.add(id)
    loadDetail(id)
  }
}

/** Ids of the last list read: a reload tells the messages that arrived since. */
const knownIds = new Set<number>()

/** Replies that arrived while the thread was out of sight (window hidden or unfocused, another
 *  screen over the list): marked read once it is seen again, not before. */
const pendingRead = new Set<number>()
const { covered } = useMailOverlay()
const onScreen = () => document.visibilityState === 'visible' && document.hasFocus() && !covered.value
function flushPendingRead() {
  if (!pendingRead.size || !onScreen()) return
  const ids = [...pendingRead]
  pendingRead.clear()
  markRead(ids)
}
/** Read on the server, and in the list's rows (those past its first page are not reread). */
function markRead(ids: number[]) {
  api.setFlag(ids, 'seen', true).then(() => listStale.markSeen(ids, true), showError)
}
useEventListener(window, 'focus', flushPendingRead)
useEventListener(document, 'visibilitychange', flushPendingRead)
watch(covered, flushPendingRead)

async function load(reset: boolean) {
  const request = ++threadRequest
  if (reset) {
    generation++
    // Not applied yet: the response kept, even a reload's, applies it.
    resetPending.value = true
    loadingIds.clear()
    // Deferred reads belong to the previous thread.
    pendingRead.clear()
  }
  let list: MessageSummary[]
  try {
    list = await api.thread(props.accountId, props.threadId, props.mailboxId)
  } catch (e) {
    // A stale read's failure no longer concerns the thread shown.
    if (request === threadRequest) throw e
    return
  }
  if (request !== threadRequest) return
  const hadHere = here.value.length > 0
  const wasReset = resetPending.value
  const arrived = wasReset ? [] : list.filter(m => !knownIds.has(m.id))
  knownIds.clear()
  for (const m of list) knownIds.add(m.id)
  // Unchanged messages keep their object, and an unchanged thread is not assigned: every card
  // would render again at each sync tick.
  const old = new Map(messages.value.map(m => [m.id, m]))
  const sign = (m: MessageSummary) => JSON.stringify(m)
  const kept = list.map((m) => {
    const was = old.get(m.id)
    return was && sign(was) === sign(m) ? was : m
  })
  const stale = wasReset ? [] : staleDetails(messages.value, list, draftIds(list, mailboxes.value))
  if (wasReset || kept.length !== messages.value.length || kept.some((m, i) => m !== messages.value[i])) messages.value = kept
  for (const id of stale) refreshDetail(id)
  if (wasReset) {
    resetPending.value = false
    details.clear()
    expanded.clear()
    unreadAtOpen.clear()
    unfolded.value = false
    for (const m of list) if (!m.flags.seen) unreadAtOpen.add(m.id)
    // Open by default: the latest message and the latest unread one (not every unread message:
    // each may carry large inline images).
    const lastUnread = list.findLast(m => !m.flags.seen)
    for (const m of [lastUnread, list.at(-1), replyTarget.value]) if (m) expanded.add(m.id)
    expanded.forEach(loadDetail)
    // Opens on the first unread message, else on the latest one (at the top when newest first).
    const firstUnread = list.find(m => !m.flags.seen)
    scrollTo(order.value === 'newest' ? (lastUnread ?? list.at(-1))?.id : (firstUnread ?? list.at(-1))?.id)
    const unread = list.filter(m => !m.flags.seen).map(m => m.id)
    if (unread.length) markRead(unread)
  } else if (hadHere && !here.value.length && !leaving) {
    // Its messages left this folder from elsewhere (dragged to a folder, another device):
    // nothing left to act on here.
    emit('close')
  } else if (arrived.length) {
    // A reply arrived while the thread is open: shown unfolded, and read if it is on screen (else
    // once the thread is seen again).
    for (const m of arrived) {
      expanded.add(m.id)
      if (!m.flags.seen) unreadAtOpen.add(m.id)
      loadDetail(m.id)
    }
    for (const m of arrived) if (!m.flags.seen) pendingRead.add(m.id)
    flushPendingRead()
  }
}

/** A failed reload is shown once (it may fail on every tick), until one succeeds again. */
let reloadFailed = false
watch(() => props.threadId, () => {
  leaving = false
  load(true).catch(showError)
  loadMuted()
}, { immediate: true })

/** A reply leaves after the undo delay, then reaches the conversation once the Sent folder is
 *  read again: seconds in all. Meanwhile it shows here, from the outbox, and stays once sent
 *  until its real message arrives (a minute at most). */
const outbox = useOutbox()
const leaving_ = ref<Leaving[]>([])
const clock = ref(Date.now())
let clockTimer: ReturnType<typeof setInterval> | undefined
onBeforeUnmount(() => clearInterval(clockTimer))
const threadIds = computed<Set<number>>((before) => {
  const ids = new Set(messages.value.map(m => m.id))
  return before && before.size === ids.size && [...ids].every(i => before.has(i)) ? before : ids
})
function reconcile(items: Outgoing[], ids: Set<number>) {
  const now = Date.now()
  const { next, clock: waiting } = reconcileLeaving(leaving_.value, items, ids, now)
  leaving_.value = next
  clock.value = now
  // Kept running while a sent reply waits (restarted at every change, it would never tick), and
  // stopped once none does.
  if (!waiting) {
    clearInterval(clockTimer)
    clockTimer = undefined
  } else if (!clockTimer) {
    clockTimer = setInterval(() => reconcile(outbox.items.value, threadIds.value), 2000)
  }
}
watch([() => outbox.items.value, threadIds], ([items, ids]) => reconcile(items, ids), { immediate: true })
watch(() => props.threadId, () => {
  leaving_.value = []
})
/** Shown: on its way, or sent and not yet arrived (a message of mine in the conversation since). */
const pendingReplies = computed(() => leaving_.value.filter((l) => {
  if (l.sentAt === null) return true
  if (clock.value - l.sentAt > SENT_SHOWN) return false
  const since = l.item.send_at - 60
  return !messages.value.some(m => m.date >= since && isMine(m.from?.email, props.accountId))
}))
/** The reply's own text: up to the quote of the message it answers. */
function ownText(preview: string) {
  const lines = preview.split('\n')
  const cut = lines.findIndex((l, i) => l.startsWith('>') || (/(a écrit|wrote)\s*:\s*$/.test(l) && (lines[i + 1] ?? '').startsWith('>')))
  return (cut >= 0 ? lines.slice(0, cut) : lines).join('\n').trim()
}
const leavingLabel = (l: Leaving) => l.sentAt !== null
  ? t('mail.threadView.outgoing.sent')
  : l.item.state === 'failed' ? t('mail.threadView.outgoing.failed') : l.item.state === 'sending' ? t('mail.threadView.outgoing.sending') : t('mail.threadView.outgoing.pending')

const muted = ref(false)
async function loadMuted() {
  const thread = props.threadId
  const value = await api.threadMuted(props.accountId, thread).catch(() => false)
  if (thread === props.threadId) muted.value = value
}
/** Muting archives what is here (the thread leaves the folder); unmuting stays. */
function toggleMuted() {
  if (resetPending.value) return
  if (muted.value) {
    run(async () => {
      await api.muteThread(props.accountId, props.threadId, false, [])
      muted.value = false
    }, false, t('mail.threadView.unmuted'), 'i-lucide-bell')
    return
  }
  const ids = here.value
  run(async () => {
    const token = await api.muteThread(props.accountId, props.threadId, true, ids)
    muted.value = true
    if (ids.length) listStale.bump()
    return token
  }, ids.length > 0, t('mail.threadView.muted'), 'i-lucide-bell-off')
}

// One reload at a time: a tick during a reload runs it again once it lands.
let reloading = false
let reloadAgain = false
async function reload() {
  if (reloading) {
    reloadAgain = true
    return
  }
  reloading = true
  try {
    await load(false)
    reloadFailed = false
  } catch (e) {
    // A temporary error: the thread stays as it was.
    if (!reloadFailed) showError(e, t('mail.threadView.openFailed'))
    reloadFailed = true
  } finally {
    reloading = false
  }
  if (reloadAgain) {
    reloadAgain = false
    reload()
  }
}
// A sync ticks a few times a second: the open thread is read again at most once a second, and not
// while another screen covers it.
const tickReloader = useReloader(() => reload(), 1000)
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

const { done } = useUndo()

/** `label`: the action can be undone, its toast offers it. */
/** The thread a leaving action is under way on: a second one on it (a double `e`) would act on
 *  the same messages. Another thread opened meanwhile is free. */
let acting: number | null = null
/** `thread`: the thread acted on, when taken before a picker (another may be shown by then). */
async function run(action: () => Promise<number | null | void>, leaves = true, label?: string, icon?: string, thread = props.threadId) {
  if (leaves && acting === thread) return
  // Another thread opened meanwhile is not the one left: the list does not move past it.
  try {
    if (leaves) {
      leaving = true
      acting = thread
    }
    const token = await action()
    if (leaves && props.threadId === thread) emit('done')
    if (label && typeof token === 'number') done(token, label, icon)
  } catch (e) {
    leaving = false
    showError(e)
  } finally {
    if (leaves && acting === thread) acting = null
  }
}

/** The thread's row in the list, taken before an action awaits: another may be open by then. */
const place = () => ({ account_id: props.accountId, thread_id: props.threadId, mailbox_id: props.mailboxId })

function onHere(action: (ids: number[]) => Promise<number | null | void>, label?: string, icon?: string) {
  const ids = here.value
  if (!ids.length) return Promise.resolve()
  const row = place()
  return run(async () => {
    const token = await action(ids)
    listStale.bump(row)
    return token
  }, true, label, icon)
}

/** The thread's messages, whose labels the menu changes (the engine leaves those in the bin). */
const labelIds = computed(() => resetPending.value ? [] : messages.value.map(m => m.id))
const hasLabels = computed(() => mailboxes.value.some(m => m.account_id === props.accountId && m.user_label))
const labelsOpen = ref(false)
const threadLabels = ref<Mailbox[]>([])
let labelsRequest = 0
async function readLabels() {
  const request = ++labelsRequest
  if (!hasLabels.value || !labelIds.value.length) {
    threadLabels.value = []
    return
  }
  const found = await api.messageLabels(labelIds.value).catch(() => null)
  if (!found || request !== labelsRequest) return
  const on = new Set(Object.values(found).flat())
  threadLabels.value = mailboxes.value
    .filter(m => m.user_label && on.has(m.id))
    .sort((a, b) => a.display_name.localeCompare(b.display_name))
}
watch(() => `${labelIds.value.join()}|${hasLabels.value}`, readLabels, { immediate: true })
const labelsRow = useTemplateRef<HTMLElement>('labelsRow')
/** Labels sit under the subject in the desktop header, at the top of the reading on mobile. */
const wide = useMediaQuery('(min-width: 1024px)')
const [DefineLabels, LabelsRow] = createReusableTemplate()
/** Labels have no colour of their own: one from the accounts' palette, steady for a name. */
function labelColor(label: Mailbox) {
  let h = 0
  for (const c of label.display_name) h = (h * 31 + c.charCodeAt(0)) >>> 0
  return ACCOUNT_COLORS[ACCOUNT_COLOR_NAMES[h % ACCOUNT_COLOR_NAMES.length]!]
}
/** From the keyboard: the row (scrolled away towards the latest message) comes back in view first,
 *  for the menu to open under its button. */
async function openLabels() {
  if (!hasLabels.value || !labelIds.value.length) return
  labelsRow.value?.scrollIntoView({ block: 'nearest' })
  await nextTick()
  labelsOpen.value = true
}
function labelsApplied(token: number | null, title: string) {
  if (typeof token === 'number') done(token, title, 'i-lucide-tag')
  listStale.bump()
  readLabels()
}
const removeLabel = (label: Mailbox) => run(async () => {
  const token = await api.setLabels(labelIds.value, [], [label.id])
  listStale.bump()
  await readLabels()
  return token
}, false, t('mail.labels.removed', { label: label.display_name }), 'i-lucide-tag')

const archiveLook = computed(() => props.restoreMode
  ? { label: t('mail.threadView.toInbox'), icon: 'i-lucide-inbox' }
  : props.doneMode ? { label: t('mail.threadView.markDone'), icon: 'i-lucide-check' } : { label: t('mail.threadView.archive'), icon: 'i-lucide-archive' })
const archive = () => (props.restoreMode
  ? onHere(api.toInbox, t('mail.threadView.movedToInbox'), 'i-lucide-inbox')
  : onHere(api.archive, props.doneMode ? t('mail.threadView.doneDone') : t('mail.threadView.archived'), archiveLook.value.icon))
const inTrash = computed(() => mailboxes.value.find(m => m.id === props.mailboxId)?.role === 'trash')
const trash = () => onHere(ids => api.trash(ids, inTrash.value), t('mail.threadView.deleted'), 'i-lucide-trash-2')
const markUnread = () => run(async () => {
  if (latestHere.value && !resetPending.value) {
    const id = latestHere.value.id
    await api.setFlag([id], 'seen', false)
    listStale.markSeen([id], false)
  }
  emit('close')
}, false)
const { ask } = useWhenPicker()
const inSnoozed = computed(() => mailboxes.value.find(m => m.id === props.mailboxId)?.role === 'snoozed')
async function snooze() {
  // Taken before the picker: the thread shown may change while it is open.
  const ids = here.value
  if (!ids.length) return
  const row = place()
  const until = await ask({ kind: 'snooze', title: t('mail.threadView.snoozeUntil') })
  if (until === null) return
  await run(async () => {
    const token = await api.snooze(ids, until)
    listStale.bump(row)
    return token
  }, true, t('mail.threadView.backOn', { when: untilLabel(until) }), 'i-lucide-alarm-clock', row.thread_id)
}
const unsnooze = () => {
  if (resetPending.value) return
  const row = place()
  return run(async () => {
    await api.unsnooze(row.account_id, row.thread_id)
    listStale.bump(row)
  })
}
// Pin marks the latest message; unpin clears every pinned message of the thread (the pin may
// sit on an older one, e.g. pinned before a reply arrived).
const toggleFlag = () => resetPending.value ? undefined : run(() => {
  emit('pinned')
  if (flagged.value) return api.setFlag(messages.value.filter(m => m.flags.flagged).map(m => m.id), 'flagged', false)
  return latestHere.value ? api.setFlag([latestHere.value.id], 'flagged', true) : Promise.resolve()
}, false)

const inJunk = computed(() => mailboxes.value.find(m => m.id === props.mailboxId)?.role === 'junk')
const sender = computed(() => [...messages.value].reverse().find(m => m.from && !isMine(m.from.email, props.accountId))?.from ?? null)
const inScreener = computed(() => mailboxes.value.find(m => m.id === props.mailboxId)?.role === 'screener')
const SCREEN_DONE = {
  inbox: { key: 'mail.screener.accepted', icon: 'i-lucide-user-round-check' },
  block: { key: 'mail.screener.refused', icon: 'i-lucide-user-round-x' },
  delete: { key: 'mail.screener.refusedDeleted', icon: 'i-lucide-trash-2' }
}
const refuseItems = computed(() => [
  { label: t('mail.screener.refuseDelete'), icon: 'i-lucide-trash-2', onSelect: () => screenSender('delete') }
])
function screenSender(verdict: keyof typeof SCREEN_DONE) {
  const from = sender.value
  if (!from) return
  const name = from.name?.trim() || from.email
  run(
    () => api.screenSender(props.accountId, from.email, verdict, false),
    true,
    t(SCREEN_DONE[verdict].key, { name }),
    SCREEN_DONE[verdict].icon
  )
}

/** Local AI, when turned on: the thread summed up, an appointment made an event. */
const { ready: aiReady } = useAi()
const toast = useToast()
const summary = ref<{ thread: number, text: string | null, error: string | null } | null>(null)
watch(() => props.threadId, () => {
  summary.value = null
  findingEvent.value = false
  suggestion.value = null
})
async function summarize() {
  const thread = props.threadId
  if (summary.value?.thread === thread && !summary.value.error) return
  summary.value = { thread, text: null, error: null }
  try {
    const text = await api.aiSummarize(props.accountId, thread)
    if (summary.value?.thread === thread) summary.value = { thread, text, error: null }
  } catch (e) {
    if (summary.value?.thread === thread) summary.value = { thread, text: null, error: String(e) }
  }
}
const eventFormOpen = ref(false)
const suggestion = ref<FoundEvent | null>(null)
const findingEvent = ref(false)
async function eventFromMessage() {
  // The latest message received (not one of our replies): the one proposing the appointment.
  const model = [...messages.value].reverse().find(m => !isMine(m.from?.email, props.accountId)) ?? latest.value
  if (!model || findingEvent.value) return
  const thread = props.threadId
  findingEvent.value = true
  try {
    const found = await api.aiFindEvent(model.id)
    // Another thread shown meanwhile: the answer was for the one left.
    if (props.threadId !== thread) return
    if (!found) {
      toast.add({ title: t('mail.threadView.ai.noEvent'), icon: 'i-lucide-calendar-x' })
      return
    }
    if (found.start === null) toast.add({ title: t('mail.threadView.ai.checkDate'), description: found.when ?? undefined, icon: 'i-lucide-calendar-search' })
    suggestion.value = found
    eventFormOpen.value = true
  } catch (e) {
    if (props.threadId === thread) showError(e)
  } finally {
    if (props.threadId === thread) findingEvent.value = false
  }
}
const aiItems = computed<DropdownMenuItem[]>(() => (aiReady.value
  ? [
      { label: t('mail.threadView.ai.summarize'), icon: 'i-lucide-sparkles', onSelect: summarize },
      { label: t('mail.threadView.ai.event'), icon: 'i-lucide-calendar-plus', onSelect: eventFromMessage }
    ]
  : []))

const summarizing = computed(() => summary.value?.thread === props.threadId && !summary.value.text && !summary.value.error)

const moreItems = computed<DropdownMenuItem[][]>(() => [
  [
    muted.value
      ? { label: t('mail.threadView.unmute'), icon: 'i-lucide-bell', onSelect: toggleMuted }
      : { label: t('mail.threadView.mute'), icon: 'i-lucide-bell-off', onSelect: toggleMuted }
  ],
  [
    inJunk.value
      ? { label: t('mail.threadView.notJunk'), icon: 'i-lucide-shield-check', onSelect: () => onHere(api.notSpam, t('mail.threadView.movedToInbox'), 'i-lucide-inbox') }
      : { label: t('mail.threadView.reportJunk'), icon: 'i-lucide-shield-alert', onSelect: () => onHere(api.reportSpam, t('mail.threadView.reportedJunk'), 'i-lucide-shield-alert') }
  ],
  [{
    label: t('mail.threadView.deleteSimilar'),
    icon: 'i-lucide-list-x',
    // The latest received message (not one of our replies) serves as the template.
    onSelect: () => {
      const model = [...messages.value].reverse().find(m => !isMine(m.from?.email, props.accountId)) ?? latest.value
      if (model) navigateTo(`/cleanup?message=${model.id}`)
    }
  }],
  sender.value
    ? [{
        label: t('mail.threadView.createRule'),
        icon: 'i-lucide-workflow',
        onSelect: () => navigateTo(`/rules?from=${encodeURIComponent(sender.value!.email)}&account=${props.accountId}`)
      }, {
        label: t('mail.threadView.block', { who: addressLabel(sender.value) }),
        icon: 'i-lucide-ban',
        color: 'error' as const,
        onSelect: () => blockSender(sender.value!.email)
      }]
    : []
])

async function blockSender(email: string) {
  const ok = await confirm(t('mail.threadView.blockConfirm', { email }), {
    title: t('mail.threadView.blockTitle'),
    kind: 'warning'
  })
  // Its inbox messages go to the junk folder: the thread leaves the list only when shown from the
  // inbox, and the rows past the first page go too.
  const leaves = mailboxes.value.find(m => m.id === props.mailboxId)?.role === 'inbox' && here.value.length > 0
  if (ok) {
    await run(async () => {
      await api.blockSender(email)
      listStale.bump()
    }, leaves)
  }
}

const moveItems = computed<DropdownMenuItem[]>(() =>
  mailboxes.value
    .filter(m => m.selectable && m.account_id === props.accountId && m.id !== props.mailboxId)
    .map(m => ({
      label: m.role ? roleLabels[m.role] : m.display_name,
      icon: m.role ? roleIcons[m.role] : 'i-lucide-folder',
      onSelect: () => onHere(ids => api.moveTo(ids, m.id, props.mailboxId), t('mail.threadView.movedTo', { folder: m.role ? roleLabels[m.role] : m.display_name }), 'i-lucide-folder-input')
    }))
)

/** Mobile: the top bar keeps the AI, archive and delete; the other thread actions join this menu. */
const mobileMoreItems = computed<DropdownMenuItem[][]>(() => [
  [
    flagged.value
      ? { label: t('mail.threadView.unpin'), icon: 'i-lucide-pin-off', onSelect: toggleFlag }
      : { label: t('mail.threadView.pin'), icon: 'i-lucide-pin', onSelect: toggleFlag },
    inSnoozed.value
      ? { label: t('mail.threadView.unsnooze'), icon: 'i-lucide-alarm-clock-off', onSelect: unsnooze }
      : { label: t('mail.threadView.later'), icon: 'i-lucide-alarm-clock', onSelect: snooze },
    { label: t('mail.threadView.move'), icon: 'i-lucide-folder-input', disabled: !moveItems.value.length, children: moveItems.value },
    { label: t('mail.threadView.markUnread'), icon: 'i-lucide-mail', onSelect: markUnread }
  ],
  ...moreItems.value
])

const account = computed(() => accountById(props.accountId))
const color = computed(() => accountColor(props.accountId))

/** Detail of the message replies go to; loaded even when its card is collapsed. */
function latestDetail() {
  return replyTarget.value && !resetPending.value ? details.get(replyTarget.value.id) : undefined
}
watch(replyTarget, (m) => {
  if (m) loadDetail(m.id)
})

/** Shortcuts of the open thread, inactive while the list has a multiple selection. */
const when = (fn: () => unknown) => () => {
  // While composing, these keys do not target the thread behind the window; nor behind an open
  // dialog or menu (an Escape closing a dialog doesn't close the thread too).
  if (props.shortcuts && !composing.value && !overlayOpen()) fn()
}

defineShortcuts({
  e: when(archive),
  delete: when(trash),
  s: when(toggleFlag),
  b: when(() => (inSnoozed.value ? unsnooze() : snooze())),
  u: when(markUnread),
  r: when(() => { const d = latestDetail(); if (d) reply(d) }),
  a: when(() => { const d = latestDetail(); if (d) reply(d, true) }),
  f: when(() => { const d = latestDetail(); if (d) forward(d) }),
  l: when(openLabels)
})
/** A key typed into a field (defineShortcuts skips those too). */
const typingIn = (t: EventTarget | null) => t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))
// Escape closes the thread. Not through defineShortcuts: its preventDefault would stop a dialog's
// or menu's own Escape (Reka skips a prevented one); `when` leaves it to them while they're open.
useEventListener('keydown', (e: KeyboardEvent) => {
  if (e.key === 'Escape' && !e.defaultPrevented && !typingIn(e.target)) when(() => emit('close'))()
})
// `#` deletes. Not through defineShortcuts either: on AZERTY under Windows it is typed with AltGr,
// reported as Ctrl+Alt, which a plain `#` shortcut never matches.
useEventListener('keydown', (e: KeyboardEvent) => {
  if (!isHashKey(e) || e.defaultPrevented || typingIn(e.target)) return
  if (!props.shortcuts || composing.value || overlayOpen()) return
  e.preventDefault()
  trash()
})
</script>

<template>
  <!-- Each label in its colour, then the button adding one: under the subject on desktop (24 px),
       at the top of the reading on mobile (30 px). -->
  <DefineLabels>
    <span
      v-for="label in threadLabels"
      :key="label.id"
      class="inline-flex h-7.5 shrink-0 items-center gap-1.5 rounded-full ps-2.5 pe-1 text-[12.5px] font-semibold lg:h-6 lg:ps-2.25 lg:text-xs"
      :class="labelColor(label).soft"
    >
      <span class="size-1.75 shrink-0 rounded-full" :class="labelColor(label).dot" />
      {{ label.display_name }}
      <UButton
        icon="i-lucide-x"
        color="neutral"
        variant="link"
        size="xs"
        class="size-5 justify-center rounded-full p-0 text-current opacity-70 hover:opacity-100 max-lg:size-6.5 lg:size-4.5"
        :aria-label="$t('mail.labels.remove', { label: label.display_name })"
        @click="removeLabel(label)"
      />
    </span>
    <LabelPicker v-model:open="labelsOpen" :account-id="accountId" :ids="labelIds" @applied="labelsApplied">
      <UTooltip :text="$t('mail.labels.title')" :kbds="['l']">
        <UButton
          icon="i-lucide-plus"
          :label="$t('mail.labels.add')"
          color="neutral"
          variant="ghost"
          class="h-7.5 shrink-0 gap-1 rounded-full border border-dashed border-accented px-2.5 text-[12.5px] font-normal text-muted lg:h-6 lg:px-2.25 lg:text-xs"
          :ui="{ leadingIcon: 'size-3.5' }"
        />
      </UTooltip>
    </LabelPicker>
  </DefineLabels>
  <UDashboardPanel id="thread" class="bg-(--surface-read)" :ui="{ body: 'gap-0 p-0 sm:gap-0 sm:p-0' }">
    <template #header>
      <div class="flex shrink-0 items-center gap-1 border-b border-default bg-default px-2 py-2.5 lg:hidden dark:bg-(--surface-side)">
        <UButton
          icon="i-lucide-arrow-left"
          color="neutral"
          variant="ghost"
          size="lg"
          class="size-11 justify-center rounded-xl text-highlighted"
          :aria-label="$t('mail.threadView.back')"
          @click="emit('close')"
        />
        <span class="flex-1" />
        <UDropdownMenu v-if="aiReady" :items="aiItems" :content="{ align: 'end' }" size="xl">
          <UButton
            icon="i-lucide-sparkles"
            variant="ghost"
            size="lg"
            class="size-11 justify-center rounded-xl bg-violet-100 text-violet-700 hover:bg-violet-200/70 dark:bg-violet-500/16 dark:text-violet-300 dark:hover:bg-violet-500/24"
            :loading="summarizing || findingEvent"
            :aria-label="$t('mail.threadView.ai.title')"
          />
        </UDropdownMenu>
        <UButton :icon="archiveLook.icon" color="neutral" variant="ghost" size="lg" class="size-11 justify-center rounded-xl" :aria-label="archiveLook.label" @click="archive" />
        <UButton icon="i-lucide-trash-2" color="neutral" variant="ghost" size="lg" class="size-11 justify-center rounded-xl" :aria-label="$t('mail.threadView.trash')" @click="trash" />
        <UDropdownMenu :items="mobileMoreItems" :content="{ align: 'end' }" size="xl">
          <UButton icon="i-lucide-ellipsis-vertical" color="neutral" variant="ghost" size="lg" class="size-11 justify-center rounded-xl" :aria-label="$t('mail.threadView.more')" />
        </UDropdownMenu>
      </div>

      <div class="hidden min-h-16 shrink-0 items-center gap-3 border-b border-default bg-(--surface-read) py-3 ps-7 pe-5 lg:flex">
        <div class="flex min-w-0 flex-1 flex-col gap-1.5">
          <h2 class="truncate text-[17px] font-bold text-highlighted" :title="subject">{{ subject }}</h2>
          <span class="flex min-w-0 items-center gap-1.5 text-xs text-muted">
            <LabelsRow v-if="wide && hasLabels && labelIds.length" />
            <UIcon
              v-if="invitationLabel"
              :name="invitation?.kind === 'cancelled' ? 'i-lucide-calendar-x' : 'i-lucide-calendar'"
              class="size-3 shrink-0"
              :class="invitation?.kind === 'cancelled' ? 'text-red-600 dark:text-red-400' : 'text-amber-600 dark:text-amber-400'"
            />
            <span class="truncate" :class="{ 'ms-1 shrink-0': wide && hasLabels && labelIds.length }">{{ wide && hasLabels && labelIds.length ? '· ' : '' }}{{ invitationLabel ? `${invitationLabel} · ` : '' }}{{ $t('mail.reader.messageCount', messages.length) }}</span>
            <span v-if="muted" class="inline-flex shrink-0 items-center gap-1 rounded-full bg-elevated px-1.5 py-px font-medium">
              <UIcon name="i-lucide-bell-off" class="size-3" />{{ $t('mail.threadView.mutedBadge') }}
            </span>
          </span>
        </div>
        <UFieldGroup class="gap-0.5 space-x-0 rounded-xl border border-default bg-(--surface-card) p-0.5">
          <UTooltip :text="archiveLook.label" :kbds="['e']">
            <UButton
              :icon="archiveLook.icon"
              :label="doneMode || restoreMode ? archiveLook.label : undefined"
              :color="doneMode || restoreMode ? 'primary' : 'neutral'"
              :variant="doneMode || restoreMode ? 'soft' : 'ghost'"
              class="h-8.5 min-w-8.5 justify-center rounded-[9px]!"
              :aria-label="archiveLook.label"
              @click="archive"
            />
          </UTooltip>
          <UTooltip v-if="inSnoozed" :text="$t('mail.threadView.unsnooze')" :kbds="['b']">
            <UButton icon="i-lucide-alarm-clock-off" color="neutral" variant="ghost" class="size-8.5 justify-center rounded-[9px]!" :aria-label="$t('mail.threadView.unsnooze')" @click="unsnooze" />
          </UTooltip>
          <UTooltip v-else :text="$t('mail.threadView.later')" :kbds="['b']">
            <UButton icon="i-lucide-alarm-clock" color="neutral" variant="ghost" class="size-8.5 justify-center rounded-[9px]!" :aria-label="$t('mail.threadView.later')" @click="snooze" />
          </UTooltip>
          <UTooltip :text="$t('mail.threadView.trash')" :kbds="['#']">
            <UButton icon="i-lucide-trash-2" color="neutral" variant="ghost" class="size-8.5 justify-center rounded-[9px]!" :aria-label="$t('mail.threadView.trash')" @click="trash" />
          </UTooltip>
        </UFieldGroup>
        <UFieldGroup class="gap-0.5 space-x-0 rounded-xl border border-default bg-(--surface-card) p-0.5">
          <UDropdownMenu :items="moveItems">
            <UTooltip :text="$t('mail.threadView.move')">
              <UButton icon="i-lucide-folder-input" color="neutral" variant="ghost" class="size-8.5 justify-center rounded-[9px]!" :aria-label="$t('mail.threadView.move')" />
            </UTooltip>
          </UDropdownMenu>
          <UTooltip :text="$t('mail.threadView.markUnread')" :kbds="['u']">
            <UButton icon="i-lucide-mail" color="neutral" variant="ghost" class="size-8.5 justify-center rounded-[9px]!" :aria-label="$t('mail.threadView.unread')" @click="markUnread" />
          </UTooltip>
          <UTooltip :text="flagged ? $t('mail.threadView.unpin') : $t('mail.threadView.pin')" :kbds="['s']">
            <UButton
              icon="i-lucide-pin"
              color="neutral"
              variant="ghost"
              class="size-8.5 justify-center rounded-[9px]!"
              :class="{ 'text-amber-700 dark:text-amber-400': flagged }"
              :aria-label="flagged ? $t('mail.threadView.unpin') : $t('mail.threadView.pin')"
              :aria-pressed="flagged"
              @click="toggleFlag"
            />
          </UTooltip>
        </UFieldGroup>
        <UDropdownMenu v-if="aiReady" :items="aiItems" :content="{ align: 'end' }">
          <UButton
            icon="i-lucide-sparkles"
            :label="$t('mail.threadView.ai.short')"
            variant="ghost"
            class="h-10 rounded-[11px] px-3 font-semibold bg-violet-100 text-violet-700 hover:bg-violet-200/70 dark:bg-violet-500/16 dark:text-violet-300 dark:hover:bg-violet-500/24"
            :loading="summarizing || findingEvent"
            :aria-label="$t('mail.threadView.ai.title')"
          />
        </UDropdownMenu>
        <UDropdownMenu :items="moreItems" :content="{ align: 'end' }">
          <UTooltip :text="$t('mail.threadView.more')">
            <UButton icon="i-lucide-ellipsis" color="neutral" variant="ghost" class="size-10 justify-center rounded-[10px] text-muted" :aria-label="$t('mail.threadView.more')" />
          </UTooltip>
        </UDropdownMenu>
      </div>
    </template>

    <template #body>
      <div ref="body" class="mx-auto flex w-full max-w-4xl flex-col gap-2.5 px-3 pt-4.5 pb-6 lg:px-7 lg:py-5">
        <!-- Held by the new-senders filter: the decision, without going back to the screener. -->
        <div
          v-if="inScreener && sender"
          class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-2xl border border-default bg-(--surface-card) px-4 py-2.5 shadow-xs dark:shadow-none"
        >
          <UIcon :name="roleIcons.screener" class="size-4.5 shrink-0 text-primary" />
          <span class="min-w-0 flex-1 truncate text-sm text-default">{{ $t('mail.screener.held', { name: sender.name?.trim() || sender.email }) }}</span>
          <div class="flex items-center gap-2 max-sm:w-full">
            <UFieldGroup class="max-sm:flex-1">
              <UButton
                :label="$t('mail.screener.refuse')"
                icon="i-lucide-user-round-x"
                color="neutral"
                variant="outline"
                size="sm"
                class="flex-1 justify-center rounded-s-[9px] max-sm:min-h-11"
                @click="screenSender('block')"
              />
              <UDropdownMenu :items="refuseItems" :content="{ align: 'end' }">
                <UButton
                  icon="i-lucide-chevron-down"
                  color="neutral"
                  variant="outline"
                  size="sm"
                  class="rounded-e-[9px] max-sm:min-h-11"
                  :aria-label="$t('mail.screener.refuseAs')"
                />
              </UDropdownMenu>
            </UFieldGroup>
            <UButton
              :label="$t('mail.screener.accept')"
              icon="i-lucide-user-round-check"
              size="sm"
              class="justify-center rounded-[9px] max-sm:min-h-11 max-sm:flex-1"
              @click="screenSender('inbox')"
            />
          </div>
        </div>
        <div v-if="summary?.thread === threadId" class="flex flex-col gap-2 rounded-2xl border border-default bg-(--surface-card) px-4 py-3 shadow-xs dark:shadow-none">
          <div class="flex items-center gap-2">
            <UIcon name="i-lucide-sparkles" class="size-4 shrink-0 text-primary" />
            <span class="flex-1 text-xs font-semibold text-muted">{{ $t('mail.threadView.ai.summaryTitle') }}</span>
            <UButton icon="i-lucide-x" color="neutral" variant="ghost" size="xs" :aria-label="$t('app.overlay.close')" @click="summary = null" />
          </div>
          <p v-if="summary.text" class="text-sm whitespace-pre-line text-default">{{ summary.text }}</p>
          <p v-else-if="summary.error" class="text-sm text-error">{{ summary.error }}</p>
          <span v-else class="flex items-center gap-2 text-sm text-muted">
            <UIcon name="i-lucide-loader-circle" class="size-4 animate-spin" />
            {{ $t('mail.threadView.ai.summarizing') }}
          </span>
        </div>
        <!-- Mobile: the subject heads the reading, with the thread's account. A lone invitation
             carries its title in its card instead. -->
        <div v-if="!invitation || messages.length > 1" class="flex flex-col gap-2 px-1 pb-1.5 lg:hidden">
          <h1 class="text-[21px] leading-tight font-bold tracking-tight text-highlighted">{{ subject }}</h1>
          <span class="inline-flex items-center gap-1.5 text-[13px] text-muted">
            <span class="size-2 shrink-0 rounded-full" :class="color.dot" />
            <span class="truncate">{{ account ? `${accountName(account)} · ` : '' }}{{ $t('mail.reader.messageCount', messages.length) }}</span>
          </span>
        </div>
        <div v-if="!wide && hasLabels && labelIds.length" ref="labelsRow" class="flex scroll-mt-4 flex-wrap items-center gap-1.5 px-1">
          <LabelsRow />
        </div>
        <template v-for="(row, i) in rows" :key="row.kind === 'message' ? row.message.id : `folded-${i}`">
          <!-- Wrapped: MessageCard has several roots, an attribute set on it would be lost. -->
          <div v-if="row.kind === 'message'" :data-message-id="row.message.id" class="scroll-mt-4">
            <MessageCard
              :message="row.message"
              :detail="details.get(row.message.id)"
              :expanded="expanded.has(row.message.id)"
              :loading="loadingIds.has(row.message.id)"
              :latest="row.message.id === replyTarget?.id"
              :draft="drafts.has(row.message.id)"
              @toggle="toggle"
            />
          </div>
          <button
            v-else
            type="button"
            class="group flex items-center gap-3 py-0.5 focus-visible:outline-none"
            @click="unfolded = true"
          >
            <span class="h-px flex-1 bg-(--ui-border)" />
            <span class="inline-flex items-center gap-1.5 rounded-full border border-default bg-(--surface-card) px-3 py-1 text-xs font-semibold text-muted transition-colors group-hover:text-highlighted group-focus-visible:ring-2 group-focus-visible:ring-primary">
              <UIcon name="i-lucide-chevrons-up-down" class="size-3.5" />
              {{ $t('mail.threadView.folded', row.count) }}
            </span>
            <span class="h-px flex-1 bg-(--ui-border)" />
          </button>
        </template>
        <div
          v-for="l in pendingReplies"
          :key="`out-${l.item.id}`"
          class="rounded-2xl border border-dashed border-default bg-(--surface-card) px-5 py-4"
          :class="l.item.state === 'failed' && l.sentAt === null ? 'border-error/50' : ''"
        >
          <div class="flex items-center gap-2 text-sm">
            <UIcon
              :name="l.sentAt !== null ? 'i-lucide-check' : l.item.state === 'failed' ? 'i-lucide-circle-alert' : 'i-lucide-loader-circle'"
              class="size-4 shrink-0"
              :class="l.sentAt !== null ? 'text-success' : l.item.state === 'failed' ? 'text-error' : 'animate-spin text-muted'"
            />
            <span class="font-semibold text-highlighted">{{ $t('mail.threadView.outgoing.you') }}</span>
            <span class="text-muted">· {{ leavingLabel(l) }}</span>
          </div>
          <p v-if="ownText(l.item.preview)" class="mt-2 line-clamp-6 whitespace-pre-line text-sm text-default">{{ ownText(l.item.preview) }}</p>
        </div>
      </div>
    </template>

    <template #footer>
      <div class="flex shrink-0 gap-2 border-t border-default bg-default px-3 pt-2.5 pb-[calc(0.75rem+env(safe-area-inset-bottom))] lg:hidden dark:bg-(--surface-side)">
        <UButton
          icon="i-lucide-reply"
          :label="$t('mail.messageCard.reply')"
          size="xl"
          class="h-12 flex-1 justify-center rounded-[14px] font-semibold"
          :disabled="!latestDetail()"
          @click="latestDetail() && reply(latestDetail()!)"
        />
        <UButton
          icon="i-lucide-reply-all"
          color="neutral"
          variant="outline"
          size="xl"
          class="size-12 justify-center rounded-[14px] bg-(--surface-card)"
          :aria-label="$t('mail.messageCard.replyAll')"
          :disabled="!latestDetail()"
          @click="latestDetail() && reply(latestDetail()!, true)"
        />
        <UButton
          icon="i-lucide-forward"
          color="neutral"
          variant="outline"
          size="xl"
          class="size-12 justify-center rounded-[14px] bg-(--surface-card)"
          :aria-label="$t('mail.messageCard.forward')"
          :disabled="!latestDetail()"
          @click="latestDetail() && forward(latestDetail()!)"
        />
      </div>
      <CalendarEventForm v-model:open="eventFormOpen" :suggestion="suggestion" />
    </template>
  </UDashboardPanel>
</template>
