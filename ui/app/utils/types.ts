// Mirrors the types serialised by facteur-core (crates/core/src/model.rs).

export type Security = 'tls' | 'starttls' | 'none'

export interface ServerConfig {
  host: string
  port: number
  security: Security
}

export type OAuthProvider = 'google' | 'microsoft'

export type AuthMethod = { kind: 'password' } | { kind: 'oauth2', provider: OAuthProvider }

export type NewAuth = { kind: 'password' } | { kind: 'oauth2', provider: OAuthProvider, ticket: string }

/** The ticket refers to the tokens the engine keeps in memory. */
export interface OAuthIdentity {
  ticket: string
  email: string | null
}

export interface Account {
  id: number
  name: string
  email: string
  username: string
  imap: ServerConfig
  smtp: ServerConfig
  auth: AuthMethod
  last_sync: number | null
  signature: string
  avatar: string | null
  label: string | null
  identities: Identity[]
  /** Palette name; null: picked by the account's rank. */
  color: string | null
  /** Picked by the user (an image, or none for initials): never replaced automatically. */
  avatar_custom: boolean
  /** New-senders filter on since this date; null: off. */
  screener_since: number | null
  /** Absent when the server does not report it. */
  quota?: Quota | null
}

export interface Quota {
  used: number
  limit: number
  at: number
}

/** Share of the quota (0-1) from which facteur warns that the mailbox is nearly full. */
export const QUOTA_WARNING = 0.9

/** Empty name and signature fall back to the account's. */
export interface Identity {
  email: string
  name: string
  signature: string
}

export interface Rule {
  id: number
  account_id: number
  name: string
  enabled: boolean
  match_all: boolean
  conditions: RuleCondition[]
  actions: RuleAction[]
  stop: boolean
}
export interface RulePreview {
  count: number
  unread: number
  with_attachments: number
  bulk: number
  oldest: number | null
  newest: number | null
  top_senders: CountedAddress[]
  messages: MessageSummary[]
}
export type RuleField = 'from' | 'to' | 'subject' | 'list_id' | 'has_attachment' | 'bulk'
export type RuleOp = 'contains' | 'not_contains' | 'is' | 'ends_with'
export interface RuleCondition { field: RuleField, op: RuleOp, value: string }
export type RuleAction =
  | { kind: 'move', mailbox_id: number }
  | { kind: 'archive' }
  | { kind: 'trash' }
  | { kind: 'mark_read' }
  | { kind: 'pin' }

export interface Invite {
  method: string
  uid: string
  summary: string
  location: string | null
  description: string | null
  start: number | null
  end: number | null
  all_day: boolean
  tzid: string | null
  organizer: Address | null
  attendees: { address: Address, partstat: string | null }[]
  recurring: boolean
  cancelled: boolean
  /** Revision of the event (SEQUENCE): above 0, the invitation is an update. */
  sequence: number
  my_response: string | null
  /** Recurring timed invitation: its next occurrence not over yet, when the rule is understood. */
  next_start: number | null
  next_end: number | null
}

export interface CountedAddress {
  name: string | null
  email: string
  count: number
}

export interface AccountStats {
  total: number
  received: number
  sent: number
  unread: number
  flagged: number
  with_attachments: number
  bulk: number
  bytes: number
  received_30d: number
  sent_30d: number
  oldest: number | null
  newest: number | null
  months: { month: string, received: number, sent: number }[]
  weekdays: number[]
  hours: number[]
  top_senders: CountedAddress[]
  top_recipients: CountedAddress[]
  folders: { id: number, name: string, role: Role | null, total: number, unread: number }[]
}

export type DiscoverySource = 'autoconfig' | 'ispdb' | 'mx' | 'srv' | 'guess'

export interface Discovered {
  imap: ServerConfig
  smtp: ServerConfig
  username: string
  source: DiscoverySource
  provider: string | null
}

export interface NewAccount {
  name: string
  email: string
  username: string
  password: string
  imap: ServerConfig
  smtp: ServerConfig
  auth: NewAuth
}

export type Role = 'inbox' | 'sent' | 'drafts' | 'trash' | 'junk' | 'archive' | 'all' | 'flagged' | 'snoozed' | 'screener' | 'refused'

export interface Mailbox {
  id: number
  account_id: number
  name: string
  display_name: string
  delimiter: string | null
  role: Role | null
  selectable: boolean
  unread: number
  /** Messages in facteur's cache. */
  total: number
  /** Messages on the server at the last sync; null until the folder has been synced. */
  server_total: number | null
  threads?: number
  unread_threads?: number
  /** Gmail: a label of the user's own, offered in the labels menu. */
  user_label?: boolean
}

export interface Address {
  name: string | null
  email: string
}

export interface Flags {
  seen: boolean
  flagged: boolean
  answered: boolean
  draft: boolean
}

export interface MessageSummary {
  id: number
  account_id: number
  mailbox_id: number
  thread_id: number
  subject: string
  from: Address | null
  to: Address[]
  date: number
  preview: string
  flags: Flags
  has_attachments: boolean
  /** Gmail: folders (labels) holding a copy of the message (search results only). */
  labels?: number[]
}

export interface Dashboard {
  received_today: number
  flagged_count: number
  flagged: MessageSummary[]
}

export type ThreadFilter = 'all' | 'unread' | 'flagged'

export type Category = 'people' | 'notification' | 'newsletter'
export type CategoryUnread = Record<Category, number>

export interface ThreadSummary {
  account_id: number
  mailbox_id: number
  thread_id: number
  latest_id: number
  subject: string
  from: Address | null
  to: Address[]
  participants: Address[]
  message_ids: number[]
  date: number
  preview: string
  count: number
  unread: number
  flagged: boolean
  has_attachments: boolean
  /** Sort key: date of the last message, or when it came back from snooze. */
  activity: number
  snoozed_until: number | null
  woken: boolean
  category: Category
  /** Gmail's All Mail: folders (labels) holding a copy of the thread's messages. */
  labels?: number[]
}

/** Subfolders included. */
export interface FolderSummary {
  mailbox: Mailbox
  children: Mailbox[]
  server_count: number | null
  local: number
  unread: number
  flagged: number
  with_attachments: number
  bytes: number
  oldest: number | null
  newest: number | null
  years: [number, number][]
  top_senders: CountedAddress[]
  recent: MessageSummary[]
  gmail: boolean
}

export interface Outgoing {
  id: number
  account_id: number
  subject: string
  to: string[]
  send_at: number
  scheduled: boolean
  state: 'pending' | 'sending' | 'failed'
  error: string | null
  remind_after: number | null
  /** The message it answers: the conversation shows it while it is on its way. */
  in_reply_to: number | null
  preview: string
}

export interface FollowUp {
  id: number
  account_id: number
  subject: string
  to: string[]
  sent_at: number
  due: number
  thread_id: number | null
  mailbox_id: number | null
}

export interface Attachment {
  index: number
  filename: string
  mime: string
  size: number
}

export interface MessageDetail {
  summary: MessageSummary
  cc: Address[]
  reply_to: Address[]
  message_id: string | null
  references: string[]
  html: string
  is_plain: boolean
  text: string | null
  has_remote_content: boolean
  attachments: Attachment[]
  unsubscribe: Unsubscribe | null
  bulk: boolean
  trust: Trust
  /** Domain (or personal address) to approve so remote images load automatically. */
  sender_key: string | null
  /** Approved sender and nothing suspicious. */
  images_allowed: boolean
  invite: Invite | null
  /** A day and time the message's own words give: offered as an event. */
  appointment: Appointment | null
}

export type TrustLevel = 'low' | 'medium' | 'high'

export type Screen = 'inbox' | 'newsletter' | 'notification' | 'block'

export interface HeldSender {
  account_id: number
  email: string
  name: string | null
  count: number
  latest: MessageSummary
  trust: TrustLevel
  /** A decision may cover the whole domain (never a public mail provider's). */
  domainable: boolean
}

/** `address` is an address or a whole domain (`@example.org`). */
export interface Screening {
  pattern: string
  decision: 'accept' | 'block'
  created_at: number
}

export interface NewMail {
  id: number
  thread_id: number
  mailbox_id: number
  from: Address | null
  subject: string
  preview: string
  bulk: boolean
  trust: TrustLevel
  held: boolean
}

export interface Trust {
  score: number
  level: TrustLevel
  reasons: { positive: boolean, text: string }[]
}

export interface AttachmentReport {
  filename: string
  declared_mime: string
  detected: string
  kind: 'image' | 'pdf' | 'text' | 'archive' | 'document' | 'executable' | 'other'
  size: number
  viewable: boolean
  view_mime: string | null
  archive_entries: string[]
  archive_count: number
  trust: Trust
}

/** From the engine's `links::analyze`, without any network lookup. */
export interface LinkFacts {
  url: string
  host: string
  domain: string
  secure: boolean
  ip: boolean
  punycode: boolean
  shown_host: string | null
  deceptive: boolean
  shortener: boolean
  redirect_to: string | null
  redirect_domain: string | null
  tracking: string[]
  clean_url: string | null
}

export interface AnnouncedDomain {
  domain: string
  /** Creation date from RDAP, Unix seconds. */
  registered: number | null
  /** Other messages (junk aside) linking to it or sent from it. */
  seen_messages: number
  sender_domain: boolean
}

/** How the sender's domain proves its mail; sent only when something is missing. */
export interface SenderAuth {
  domain: string
  /** Published; null when the lookup failed. */
  dmarc: boolean | null
  spf: boolean | null
  dkim: { state: 'own' | 'signed' | 'invalid' | 'missing' | 'unknown' } | { state: 'other', domain: string }
}

export interface LinkReport extends LinkFacts {
  registered: number | null
  seen_messages: number
  announced: AnnouncedDomain | null
  sender_auth: SenderAuth | null
  trust: Trust
}

export interface Unsubscribe {
  one_click: string | null
  web: string | null
  mailto: string | null
}

export type Unsubscribed =
  | { method: 'one_click' }
  | { method: 'mailto', address: string }
  | { method: 'confirm', address: string, subject: string }
  | { method: 'web', url: string }

export interface MailClientStatus {
  supported: boolean
  registered: boolean
  default: boolean
}

export type NotificationMode = 'people' | 'all' | 'none'

export interface OutgoingAttachment {
  filename: string
  mime: string
  data: string
}

export interface Draft {
  account_id: number
  to: string[]
  cc: string[]
  bcc: string[]
  subject: string
  body: string
  html: string | null
  in_reply_to: number | null
  attachments: OutgoingAttachment[]
  forwarded: ForwardedAttachment[]
  /** Message-ID of the saved draft, returned by the engine on first save. */
  draft_id: string | null
  /** Sending alias; absent: the account's address. */
  from?: string | null
  /** Threading headers of the message replied to (Message-ID, References), kept by the engine. */
  reply_ids?: [string, string[]] | null
  /** Drafts copy without a Message-ID this draft was reopened from (deleted by the engine on save). */
  source?: number | null
}

export interface ForwardedAttachment {
  message_id: number
  index: number
  filename: string
  mime: string
  size: number
}

export interface FailedOp {
  id: number
  account_id: number
  label: string
  error: string
  created_at: number
}

export interface OpsStatus {
  pending: number
  failed: FailedOp[]
}

export type OpState = 'held' | 'blocked' | 'waiting' | 'retrying' | 'failed'

export interface OpView {
  id: number
  account_id: number
  /** `move_set`, `append`, `store_set`… */
  kind: string
  label: string
  mailbox: string | null
  dest: string | null
  created_at: number
  attempts: number
  error: string | null
  not_before: number | null
  failed: boolean
  size: number
  state: OpState
  blocked_by: number | null
}

export interface OpsDetails {
  ops: OpView[]
  /** Account id → last error while sending changes or syncing. */
  errors: Record<number, string>
}

export type EngineEvent =
  | { type: 'sync_started', account_id: number }
  | { type: 'sync_finished', account_id: number, new_messages: number, new_in_inbox: number, error: string | null }
  | { type: 'changed', account_id: number }
  | { type: 'backup_progress', account_id: number, mailbox: string, done: number, total: number }
  | { type: 'backup_finished', account_id: number, downloaded: number, messages: number, error: string | null }
  | { type: 'restore_progress', account_id: number, mailbox: string, done: number, total: number }
  | { type: 'ops_changed' }
  | { type: 'sync_progress', account_id: number, mailbox: string, done: number, total: number }
  | { type: 'new_mail', account_id: number, messages: NewMail[] }
  | { type: 'logos_changed' }
  | { type: 'outbox_changed' }
  | { type: 'send_failed', id: number, account_id: number, subject: string, error: string, retrying: boolean }
  | { type: 'follow_ups_due', follow_ups: FollowUp[] }
  | { type: 'calendar_changed', account_id?: number | null }
  /** The account's credentials were refused (`error`), or accepted again (`null`). */
  | { type: 'auth_changed', account_id: number, error: string | null }
  /** The account's server keeps dropping connections: the single-connection mode is offered. */
  | { type: 'one_connection_advised', account_id: number }
  /** An event's reminder: shown by the system's notification. */
  | { type: 'event_reminder', event: CalendarEvent, minutes: number }
  | { type: 'ai_changed' }
  /** An opened message's trust, weighed again after the engine checked its DKIM signatures. */
  | { type: 'trust_changed', message_id: number, trust: Trust }

/** A thread, or a single message in search results. */
export interface ListItem {
  key: string
  accountId: number
  /** Folder of the most recent message: actions act from there. */
  mailboxId: number
  accountLabel: string | null
  accountDot: string | null
  threadId: number
  latestId: number
  /** Messages targeted by actions taken from the list. */
  ids: number[]
  avatar: Address | null
  who: string
  subject: string
  preview: string
  date: number
  count: number
  unread: boolean
  flagged: boolean
  attachments: boolean
  /** Sort date: return from snooze, else `date`. */
  activity: number
  snoozedUntil: number | null
  woken: boolean
  /** Gmail's All Mail: the thread's labels, as shown. */
  labels?: string[]
}

export interface BackupManifest {
  email: string
  last_backup: number
  messages: number
  bytes: number
  folders: { name: string, path: string, messages: number }[]
  /** Messages the server did not send during that backup (0 or absent: complete). */
  failed?: number
  /** The backup's directory: what a restore names it by. */
  dir: string
}

export interface BackupStatus {
  account_id: number
  email: string
  manifest: BackupManifest | null
}

export interface BackupStats {
  folders: number
  messages: number
  downloaded: number
  bytes: number
}

export interface RestoreStats {
  target: string
  folders: number
  messages: number
  skipped: number
  /** Not restored: refused by the server or missing from the backup. */
  failed: number
  /** The first few: subject (or file) and reason. */
  failures: string[]
}

export interface RestoreFilter {
  folders: string[]
  since: number | null
  until: number | null
  query: string | null
  flagged_only: boolean
}

export type RestoreTarget = 'new_folder' | 'original'

export interface RestoreRequest {
  filter: RestoreFilter
  ids: number[] | null
  target: RestoreTarget
}

export interface CatalogFolder {
  path: string
  name: string
  total: number
  matching: number
}

export interface CatalogEntry {
  id: number
  folder: string
  received: number
  date: number
  from: Address | null
  subject: string
  size: number
  flags: string
}

export interface Catalog {
  folders: CatalogFolder[]
  matching: number
  matching_bytes: number
  entries: CatalogEntry[]
  oldest: number | null
  newest: number | null
}

export interface BackupMessage {
  subject: string
  from: Address | null
  to: Address[]
  cc: Address[]
  date: number
  html: string
  is_plain: boolean
  has_remote_content: boolean
  attachments: Attachment[]
}

export interface SimilarCriteria {
  account_id: number | null
  from_address: string | null
  from_domain: string | null
  to_address: string | null
  subject: string | null
  /** Words the message text must contain (downloaded bodies only). */
  body: string | null
  link_domains: string[]
  list_id: string | null
  bulk_only: boolean
  /** The other messages of the conversations found come too. */
  whole_threads?: boolean
  /** Inclusive bounds. */
  date_from: number | null
  date_to: number | null
}

export interface SimilarResult {
  count: number
  bytes: number
  ids: number[]
  messages: MessageSummary[]
  /** For each listed message (same order), the ids of all its copies. */
  copies: number[][]
}

export interface SimilarFacets {
  account_id: number
  from: Address | null
  from_domain: string | null
  to: Address[]
  subject: string
  subject_core: string
  link_domains: { domain: string, links: number }[]
  list_id: string | null
  bulk: boolean
}

export interface Coverage {
  account_id: number
  local: number
  /** Unknown until every folder has been synced. */
  server: number | null
  bodies: number
}

export type CalendarSource = 'google' | 'microsoft' | 'caldav'

export interface CalendarInfo {
  id: number
  account_id: number
  name: string
  /** "#rrggbb"; none: the account's colour is used. */
  color: string | null
  source: CalendarSource
  enabled: boolean
  read_only: boolean
  /** Why its last sync failed (the account's other calendars may be fine). */
  error: string | null
}

export type CalendarEventStatus = 'confirmed' | 'tentative' | 'cancelled'

/** One instance of an event (recurrences are expanded by the engine). */
export interface CalendarEvent {
  id: number
  calendar_id: number
  account_id: number
  uid: string
  /** Empty for a single event; with `calendar_id` and `uid`, the occurrence's key across syncs. */
  instance: string
  summary: string
  location: string | null
  description: string | null
  /** Unix seconds. All-day events: local midnights, `end` exclusive. */
  start: number
  end: number
  all_day: boolean
  status: CalendarEventStatus | null
  organizer: string | null
  /** The event in its web calendar (Google Calendar). */
  url: string | null
  /** Takes the time: not shown as free, not declined, not cancelled. */
  busy: boolean
  /** An occurrence of a series: a change applies to it alone. */
  recurring: boolean
  /** facteur may change or delete it. */
  editable: boolean
  /** The account's answer to someone else's invitation; null when it is not invited. */
  response: EventAnswer | 'needs_action' | null
  /** That answer can be sent from the calendar. */
  respondable: boolean
  /** Who is invited, the organizer first; empty without guests. */
  attendees: EventAttendee[]
  /** The video meeting to join. */
  meeting: string | null
  /** Minutes before the start when the provider reminds of it. */
  reminders: number[]
}

export interface EventAttendee {
  email: string
  name?: string | null
  response?: EventAnswer | 'needs_action' | null
  organizer?: boolean
}

export type EventAnswer = 'accepted' | 'tentative' | 'declined'

/** How an account's address book stands; none before its first sync. */
export interface AddressBookStatus {
  account_id: number
  state: 'ok' | 'needs_reconnect' | 'not_configured' | 'unavailable' | 'error'
  message: string | null
  last_sync: number | null
  contacts: number
}

/** An event to create, or what an event becomes. Times as in `CalendarEvent`. */
export interface EventDraft {
  summary: string
  location: string | null
  description: string | null
  start: number
  end: number
  all_day: boolean
  /** A new event's rule, or the whole series' when a series is changed; `null`: a single event
   *  (a series with a custom rule keeps it). */
  repeat?: Repeat | null
  /** The guests to invite (the organizer left out); `undefined`: unchanged. */
  attendees?: EventAttendee[]
}

export type RepeatFreq = 'daily' | 'weekly' | 'monthly' | 'yearly'

/** How an event repeats. `weekdays`: 0 for Monday to 6 for Sunday; `until`: `YYYY-MM-DD`. */
export interface Repeat {
  freq: RepeatFreq
  interval: number
  weekdays: number[]
  by_weekday: boolean
  until: string | null
  count: number | null
}

/** A series' rule now; `custom`: one the form cannot show, changed in its own app. */
export interface SeriesRepeat {
  repeat: Repeat | null
  custom: boolean
}

/** How an event repeats, as the event form edits it. */
export interface RepeatForm {
  kind: 'none' | 'custom' | RepeatFreq
  interval: number
  weekdays: number[]
  byWeekday: boolean
  end: 'never' | 'until' | 'count'
  until: string
  count: number
}

/** `partial`: some calendars synced, others failed (see their `error`). */
export type CalendarState = 'ok' | 'partial' | 'needs_reconnect' | 'not_configured' | 'unavailable' | 'error'

export interface CalendarStatus {
  account_id: number
  state: CalendarState
  message: string | null
  /** CalDAV accounts: the address discovered or entered. */
  caldav_url: string | null
  last_sync: number | null
  /** The span the stored events cover (Unix seconds, `to` exclusive); null before a first
   *  sync. Nothing is known outside it. */
  synced_from: number | null
  synced_to: number | null
  /** The user does not use this account's calendar: never synced, left out of the calendar. */
  ignored: boolean
}

/** A local AI model offered, tested with facteur. */
export interface AiModel {
  id: string
  name: string
  tier: 'light' | 'balanced' | 'precise' | 'dictation'
  file: string
  bytes: number
  /** The computer's memory it wants, in GB. */
  memory: number
}

/** The local AI: off (`model` null), or the model in use (an offered one's id, or `file`). */
export interface AiStatus {
  available: boolean
  model: string | null
  file: string | null
  ready: boolean
  download: { model: string, done: number, total: number } | null
  error: string | null
  memory: number | null
  fitting: string
  models: AiModel[]
  /** Dictation: on (its model downloaded) and ready. */
  dictation: boolean
  dictation_ready: boolean
  dictation_model: AiModel
}

/** An appointment the local AI found in a message. Times null when its day was not understood. */
export interface Appointment {
  /** The words as written, marked in the message. */
  phrase: string
  event: FoundEvent
}

export interface FoundEvent {
  title: string
  when: string | null
  place: string | null
  start: number | null
  end: number | null
  all_day: boolean
}
