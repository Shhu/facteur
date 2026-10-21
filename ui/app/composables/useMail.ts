import { invoke } from '~/utils/ipc'
import type { AccountColor } from '~/utils/accountColors'
import type { Diagnostics, LogLine } from '~/composables/usePerf'
import type {
  Account, AccountStats, AddressBookStatus, CalendarEvent, CalendarInfo, EventAnswer, EventDraft, SeriesRepeat, CalendarStatus, AiStatus, FoundEvent, OpsDetails, Address, Coverage, MailClientStatus, BackupManifest, BackupMessage, BackupStats, BackupStatus, Catalog, AttachmentReport, Category, CategoryUnread, Dashboard, Discovered, Draft, FolderSummary, FollowUp, HeldSender, Identity, Rule, RulePreview, LinkFacts, LinkReport, Outgoing, Mailbox, MessageDetail, MessageSummary, NewAccount, OAuthIdentity, OAuthProvider, OpsStatus, Unsubscribed,
  RestoreFilter, RestoreRequest, RestoreStats, Screen, Screening, SimilarCriteria, SimilarFacets, SimilarResult, ThreadFilter, ThreadSummary
} from '~/utils/types'

/** Engine commands (src-tauri/src/lib.rs). */
/** The row and the occurrence it stood for: the engine refuses a row given to another since. */
const eventRef = (e: CalendarEvent) => ({ id: e.id, uid: e.uid, instance: e.instance })

export const api = {
  accounts: () => invoke<Account[]>('accounts'),
  discover: (email: string) => invoke<Discovered>('discover', { email }),
  /** `extended`: also what is asked for only when used (Microsoft: calendar writing, contacts). */
  oauthLogin: (provider: OAuthProvider, email: string | null, extended = false) =>
    invoke<OAuthIdentity>('oauth_login', { provider, email, extended }),
  addAccount: (account: NewAccount) => invoke<Account>('add_account', { account }),
  removeAccount: (id: number) => invoke<void>('remove_account', { id }),
  mailboxes: (accountId: number) => invoke<Mailbox[]>('mailboxes', { accountId }),
  /** `category`: smart inbox tab (people, notifications or newsletters only). */
  threads: (mailboxId: number, filter: ThreadFilter, limit: number, offset: number, category: Category | null = null) =>
    invoke<ThreadSummary[]>('threads', { mailboxId, filter, limit, offset, category }),
  unifiedThreads: (filter: ThreadFilter, limit: number, offset: number, category: Category | null = null) =>
    invoke<ThreadSummary[]>('unified_threads', { filter, limit, offset, category }),
  /** `null`: the unified inbox. */
  categoryUnread: (mailboxId: number | null) => invoke<CategoryUnread>('category_unread', { mailboxId }),
  setSenderCategory: (email: string, category: Category) => invoke<void>('set_sender_category', { email, category }),
  senderCategory: (email: string) => invoke<Category | null>('sender_category', { email }),
  /** "Done" view: what left the inboxes, all accounts together. */
  doneThreads: (filter: ThreadFilter, limit: number, offset: number) =>
    invoke<ThreadSummary[]>('done_threads', { filter, limit, offset }),
  toInbox: (ids: number[]) => invoke<number | null>('to_inbox', { ids }),
  dashboard: (since: number) => invoke<Dashboard>('dashboard', { since }),
  opsStatus: () => invoke<OpsStatus>('ops_status'),
  opsDetails: () => invoke<OpsDetails>('ops_details'),
  /** Without `confirmed`, a mail unsubscribe first returns what would be sent (`confirm`). */
  unsubscribe: (id: number, confirmed = false) => invoke<Unsubscribed>('unsubscribe', { id, confirmed }),
  // Moving actions return an undo token, valid for a few seconds (useUndo).
  reportSpam: (ids: number[]) => invoke<number | null>('report_spam', { ids }),
  notSpam: (ids: number[]) => invoke<number | null>('not_spam', { ids }),
  undo: (token: number) => invoke<void>('undo', { token }),
  createFolder: (accountId: number, parent: number | null, name: string) => invoke<number>('create_folder', { accountId, parent, name }),
  renameFolder: (mailboxId: number, name: string) => invoke<void>('rename_folder', { mailboxId, name }),
  rules: () => invoke<Rule[]>('rules'),
  saveRule: (rule: Rule) => invoke<number>('save_rule', { rule }),
  deleteRule: (id: number) => invoke<void>('delete_rule', { id }),
  reorderRules: (ids: number[]) => invoke<void>('reorder_rules', { ids }),
  runRule: (id: number) => invoke<number>('run_rule', { id }),
  /** Dry run on the inbox; nothing is changed. */
  previewRule: (rule: Rule) => invoke<RulePreview>('preview_rule', { rule }),
  setIdentities: (accountId: number, identities: Identity[]) => invoke<void>('set_identities', { accountId, identities }),
  respondInvite: (id: number, partstat: 'ACCEPTED' | 'TENTATIVE' | 'DECLINED') => invoke<void>('respond_invite', { id, partstat }),
  saveInvite: (id: number, name: string) => invoke<string | null>('save_invite', { id, name }),
  messageSource: (id: number) => invoke<string>('message_source', { id }),
  saveMessageSource: (id: number, name: string) => invoke<string | null>('save_message_source', { id, name }),
  blockSender: (email: string) => invoke<void>('block_sender', { email }),
  unblockSender: (email: string) => invoke<void>('unblock_sender', { email }),
  blockedSenders: () => invoke<string[]>('blocked_senders'),
  setAccountLabel: (accountId: number, label: string) => invoke<void>('set_account_label', { accountId, label }),
  /** Emits no engine change event: reload the accounts afterwards (`loadAccounts`). */
  setAccountColor: (accountId: number, color: string) => invoke<void>('set_account_color', { accountId, color }),
  /** Accounts not listed follow. */
  reorderAccounts: (ids: number[]) => invoke<void>('reorder_accounts', { ids }),
  openLogs: () => invoke<void>('open_logs'),
  diagnostics: () => invoke<Diagnostics>('diagnostics'),
  readLog: (lines: number, level: string | null, query: string | null) => invoke<LogLine[]>('read_log', { lines, level, query }),
  /** `false`: none found, initials shown. */
  refreshAccountAvatar: (accountId: number) => invoke<boolean>('refresh_account_avatar', { accountId }),
  /** `image`: a data URL picked by the user; `null`: initials. */
  setAccountAvatar: (accountId: number, image: string | null) => invoke<void>('set_account_avatar', { accountId, image }),
  /** Turned off, the messages still waiting go back to the inbox. */
  setScreener: (accountId: number, on: boolean) => invoke<void>('set_screener', { accountId, on }),
  /** `null`: all accounts. Most recent first. */
  heldSenders: (accountId: number | null) => invoke<HeldSender[]>('held_senders', { accountId }),
  /** `domain`: every address of their domain. Undoable. */
  screenSender: (accountId: number, email: string, verdict: Screen, domain: boolean) =>
    invoke<number | null>('screen_sender', { accountId, email, verdict, domain }),
  screening: (accountId: number) => invoke<Screening[]>('screening', { accountId }),
  /** The sender's next message is screened again, unless they are known by then. */
  forgetScreening: (accountId: number, pattern: string) => invoke<void>('forget_screening', { accountId, pattern }),
  /** `inboxOnly`: the inbox only (home screen), not every folder. */
  markAccountRead: (accountId: number, inboxOnly = false) => invoke<number>('mark_account_read', { accountId, inboxOnly }),
  accountStats: (accountId: number) => invoke<AccountStats>('account_stats', { accountId }),
  /** `null`: no logo (personal address, setting off, nothing found). */
  senderLogos: (emails: string[]) => invoke<Record<string, string | null>>('sender_logos', { emails }),
  similarFacets: (id: number) => invoke<SimilarFacets>('similar_facets', { id }),
  findSimilar: (criteria: SimilarCriteria) => invoke<SimilarResult>('find_similar', { criteria }),
  similarFromServer: (criteria: SimilarCriteria) => invoke<number>('similar_from_server', { criteria }),
  coverage: () => invoke<Coverage[]>('coverage'),
  historyBusy: () => invoke<number[]>('history_busy'),
  fetchAllHistory: (accountId: number) => invoke<number>('fetch_all_history', { accountId }),
  trustedImages: () => invoke<string[]>('trusted_images'),
  addTrustedImages: (entry: string) => invoke<string>('add_trusted_images', { entry }),
  removeTrustedImages: (key: string) => invoke<void>('remove_trusted_images', { key }),
  getSetting: (key: string) => invoke<string | null>('get_setting', { key }),
  setSetting: (key: string, value: string) => invoke<void>('set_setting', { key, value }),
  /** Accounts whose server keeps dropping connections: the single-connection mode is offered. */
  connectionAdvice: () => invoke<number[]>('connection_advice'),
  aiStatus: () => invoke<AiStatus>('ai_status'),
  /** `ai_changed` follows as it downloads. */
  aiInstall: (id: string) => invoke<void>('ai_install', { id }),
  aiCancel: () => invoke<void>('ai_cancel'),
  aiUseFile: (path: string) => invoke<void>('ai_use_file', { path }),
  aiTurnOff: () => invoke<void>('ai_turn_off'),
  aiSummarize: (accountId: number, threadId: number) => invoke<string>('ai_summarize', { accountId, threadId }),
  aiFindEvent: (id: number) => invoke<FoundEvent | null>('ai_find_event', { id }),
  dictationOff: () => invoke<void>('dictation_off'),
  /** `pcm`: 16 kHz mono, 16-bit little-endian. */
  aiTranscribe: (pcm: Uint8Array, language: string) => invoke<string>('ai_transcribe', pcm, { headers: { language } }),
  /** `true` turns the mode on, `false` never offers it again, `null`: later. */
  answerConnectionAdvice: (accountId: number, accept: boolean | null) => invoke<void>('answer_connection_advice', { accountId, accept }),
  mailClientStatus: () => invoke<MailClientStatus>('mail_client_status'),
  registerMailClient: () => invoke<void>('register_mail_client'),
  takeLaunchMailto: () => invoke<string | null>('take_launch_mailto'),
  reconnect: (accountId: number, ticket: string) => invoke<Account>('reconnect', { accountId, ticket }),
  /** Checked with the server, then saved to the keychain. */
  updatePassword: (accountId: number, password: string) => invoke<void>('update_password', { accountId, password }),
  /** Account id → why the server refused its credentials. */
  authProblems: () => invoke<Record<number, string>>('auth_problems'),
  /** After sleep or a network return: the engine syncs and listens again now. */
  wake: () => invoke<void>('wake'),
  retryOp: (id: number) => invoke<void>('retry_op', { id }),
  discardOp: (id: number) => invoke<void>('discard_op', { id }),
  setSignature: (accountId: number, signature: string) => invoke<void>('set_signature', { accountId, signature }),
  contacts: (query: string) => invoke<Address[]>('contacts', { query }),
  saveDraft: (draft: Draft) => invoke<string>('save_draft', { draft }),
  discardDraft: (accountId: number, draftId: string) => invoke<void>('discard_draft', { accountId, draftId }),
  openDraft: (id: number) => invoke<Draft>('open_draft', { id }),
  loadOlder: (mailboxId: number) => invoke<number>('load_older', { mailboxId }),
  /** `bins`: also search the bin and junk folders. */
  searchServer: (accountId: number, query: string, bins = false) => invoke<MessageSummary[]>('search_server', { accountId, query, bins }),
  backup: (accountId: number, dest: string) => invoke<BackupStats>('backup', { accountId, dest }),
  backupBusy: () => invoke<number[]>('backup_busy'),
  backupStatus: (dest: string) => invoke<BackupStatus[]>('backup_status', { dest }),
  listBackups: (dest: string) => invoke<BackupManifest[]>('list_backups', { dest }),
  backupCatalog: (dest: string, email: string, filter: RestoreFilter, limit: number, offset: number) =>
    invoke<Catalog>('backup_catalog', { dest, email, filter, limit, offset }),
  backupMessage: (dest: string, email: string, id: number) => invoke<BackupMessage>('backup_message', { dest, email, id }),
  restore: (dest: string, source: string, accountId: number, request: RestoreRequest) =>
    invoke<RestoreStats>('restore', { dest, source, accountId, request }),
  /** `prefer`: the folder the thread is opened from; its copy of each message is the one shown. */
  thread: (accountId: number, threadId: number, prefer?: number | null) => invoke<MessageSummary[]>('thread', { accountId, threadId, prefer: prefer ?? null }),
  message: (id: number) => invoke<MessageDetail>('message', { id }),
  search: (accountId: number, query: string, bins = false) => invoke<MessageSummary[]>('search', { accountId, query, bins }),
  setFlag: (ids: number[], flag: 'seen' | 'flagged', value: boolean) => invoke<void>('set_flag', { ids, flag, value }),
  /** `from`: the folder they are moved from (Gmail: the label they leave). */
  moveTo: (ids: number[], mailboxId: number, from?: number | null) => invoke<number | null>('move_to', { ids, mailboxId, from: from ?? null }),
  /** Gmail: the messages stay where they are. */
  setLabels: (ids: number[], add: number[], remove: number[]) => invoke<number | null>('set_labels', { ids, add, remove }),
  messageLabels: (ids: number[]) => invoke<Record<number, number[]>>('message_labels', { ids }),
  archive: (ids: number[]) => invoke<number | null>('archive', { ids }),
  /** Muting archives `ids` (the conversation's messages in the folder shown). */
  muteThread: (accountId: number, threadId: number, muted: boolean, ids: number[]) => invoke<number | null>('mute_thread', { accountId, threadId, muted, ids }),
  threadMuted: (accountId: number, threadId: number) => invoke<boolean>('thread_muted', { accountId, threadId }),
  /** `purge`: from the bin itself, where deleting again is for good. */
  trash: (ids: number[], purge = false) => invoke<number | null>('trash', { ids, purge }),
  /** Undoable. */
  trashFolder: (mailboxId: number) => invoke<number | null>('trash_folder', { mailboxId }),
  /** The outbox id, or `null` if already sent. */
  send: (draft: Draft, sendAt: number | null = null, remindAfter: number | null = null) =>
    invoke<number | null>('send', { draft, sendAt, remindAfter }),
  outbox: () => invoke<Outgoing[]>('outbox'),
  /** `recent`: how many recent messages to list (50 by default). */
  folderSummary: (mailboxId: number, recent?: number) => invoke<FolderSummary>('folder_summary', { mailboxId, recent }),
  /** Sub-folders included; 50 by default. */
  folderMessagesFrom: (mailboxId: number, from: string, limit?: number) =>
    invoke<MessageSummary[]>('folder_messages_from', { mailboxId, from, limit }),
  /** Permanent deletion. */
  emptyFolder: (mailboxId: number) => invoke<number>('empty_folder', { mailboxId }),
  /** `dest`: where the messages go; `null`: the bin. */
  deleteFolder: (mailboxId: number, dest: number | null) => invoke<void>('delete_folder', { mailboxId, dest }),
  inspectLink: (messageId: number, href: string, text: string) => invoke<LinkReport>('inspect_link', { messageId, href, text }),
  checkLink: (href: string, text: string) => invoke<LinkFacts | null>('check_link', { href, text }),
  cancelSend: (id: number) => invoke<Draft>('cancel_send', { id }),
  sendNow: (id: number) => invoke<void>('send_now', { id }),
  snooze: (ids: number[], until: number) => invoke<number | null>('snooze', { ids, until }),
  unsnooze: (accountId: number, threadId: number) => invoke<void>('unsnooze', { accountId, threadId }),
  followUps: () => invoke<FollowUp[]>('follow_ups'),
  dismissFollowUp: (id: number) => invoke<void>('dismiss_follow_up', { id }),
  sync: (accountId: number) => invoke<void>('sync', { accountId }),
  calendars: () => invoke<CalendarInfo[]>('calendars'),
  /** Enabled calendars only, sorted by start. */
  calendarEvents: (from: number, to: number) => invoke<CalendarEvent[]>('calendar_events', { from, to }),
  setCalendarEnabled: (id: number, enabled: boolean) => invoke<void>('set_calendar_enabled', { id, enabled }),
  createEvent: (calendarId: number, draft: EventDraft, key: string) => invoke<number | null>('create_event', { calendarId, draft, key }),
  updateEvent: (e: CalendarEvent, draft: EventDraft, series: boolean) => invoke<void>('update_event', { event: eventRef(e), draft, series }),
  eventRepeat: (e: CalendarEvent) => invoke<SeriesRepeat>('event_repeat', { event: eventRef(e) }),
  deleteEvent: (e: CalendarEvent, series: boolean) => invoke<void>('delete_event', { event: eventRef(e), series }),
  respondEvent: (e: CalendarEvent, answer: EventAnswer, series: boolean) => invoke<void>('respond_event', { event: eventRef(e), answer, series }),
  addressBookStatus: () => invoke<AddressBookStatus[]>('address_book_status'),
  syncAddressBooks: (accountId: number | null) => invoke<void>('sync_address_books', { accountId }),
  /** `null`: all accounts. `calendar_changed` follows. */
  syncCalendars: (accountId: number | null) => invoke<void>('sync_calendars', { accountId }),
  calendarStatus: () => invoke<CalendarStatus[]>('calendar_status'),
  setCalendarIgnored: (accountId: number, ignored: boolean) => invoke<void>('set_calendar_ignored', { accountId, ignored }),
  /** `null`: back to the discovered address. */
  setCaldavUrl: (accountId: number, url: string | null) => invoke<void>('set_caldav_url', { accountId, url }),
  /** Save commands open the engine's Save As dialog; they return the chosen path, or `null`. */
  /** `confirmed`: the UI already warned about a suspicious file (the engine asks otherwise). */
  /** `openable`: facteur may open the saved file with the system's program (`openSaved`). */
  saveAttachment: (id: number, index: number, confirmed = false) => invoke<{ path: string, openable: boolean } | null>('save_attachment', { id, index, confirmed }),
  openSaved: (path: string) => invoke<void>('open_saved', { path }),
  inspectAttachment: (id: number, index: number) => invoke<AttachmentReport>('inspect_attachment', { id, index }),
  /** Bytes to display, with the type sniffed from the content by the engine (never the sender's
   *  declared type). The response is framed as [n][n bytes of ASCII MIME][content]. */
  attachmentView: async (id: number, index: number) => {
    const buf = new Uint8Array(await invoke<ArrayBuffer>('attachment_view', { id, index }))
    const n = buf[0] ?? 0
    return { mime: new TextDecoder().decode(buf.subarray(1, 1 + n)), data: buf.slice(1 + n).buffer }
  }
}

/** Latest mailbox request per account: an older answer arriving later is ignored. */
const mailboxesRequest = new Map<number, number>()
let requestCounter = 0
/** An accounts answer older than the one applied is dropped (its caller then sees the newer list). */
let accountsRequest = 0
let accountsApplied = 0

export function useMail() {
  const accounts = useState<Account[]>('accounts', () => [])
  /** Account of the folder shown; the default account for composing. */
  const accountId = useState<number | null>('accountId', () => null)
  const mailboxes = useState<Mailbox[]>('mailboxes', () => [])
  const syncing = useState<number[]>('syncing', () => [])
  const syncProgress = useState<Record<number, { mailbox: string, done: number, total: number }>>('sync-progress', () => ({}))
  /** Bumped on every cache change: views watch it to reload. */
  const tick = useState('tick', () => 0)

  const account = computed(() => accounts.value.find(a => a.id === accountId.value) ?? null)
  const isSyncing = computed(() => syncing.value.length > 0)
  const { unreadOf } = useFolderCounts()
  const unifiedUnread = computed(() => mailboxes.value.filter(m => m.role === 'inbox').reduce((n, m) => n + unreadOf(m), 0))

  async function loadAccounts() {
    const mine = ++accountsRequest
    const list = await api.accounts()
    if (mine < accountsApplied) return
    accountsApplied = mine
    accounts.value = list
    if (!accounts.value.some(a => a.id === accountId.value)) {
      accountId.value = accounts.value[0]?.id ?? null
    }
  }

  /** Other accounts' folders are left as they are. */
  async function loadMailboxes(only?: number[]) {
    const targets = accounts.value.filter(a => !only || only.includes(a.id))
    const request = ++requestCounter
    for (const a of targets) mailboxesRequest.set(a.id, request)
    const lists = await Promise.all(targets.map(a => api.mailboxes(a.id).then(list => ({ id: a.id, list }))))
    // A newer request for an account makes this one stale for it.
    const fresh = lists.filter(l => mailboxesRequest.get(l.id) === request)
    if (!fresh.length) return
    const ids = new Set(fresh.map(l => l.id))
    const known = new Set(accounts.value.map(a => a.id))
    mailboxes.value = [
      ...mailboxes.value.filter(m => !ids.has(m.account_id) && known.has(m.account_id)),
      ...fresh.flatMap(l => l.list)
    ]
  }

  function mailboxByRole(role: Mailbox['role'], account = accountId.value) {
    return mailboxes.value.find(m => m.role === role && m.account_id === account)
  }

  function accountById(id: number) {
    return accounts.value.find(a => a.id === id) ?? null
  }

  /** Account address and aliases, lowercased. */
  const ownAddresses = computed(() => new Map(accounts.value.map(a => [
    a.id,
    new Set([a.email, ...a.identities.map(i => i.email)].map(e => e.toLowerCase()))
  ])))
  const noAddress: ReadonlySet<string> = new Set()
  function ownSet(accountId: number): ReadonlySet<string> {
    return ownAddresses.value.get(accountId) ?? noAddress
  }
  /** Without `accountId`, any account's addresses count. */
  function isMine(email: string | null | undefined, accountId?: number): boolean {
    if (!email) return false
    const e = email.toLowerCase()
    if (accountId !== undefined) return ownSet(accountId).has(e)
    return [...ownAddresses.value.values()].some(s => s.has(e))
  }

  /** Failures arrive through the engine's `sync_finished` event. */
  async function syncAll() {
    await syncByUser(accounts.value.map(a => a.id))
  }

  function accountColor(id: number): AccountColor {
    const a = accountById(id)
    return ACCOUNT_COLORS[a ? accountColorName(a, accounts.value) : 'orange']
  }

  return {
    accounts, accountId, account, accountById, accountColor, mailboxes, syncing, syncProgress, isSyncing, unifiedUnread, tick,
    ownSet, isMine, loadAccounts, loadMailboxes, mailboxByRole, syncAll
  }
}

export function useErrorToast() {
  const toast = useToast()
  return (e: unknown, title = tr('common.error')) => {
    toast.add({ title, description: String(e), color: 'error', icon: 'i-lucide-circle-alert' })
    logUi('warn', `${title}: ${String(e)}`)
  }
}

/** For errors that would otherwise leave no trace in facteur's log file. */
export function logUi(level: 'error' | 'warn' | 'info', message: string) {
  invoke('log_ui', { level, message }).catch(() => {})
}
