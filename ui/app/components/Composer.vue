<script setup lang="ts">
import { confirm } from '~/composables/useConfirm'
import { createReusableTemplate, useEventListener, useMediaQuery } from '@vueuse/core'
import type { Draft, OutgoingAttachment } from '~/utils/types'
import { SIGNATURE_CLASS } from '~/utils/editorExtensions'
import { MAX_MESSAGE_BYTES, draftBytes } from '~/composables/useComposer'

/** Idle time before the draft autosaves; longer with attachments, which are re-sent in full
 *  on every save. */
const AUTOSAVE_MS = 3000
const AUTOSAVE_HEAVY_MS = 15000
const autosaveDelay = () => (draft.value?.attachments.length || draft.value?.forwarded.length ? AUTOSAVE_HEAVY_MS : AUTOSAVE_MS)

const { open, draft, session, unsaved, editOutgoing, onBeforeReplace, onBeforeQuit } = useComposer()
const { ask } = useWhenPicker()
const { accounts, accountById, accountColor } = useMail()
const showError = useErrorToast()
const toast = useToast()
const { t } = useI18n()
const desktop = useMediaQuery('(min-width: 1024px)')
const maximized = ref(false)

const slideoverUi = computed(() => desktop.value
  ? {
      overlay: 'bg-neutral-900/25 dark:bg-black/55',
      content: [
        maximized.value ? 'max-w-none' : 'max-w-[760px]',
        'divide-y-0 border-s border-default bg-(--surface-card) ring-0 sm:ring-0',
        'sm:shadow-[-24px_0_60px_rgba(24,24,27,0.18)] dark:sm:shadow-[-24px_0_60px_rgba(0,0,0,0.5)]'
      ].join(' ')
    }
  : { content: 'max-w-none divide-y-0 bg-(--surface-card) ring-0 sm:shadow-none sm:ring-0' })

/** The fields place the cursor themselves (To, or the text of a reply): the dialog must not move
 *  it to its first button (whose tooltip would show). */
const keepFocus = (e: Event) => e.preventDefault()

type SenderItem = { label: string, value: string, accountId: number, name: string, email: string }
const [DefineSender, ReuseSender] = createReusableTemplate<{ item: SenderItem, full?: boolean }>()
const [DefineFromRow, ReuseFromRow] = createReusableTemplate()
const [DefineSubjectRow, ReuseSubjectRow] = createReusableTemplate()
const [DefineAttachments, ReuseAttachments] = createReusableTemplate()
const subjectId = useId()

const sending = ref(false)
const showCopies = ref(false)
const fileInput = useTemplateRef('fileInput')

type RecipientField = { flush: () => void, focus: () => void }
const toInput = useTemplateRef<RecipientField>('toInput')
const ccInput = useTemplateRef<RecipientField>('ccInput')
const bccInput = useTemplateRef<RecipientField>('bccInput')
function flushRecipients() {
  toInput.value?.flush()
  ccInput.value?.flush()
  bccInput.value?.flush()
}

const hasRecipient = computed(() => !!draft.value && (draft.value.to.length + draft.value.cc.length + draft.value.bcc.length) > 0)

async function showCopyRows() {
  showCopies.value = true
  await nextTick()
  ccInput.value?.focus()
}

const dirty = ref(false)
const saving = ref(false)
const savedAt = ref<number | null>(null)
/** The last save failed: closing must not drop the draft silently. */
const saveFailed = ref(false)
/** Non-zero while we change the draft ourselves (opening, id received): not a keystroke. */
let quiet = 0
let timer: ReturnType<typeof setTimeout> | undefined

async function silently(change: () => void) {
  quiet++
  try {
    change()
    // The deep watcher runs before the next tick resolves.
    await nextTick()
  } finally {
    quiet--
  }
}

/** Fingerprint of the last saved draft: unchanged means nothing to send. */
let lastSaved: string | null = null
/** Cheap fingerprint: attachments by name and size, not content. Excludes the draft id, which
 *  the engine assigns. */
function fingerprint(d: Draft): string {
  return JSON.stringify([
    d.account_id, d.from ?? null, d.to, d.cc, d.bcc, d.subject, d.body, d.html, d.in_reply_to,
    d.attachments.map(a => [a.filename, a.data.length]),
    d.forwarded.map(f => [f.message_id, f.index])
  ])
}

/** Bumped when the draft moves to another account: a save still on its way was made for the old one. */
let accountGen = 0
/** The editor's first plain-text version of a draft is not an edit. */
let textSynced = false

// Every draft shown (new, reply, reopened, even while another one was open) starts afresh:
// the editor is recreated (`session` is its key) and the per-draft state is reset.
watch(session, () => {
  const d = draft.value
  if (!d) return
  clearTimeout(timer)
  remindAfter.value = null
  dirty.value = false
  saveFailed.value = false
  savedAt.value = null
  lastSaved = null
  textSynced = false
  showCopies.value = d.cc.length > 0 || d.bcc.length > 0
  // The editor normalises the HTML on open: not a keystroke.
  quiet++
  setTimeout(() => {
    quiet--
  }, 300)
  // Taken back from the outbox: this window holds the only copy, saved as a draft at once.
  if (unsaved.value) {
    unsaved.value = false
    dirty.value = true
    save()
  }
})

watch(draft, (d, prev) => {
  // Another draft replacing this one is not an edit (the session watcher resets the state).
  if (quiet || !d || d !== prev) return
  dirty.value = true
  clearTimeout(timer)
  // Being sent: no save now (it would put back in Drafts what the engine is taking out of it);
  // should the send fail, the save follows.
  if (sending.value) return
  timer = setTimeout(save, autosaveDelay())
}, { deep: true })

/** A keystroke in the text: unsaved now, saved after the usual delay (its HTML is written out on
 *  a pause, or by the save itself). */
function onEdited() {
  if (quiet || !draft.value) return
  dirty.value = true
  clearTimeout(timer)
  if (sending.value) return
  timer = setTimeout(save, autosaveDelay())
}

/** Editor updates for the draft of session `of` only: an editor unmounting after another draft
 *  replaced its own writes its last keys out, which must not land in the new one. */
const htmlFor = (of: number) => (v: string) => {
  if (draft.value && of === session.value) draft.value.html = v
}
const textFor = (of: number) => (text: string) => {
  if (of === session.value) onText(text)
}

function onText(text: string) {
  const d = draft.value
  if (!d || d.body === text) return
  if (textSynced) {
    d.body = text
    return
  }
  textSynced = true
  silently(() => {
    d.body = text
  })
}

const isEmpty = computed(() => {
  const d = draft.value
  if (!d) return true
  if (d.to.length || d.cc.length || d.bcc.length || d.subject.trim() || d.attachments.length || d.forwarded.length) return false
  if (d.html === null) return !d.body.trim()
  const rest = withoutSignature(d.html, senderSignature(accountById(d.account_id), d.from))
  return !htmlText(rest).trim() && !/<img\b/i.test(rest)
})

/** Save in flight: send, discard and close wait for it, to learn the draft id and leave no
 *  ghost draft on the server. */
let inFlight: Promise<boolean> | null = null

/** Saves the draft if it changed. Resolves to false when the save failed (the draft is then
 *  still unsaved: the caller must not drop it). */
async function save(): Promise<boolean> {
  // What was typed since the last pause goes in first.
  editor.value?.flush()
  clearTimeout(timer)
  // Being sent: a save now would put back in Drafts what the engine is taking out of it (should
  // the send fail, the save follows).
  if (sending.value) return true
  if (inFlight) await inFlight
  const d = draft.value
  if (!d || !dirty.value || isEmpty.value) return true
  const print = fingerprint(d)
  dirty.value = false
  // Edited then reverted: the server already has this version.
  if (print === lastSaved) return true
  saving.value = true
  const gen = accountGen
  const account = d.account_id
  const sentId = d.draft_id
  const job = (async () => {
    try {
      const id = await api.saveDraft(d)
      if (gen !== accountGen) {
        // The draft moved to another account meanwhile: this copy is an orphan in the old
        // account's Drafts. Its previous id (`sentId`) is removed by the account switch itself.
        if (id !== sentId) api.discardDraft(account, id).catch(showError)
        return true
      }
      if (draft.value === d) {
        lastSaved = print
        saveFailed.value = false
        savedAt.value = Date.now() / 1000
        if (d.draft_id !== id) {
          await silently(() => {
            d.draft_id = id
          })
        }
      }
      return true
    } catch (e) {
      if (draft.value === d && gen === accountGen) {
        dirty.value = true
        saveFailed.value = true
      }
      showError(e, t('app.composer.draftNotSaved'))
      return false
    } finally {
      saving.value = false
      inFlight = null
    }
  })()
  inFlight = job
  const ok = await job
  // Keystrokes during the save: another save follows (after a failure, the next edit, a save or
  // the close retries, rather than an error toast every few seconds).
  if (ok && dirty.value && draft.value === d && !sending.value) timer = setTimeout(save, autosaveDelay())
  return ok
}

function saveNow() {
  flushRecipients()
  dirty.value = true
  return save()
}

/** Waits for the save in flight without starting one (nor letting a queued one start later). */
async function settle() {
  clearTimeout(timer)
  while (inFlight) await inFlight
  clearTimeout(timer)
}

async function confirmLoss(): Promise<boolean> {
  const message = t('common.compose.unsavedLeave')
  try {
    return await confirm(message, {
      title: t('app.composer.draftNotSaved'),
      kind: 'warning',
      okLabel: t('common.compose.leaveAnyway'),
      cancelLabel: t('common.compose.keepEditing')
    })
  } catch {
    return window.confirm(message)
  }
}

/** Saves the open draft before it leaves the window (close, another draft, quit). False: the
 *  save failed and the user keeps the draft. */
async function secure(): Promise<boolean> {
  // A send under way decides first: sent, there is nothing left to save; refused, the draft is
  // saved as usual.
  while (sendJob) await sendJob
  if (!open.value || !draft.value) return true
  flushRecipients()
  if (await save()) return true
  return confirmLoss()
}

// A reply or new message is about to replace this draft: save it first (after the save already
// in flight, so that no second copy is created).
onBeforeReplace(secure)
onBeforeQuit(secure)

const status = computed<{ text: string, short: string, icon?: string, tone?: string } | null>(() => {
  if (saving.value) {
    const text = t('app.composer.saving')
    return { text, short: text, icon: 'i-lucide-loader-circle', tone: 'animate-spin text-dimmed' }
  }
  if (saveFailed.value && dirty.value) {
    const text = t('app.composer.draftNotSaved')
    return { text, short: text, icon: 'i-lucide-triangle-alert', tone: 'text-warning' }
  }
  if (dirty.value && !isEmpty.value) {
    const text = t('app.composer.unsaved')
    return { text, short: text }
  }
  if (savedAt.value) {
    return {
      text: t('app.composer.savedAt', { time: shortDate(savedAt.value) }),
      short: t('app.composer.saved'),
      icon: 'i-lucide-check',
      tone: 'text-success'
    }
  }
  if (draft.value?.draft_id) {
    const text = t('app.composer.draft')
    return { text, short: text }
  }
  return null
})

const title = computed(() => {
  const d = draft.value
  return d?.subject.trim() || (d?.in_reply_to ? t('app.composer.reply') : d?.draft_id ? t('app.composer.draft') : t('app.composer.newMessage'))
})

/** Closing (cross, Esc) keeps the draft; if it could not be saved, the window stays open unless
 *  confirmed. */
let closing = false
async function close() {
  if (closing) return
  closing = true
  try {
    if (!(await secure())) return
    clearTimeout(timer)
    open.value = false
    draft.value = null
  } finally {
    closing = false
  }
}

async function discard() {
  await settle()
  const d = draft.value
  open.value = false
  draft.value = null
  if (d?.draft_id) await api.discardDraft(d.account_id, d.draft_id).catch(showError)
}

// Closing the window first saves the open draft: the default layout's close guard calls
// `secure` (registered above with onBeforeQuit), then lets the messages just sent leave.

/** Page hidden or unloading (app sent to the background, reload): save without waiting. */
function saveSoon() {
  if (open.value && dirty.value) save()
}
const onVisibility = () => {
  if (document.visibilityState === 'hidden') saveSoon()
}
onMounted(() => {
  document.addEventListener('visibilitychange', onVisibility)
  window.addEventListener('pagehide', saveSoon)
  window.addEventListener('beforeunload', saveSoon)
})
onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', onVisibility)
  window.removeEventListener('pagehide', saveSoon)
  window.removeEventListener('beforeunload', saveSoon)
})

/** Value: "account|alias" (empty alias: the account itself). */
const fromItems = computed<SenderItem[]>(() => accounts.value.flatMap(a => [
  { label: `${a.name} <${a.email}>`, value: `${a.id}|`, accountId: a.id, name: a.name || accountName(a), email: a.email },
  ...a.identities.map(i => ({ label: `${i.name || a.name} <${i.email}>`, value: `${a.id}|${i.email}`, accountId: a.id, name: i.name || a.name || accountName(a), email: i.email }))
]))
/** The draft's sender when it is none of these (an alias removed since, a draft written from
 *  another address): shown as it is, with the right ones to pick instead. */
const unknownSender = computed<SenderItem | null>(() => {
  const d = draft.value
  if (!d || fromItems.value.some(i => i.value === sender.value)) return null
  const email = d.from || accountById(d.account_id)?.email || ''
  return { label: email, value: sender.value, accountId: d.account_id, name: t('app.composer.unknownSender'), email }
})
const senderItems = computed(() => (unknownSender.value ? [unknownSender.value, ...fromItems.value] : fromItems.value))
const manySenders = computed(() => senderItems.value.length > 1)
const senderItem = computed(() => senderItems.value.find(i => i.value === sender.value) ?? null)
const senderAccount = computed(() => (draft.value ? accountById(draft.value.account_id) : null))
const senderColor = computed(() => accountColor(draft.value?.account_id ?? 0))

const signatureNote = computed(() => {
  const d = draft.value
  const account = senderAccount.value
  if (!d || !account) return null
  const identity = d.from ? account.identities.find(i => i.email.toLowerCase() === d.from!.toLowerCase()) : undefined
  const own = identity && signatureContent(identity.signature)
  const name = own ? (identity.name || identity.email) : accountName(account)
  if (!senderSignature(account, d.from)) return { text: t('app.composer.signatureNone', { name }), link: t('app.composer.signatureAdd') }
  const present = !!d.html?.includes(`class="${SIGNATURE_CLASS}"`)
  return {
    text: t(present ? 'app.composer.signatureIn' : 'app.composer.signatureRemoved', { name }),
    link: t('app.composer.signatureEdit')
  }
})

/** Signature settings of the sending account: the draft is saved and the window closed first. */
async function editSignature() {
  const id = draft.value?.account_id
  if (id === undefined) return
  await close()
  if (open.value) return
  await navigateTo(`/settings/account/${id}`)
}

/** Another account: the draft already saved in the old account's Drafts is removed there, the
 *  next save goes to the new one. A save in flight for the old account is awaited first; the
 *  copy it creates is removed when it returns (see `save`). */
function retire(d: Draft) {
  accountGen++
  const oldAccount = d.account_id
  const oldId = d.draft_id
  const pending = inFlight
  d.draft_id = null
  lastSaved = null
  if (!oldId) return
  ;(async () => {
    if (pending) await pending
    await api.discardDraft(oldAccount, oldId).catch(showError)
  })()
}

const sender = computed({
  get: () => (draft.value ? `${draft.value.account_id}|${draft.value.from ?? ''}` : ''),
  set: (v: string) => {
    const d = draft.value
    if (!d) return
    const [account, alias] = v.split('|')
    const next = Number(account)
    const nextFrom = alias || null
    if (next === d.account_id && nextFrom === (d.from ?? null)) return
    // The last keys first: the signature is swapped in the text as typed.
    editor.value?.flush()
    // The signature follows the sender: its block is replaced as a whole.
    const before = senderSignature(accountById(d.account_id), d.from)
    const after = senderSignature(accountById(next), nextFrom)
    if (next !== d.account_id) retire(d)
    d.account_id = next
    d.from = nextFrom
    if (before !== after && d.html !== null) d.html = swapSignature(d.html, before, after)
  }
})

function readFile(file: File): Promise<OutgoingAttachment> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve({
      filename: file.name,
      mime: file.type || 'application/octet-stream',
      // data:<mime>;base64,<content>
      data: String(reader.result).split(',')[1] ?? ''
    })
    reader.onerror = () => reject(reader.error)
    reader.readAsDataURL(file)
  })
}

const attachedBytes = computed(() => (draft.value ? draftBytes({ ...draft.value, html: null, body: '' }) : 0))

async function attach(files: File[]) {
  const d = draft.value
  // Being sent: the message has left with what it had.
  if (!d || sending.value) return
  const max = Math.round(MAX_MESSAGE_BYTES / 1024 / 1024)
  for (const file of files) {
    // Encoded in base64: a third larger than the file. Refused before reading it in full.
    if (draftBytes(d) + Math.ceil(file.size / 3) * 4 > MAX_MESSAGE_BYTES) {
      toast.add({ title: t('app.composer.tooLarge', { name: file.name, max }), color: 'warning', icon: 'i-lucide-file-warning' })
      continue
    }
    const attachment = await readFile(file)
    if (draft.value === d) d.attachments.push(attachment)
  }
}

async function addFiles(e: Event) {
  await attach(Array.from((e.target as HTMLInputElement).files ?? []))
  ;(e.target as HTMLInputElement).value = ''
}

const dragging = ref(0)
const hasFiles = (e: DragEvent) => !!e.dataTransfer?.types.includes('Files')
function onDrop(e: DragEvent) {
  dragging.value = 0
  const files = Array.from(e.dataTransfer?.files ?? [])
  if (files.length) attach(files)
}
// The editor handles a drop on its own text and stops it, and a drag leaving the window fires no
// dragleave here: whatever ends the drag clears the drop zone.
const endDrag = () => { dragging.value = 0 }
useEventListener(window, 'drop', endDrag, { capture: true })
useEventListener(window, 'dragend', endDrag, { capture: true })
useEventListener(window, 'dragleave', (e: DragEvent) => { if (!e.relatedTarget) endDrag() }, { capture: true })

const DAY = 86_400
/** Follow-up if nobody replied after this delay (seconds). */
const remindAfter = ref<number | null>(null)
const remindChoices = computed(() => [
  { label: t('app.composer.remind.1d'), short: t('app.composer.remind.1dShort'), value: DAY },
  { label: t('app.composer.remind.2d'), short: t('app.composer.remind.2dShort'), value: 2 * DAY },
  { label: t('app.composer.remind.3d'), short: t('app.composer.remind.3dShort'), value: 3 * DAY },
  { label: t('app.composer.remind.1w'), short: t('app.composer.remind.1wShort'), value: 7 * DAY }
])
const remindItems = computed(() => [
  [{ type: 'label' as const, label: t('app.composer.remind.heading') }],
  [
    ...remindChoices.value.map(c => ({
      label: c.label,
      icon: remindAfter.value === c.value ? 'i-lucide-check' : undefined,
      onSelect: () => { remindAfter.value = c.value }
    })),
    { label: t('app.composer.remind.none'), icon: remindAfter.value === null ? 'i-lucide-check' : undefined, onSelect: () => { remindAfter.value = null } }
  ]
])
const remindShort = computed(() => remindChoices.value.find(c => c.value === remindAfter.value)?.short)

/** The send under way: closing the window or replacing the draft waits for it. */
let sendJob: Promise<void> | null = null

/** Sends now (after the undo delay) or at `at`. */
async function send(at: number | null = null) {
  const d = draft.value
  if (!d || sending.value) return
  // The address still being typed goes too (⌘↵ while the cursor is in the field), and the text
  // typed since the last pause.
  flushRecipients()
  editor.value?.flush()
  if (!d.to.length && !d.cc.length && !d.bcc.length) {
    toast.add({ title: t('app.composer.noRecipient'), color: 'warning', icon: 'i-lucide-user-x' })
    return
  }
  if (draftBytes(d) > MAX_MESSAGE_BYTES) {
    toast.add({ title: t('app.composer.messageTooLarge', { max: Math.round(MAX_MESSAGE_BYTES / 1024 / 1024) }), color: 'warning', icon: 'i-lucide-file-warning' })
    return
  }
  // From here the text is read-only and edits are not saved: what is sent is what is shown.
  sending.value = true
  const job = sendDraft(d, at)
  sendJob = job
  try {
    await job
  } finally {
    if (sendJob === job) sendJob = null
  }
}

async function sendDraft(d: Draft, at: number | null) {
  await settle()
  try {
    // The engine deletes the saved draft once the message is sent.
    const id = await api.send(d, at, remindAfter.value)
    // Another draft opened meanwhile stays open.
    if (draft.value === d) {
      clearTimeout(timer)
      open.value = false
      draft.value = null
    }
    const undo = id === null ? undefined : [{ label: t('common.undo'), color: 'neutral' as const, variant: 'outline' as const, onClick: () => editOutgoing(id) }]
    if (at !== null) {
      toast.add({ title: t('app.composer.scheduled', { when: untilLabel(at) }), icon: 'i-lucide-calendar-clock', actions: undo })
    } else if (id !== null) {
      // Undo delay: the toast lasts as long.
      const delay = Number(await api.getSetting('undo_send').catch(() => null) ?? 10) || 10
      toast.add({ title: t('app.composer.sendingIn', { n: delay }), icon: 'i-lucide-send', duration: delay * 1000, actions: undo })
    } else {
      toast.add({ title: t('app.composer.sent'), icon: 'i-lucide-send' })
    }
  } catch (e) {
    showError(e, t('app.composer.sendFailed'))
  } finally {
    sending.value = false
    // Not sent: changes made meanwhile (recipients, subject) are saved as usual.
    if (draft.value === d && dirty.value) {
      clearTimeout(timer)
      timer = setTimeout(save, autosaveDelay())
    }
  }
}

async function sendLater() {
  const at = await ask({ kind: 'send', title: t('app.composer.sendLater') })
  if (at !== null) await send(at)
}

const sendMenu = computed(() => [[{ label: t('app.composer.sendLaterMenu'), icon: 'i-lucide-calendar-clock', onSelect: sendLater }]])

/** Phone: what the desktop footer holds besides sending and attaching, at the end of the
 *  formatting bar's "more" menu. */

const { templates } = useTemplates()

/** Dictation by the local model, when turned on: what is said goes in at the cursor. */
const { status: aiStatus } = useAi()
const dictationReady = computed(() => !!aiStatus.value?.dictation_ready)
const dictation = useDictation()
async function toggleDictation() {
  if (dictation.transcribing.value) return
  if (!dictation.recording.value) {
    try {
      await dictation.start(toggleDictation)
    } catch (e) {
      showError(e, t('app.composer.dictation.noMicrophone'))
    }
    return
  }
  // Only into the draft it was said for: another one opened meanwhile gets nothing.
  const said = draft.value
  try {
    const text = await dictation.stop()
    if (text && draft.value === said && open.value) editor.value?.insert(`${escapeHtml(text)} `)
  } catch (e) {
    showError(e)
  }
}
// The draft closed, sent or replaced: the microphone stops, what it heard is dropped.
watch([open, draft], ([isOpen, current], [, before]) => {
  if (!isOpen || current !== before) dictation.cancel()
})
const dictationLabel = computed(() => (dictation.recording.value ? t('app.composer.dictation.stop') : dictation.transcribing.value ? t('app.composer.dictation.writing') : t('app.composer.dictation.start')))
const editor = useTemplateRef<{ insert: (html: string) => void, flush: () => void }>('editor')
/** The templates screen sits under the window: closed first, its draft kept (reopened from Drafts). */
async function manageTemplates() {
  await close()
  if (!open.value) navigateTo('/templates')
}
const templateItems = computed(() => [
  templates.value.map(tpl => ({ label: tpl.name || t('app.composer.templates.untitled'), icon: 'i-lucide-file-text', onSelect: () => editor.value?.insert(tpl.html) })),
  [{ label: templates.value.length ? t('app.composer.templates.manage') : t('app.composer.templates.create'), icon: 'i-lucide-settings-2', onSelect: manageTemplates }]
].filter(group => group.length))

const mobileMenu = computed(() => [
  [{ label: t('app.composer.templates.label'), icon: 'i-lucide-file-text', children: templateItems.value }],
  [
    { label: t('app.composer.sendLaterMenu'), icon: 'i-lucide-calendar-clock', onSelect: sendLater },
    {
      label: remindShort.value ? t('app.composer.remind.set', { delay: remindShort.value }) : t('app.composer.remind.tooltip'),
      icon: 'i-lucide-bell',
      color: remindAfter.value ? 'primary' as const : undefined,
      children: remindItems.value
    },
    { label: t('app.composer.saveDraft'), icon: 'i-lucide-save', onSelect: () => { saveNow() } }
  ],
  [{ label: t('app.composer.discard'), icon: 'i-lucide-trash-2', color: 'error' as const, onSelect: discard }]
])

/** Phone: height of the on-screen keyboard covering the window, when the web view does not
 *  shrink for it (0 when it does); keeps the formatting bar above it. */
const keyboard = ref(0)
function measureKeyboard() {
  const v = window.visualViewport
  keyboard.value = v ? Math.max(0, Math.round(window.innerHeight - v.height - v.offsetTop)) : 0
}
useEventListener(() => (import.meta.client ? window.visualViewport : null), 'resize', measureKeyboard)
useEventListener(() => (import.meta.client ? window.visualViewport : null), 'scroll', measureKeyboard)

defineShortcuts({
  meta_enter: { usingInput: true, handler: () => open.value && send() },
  meta_shift_enter: { usingInput: true, handler: () => open.value && sendLater() },
  meta_s: { usingInput: true, handler: () => open.value && saveNow() }
})
</script>

<template>
  <DefineSender v-slot="{ item, full }">
    <UAvatar
      :src="accountById(item.accountId)?.avatar ?? undefined"
      :alt="item.name"
      :text="initials(accountById(item.accountId) ? accountName(accountById(item.accountId)!) : item.email)"
      class="size-7 shrink-0 text-[10px]"
      :class="accountColor(item.accountId).solid"
      :ui="{ fallback: 'text-inherit font-bold' }"
    />
    <span class="min-w-0 truncate" :class="full ? 'flex-1' : ''">
      <b class="font-semibold text-highlighted">{{ item.name }}</b>
      <span class="ms-1.5 font-normal text-muted">{{ item.email }}</span>
    </span>
  </DefineSender>

  <DefineFromRow>
    <div v-if="draft" class="flex items-center border-b border-default" :class="desktop ? 'min-h-13 gap-3' : 'min-h-13 gap-2.5'">
      <span class="shrink-0 text-[13px] text-muted" :class="desktop ? 'w-11' : 'w-10'">{{ $t('app.composer.from') }}</span>
      <USelectMenu
        v-if="manySenders && senderItem"
        v-model="sender"
        :items="senderItems"
        value-key="value"
        :search-input="false"
        variant="none"
        trailing-icon=""
        :aria-label="$t('app.composer.sender')"
        :content="{ align: 'start' }"
        class="h-9 min-w-0 gap-2 rounded-full ps-1 pe-2.5 text-[13.5px] ring-1"
        :class="[senderColor.soft, senderColor.ring, desktop ? 'max-w-full' : 'flex-1']"
        :ui="{ trailing: 'hidden', content: 'w-auto min-w-80 max-w-[min(34rem,92vw)]', item: 'items-center gap-2.5 px-2 py-1.5 text-[13.5px]' }"
      >
        <ReuseSender :item="senderItem" :full="!desktop" />
        <UTooltip v-if="unknownSender" :text="$t('app.composer.unknownSenderHint')">
          <UIcon name="i-lucide-triangle-alert" class="size-3.75 shrink-0 text-warning" :aria-label="$t('app.composer.unknownSenderHint')" />
        </UTooltip>
        <UIcon name="i-lucide-chevron-down" class="size-3.75 shrink-0 text-muted" />
        <template #item="{ item }">
          <ReuseSender :item="item" full />
          <UIcon v-if="item.value === unknownSender?.value" name="i-lucide-triangle-alert" class="size-4 shrink-0 text-warning" />
          <UIcon v-if="item.value === sender" name="i-lucide-check" class="size-4 shrink-0 text-primary" />
        </template>
      </USelectMenu>
      <span
        v-else-if="senderItem"
        class="inline-flex h-9 min-w-0 items-center gap-2 rounded-full ps-1 pe-3 text-[13.5px] ring-1"
        :class="[senderColor.soft, senderColor.ring, desktop ? 'max-w-full' : 'flex-1']"
      >
        <ReuseSender :item="senderItem" :full="!desktop" />
      </span>
    </div>
  </DefineFromRow>

  <DefineSubjectRow>
    <div v-if="draft" class="flex items-center border-b border-default" :class="desktop ? 'min-h-14 gap-3' : 'min-h-13.5 gap-2.5'">
      <label :for="subjectId" class="shrink-0 text-[13px] text-muted" :class="desktop ? 'w-11' : 'w-10'">{{ $t('app.composer.subject') }}</label>
      <input
        :id="subjectId"
        v-model="draft.subject"
        type="text"
        :placeholder="$t('app.composer.subjectPlaceholder')"
        class="h-8.5 min-w-0 flex-1 border-0 bg-transparent text-base font-semibold text-highlighted outline-none placeholder:font-normal placeholder:text-dimmed"
      >
    </div>
  </DefineSubjectRow>

  <DefineAttachments>
    <template v-if="draft">
      <UBadge
        v-for="(a, i) in draft.forwarded"
        :key="`f${i}`"
        color="neutral"
        variant="outline"
        size="lg"
        icon="i-lucide-forward"
        :title="$t('app.composer.forwardedAttachment')"
        class="max-w-full"
      >
        <span class="truncate">{{ a.filename }} · {{ fileSize(a.size) }}</span>
        <UButton icon="i-lucide-x" size="xs" color="neutral" variant="link" :aria-label="$t('app.composer.remove')" @click="draft.forwarded.splice(i, 1)" />
      </UBadge>
      <UBadge
        v-for="(a, i) in draft.attachments"
        :key="`a${i}`"
        color="neutral"
        variant="outline"
        size="lg"
        icon="i-lucide-paperclip"
        class="max-w-full"
      >
        <span class="truncate">{{ a.filename }} · {{ fileSize(Math.floor(a.data.length * 3 / 4)) }}</span>
        <UButton icon="i-lucide-x" size="xs" color="neutral" variant="link" :aria-label="$t('app.composer.remove')" @click="draft.attachments.splice(i, 1)" />
      </UBadge>
    </template>
  </DefineAttachments>

  <USlideover
    :open="open"
    :title="title"
    :description="status?.text"
    :dismissible="false"
    :content="{ onOpenAutoFocus: keepFocus }"
    :ui="slideoverUi"
    @update:open="(o) => { if (!o) close() }"
  >
    <template #content>
      <div
        v-if="draft"
        class="relative flex min-h-0 flex-1 flex-col"
        @dragenter="(e: DragEvent) => { if (hasFiles(e)) dragging++ }"
        @dragleave="(e: DragEvent) => { if (hasFiles(e)) dragging = Math.max(0, dragging - 1) }"
        @dragover="(e: DragEvent) => { if (hasFiles(e)) e.preventDefault() }"
        @drop.prevent="onDrop"
      >
        <div
          v-if="dragging"
          class="pointer-events-none absolute inset-2 z-40 flex items-center justify-center rounded-xl border-2 border-dashed border-primary bg-primary/10 text-sm font-medium text-primary"
        >
          {{ $t('app.composer.dropHint') }}
        </div>
        <input ref="fileInput" type="file" multiple class="hidden" @change="addFiles">

        <template v-if="desktop">
          <header class="flex h-15 shrink-0 items-center gap-2 border-b border-default ps-6 pe-3">
            <h2 class="min-w-0 flex-1 truncate text-base font-bold text-highlighted">{{ title }}</h2>
            <span v-if="status" class="me-1.5 inline-flex shrink-0 items-center gap-1.5 text-xs text-muted" aria-live="polite">
              <UIcon v-if="status.icon" :name="status.icon" class="size-3.25" :class="status.tone" />
              {{ status.text }}
            </span>
            <UTooltip :text="maximized ? $t('app.composer.restore') : $t('app.composer.maximize')">
              <UButton
                :icon="maximized ? 'i-lucide-minimize-2' : 'i-lucide-maximize-2'"
                color="neutral"
                variant="ghost"
                class="size-9 justify-center rounded-[10px] text-muted"
                :aria-label="maximized ? $t('app.composer.restore') : $t('app.composer.maximize')"
                @click="maximized = !maximized"
              />
            </UTooltip>
            <UTooltip :text="$t('app.composer.close')">
              <UButton
                icon="i-lucide-x"
                color="neutral"
                variant="ghost"
                class="size-9 justify-center rounded-[10px] text-muted"
                :aria-label="$t('app.composer.close')"
                @click="close()"
              />
            </UTooltip>
          </header>

          <div class="flex shrink-0 flex-col px-6">
            <ReuseFromRow />
            <RecipientInput
              ref="toInput"
              v-model="draft.to"
              :label="$t('app.composer.to')"
              :placeholder="$t('app.composer.toPlaceholder')"
            >
              <template v-if="!showCopies" #trailing>
                <button type="button" class="h-7.5 shrink-0 rounded-lg px-2.5 text-[13px] font-semibold text-primary hover:bg-primary/10" @click="showCopyRows">
                  {{ $t('app.composer.copies') }}
                </button>
              </template>
            </RecipientInput>
            <template v-if="showCopies">
              <RecipientInput ref="ccInput" v-model="draft.cc" :label="$t('app.composer.cc')" />
              <RecipientInput ref="bccInput" v-model="draft.bcc" :label="$t('app.composer.bcc')" />
            </template>
            <ReuseSubjectRow />
          </div>

          <MailEditor
            ref="editor"
            :key="session"
            composer
            :model-value="draft.html ?? ''"
            :readonly="sending"
            :max-bytes="MAX_MESSAGE_BYTES"
            :used-bytes="attachedBytes"
            autofocus="start"
            :placeholder="$t('app.composer.bodyPlaceholder')"
            @update:model-value="htmlFor(session)"
            @text="textFor(session)"
            @changed="onEdited"
            @files="attach"
          >
            <template #footer>
              <div v-if="signatureNote" class="mx-7 mt-3.5 mb-4.5 flex items-center gap-2.5 rounded-xl border border-dashed border-default px-3 py-2.5 text-[12.5px] text-muted">
                <UIcon name="i-lucide-pen-line" class="size-3.75 shrink-0" />
                <span class="min-w-0 flex-1">{{ signatureNote.text }}</span>
                <button type="button" class="shrink-0 font-semibold text-primary hover:underline" @click="editSignature">
                  {{ signatureNote.link }}
                </button>
              </div>
            </template>
          </MailEditor>

          <div v-if="draft.attachments.length || draft.forwarded.length" class="flex max-h-28 shrink-0 flex-wrap gap-2 overflow-y-auto border-t border-default px-6 py-2.5">
            <ReuseAttachments />
          </div>

          <footer class="flex shrink-0 items-center gap-2 border-t border-default bg-(--surface-side) pt-3.5 pe-5 pb-4 ps-6">
            <UFieldGroup>
              <UButton
                icon="i-lucide-send"
                :label="$t('app.composer.send')"
                :loading="sending"
                class="h-11 rounded-xl ps-3.5 pe-4 text-[15px] font-semibold"
                @click="send()"
              />
              <UDropdownMenu :items="sendMenu" :content="{ align: 'start' }">
                <UButton
                  icon="i-lucide-chevron-down"
                  :aria-label="$t('app.composer.sendOptions')"
                  :disabled="sending"
                  class="h-11 w-9.5 justify-center rounded-xl border-s border-white/30 dark:border-black/25"
                />
              </UDropdownMenu>
            </UFieldGroup>
            <span class="mx-1.5 inline-flex items-center gap-1"><UKbd value="meta" /><UKbd value="enter" /></span>
            <span class="mx-1 h-6 w-px bg-(--ui-border)" aria-hidden="true" />
            <UTooltip :text="$t('app.composer.attach')">
              <UButton
                icon="i-lucide-paperclip"
                color="neutral"
                variant="ghost"
                class="size-10 justify-center rounded-[10px] text-default"
                :aria-label="$t('app.composer.attach')"
                @click="fileInput?.click()"
              />
            </UTooltip>
            <UTooltip v-if="dictationReady" :text="dictationLabel">
              <UButton
                :icon="dictation.recording.value ? 'i-lucide-square' : 'i-lucide-mic'"
                :color="dictation.recording.value ? 'error' : 'neutral'"
                :variant="dictation.recording.value ? 'soft' : 'ghost'"
                :loading="dictation.transcribing.value"
                class="size-10 justify-center rounded-[10px]"
                :class="dictation.recording.value ? 'animate-pulse' : 'text-default'"
                :aria-label="dictationLabel"
                :aria-pressed="dictation.recording.value"
                @click="toggleDictation"
              />
            </UTooltip>
            <UDropdownMenu :items="templateItems">
              <UTooltip :text="$t('app.composer.templates.label')">
                <UButton
                  icon="i-lucide-file-text"
                  color="neutral"
                  variant="ghost"
                  class="size-10 justify-center rounded-[10px] text-default"
                  :aria-label="$t('app.composer.templates.label')"
                />
              </UTooltip>
            </UDropdownMenu>
            <UDropdownMenu :items="remindItems">
              <UTooltip :text="$t('app.composer.remind.tooltip')">
                <UButton
                  icon="i-lucide-bell"
                  :label="remindShort"
                  :color="remindAfter ? 'primary' : 'neutral'"
                  :variant="remindAfter ? 'soft' : 'ghost'"
                  class="h-10 min-w-10 justify-center rounded-[10px]"
                  :class="remindAfter ? '' : 'text-default'"
                  :aria-label="$t('app.composer.remind.label')"
                />
              </UTooltip>
            </UDropdownMenu>
            <UTooltip :text="$t('app.composer.saveDraft')" :kbds="['meta', 's']">
              <UButton
                icon="i-lucide-save"
                color="neutral"
                variant="ghost"
                class="size-10 justify-center rounded-[10px] text-default"
                :aria-label="$t('app.composer.saveDraft')"
                :loading="saving"
                @click="saveNow()"
              />
            </UTooltip>
            <UButton
              class="ms-auto h-10 rounded-[10px] px-3 text-[13.5px] font-semibold"
              icon="i-lucide-trash-2"
              color="error"
              variant="ghost"
              :label="$t('app.composer.discard')"
              @click="discard"
            />
          </footer>
        </template>

        <div
          v-else
          class="flex min-h-0 flex-1 flex-col pt-[env(safe-area-inset-top)]"
          :style="keyboard ? { paddingBottom: `${keyboard}px` } : undefined"
        >
          <header class="flex shrink-0 items-center gap-1.5 border-b border-default py-2.5 ps-2 pe-2.5">
            <UButton
              icon="i-lucide-x"
              color="neutral"
              variant="ghost"
              size="lg"
              class="size-11 justify-center rounded-xl text-highlighted"
              :aria-label="$t('app.composer.close')"
              @click="close()"
            />
            <div class="flex min-w-0 flex-1 flex-col">
              <h1 class="truncate text-base font-bold text-highlighted">{{ title }}</h1>
              <span v-if="status" class="truncate text-[11.5px] text-dimmed" aria-live="polite">{{ status.short }}</span>
            </div>
            <UButton
              v-if="dictationReady"
              :icon="dictation.recording.value ? 'i-lucide-square' : 'i-lucide-mic'"
              :color="dictation.recording.value ? 'error' : 'neutral'"
              :variant="dictation.recording.value ? 'soft' : 'ghost'"
              :loading="dictation.transcribing.value"
              size="lg"
              class="size-11 shrink-0 justify-center rounded-xl"
              :class="dictation.recording.value ? 'animate-pulse' : 'text-default'"
              :aria-label="dictationLabel"
              :aria-pressed="dictation.recording.value"
              @click="toggleDictation"
            />
            <UButton
              icon="i-lucide-paperclip"
              color="neutral"
              variant="ghost"
              size="lg"
              class="size-11 shrink-0 justify-center rounded-xl text-default"
              :aria-label="$t('app.composer.attach')"
              @click="fileInput?.click()"
            />
            <UButton
              icon="i-lucide-send"
              :label="$t('app.composer.send')"
              :loading="sending"
              class="h-11 shrink-0 gap-1.75 rounded-[14px] px-3.5 text-[15px] font-semibold"
              :ui="{ leadingIcon: 'size-4.25' }"
              :class="hasRecipient ? '' : 'opacity-50'"
              @click="send()"
            />
          </header>

          <div class="flex min-h-0 flex-1 flex-col overflow-y-auto overscroll-contain">
            <div class="flex shrink-0 flex-col px-4">
              <ReuseFromRow />
              <RecipientInput
                ref="toInput"
                v-model="draft.to"
                touch
                :label="$t('app.composer.to')"
                :placeholder="$t('app.composer.toPlaceholder')"
              >
                <template v-if="!showCopies" #trailing>
                  <button type="button" class="h-9 shrink-0 rounded-lg px-1.5 text-[13px] font-semibold text-primary active:bg-primary/10" @click="showCopyRows">
                    {{ $t('app.composer.copies') }}
                  </button>
                </template>
              </RecipientInput>
              <template v-if="showCopies">
                <RecipientInput ref="ccInput" v-model="draft.cc" touch :label="$t('app.composer.cc')" />
                <RecipientInput ref="bccInput" v-model="draft.bcc" touch :label="$t('app.composer.bcc')" />
              </template>
              <ReuseSubjectRow />
            </div>
            <div v-if="draft.attachments.length || draft.forwarded.length" class="flex shrink-0 flex-wrap gap-2 border-b border-default px-4 py-2.5">
              <ReuseAttachments />
            </div>
            <MailEditor
              ref="editor"
              :key="session"
              composer
              touch
              :more-items="mobileMenu"
              :model-value="draft.html ?? ''"
              :readonly="sending"
              :max-bytes="MAX_MESSAGE_BYTES"
              :used-bytes="attachedBytes"
              autofocus="start"
              :placeholder="$t('app.composer.bodyPlaceholder')"
              @update:model-value="htmlFor(session)"
              @text="textFor(session)"
              @changed="onEdited"
              @files="attach"
            />
          </div>
        </div>
      </div>
    </template>
  </USlideover>
</template>
