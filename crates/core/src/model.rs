use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    Tls,
    StartTls,
    /// No encryption: only for a local test server.
    None,
}

impl Security {
    pub fn as_str(self) -> &'static str {
        match self {
            Security::Tls => "tls",
            Security::StartTls => "starttls",
            Security::None => "none",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "starttls" => Security::StartTls,
            "none" => Security::None,
            _ => Security::Tls,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    Google,
    Microsoft,
}

impl OAuthProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            OAuthProvider::Google => "google",
            OAuthProvider::Microsoft => "microsoft",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "google" => Some(OAuthProvider::Google),
            "microsoft" => Some(OAuthProvider::Microsoft),
            _ => None,
        }
    }
}

/// The secret (password or refresh token) lives in the system keyring, never here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AuthMethod {
    #[default]
    Password,
    OAuth2 {
        provider: OAuthProvider,
    },
}

/// `ticket` names the tokens the engine keeps in memory after a completed OAuth2 flow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum NewAuth {
    #[default]
    Password,
    OAuth2 {
        provider: OAuthProvider,
        ticket: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct NewAccount {
    pub name: String,
    pub email: String,
    pub username: String,
    #[serde(default)]
    pub password: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    #[serde(default)]
    pub auth: NewAuth,
}

// Leaves out the password so a `{:?}` in a log never writes it.
impl std::fmt::Debug for NewAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewAccount")
            .field("email", &self.email)
            .field("username", &self.username)
            .field("imap", &self.imap)
            .field("smtp", &self.smtp)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub username: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub auth: AuthMethod,
    pub last_sync: Option<i64>,
    /// Plain text, appended under "-- " when composing.
    pub signature: String,
    /// Data URL, fetched from the OAuth provider.
    pub avatar: Option<String>,
    /// Shown instead of the address ("Personal", "Club"...).
    pub label: Option<String>,
    pub identities: Vec<Identity>,
    /// Palette name; `None`: assigned by the account's rank.
    #[serde(default)]
    pub color: Option<String>,
    /// The avatar was chosen by the user (an uploaded image, or none: initials): facteur no
    /// longer replaces it with the provider's photo or the domain's logo.
    #[serde(default)]
    pub avatar_custom: bool,
    /// Screener on since this date: the first messages of unknown senders received after it
    /// wait in the "New senders" folder. `None`: off.
    #[serde(default)]
    pub screener_since: Option<i64>,
    /// From IMAP QUOTA, when the server supports it.
    #[serde(default)]
    pub quota: Option<Quota>,
}

/// Bytes, as last read from the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    pub used: i64,
    pub limit: i64,
    pub at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Identity {
    pub email: String,
    /// Empty: the account's name.
    #[serde(default)]
    pub name: String,
    /// Empty: the account's signature.
    #[serde(default)]
    pub signature: String,
}

/// Every criterion that is set must match.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SimilarCriteria {
    /// `None`: all accounts.
    #[serde(default)]
    pub account_id: Option<i64>,
    #[serde(default)]
    pub from_address: Option<String>,
    /// Subdomains included.
    #[serde(default)]
    pub from_domain: Option<String>,
    /// To or Cc.
    #[serde(default)]
    pub to_address: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// Uses the full-text index, so only downloaded bodies match.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub link_domains: Vec<String>,
    #[serde(default)]
    pub list_id: Option<String>,
    #[serde(default)]
    pub bulk_only: bool,
    /// Inclusive bounds; they narrow the other criteria but are not one on their own.
    #[serde(default)]
    pub date_from: Option<i64>,
    #[serde(default)]
    pub date_to: Option<i64>,
    /// Also brings in the rest of each matched conversation; not a criterion of its own.
    #[serde(default)]
    pub whole_threads: bool,
}

impl SimilarCriteria {
    pub fn is_empty(&self) -> bool {
        self.from_address.is_none()
            && self.from_domain.is_none()
            && self.to_address.is_none()
            && self.subject.as_deref().is_none_or(|s| s.trim().is_empty())
            && self.body.as_deref().is_none_or(|s| s.trim().is_empty())
            && self.link_domains.is_empty()
            && self.list_id.is_none()
            && !self.bulk_only
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SimilarResult {
    /// Distinct messages: one filed in two Gmail folders counts once.
    pub count: i64,
    pub bytes: i64,
    /// Copies included.
    pub ids: Vec<i64>,
    pub messages: Vec<MessageSummary>,
    /// For each listed message (same order as `messages`), the ids of all its copies (Gmail
    /// keeps one message under several labels): acting on a message acts on all of them.
    pub copies: Vec<Vec<i64>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimilarFacets {
    pub account_id: i64,
    pub from: Option<Address>,
    pub from_domain: Option<String>,
    pub to: Vec<Address>,
    pub subject: String,
    /// Without "Re:", "Fwd:"...: what stays the same from one mailing to the next.
    pub subject_core: String,
    pub link_domains: Vec<LinkDomain>,
    pub list_id: Option<String>,
    pub bulk: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkDomain {
    pub domain: String,
    pub links: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountStats {
    /// Distinct messages: one filed in two Gmail folders counts once.
    pub total: i64,
    pub received: i64,
    pub sent: i64,
    pub unread: i64,
    pub flagged: i64,
    pub with_attachments: i64,
    pub bulk: i64,
    pub bytes: i64,
    pub received_30d: i64,
    pub sent_30d: i64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    /// Last 12 months, oldest first ("2026-10").
    pub months: Vec<MonthStat>,
    /// Received per weekday, Monday first.
    pub weekdays: Vec<i64>,
    pub hours: Vec<i64>,
    pub top_senders: Vec<CountedAddress>,
    pub top_recipients: Vec<CountedAddress>,
    pub folders: Vec<FolderStat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthStat {
    pub month: String,
    pub received: i64,
    pub sent: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountedAddress {
    pub name: Option<String>,
    pub email: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderStat {
    pub id: i64,
    pub name: String,
    pub role: Option<Role>,
    pub total: i64,
    pub unread: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    Flagged,
    /// Gmail's Important label (`\Important`, RFC 8457), whatever the interface language names it.
    Important,
    /// Created by facteur, no SPECIAL-USE equivalent.
    Snoozed,
    /// First messages from unknown senders, waiting for the user's decision (folder created by
    /// facteur when the screener is on).
    Screener,
    Refused,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Inbox => "inbox",
            Role::Sent => "sent",
            Role::Drafts => "drafts",
            Role::Trash => "trash",
            Role::Junk => "junk",
            Role::Archive => "archive",
            Role::All => "all",
            Role::Flagged => "flagged",
            Role::Important => "important",
            Role::Snoozed => "snoozed",
            Role::Screener => "screener",
            Role::Refused => "refused",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "inbox" => Role::Inbox,
            "sent" => Role::Sent,
            "drafts" => Role::Drafts,
            "trash" => Role::Trash,
            "junk" => Role::Junk,
            "archive" => Role::Archive,
            "all" => Role::All,
            "flagged" => Role::Flagged,
            "important" => Role::Important,
            "snoozed" => Role::Snoozed,
            "screener" => Role::Screener,
            "refused" => Role::Refused,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Mailbox {
    pub id: i64,
    pub account_id: i64,
    /// Raw IMAP name (modified UTF-7), used in commands.
    pub name: String,
    pub display_name: String,
    pub delimiter: Option<String>,
    pub role: Option<Role>,
    pub selectable: bool,
    pub unread: i64,
    /// Messages in facteur's cache.
    pub total: i64,
    /// Messages on the server at the last sync; `None` until the folder has been synced.
    pub server_total: Option<i64>,
    /// Conversations in the cache, as the lists show them.
    #[serde(default)]
    pub threads: i64,
    #[serde(default)]
    pub unread_threads: i64,
    /// Gmail: a label of the user's own, offered to add to or take from a message (not the inbox,
    /// Sent, Important… nor facteur's folders).
    #[serde(default)]
    pub user_label: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageSummary {
    pub id: i64,
    pub account_id: i64,
    pub mailbox_id: i64,
    pub thread_id: i64,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub date: i64,
    pub preview: String,
    pub flags: Flags,
    pub has_attachments: bool,
    /// Gmail: the folders (labels) holding a copy of the message, as in `ThreadSummary::labels`.
    /// Filled by the search only.
    #[serde(default)]
    pub labels: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadSummary {
    pub account_id: i64,
    /// Folder of the latest message (differs across folders in the unified inbox).
    pub mailbox_id: i64,
    pub thread_id: i64,
    pub latest_id: i64,
    pub subject: String,
    /// Of the latest message.
    pub from: Option<Address>,
    pub to: Vec<Address>,
    /// Senders in this folder, deduplicated, oldest first.
    pub participants: Vec<Address>,
    /// The thread's messages in this folder: what actions from the list apply to.
    pub message_ids: Vec<i64>,
    pub date: i64,
    pub preview: String,
    pub count: i64,
    pub unread: i64,
    pub flagged: bool,
    pub has_attachments: bool,
    /// Sort key: the latest message's date, or when the thread came back from a snooze.
    pub activity: i64,
    pub snoozed_until: Option<i64>,
    /// Came back from a snooze.
    pub woken: bool,
    /// A person anywhere in the thread wins, then notification, then newsletter.
    pub category: Category,
    /// Gmail's All Mail: the folders (labels) holding a copy of the thread's messages, the inbox
    /// and the user's own, in name order. Empty elsewhere.
    #[serde(default)]
    pub labels: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreRequest {
    #[serde(default)]
    pub filter: crate::backup::RestoreFilter,
    /// Catalogue ids; `None`: everything the filter matches.
    #[serde(default)]
    pub ids: Option<Vec<i64>>,
    pub target: crate::backup::RestoreTarget,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupMessage {
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub date: i64,
    pub html: String,
    pub is_plain: bool,
    pub has_remote_content: bool,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dashboard {
    /// Arrived in the inboxes since the given instant (start of the day).
    pub received_today: i64,
    pub flagged_count: i64,
    /// One copy per message.
    pub flagged: Vec<MessageSummary>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    #[default]
    People,
    /// Automated (alerts, confirmations, no-reply).
    Notification,
    /// Bulk (newsletters, lists, promotions).
    Newsletter,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::People => "people",
            Category::Notification => "notification",
            Category::Newsletter => "newsletter",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "people" => Category::People,
            "notification" => Category::Notification,
            "newsletter" => Category::Newsletter,
            _ => return None,
        })
    }

    pub const ALL: [Category; 3] = [Category::People, Category::Notification, Category::Newsletter];

    /// A person outranks the rest in a thread. Must match `store::CATEGORY_RANK`, its SQL twin.
    pub fn rank(self) -> i64 {
        match self {
            Category::People => 0,
            Category::Notification => 1,
            Category::Newsletter => 2,
        }
    }

    pub fn from_rank(rank: i64) -> Self {
        Category::ALL.into_iter().find(|c| c.rank() == rank).unwrap_or(Category::Newsletter)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadFilter {
    #[default]
    All,
    Unread,
    Flagged,
}

#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub index: usize,
    pub filename: String,
    pub mime: String,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageDetail {
    pub summary: MessageSummary,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    /// Sanitized, `cid:` images inlined. Always set: plain text is converted.
    pub html: String,
    pub is_plain: bool,
    pub text: Option<String>,
    pub has_remote_content: bool,
    pub attachments: Vec<Attachment>,
    pub unsubscribe: Option<crate::mime::Unsubscribe>,
    /// Bulk or automated: not from a person.
    pub bulk: bool,
    /// Below the threshold, facteur disables whatever contacts the sender.
    pub trust: crate::trust::Trust,
    /// Key the sender's images are approved under: the domain, or a personal address.
    pub sender_key: Option<String>,
    /// Approved sender and message not suspicious.
    pub images_allowed: bool,
    /// Signed by its sender's domain (DMARC or facteur's own DKIM check): an approval of that
    /// domain applies to it. Unproven, anyone could have written it under that name.
    pub sender_proven: bool,
    /// Written here (one's Sent or Drafts): the sender's approval does not apply, its images
    /// being those of the mail it quotes.
    pub own_copy: bool,
    /// Approved domains its remote content comes from: loaded whatever the sender.
    pub image_hosts: Vec<String>,
    /// Domains its remote content comes from that are not approved.
    pub blocked_hosts: Vec<String>,
    /// All its remote content comes from approved domains.
    pub images_covered: bool,
    pub invite: Option<crate::calendar::Invite>,
    /// A day and time the message's own words give (not an invitation): offered as an event.
    pub appointment: Option<crate::ai::Appointment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewMail {
    pub id: i64,
    pub thread_id: i64,
    pub mailbox_id: i64,
    pub from: Option<Address>,
    pub subject: String,
    pub preview: String,
    pub bulk: bool,
    pub trust: crate::trust::Level,
    /// From a new sender: held in the "New senders" folder by the screener.
    #[serde(default)]
    pub held: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Unsubscribed {
    OneClick,
    Mailto { address: String },
    /// The sender picks recipient and content freely: the user must see and confirm the message.
    Confirm { address: String, subject: String },
    Web { url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingAttachment {
    pub filename: String,
    pub mime: String,
    /// Base64.
    pub data: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Draft {
    pub account_id: i64,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    pub subject: String,
    /// Plain text: the message itself, or the text version of the HTML.
    pub body: String,
    /// Images are data URLs here; they go out as inline `cid:` parts.
    #[serde(default)]
    pub html: Option<String>,
    /// Feeds In-Reply-To/References and the \Answered flag.
    #[serde(default)]
    pub in_reply_to: Option<i64>,
    #[serde(default)]
    pub attachments: Vec<OutgoingAttachment>,
    /// Read from the cache at send time.
    #[serde(default)]
    pub forwarded: Vec<ForwardedAttachment>,
    /// Message-ID of the draft saved on the server; stable across saves.
    #[serde(default)]
    pub draft_id: Option<String>,
    /// An alias of the account; `None`: the account's address.
    #[serde(default)]
    pub from: Option<String>,
    /// Reply to an invite (iCalendar `METHOD:REPLY`), attached as `text/calendar`.
    #[serde(default)]
    pub calendar: Option<String>,
    /// Captured when queued: the original message may have left the cache by send time.
    #[serde(default)]
    pub reply_ids: Option<(String, Vec<String>)>,
    /// Local id of the Drafts message this draft was reopened from, when that copy has no
    /// Message-ID: it cannot be replaced by the next save, so it is deleted then (or on send).
    #[serde(default)]
    pub source: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardedAttachment {
    /// Local id of the original message; `index` is the attachment's rank in it.
    pub message_id: i64,
    pub index: usize,
    pub filename: String,
    pub mime: String,
    pub size: usize,
}

/// Subfolders included.
#[derive(Debug, Clone, Serialize)]
pub struct FolderSummary {
    pub mailbox: Mailbox,
    pub children: Vec<Mailbox>,
    /// `None` if one of the folders was never synced.
    pub server_count: Option<i64>,
    /// Messages in the cache; the figures below are about them.
    pub local: i64,
    pub unread: i64,
    pub flagged: i64,
    pub with_attachments: i64,
    pub bytes: i64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
    /// Oldest year first.
    pub years: Vec<(i32, i64)>,
    pub top_senders: Vec<CountedAddress>,
    pub recent: Vec<MessageSummary>,
    /// Gmail: a folder is a label, deleting it keeps the messages.
    pub gmail: bool,
}

/// Waiting in the undo delay, scheduled, or failed.
#[derive(Debug, Clone, Serialize)]
pub struct Outgoing {
    pub id: i64,
    pub account_id: i64,
    pub subject: String,
    pub to: Vec<String>,
    pub send_at: i64,
    /// Scheduled by the user; otherwise in the undo delay.
    pub scheduled: bool,
    /// `pending`, `sending` or `failed`.
    pub state: String,
    pub error: Option<String>,
    /// Follow-up reminder, in seconds after sending.
    pub remind_after: Option<i64>,
    /// The message it answers (a reply): the conversation shows it while it is on its way.
    pub in_reply_to: Option<i64>,
    /// The start of its text, as the conversation shows it meanwhile.
    pub preview: String,
}

/// A sent message still unanswered when its reminder is due.
#[derive(Debug, Clone, Serialize)]
pub struct FollowUp {
    pub id: i64,
    pub account_id: i64,
    pub subject: String,
    pub to: Vec<String>,
    pub sent_at: i64,
    pub due: i64,
    /// Set once the sent message is in the cache.
    pub thread_id: Option<i64>,
    pub mailbox_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagKind {
    Seen,
    Flagged,
}

impl FlagKind {
    pub fn imap(self) -> &'static str {
        match self {
            FlagKind::Seen => "\\Seen",
            FlagKind::Flagged => "\\Flagged",
        }
    }

    pub fn column(self) -> &'static str {
        match self {
            FlagKind::Seen => "seen",
            FlagKind::Flagged => "flagged",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Screen {
    /// Accepted: to the inbox.
    Inbox,
    /// Accepted, sorted as a newsletter by the smart inbox.
    Newsletter,
    /// Accepted, sorted as a notification by the smart inbox.
    Notification,
    /// Refused: to the "Screened out" folder, now and later.
    Block,
}

/// A sender whose messages wait in the "New senders" folder.
#[derive(Debug, Clone, Serialize)]
pub struct HeldSender {
    pub account_id: i64,
    pub email: String,
    pub name: Option<String>,
    pub count: i64,
    pub latest: MessageSummary,
    pub trust: crate::trust::Level,
    /// A decision may cover the sender's whole domain: not a public mail provider's.
    pub domainable: bool,
}

/// A decision of the screener: about one address, or a whole domain (`@example.org`).
#[derive(Debug, Clone, Serialize)]
pub struct Screening {
    pub pattern: String,
    /// `accept` or `block`.
    pub decision: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalendarSource {
    Google,
    Caldav,
    /// Outlook (Microsoft Graph).
    Microsoft,
}

impl CalendarSource {
    pub fn as_str(self) -> &'static str {
        match self {
            CalendarSource::Google => "google",
            CalendarSource::Caldav => "caldav",
            CalendarSource::Microsoft => "microsoft",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "google" => CalendarSource::Google,
            "microsoft" => CalendarSource::Microsoft,
            _ => CalendarSource::Caldav,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CalendarInfo {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    /// `#rrggbb`, from the provider.
    pub color: Option<String>,
    pub source: CalendarSource,
    /// Shown in facteur (its events are synced and listed).
    pub enabled: bool,
    pub read_only: bool,
    /// Why its last sync failed (the other calendars of the account may be fine).
    pub error: Option<String>,
}

/// One occurrence of an event (recurring events are expanded).
#[derive(Debug, Clone, Default, Serialize)]
pub struct CalendarEvent {
    pub id: i64,
    pub calendar_id: i64,
    pub account_id: i64,
    pub uid: String,
    /// Tells the occurrences of a series apart (empty for a single event): with `calendar_id`
    /// and `uid`, the occurrence's key across syncs.
    pub instance: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    /// Unix seconds. A whole-day event starts and ends at local midnight, `end` exclusive.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    /// `confirmed`, `tentative` or `cancelled`.
    pub status: Option<String>,
    pub organizer: Option<String>,
    pub url: Option<String>,
    /// Takes the time: not shown as free, not declined by the account, not cancelled.
    pub busy: bool,
    /// An occurrence of a series (a change applies to that occurrence only).
    pub recurring: bool,
    /// facteur may change or delete it: a writable calendar, organized by the account.
    pub editable: bool,
    /// The account's answer to someone else's invitation (`needs_action`, `accepted`,
    /// `tentative`, `declined`); `None` when it is not invited.
    pub response: Option<String>,
    /// facteur may send that answer from the calendar.
    pub respondable: bool,
    /// Who is invited, the organizer included; empty for an event without guests.
    pub attendees: Vec<EventAttendee>,
    /// The video meeting to join (Google Meet, Teams, Zoom…).
    pub meeting: Option<String>,
    /// Minutes before the start when the provider reminds of it.
    pub reminders: Vec<i64>,
    /// The IANA zone a timed event was set in, when the provider names one.
    pub zone: Option<String>,
}

/// A guest of an event. `response`: `needs_action`, `accepted`, `tentative` or `declined`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventAttendee {
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub organizer: bool,
}

/// An event as the user saw it: its row, and the occurrence the row stood for then.
#[derive(Debug, Clone, Deserialize)]
pub struct EventRef {
    pub id: i64,
    pub uid: String,
    pub instance: String,
}

/// An event to create, or what an event becomes.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct EventDraft {
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    /// Unix seconds, `end` exclusive. A whole-day event gives the local midnights of its first day
    /// and of the day after its last, as [`CalendarEvent`] does.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    /// How it repeats: a new event's rule, or the whole series' when a series is changed;
    /// `None`: a single event (for a series, it stops repeating).
    #[serde(default)]
    pub repeat: Option<crate::repeat::Repeat>,
    /// The guests to invite (the organizer left out); `None`: unchanged.
    #[serde(default)]
    pub attendees: Option<Vec<EventAttendee>>,
    /// The IANA zone of a timed event's times (`None`: as before; a new event in UTC, or in the
    /// computer's zone for a series).
    #[serde(default)]
    pub zone: Option<String>,
    /// Minutes before the start to be reminded at (`None`: the calendar's own, or unchanged).
    #[serde(default)]
    pub reminders: Option<Vec<i64>>,
}

/// How a series repeats now, for its form. `custom`: a rule facteur cannot put in the form's terms
/// (`repeat` is then `None`), changed only in its own calendar app.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesRepeat {
    pub repeat: Option<crate::repeat::Repeat>,
    pub custom: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarState {
    Ok,
    /// Some calendars synced, others failed (see their `error`).
    Partial,
    /// Google account whose authorization does not cover the calendars: sign in again.
    NeedsReconnect,
    /// No CalDAV server found, and none given.
    NotConfigured,
    /// Not supported for this account yet (Microsoft).
    Unavailable,
    Error,
}

impl CalendarState {
    pub fn as_str(self) -> &'static str {
        match self {
            CalendarState::Ok => "ok",
            CalendarState::Partial => "partial",
            CalendarState::NeedsReconnect => "needs_reconnect",
            CalendarState::NotConfigured => "not_configured",
            CalendarState::Unavailable => "unavailable",
            CalendarState::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ok" => CalendarState::Ok,
            "partial" => CalendarState::Partial,
            "needs_reconnect" => CalendarState::NeedsReconnect,
            "not_configured" => CalendarState::NotConfigured,
            "unavailable" => CalendarState::Unavailable,
            "error" => CalendarState::Error,
            _ => return None,
        })
    }
}

/// How an account's address book stands; none before its first sync.
#[derive(Debug, Clone, Serialize)]
pub struct AddressBookStatus {
    pub account_id: i64,
    /// `ok`, `needs_reconnect`, `not_configured` (no CardDAV service found) or `error`.
    pub state: CalendarState,
    pub message: Option<String>,
    pub last_sync: Option<i64>,
    pub contacts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalendarStatus {
    pub account_id: i64,
    pub state: CalendarState,
    pub message: Option<String>,
    /// The CalDAV address given by the user, or else the one found by discovery.
    pub caldav_url: Option<String>,
    pub last_sync: Option<i64>,
    /// The time span the stored events cover (Unix seconds, `to` exclusive), for every shown
    /// calendar of the account; `None` before a first sync. Nothing is known outside it.
    pub synced_from: Option<i64>,
    pub synced_to: Option<i64>,
    /// The user does not use this account's calendar (never synced, left out of the calendar).
    pub ignored: bool,
}
