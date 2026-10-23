use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, broadcast};

use crate::error::{Error, Result, bounded};
use crate::imap::{self, Credential, Session};
use crate::{backup, oauth};
use crate::model::*;
use crate::ops::Op;
use crate::secrets::Secrets;
use crate::store::{Store, Verdict};
use crate::sync::{self, Report};
use crate::{mime, smtp};
use futures::FutureExt as _;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

mod diagnostics;
pub use diagnostics::{
    AccountDiagnostics, CalendarDiagnostics, ConnectionDiagnostics, Diagnostics, LastSync, ListenerStatus, ProcessMemory, QueueDiagnostics, RuntimeDiagnostics,
    SyncDiagnostics,
};

pub fn subject_core(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let lower = s.to_lowercase();
        let Some(p) = ["re:", "re :", "fwd:", "fw:", "tr:", "tr :", "réf:", "aw:", "wg:"].iter().find(|p| lower.starts_with(**p)) else {
            break;
        };
        s = s[p.len()..].trim_start();
    }
    s.to_string()
}

/// (address, subject, body) of a `mailto:` link (RFC 6068).
pub fn parse_mailto(link: &str) -> (String, Option<String>, Option<String>) {
    let rest = link.strip_prefix("mailto:").or_else(|| link.strip_prefix("MAILTO:")).unwrap_or(link);
    let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut subject = None;
    let mut body = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = crate::oauth::urldecode(v);
        match k.to_ascii_lowercase().as_str() {
            "subject" => subject = Some(v),
            "body" => body = Some(v),
            _ => {}
        }
    }
    (crate::oauth::urldecode(address), subject, body)
}

/// Whole labels only: imap.xgmail.com is not Google's.
fn oauth_host(provider: OAuthProvider, host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let domains: &[&str] = match provider {
        OAuthProvider::Google => &["gmail.com", "googlemail.com", "google.com"],
        OAuthProvider::Microsoft => &["office365.com", "outlook.com"],
    };
    domains.iter().any(|d| host == *d || host.strip_suffix(d).is_some_and(|rest| rest.ends_with('.')))
}

fn new_message_id(email: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let domain = email.rsplit_once('@').map_or("facteur.local", |(_, d)| d);
    format!("{nanos:x}.{}.{}@{domain}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    SyncStarted { account_id: i64 },
    SyncProgress { account_id: i64, mailbox: String, done: usize, total: usize },
    SyncFinished { account_id: i64, new_messages: usize, new_in_inbox: usize, error: Option<String> },
    Changed { account_id: i64 },
    BackupProgress { account_id: i64, mailbox: String, done: usize, total: usize },
    BackupFinished { account_id: i64, downloaded: usize, messages: usize, error: Option<String> },
    RestoreProgress { account_id: i64, mailbox: String, done: usize, total: usize },
    OpsChanged,
    LogosChanged,
    OutboxChanged,
    /// `retrying`: facteur will try again on its own.
    SendFailed { id: i64, account_id: i64, subject: String, error: String, retrying: bool },
    FollowUpsDue { follow_ups: Vec<FollowUp> },
    /// Blocked senders excluded.
    NewMail { account_id: i64, messages: Vec<NewMail> },
    CalendarChanged,
    AuthChanged { account_id: i64, error: Option<String> },
    /// The account's server keeps dropping connections: its single-connection mode is offered.
    OneConnectionAdvised { account_id: i64 },
    /// An event's reminder is due: it starts in `minutes`.
    EventReminder { event: CalendarEvent, minutes: i64 },
    /// The local AI's state changed (a model downloading, ready, turned off).
    AiChanged,
    /// Weighed again once facteur checked the DKIM signatures itself (see [`Engine::message`]).
    TrustChanged { message_id: i64, trust: crate::trust::Trust },
}

/// The local AI: off (`model` none), or the model in use (an offered one's id, or `file` for
/// one picked from the disk), and the model downloading.
#[derive(Debug, Clone, Serialize)]
pub struct AiStatus {
    /// This build carries the local AI.
    pub available: bool,
    pub model: Option<String>,
    pub file: Option<String>,
    /// The model's file is there.
    pub ready: bool,
    pub download: Option<AiDownload>,
    /// Why the last download failed.
    pub error: Option<String>,
    /// The computer's memory, in bytes, and the offered model fitting it.
    pub memory: Option<u64>,
    pub fitting: String,
    pub models: Vec<crate::ai::Model>,
    /// Dictation: on (its model downloaded) and ready.
    pub dictation: bool,
    pub dictation_ready: bool,
    pub dictation_model: crate::ai::Model,
    /// The graphics card may be used, and the one the model last ran on.
    pub graphics: bool,
    pub card: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AiDownload {
    pub model: String,
    pub done: u64,
    pub total: u64,
    #[serde(skip)]
    cancel: Arc<std::sync::atomic::AtomicBool>,
    #[serde(skip)]
    error: Option<String>,
    #[serde(skip)]
    running: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpsStatus {
    pub pending: i64,
    pub failed: Vec<crate::store::FailedOp>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpState {
    Held,
    /// Waits behind an operation still within its undo window (order matters).
    Blocked,
    Waiting,
    /// Failed before (network, timeout): tried again at the next sync.
    Retrying,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpView {
    #[serde(flatten)]
    pub op: crate::store::OpRow,
    pub state: OpState,
    pub blocked_by: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpsDetails {
    pub ops: Vec<OpView>,
    /// Cleared on success.
    pub errors: HashMap<i64, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupStatus {
    pub account_id: i64,
    pub email: String,
    /// `None` if the account was never backed up there.
    pub manifest: Option<backup::Manifest>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Purpose {
    Sync,
    Interactive,
    /// A long replay (a purge, a large move) must not make the opening of a message wait.
    Queue,
    /// Longer work started from the interface (history, server search, folder refresh): kept off
    /// the interactive connection so it never delays opening a message.
    Background,
}

struct Pooled {
    session: Session,
    caps: Option<sync::Caps>,
    /// A connection idle for long is checked before use.
    used: Instant,
}

impl Pooled {
    async fn caps(&mut self) -> Result<sync::Caps> {
        if let Some(caps) = self.caps {
            return Ok(caps);
        }
        let caps = bounded(30, &crate::tr!("capacités du serveur", "server capabilities"), sync::capabilities(&mut self.session)).await?;
        self.caps = Some(caps);
        Ok(caps)
    }
}

type Slot = Arc<AsyncMutex<Option<Pooled>>>;
type Conn = OwnedMutexGuard<Option<Pooled>>;

/// A session for a long job (backup, restore, history): its own, or the account's one connection,
/// given over between two steps to whoever waits for it (see [`backup::Lease`]).
struct Dedicated<'a> {
    engine: &'a Engine,
    account_id: i64,
    held: Option<Held>,
}

enum Held {
    Own(Session),
    Shared(Conn),
}

impl Dedicated<'_> {
    /// Its own is closed; the shared one goes back to the pool.
    async fn finish(mut self) {
        match self.held.take() {
            Some(Held::Own(mut session)) => logout(&mut session).await,
            Some(Held::Shared(conn)) => drop(conn),
            None => {}
        }
    }
}

impl Drop for Dedicated<'_> {
    /// Not finished (an error, a timeout mid-command): the shared connection is in an uncertain
    /// state, closed rather than handed to the next user.
    fn drop(&mut self) {
        if let Some(Held::Shared(mut conn)) = self.held.take() {
            *conn = None;
        }
    }
}

impl backup::Lease for Dedicated<'_> {
    fn session(&mut self) -> &mut Session {
        match self.held.as_mut().expect("session held") {
            Held::Own(session) => session,
            Held::Shared(conn) => &mut conn.as_mut().expect("session open").session,
        }
    }

    async fn give_way(&mut self) -> Result<()> {
        let shared = matches!(self.held, Some(Held::Shared(_)));
        if !shared || self.engine.waiting_on(self.account_id).load(std::sync::atomic::Ordering::Relaxed) == 0 {
            return Ok(());
        }
        // Given back whole: the one waiting takes it (the lock is fair), this job queues after.
        if let Some(Held::Shared(conn)) = self.held.take() {
            drop(conn);
        }
        tokio::task::yield_now().await;
        self.held = Some(Held::Shared(self.engine.session(self.account_id, Purpose::Background).await?));
        Ok(())
    }
}

impl std::ops::Deref for Dedicated<'_> {
    type Target = Session;

    fn deref(&self) -> &Session {
        match self.held.as_ref().expect("session held") {
            Held::Own(session) => session,
            Held::Shared(conn) => &conn.as_ref().expect("session open").session,
        }
    }
}

impl std::ops::DerefMut for Dedicated<'_> {
    fn deref_mut(&mut self) -> &mut Session {
        backup::Lease::session(self)
    }
}

/// Counted among those waiting for the account's one connection while it lives.
struct Waiting(Arc<std::sync::atomic::AtomicUsize>);

impl Waiting {
    fn new(count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Waiting(count)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

const POOL_CHECK_AFTER: Duration = Duration::from_secs(60);
/// Some hosts cap the connections of an address (OVH cut them and answered with empty folder
/// lists). At rest, only IDLE stays open.
const POOL_CLOSE_AFTER: Duration = Duration::from_secs(120);
const SYNC_RETRY_AFTER: Duration = Duration::from_secs(5);
/// A sign-in refused "for now" for this long is no longer passing: the user is asked to sign in again.
/// The waits before the next tries of a sign-in Outlook turns down "for now".
const REFUSED_RETRIES: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(30)];
/// The longest a keychain write is waited for.
const KEYCHAIN_WAIT: Duration = Duration::from_secs(5);
const REFUSED_FOR_NOW_LIMIT: Duration = Duration::from_secs(30 * 60);
/// Refusals "for now" further apart are no run: a sleep or an evening between them.
const REFUSED_FOR_NOW_GAP: Duration = Duration::from_secs(10 * 60);
/// At least this many refusals in the run before the account waits for the user.
const REFUSED_FOR_NOW_COUNT: u32 = 5;
/// A full sync the server cut on both attempts is retried this long after, so a passing cut
/// (a host dropping a mailbox's sessions now and then) shows for a minute at most.
const SYNC_FOLLOW_UP: Duration = Duration::from_secs(60);
/// Some servers and NAT boxes drop a silent connection well before the 29 minutes RFC 2177 allows.
const IDLE_RENEW: Duration = Duration::from_secs(10 * 60);
const BURST_QUIET: Duration = Duration::from_millis(2500);
const SYNC_TICK: Duration = Duration::from_secs(60);
/// Minutes: inbox, folders with a role, every folder.
const SYNC_EVERY_DEFAULT: [u64; 3] = [1, 5, 15];
const BURST_MAX: Duration = Duration::from_secs(10);
/// A listening connection that lasted this long was sound: the reconnection backoff starts over.
/// Settings: the local AI's model (an offered one's id, or `file`), and the user's own file.
const AI_MODEL: &str = "ai_model";
const AI_FILE: &str = "ai_file";
/// `off`: the local AI on the processor only.
const AI_GRAPHICS: &str = "ai_graphics";
/// Dictation, apart from the rest: its model downloaded (`whisper-turbo`), or none.
const DICTATION_MODEL: &str = "dictation_model";

/// How long dropped connections are looked back on (see [`chronic_cuts`]).
const CUTS_WINDOW: Duration = Duration::from_secs(60 * 60);
const IDLE_HEALTHY: Duration = Duration::from_secs(2 * 60);

/// The `ticket` names the tokens kept in memory until the account is created.
#[derive(Debug, Clone, Serialize)]
pub struct OAuthIdentity {
    pub ticket: String,
    /// The sign-in name. A Microsoft account opened with a Gmail address has a mailbox of its own
    /// (@outlook.com), which the user gives.
    pub email: Option<String>,
}

pub struct Engine {
    store: Store,
    secrets: Secrets,
    events: broadcast::Sender<Event>,
    slots: Mutex<HashMap<(i64, Purpose), Slot>>,
    /// One connection for everything: requests other than the sync waiting for it, per account.
    waiting: Mutex<HashMap<i64, Arc<std::sync::atomic::AtomicUsize>>>,
    flush_locks: Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>,
    /// In memory only. The expiry is wall-clock time (see [`oauth::Tokens::expires_at`]).
    tokens: Mutex<HashMap<i64, (String, SystemTime)>>,
    /// Since when the IMAP server has been turning the account's sign-in down "for now".
    /// Refusals "for now" in a row: (the first, the last, how many).
    refused_since: Mutex<HashMap<i64, (SystemTime, SystemTime, u32)>>,
    /// Microsoft: the calendar's tokens (Graph), apart from the mail's.
    /// Microsoft Graph token, its expiry, and whether it may write to the calendars.
    calendar_tokens: Mutex<HashMap<i64, (String, SystemTime, bool)>>,
    /// Microsoft accounts whose consent covers reading the calendars only (until reconnected).
    calendar_read_only: Mutex<HashSet<i64>>,
    /// Nothing connects with refused credentials until the user acts, so the server is not
    /// hammered with logins it refuses (it may end up banning the address).
    auth_refused: Mutex<HashMap<i64, String>>,
    /// Refresh tokens the keychain could not store: kept in `secret_cache`, written again later.
    unsaved_secrets: Mutex<HashSet<i64>>,
    /// Wakes the periodic sync and the listening connections' pauses (see [`Engine::resume`]).
    resume: tokio::sync::Notify,
    /// The attachment decoded last (message, index), for a minute (see `attachment`).
    #[allow(clippy::type_complexity)]
    last_attachment: Mutex<Option<((i64, usize), Instant, Arc<(String, String, Vec<u8>)>)>>,
    /// Sends running per account (see `deliver_outgoing`).
    sending: Mutex<HashMap<i64, Arc<tokio::sync::Semaphore>>>,
    last_resume: Mutex<Option<Instant>>,
    /// Connections the servers dropped lately (see [`chronic_cuts`]).
    cuts: Mutex<VecDeque<Cut>>,
    /// Accounts offered the single-connection mode this session; those still unanswered.
    advised: Mutex<HashSet<i64>>,
    advice: Mutex<HashSet<i64>>,
    /// Local AI: where its model is downloaded, the download under way, and one answer at a time.
    ai_dir: PathBuf,
    ai_download: Mutex<Option<AiDownload>>,
    ai_busy: tokio::sync::Mutex<()>,
    /// Forgotten after a quarter of an hour.
    pending_oauth: Mutex<HashMap<String, (oauth::Tokens, Instant)>>,
    idling: Mutex<HashSet<i64>>,
    logo_tried: Mutex<HashSet<i64>>,
    queue_errors: Mutex<HashMap<i64, String>>,
    /// One backup or restore at a time per account.
    archiving: Mutex<HashSet<i64>>,
    fetching_history: Mutex<HashSet<i64>>,
    logos_in_flight: Mutex<HashSet<String>>,
    /// RDAP creation dates; `None`: unknown.
    domain_ages: Mutex<HashMap<String, Option<i64>>>,
    /// DMARC and SPF published by a sender's domain (see [`Engine::sender_records`]).
    sender_records: Mutex<HashMap<String, (Option<bool>, Option<bool>)>>,
    dkim: crate::dkim::Verifier,
    /// Messages whose DKIM check is under way or brought no verdict, and since when.
    dkim_tried: Mutex<HashMap<i64, Instant>>,
    undos: Mutex<HashMap<u64, Undo>>,
    next_undo: std::sync::atomic::AtomicU64,
    /// Recomputed every 10 minutes.
    authservs: Mutex<HashMap<i64, (Option<String>, Instant)>>,
    /// Wakes the scheduler (sends, snooze returns, follow-ups) before its planned time.
    scheduler: tokio::sync::Notify,
    /// Replays requested but not started yet (the queue not read), with the folders to refresh
    /// after: further actions join them instead of starting more.
    flush_requests: Mutex<HashMap<i64, HashSet<i64>>>,
    /// The server's echo of a burst of local actions is synced once the burst is over.
    local_changes: Mutex<HashMap<i64, Instant>>,
    periodic: Mutex<HashSet<i64>>,
    idle_stops: Mutex<HashMap<i64, tokio::sync::watch::Sender<bool>>>,
    /// Never on disk: the keychain is slow, and may prompt, on some systems.
    secret_cache: Mutex<HashMap<i64, String>>,
    oauth_locks: Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>,
    /// Whether a sync was asked for meanwhile: it runs again when done.
    calendar_syncing: Mutex<HashMap<i64, bool>>,
    /// Same: a full sync asked for meanwhile runs once more after, instead of queuing on the connection.
    full_syncs: Mutex<HashMap<i64, bool>>,
    drop_follow_ups: Mutex<HashSet<i64>>,
    /// Accounts with no calendar access are rechecked by all-account syncs only now and then.
    calendar_checked: Mutex<HashMap<i64, Instant>>,
    /// Learned from servers that cut a silent connection sooner than `IDLE_RENEW`.
    idle_renew: Mutex<HashMap<i64, IdleRenew>>,
    runtime: std::sync::OnceLock<tokio::runtime::Handle>,
    /// By (account, Message-ID).
    draft_prints: Mutex<HashMap<(i64, String), u64>>,
    draft_flushes: Mutex<HashSet<i64>>,
    diag: diagnostics::Tracker,
}

/// A saved draft above this size is uploaded [`DRAFT_APPEND_DELAY`] later, so that saves in a
/// row upload only the last version.
const LARGE_DRAFT: usize = 1 << 20;
const DRAFT_APPEND_DELAY: Duration = Duration::from_secs(30);

/// The Message-ID is left out: the first save gives it.
fn draft_print(draft: &mut Draft) -> Result<u64> {
    use std::hash::{Hash, Hasher};
    let id = draft.draft_id.take();
    let bytes = serde_json::to_vec(&*draft);
    draft.draft_id = id;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes?.hash(&mut h);
    Ok(h.finish())
}

const CALENDAR_PAST: i64 = 60 * 86400;
const CALENDAR_AHEAD: i64 = 365 * 86400;
/// The mail syncs first after the start.
const CALENDAR_FIRST: Duration = Duration::from_secs(20);
const CALENDAR_EVERY: Duration = Duration::from_secs(15 * 60);
const CALENDAR_AFTER_RESUME: Duration = Duration::from_secs(10);
const CALENDAR_RECHECK: Duration = Duration::from_secs(6 * 3600);
/// Seconds.
const CALENDAR_TIMEOUT: u64 = 180;
/// An unchanged calendar (same CalDAV ctag, no Google change) is refetched after this long
/// anyway, as the window moves.
const CALENDAR_REFETCH: i64 = 6 * 3600;
/// Seconds: an address book is read again this often, and one that failed after this long.
const CONTACTS_EVERY: i64 = 6 * 3600;
const CONTACTS_RETRY: i64 = 15 * 60;
/// Seconds: Google's change times and the computer's clock may differ a little.
const CHANGES_MARGIN: i64 = 300;

fn calendar_window() -> (i64, i64) {
    let now = crate::store::now();
    (now - CALENDAR_PAST, now + CALENDAR_AHEAD)
}

/// `Err`: why there are none (yet).
fn calendar_support(account: &Account) -> std::result::Result<CalendarSource, String> {
    match account.auth {
        AuthMethod::OAuth2 { provider: OAuthProvider::Google } => Ok(CalendarSource::Google),
        AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft } => Ok(CalendarSource::Microsoft),
        AuthMethod::Password if crate::autoconfig::registrable(&account.imap.host) == "gmail.com" => Err(crate::tr!(
            "agendas Google : connectez le compte avec Google plutôt qu'un mot de passe",
            "Google calendars: sign the account in with Google instead of a password"
        )),
        AuthMethod::Password => Ok(CalendarSource::Caldav),
    }
}

/// A sync asked for meanwhile is not lost: the one under way runs again ([`CalendarRun::again`]).
/// Full mail syncs are merged the same way.
struct CalendarRun<'a> {
    map: &'a Mutex<HashMap<i64, bool>>,
    id: i64,
    done: bool,
}

impl<'a> CalendarRun<'a> {
    /// `None`: a sync of that account is under way; it is told to run again.
    fn start(map: &'a Mutex<HashMap<i64, bool>>, id: i64) -> Option<Self> {
        let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
        match m.get_mut(&id) {
            Some(again) => {
                *again = true;
                None
            }
            None => {
                m.insert(id, false);
                Some(CalendarRun { map, id, done: false })
            }
        }
    }

    /// The slot is given back in the same step as the check, so that no request falls between.
    fn again(&mut self) -> bool {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(&self.id) == Some(&true) {
            m.insert(self.id, false);
            return true;
        }
        m.remove(&self.id);
        self.done = true;
        false
    }
}

impl Drop for CalendarRun<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
        }
    }
}

#[derive(Default)]
struct CalendarFetches {
    synced: usize,
    failed: Vec<(String, Error)>,
}

impl CalendarFetches {
    /// Partial when some calendars failed but not all, else the first error.
    fn result(self) -> Result<(CalendarState, Option<String>)> {
        if self.failed.is_empty() {
            return Ok((CalendarState::Ok, None));
        }
        if self.synced == 0 {
            return Err(self.failed.into_iter().next().map(|(_, e)| e).unwrap_or(Error::Other(String::new())));
        }
        let names = self.failed.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ");
        Ok((CalendarState::Partial, Some(crate::tr!("agendas non synchronisés : {names}", "calendars not synced: {names}"))))
    }
}

/// Used only while still on the site trusted then and the password still goes there; never
/// trusted merely for being stored.
fn remembered_home(settings: &crate::store::CalendarAccount, client: &crate::caldav::Client) -> Option<reqwest::Url> {
    let home = reqwest::Url::parse(settings.home_url.as_deref()?).ok()?;
    let same_site = crate::caldav::site_of(&home).is_some_and(|s| settings.home_site.as_deref() == Some(s.as_str()));
    (same_site && client.trusts(&home)).then_some(home)
}

/// A message moved a moment ago has no UID until the server confirms the move.
fn in_flight() -> Error {
    Error::Other(crate::tr!(
        "un message attend encore la confirmation du serveur : réessayez dans un instant",
        "a message is still waiting for the server to confirm a move: try again in a moment"
    ))
}

/// Words of an invitation reply's subject and text, read by the organizer.
fn answer_words(partstat: &str) -> Result<(String, &'static str, &'static str)> {
    Ok(match partstat {
        "ACCEPTED" => (crate::tr!("Accepté", "Accepted"), "accepte", "has accepted"),
        "TENTATIVE" => (crate::tr!("Provisoire", "Tentative"), "participera peut-être à", "may attend"),
        "DECLINED" => (crate::tr!("Refusé", "Declined"), "décline", "has declined"),
        _ => return Err(Error::Other(crate::tr!("réponse inconnue : {partstat}", "unknown response: {partstat}"))),
    })
}

/// Connections a server search shares the folders between, from that many folders on.
const SEARCH_LANES: usize = 3;
const SEARCH_LANE_MIN: usize = 6;

/// Seconds.
const EVENT_TIMEOUT: u64 = 60;

fn not_editable() -> Error {
    Error::Other(crate::tr!("facteur ne peut pas modifier cet événement", "facteur cannot change this event"))
}

/// Trimmed and bounded; the end is never before the start.
fn checked_draft(mut d: EventDraft) -> Result<EventDraft> {
    let cap = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    d.summary = cap(&d.summary, 1000);
    d.location = d.location.map(|l| cap(&l, 1000)).filter(|l| !l.is_empty());
    d.description = d.description.map(|t| cap(&t, 20_000)).filter(|t| !t.is_empty());
    if d.end < d.start {
        return Err(Error::Other(crate::tr!("l'événement finit avant de commencer", "the event ends before it starts")));
    }
    if let Some(r) = &mut d.repeat {
        r.interval = r.interval.clamp(1, 999);
        r.weekdays.retain(|day| *day < 7);
        r.count = r.count.map(|n| n.clamp(1, 999));
        if r.until.as_deref().is_some_and(|u| chrono::NaiveDate::parse_from_str(u, "%Y-%m-%d").is_err()) {
            return Err(Error::Other(crate::tr!("date de fin de répétition illisible", "unreadable end date for the repetition")));
        }
    }
    Ok(d)
}

/// The first date of a series rebuilt from the occurrence `e` became `draft` (see
/// [`crate::calendar::series_first`]): start, end, whole day, first day.
fn rebuilt_first(first: chrono::NaiveDate, e: &CalendarEvent, draft: &EventDraft) -> Result<(i64, i64, bool, chrono::NaiveDate)> {
    crate::calendar::series_first(first, e.start, draft)
        .map(|(start, end, day)| (start, end, draft.all_day, day))
        .ok_or_else(|| Error::Other(crate::tr!("date de la série hors calendrier", "the series' date is out of range")))
}

/// The account as an event's organizer.
fn organizer_of(account: &Account) -> EventAttendee {
    EventAttendee { email: account.email.clone(), name: Some(account.name.clone()).filter(|n| !n.trim().is_empty()), response: None, organizer: true }
}

/// A draft's start, end, whole day and first day, for the series it starts.
fn draft_first(d: &EventDraft) -> Result<(i64, i64, bool, chrono::NaiveDate)> {
    let day = crate::calendar::first_day(d.start, d.end, d.all_day, d.zone.as_deref());
    day.map(|day| (d.start, d.end, d.all_day, day)).ok_or_else(|| Error::Other(crate::tr!("date de l'événement hors calendrier", "the event's date is out of range")))
}

/// A series' rule read back: as the form puts it, or custom (none at all included: dates listed
/// one by one).
fn series_repeat(rule: Option<crate::repeat::Repeat>) -> SeriesRepeat {
    SeriesRepeat { custom: rule.is_none(), repeat: rule }
}

/// What a change to a whole series does: `None` moves its times (and changes its texts) and keeps
/// its rule; `Some(repeat)` rebuilds it from a new first date, repeating as `repeat`. A rule
/// facteur cannot write back keeps the series' days.
fn series_plan(e: &CalendarEvent, draft: &EventDraft, changes: &crate::calendar::EventChanges, now: &SeriesRepeat, first: chrono::NaiveDate) -> Result<Option<Option<crate::repeat::Repeat>>> {
    // Another zone: the series is written again in it.
    // Days compared in the event's zone: a time change across the computer's midnight is no move.
    let moved = changes.rezoned(e)
        || changes.times.is_some_and(|(start, end, all_day)| {
            all_day != e.all_day
                || crate::calendar::first_day(start, end, all_day, changes.zone.as_deref()) != crate::calendar::first_day(e.start, e.end, e.all_day, e.zone.as_deref())
        });
    // Compared as written: weekly on no day named is weekly on the first day's.
    let written = |r: &Option<crate::repeat::Repeat>| r.as_ref().map(|r| r.rrule(first, None));
    let ruled = if now.custom { draft.repeat.is_some() } else { written(&draft.repeat) != written(&now.repeat) };
    if !moved && !ruled {
        return Ok(None);
    }
    if now.custom && !ruled {
        return Err(Error::Other(crate::tr!(
            "cette série suit une règle personnalisée : changez ses jours dans l'agenda d'origine, ou choisissez une autre répétition",
            "this series follows a custom rule: change its days in its own calendar app, or pick another repetition"
        )));
    }
    Ok(Some(draft.repeat.clone()))
}

/// The event as facteur wrote it to a CalDAV server, until the sync reads it back.
fn written_instance(ics: &str, href: &str, draft: &EventDraft) -> Result<crate::calendar::Instance> {
    let (from, to) = (draft.start - 2 * 86400, draft.end.max(draft.start) + 2 * 86400);
    let mut event = crate::calendar::instances(ics, from, to, &[])
        .into_iter()
        .next()
        .ok_or_else(|| Error::Other("iCalendar".into()))?;
    event.href = Some(href.to_string());
    event.own = true;
    Ok(event)
}

/// From the latest fetch back, up to the oldest fetch ahead (see `calendar_window`).
fn synced_span(fetches: Option<(i64, i64)>) -> (Option<i64>, Option<i64>) {
    fetches.map_or((None, None), |(oldest, latest)| (Some(latest - CALENDAR_PAST), Some(oldest + CALENDAR_AHEAD)))
}

/// Seconds (setting `undo_send`).
const UNDO_SEND_DEFAULT: i64 = 10;
/// Attempts of a failing send (network down at the planned time) before waiting for the user.
const SEND_ATTEMPTS: i64 = 4;
/// Tries of a message while the server cannot be reached: about a day at 15 minutes apart.
const UNREACHABLE_ATTEMPTS: i64 = 100;
const SNOOZE_FOLDER: &str = "Plus tard";
/// In the interface's language.
fn screener_folder() -> String {
    crate::utf7::encode(&crate::tr!("Nouveaux expéditeurs", "New senders"))
}
fn refused_folder() -> String {
    crate::utf7::encode(&crate::tr!("Refusés", "Screened out"))
}
#[derive(Default)]
struct Ruled {
    /// The screener has nothing left to do with them.
    moved: HashSet<i64>,
    read: HashSet<i64>,
}

impl Ruled {
    /// Handled by a rule: no notification.
    fn contains(&self, id: &i64) -> bool {
        self.moved.contains(id) || self.read.contains(id)
    }
}

/// Setting: "1" while messages of a screener turned off still wait to go back to the inbox.
fn release_key(account_id: i64) -> String {
    format!("screener_release:{account_id}")
}
/// Released whatever happens, including when the task is dropped midway.
struct Busy<'a> {
    set: &'a Mutex<HashSet<i64>>,
    id: i64,
}

impl<'a> Busy<'a> {
    fn take(set: &'a Mutex<HashSet<i64>>, id: i64) -> Option<Self> {
        set.lock().unwrap().insert(id).then_some(Busy { set, id })
    }
}

/// An account's periodic sync under way, taken out of `periodic` when it ends, however it ends.
struct PeriodicRun(Arc<Engine>, i64);

impl Drop for PeriodicRun {
    fn drop(&mut self) {
        self.0.periodic.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// The interface's spinner goes off even on a panic or a dropped task. Disarmed on the normal
/// path, which reports the outcome itself.
struct SyncFinishedGuard<'a> {
    engine: &'a Engine,
    account_id: i64,
    armed: bool,
}

impl Drop for SyncFinishedGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let error = if std::thread::panicking() {
            crate::tr!("synchronisation interrompue par une erreur interne", "sync stopped by an internal error")
        } else {
            crate::tr!("synchronisation interrompue", "sync interrupted")
        };
        self.engine.emit(Event::SyncFinished { account_id: self.account_id, new_messages: 0, new_in_inbox: 0, error: Some(error) });
    }
}

const FLUSH_LOCK_WAIT: Duration = Duration::from_secs(120);
/// A safety net so that a stuck holder never hangs every caller. A full sync (up to 6 hours) can
/// hold its connection longer: a second full sync is not queued behind it (see [`Engine::sync`]).
const SLOT_WAIT: Duration = Duration::from_secs(45 * 60);
const SETTLE_WAIT: Duration = Duration::from_secs(15);

/// Undo window: the operations wait in the queue this long before going to the server.
const UNDO_SECS: i64 = 8;
/// A message whose DKIM check brought no verdict is checked again after this long at the soonest.
const DKIM_RETRY: Duration = Duration::from_secs(3600);
/// A message counts as just arrived, for the DKIM key lookups, when the server received it
/// within this window (seconds) or since the account's previous full sync.
const DKIM_ARRIVAL: i64 = 2 * 3600;

#[derive(Default)]
struct Undo {
    /// (account, queued op).
    ops: Vec<(i64, i64)>,
    /// What each of `ops` holds, checked before it is removed (ids are reused).
    op_texts: Vec<String>,
    /// (message, folder and UID before the move).
    moves: Vec<(i64, i64, Option<u32>)>,
    /// Gmail converted to labels: the labels messages had before.
    labels: Vec<(i64, Vec<i64>)>,
    /// (account, thread).
    snoozes: Vec<(i64, i64)>,
    /// (account, pattern, decision before).
    screening: Vec<(i64, String, Option<String>)>,
    categories: Vec<crate::store::SenderCategorySnapshot>,
}

fn group_by_home(rehome: &[(i64, i64)]) -> Vec<(i64, Vec<i64>)> {
    let mut groups: Vec<(i64, Vec<i64>)> = Vec::new();
    for (id, home) in rehome {
        match groups.iter_mut().find(|(h, _)| h == home) {
            Some((_, ids)) => ids.push(*id),
            None => groups.push((*home, vec![*id])),
        }
    }
    groups
}

impl Engine {
    pub fn open(data_dir: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(data_dir)?;
        let store = Store::open(&data_dir.join("facteur.db"))?;
        // The engine's texts use the language picked in the interface at the last launch.
        if let Ok(Some(language)) = store.setting("language") {
            crate::i18n::set(&language);
        }
        crate::ai::set_graphics(store.setting(AI_GRAPHICS).ok().flatten().as_deref() != Some("off"));
        Ok(Arc::new(Engine {
            store,
            secrets: Secrets::new(),
            events: broadcast::channel(256).0,
            slots: Mutex::default(),
            waiting: Mutex::default(),
            flush_locks: Mutex::default(),
            tokens: Mutex::default(),
            refused_since: Mutex::default(),
            calendar_tokens: Mutex::default(),
            calendar_read_only: Mutex::default(),
            auth_refused: Mutex::default(),
            unsaved_secrets: Mutex::default(),
            resume: tokio::sync::Notify::new(),
            sending: Mutex::default(),
            last_attachment: Mutex::default(),
            last_resume: Mutex::default(),
            cuts: Mutex::default(),
            advised: Mutex::default(),
            advice: Mutex::default(),
            ai_dir: data_dir.join("models"),
            ai_download: Mutex::default(),
            ai_busy: tokio::sync::Mutex::new(()),
            pending_oauth: Mutex::default(),
            idling: Mutex::default(),
            logo_tried: Mutex::default(),
            queue_errors: Mutex::default(),
            archiving: Mutex::default(),
            fetching_history: Mutex::default(),
            logos_in_flight: Mutex::default(),
            domain_ages: Mutex::default(),
            sender_records: Mutex::default(),
            dkim: crate::dkim::Verifier::default(),
            dkim_tried: Mutex::default(),
            undos: Mutex::default(),
            authservs: Mutex::default(),
            next_undo: std::sync::atomic::AtomicU64::new(1),
            scheduler: tokio::sync::Notify::new(),
            flush_requests: Mutex::default(),
            local_changes: Mutex::default(),
            periodic: Mutex::default(),
            idle_stops: Mutex::default(),
            secret_cache: Mutex::default(),
            oauth_locks: Mutex::default(),
            calendar_syncing: Mutex::default(),
            full_syncs: Mutex::default(),
            drop_follow_ups: Mutex::default(),
            calendar_checked: Mutex::default(),
            idle_renew: Mutex::default(),
            runtime: std::sync::OnceLock::new(),
            draft_prints: Mutex::default(),
            draft_flushes: Mutex::default(),
            diag: diagnostics::Tracker::new(),
        }))
    }

    /// Background tasks always start on this runtime, whatever thread asks: an interface command
    /// running on another runtime would otherwise not get the replay it triggers. Without it, tasks
    /// go to the caller's runtime.
    pub fn set_runtime(&self, handle: tokio::runtime::Handle) {
        let _ = self.runtime.set(handle);
    }

    fn spawn<F>(&self, task: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        match self.runtime.get() {
            Some(rt) => rt.spawn(task),
            None => tokio::spawn(task),
        }
    }

    /// Password from the keychain, or an OAuth2 access token refreshed as needed. Refused at once,
    /// without asking the server, while the account's credentials stand refused.
    async fn credential(&self, account: &Account) -> Result<Credential> {
        if let Some(why) = self.auth_refused.lock().unwrap().get(&account.id).cloned() {
            return Err(Error::Auth(why));
        }
        match account.auth {
            AuthMethod::Password => Ok(Credential::Password(self.secret(account.id).await?)),
            AuthMethod::OAuth2 { provider } => {
                let valid = || {
                    self.tokens
                        .lock()
                        .unwrap()
                        .get(&account.id)
                        .filter(|(_, expires_at)| *expires_at > SystemTime::now() + Duration::from_secs(60))
                        .map(|(token, _)| token.clone())
                };
                if let Some(token) = valid() {
                    return Ok(Credential::Token(token));
                }
                // Single flight: connections opened together wait for one refresh instead of
                // each spending (and maybe rotating) the refresh token.
                let lock = self.oauth_locks.lock().unwrap().entry(account.id).or_default().clone();
                let _refreshing = lock.lock().await;
                if let Some(token) = valid() {
                    return Ok(Credential::Token(token));
                }
                let refresh_token = self.secret(account.id).await?;
                let fresh = match oauth::refresh(provider, &refresh_token).await {
                    Ok(fresh) => fresh,
                    Err(Error::Auth(why)) => {
                        self.refuse(account.id, &why);
                        return Err(Error::Auth(why));
                    }
                    Err(e) => return Err(e),
                };
                if let Some(new_refresh) = fresh.refresh_token.as_deref().filter(|t| *t != refresh_token) {
                    // The old one may already be void: the new one is kept in memory whatever happens, and
                    // written again later if the keychain refuses it now.
                    // Bounded: this lock holds every connection of the account, and a keychain can
                    // block for long. Too slow, it is written later like a refused write.
                    if let Err(e) = self.store_secret(account.id, new_refresh).await {
                        tracing::error!("account {}: new refresh token not saved to the keychain (retried later): {e}", account.id);
                        self.secret_cache.lock().unwrap().insert(account.id, new_refresh.to_string());
                        self.unsaved_secrets.lock().unwrap().insert(account.id);
                    }
                }
                self.tokens.lock().unwrap().insert(account.id, (fresh.access_token.clone(), fresh.expires_at));
                Ok(Credential::Token(fresh.access_token))
            }
        }
    }

    /// An access token refused although not expired (revoked, or the clock was off) is dropped and
    /// a fresh one tried, once. So is one the server turns down "for now": Outlook's "authenticated
    /// but not connected" can stick to a token. Turned down for longer than
    /// [`REFUSED_FOR_NOW_LIMIT`], the account waits for the user to sign in again, which is what
    /// clears it then.
    async fn login(&self, account: &Account) -> Result<Session> {
        let credential = self.credential(account).await?;
        let res = imap::connect(&account.imap, &account.username, &credential).await;
        let res = match (res, &credential) {
            (Err(Error::Auth(_) | Error::Refused(_)), Credential::Token(used)) => {
                self.forget_token(account.id, Some(used.as_str()));
                let credential = self.credential(account).await?;
                match imap::connect(&account.imap, &account.username, &credential).await {
                    // The provider has just issued that token: the grant holds, the server is the
                    // one failing (an Outlook backend issue). Signing in again would give the same
                    // token: it is a passing refusal.
                    Err(Error::Auth(why)) => Err(Error::Refused(why)),
                    // Outlook turns sign-ins down this way for about a minute after a launch (its
                    // new token or the last session's end not taken in yet): tried again a few
                    // times over that minute before telling.
                    Err(Error::Refused(why)) if imap::cured_by_signing_in(&why) => {
                        let mut res = Err(Error::Refused(why));
                        for wait in REFUSED_RETRIES {
                            tokio::time::sleep(wait).await;
                            res = imap::connect(&account.imap, &account.username, &credential).await;
                            if !matches!(&res, Err(Error::Refused(_))) {
                                break;
                            }
                        }
                        res
                    }
                    res => res,
                }
            }
            (res, _) => res,
        };
        match &res {
            Err(Error::Auth(why)) => self.refuse(account.id, why),
            Err(Error::Refused(why)) => self.refused_for_now(account.id, why, SystemTime::now()),
            Ok(_) => {
                self.refused_since.lock().unwrap().remove(&account.id);
            }
            Err(_) => {}
        }
        res
    }

    /// A sign-in turned down "for now" at `now`: refused again and again for
    /// [`REFUSED_FOR_NOW_LIMIT`] (none more than [`REFUSED_FOR_NOW_GAP`] apart, at least
    /// [`REFUSED_FOR_NOW_COUNT`] times), the account waits for the user.
    fn refused_for_now(&self, account_id: i64, why: &str, now: SystemTime) {
        if !imap::cured_by_signing_in(why) {
            return;
        }
        let (since, count) = {
            let mut runs = self.refused_since.lock().unwrap();
            let run = runs.entry(account_id).or_insert((now, now, 0));
            if now.duration_since(run.1).unwrap_or_default() > REFUSED_FOR_NOW_GAP {
                *run = (now, now, 0);
            }
            run.1 = now;
            run.2 += 1;
            (run.0, run.2)
        };
        if now.duration_since(since).unwrap_or_default() < REFUSED_FOR_NOW_LIMIT || count < REFUSED_FOR_NOW_COUNT {
            return;
        }
        self.refused_since.lock().unwrap().remove(&account_id);
        let why = crate::tr!(
            "le serveur refuse la connexion depuis plus de 30 minutes, reconnectez le compte ({why})",
            "the server has refused the sign-in for more than 30 minutes, sign in again ({why})"
        );
        self.refuse(account_id, &why);
    }

    /// Only if it is still `used`, when given: a connection that failed with an old token must not
    /// throw away a fresh one.
    fn forget_token(&self, account_id: i64, used: Option<&str>) {
        let mut tokens = self.tokens.lock().unwrap();
        if tokens.get(&account_id).is_some_and(|(token, _)| used.is_none_or(|u| u == token)) {
            tokens.remove(&account_id);
        }
    }

    /// Nothing tries the credentials again until they change.
    fn refuse(&self, account_id: i64, why: &str) {
        let previous = self.auth_refused.lock().unwrap().insert(account_id, why.to_string());
        if previous.as_deref() != Some(why) {
            tracing::warn!("account {account_id}: credentials refused, waiting for the user: {why}");
            self.emit(Event::AuthChanged { account_id, error: Some(why.to_string()) });
        }
    }

    fn clear_refusal(&self, account_id: i64) {
        if self.auth_refused.lock().unwrap().remove(&account_id).is_some() {
            self.emit(Event::AuthChanged { account_id, error: None });
        }
        // Its sends that waited for it go now.
        self.retry_sends_now(Some(account_id));
    }

    /// Sends waiting after a failure go at once (an account signed in again, the network back).
    fn retry_sends_now(&self, account_id: Option<i64>) {
        match self.store.retry_outgoing_now(account_id, crate::store::now()) {
            Ok(0) => {}
            Ok(_) => self.scheduler.notify_one(),
            Err(e) => tracing::warn!("waiting sends not brought forward: {e}"),
        }
    }

    /// They wait for [`Engine::update_password`] (password) or [`Engine::reconnect`] (OAuth2).
    pub fn auth_problems(&self) -> HashMap<i64, String> {
        self.auth_refused.lock().unwrap().clone()
    }

    /// Checked with the IMAP server before being saved.
    pub async fn update_password(self: &Arc<Self>, account_id: i64, password: &str) -> Result<()> {
        let account = self.store.account(account_id)?;
        if account.auth != AuthMethod::Password {
            return Err(Error::Other(crate::tr!(
                "ce compte se connecte sans mot de passe : reconnectez-le",
                "this account signs in without a password: reconnect it"
            )));
        }
        if password.is_empty() {
            return Err(Error::Other(crate::tr!("mot de passe vide", "empty password")));
        }
        let mut session = imap::connect(&account.imap, &account.username, &Credential::Password(password.to_string())).await?;
        logout(&mut session).await;
        self.store_secret(account_id, password).await?;
        self.clear_refusal(account_id);
        self.slots.lock().unwrap().retain(|(a, _), _| *a != account_id);
        // The listening connection, paused after the refusal, starts again now.
        self.resume.notify_waiters();
        let this = self.clone();
        self.spawn(async move {
            if let Err(e) = this.sync(account_id).await {
                tracing::warn!("sync after a new password (account {account_id}): {e}");
            }
            // The calendars were refused with the old password too: checked now rather than at the next
            // periodic recheck.
            this.calendar_checked.lock().unwrap().remove(&account_id);
            if let Err(e) = this.sync_calendars(Some(account_id)).await {
                tracing::warn!("calendar sync after a new password (account {account_id}): {e}");
            }
            if let Err(e) = this.sync_address_books(Some(account_id), true).await {
                tracing::warn!("address book sync after a new password (account {account_id}): {e}");
            }
        });
        Ok(())
    }

    async fn persist_unsaved_secrets(&self) {
        let pending: Vec<i64> = self.unsaved_secrets.lock().unwrap().iter().copied().collect();
        for account_id in pending {
            let Some(secret) = self.secret_cache.lock().unwrap().get(&account_id).cloned() else { continue };
            match self.store_secret(account_id, &secret).await {
                Ok(()) => tracing::info!("account {account_id}: refresh token saved to the keychain"),
                Err(e) => tracing::warn!("account {account_id}: refresh token still not saved to the keychain: {e}"),
            }
        }
    }

    /// Told by the interface on wake from sleep or back online: listening connections are likely
    /// dead. Calls closer than 20 s apart count once.
    pub fn resume(&self) {
        {
            let mut last = self.last_resume.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(20)) {
                return;
            }
            *last = Some(Instant::now());
        }
        tracing::info!("resumed (wake from sleep or back online): syncing now");
        self.retry_sends_now(None);
        // Kept connections may belong to the network before (and on Linux, Instant does not
        // count the sleep): each is checked with a NOOP before its next use. One in use is left
        // alone, and none is closed here (one connection for everything must stay one).
        let slots: Vec<Slot> = self.slots.lock().unwrap().values().cloned().collect();
        for slot in slots {
            if let Ok(mut guard) = slot.try_lock_owned()
                && let Some(conn) = guard.as_mut()
                && let Some(old) = Instant::now().checked_sub(POOL_CHECK_AFTER * 2)
            {
                conn.used = old;
            }
        }
        self.resume.notify_waiters();
        self.scheduler.notify_one();
    }

    /// Lets facteur start again with an empty database; the mail is downloaded again.
    pub fn set_aside_database(data_dir: &Path) -> Result<std::path::PathBuf> {
        let db = data_dir.join("facteur.db");
        let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
        let aside = data_dir.join(format!("facteur.db.corrupt-{stamp}"));
        std::fs::rename(&db, &aside)?;
        // The journal belongs to the old file: left there, it would be applied to the new one.
        for suffix in ["-wal", "-shm", "-journal"] {
            let from = data_dir.join(format!("facteur.db{suffix}"));
            if from.exists() {
                std::fs::rename(&from, data_dir.join(format!("facteur.db.corrupt-{stamp}{suffix}")))?;
            }
        }
        Ok(aside)
    }

    /// Read once from the keychain, off the async threads: keychain calls block, sometimes for long.
    async fn secret(&self, account_id: i64) -> Result<String> {
        if let Some(secret) = self.secret_cache.lock().unwrap().get(&account_id).cloned() {
            return Ok(secret);
        }
        let secret = tokio::task::spawn_blocking(move || Secrets::new().get(account_id))
            .await
            .map_err(|e| Error::Secret(e.to_string()))??;
        self.secret_cache.lock().unwrap().insert(account_id, secret.clone());
        Ok(secret)
    }

    /// Bounded: a keychain waiting for an unlock prompt must not hold the caller (the account's
    /// OAuth lock, the periodic loop). Too slow, the secret is written again later.
    async fn store_secret(&self, account_id: i64, secret: &str) -> Result<()> {
        let owned = secret.to_string();
        let write = tokio::task::spawn_blocking(move || Secrets::new().set(account_id, &owned));
        tokio::time::timeout(KEYCHAIN_WAIT, write)
            .await
            .map_err(|_| Error::Secret(crate::tr!("trousseau trop lent", "keychain too slow")))?
            .map_err(|e| Error::Secret(e.to_string()))??;
        self.secret_cache.lock().unwrap().insert(account_id, secret.to_string());
        self.unsaved_secrets.lock().unwrap().remove(&account_id);
        Ok(())
    }

    /// The tokens stay in memory until [`Engine::add_account`].
    pub async fn oauth_login(
        &self,
        provider: OAuthProvider,
        login_hint: Option<&str>,
        extended: bool,
        open: impl FnOnce(&str) -> Result<()>,
    ) -> Result<OAuthIdentity> {
        let login = oauth::begin(provider, login_hint, extended).await?;
        open(&login.url)?;
        let tokens = login.finish().await?;
        if tokens.refresh_token.is_none() {
            return Err(Error::Other(crate::tr!(
                "le fournisseur n'a pas rendu de jeton de rafraîchissement : retirez l'accès de facteur \
                 dans les réglages de sécurité du compte, puis recommencez",
                "the provider did not return a refresh token: remove facteur's access in the account's \
                 security settings, then try again"
            )));
        }
        let ticket = oauth::ticket();
        let email = tokens.email.clone();
        let mut pending = self.pending_oauth.lock().unwrap();
        // Abandoned flows (window closed before the account was created) do not keep their tokens.
        pending.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(15 * 60));
        pending.insert(ticket.clone(), (tokens, Instant::now()));
        Ok(OAuthIdentity { ticket, email })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    fn emit(&self, e: Event) {
        let _ = self.events.send(e);
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    async fn session(&self, account_id: i64, purpose: Purpose) -> Result<Conn> {
        let single = self.single_connection(account_id);
        // Waiting behind a sync on the one connection: it gives way between two folders.
        let _waiting = (single && purpose != Purpose::Sync).then(|| Waiting::new(self.waiting_on(account_id)));
        let purpose = if single { Purpose::Sync } else { purpose };
        let slot = self.slots.lock().unwrap().entry((account_id, purpose)).or_default().clone();
        let mut guard = tokio::time::timeout(SLOT_WAIT, slot.lock_owned()).await.map_err(|_| {
            Error::Other(crate::tr!("connexion au serveur occupée depuis trop longtemps", "server connection busy for too long"))
        })?;
        drop(_waiting);
        // Used a moment ago: still alive (TCP keepalive notices a dead one). After a while, a quick
        // NOOP; a server that takes more than 3 s to answer it is not worth waiting for.
        if let Some(conn) = guard.as_mut()
            && conn.used.elapsed() > POOL_CHECK_AFTER
            && !matches!(tokio::time::timeout(Duration::from_secs(3), conn.session.run_command_and_check_ok("NOOP")).await, Ok(Ok(_)))
        {
            *guard = None;
        }
        match guard.as_mut() {
            Some(conn) => conn.used = Instant::now(),
            None => {
                let account = self.store.account(account_id)?;
                let session = self.login(&account).await?;
                *guard = Some(Pooled { session, caps: None, used: Instant::now() });
            }
        }
        Ok(guard)
    }

    fn waiting_on(&self, account_id: i64) -> Arc<std::sync::atomic::AtomicUsize> {
        self.waiting.lock().unwrap().entry(account_id).or_default().clone()
    }

    /// At quit: every connection is closed with a LOGOUT, the listening ones stopped first, so
    /// that a server counting sessions (Outlook) does not turn down the next launch's sign-ins for
    /// the ones it still believes open. Bounded: quitting must not wait for a slow server.
    pub async fn sign_out(&self) {
        for stop in self.idle_stops.lock().unwrap().values() {
            let _ = stop.send(true);
        }
        let slots: Vec<Slot> = self.slots.lock().unwrap().values().cloned().collect();
        let mut sessions = Vec::new();
        for slot in slots {
            if let Ok(mut guard) = slot.try_lock_owned()
                && let Some(conn) = guard.take()
            {
                sessions.push(conn.session);
            }
        }
        let out = futures::future::join_all(sessions.iter_mut().map(logout));
        let _ = tokio::time::timeout(Duration::from_secs(2), out).await;
    }

    async fn close_idle_connections(&self) {
        let slots: Vec<Slot> = self.slots.lock().unwrap().values().cloned().collect();
        for slot in slots {
            let Ok(mut guard) = slot.try_lock_owned() else { continue };
            if guard.as_ref().is_some_and(|c| c.used.elapsed() > POOL_CLOSE_AFTER)
                && let Some(mut conn) = guard.take()
            {
                drop(guard);
                let _ = tokio::time::timeout(Duration::from_secs(5), logout(&mut conn.session)).await;
            }
        }
    }

    fn flush_lock(&self, account_id: i64) -> Arc<AsyncMutex<()>> {
        self.flush_locks.lock().unwrap().entry(account_id).or_default().clone()
    }

    /// Checks the IMAP credentials before saving. The durable secret goes to the system keychain.
    pub async fn add_account(&self, new: NewAccount) -> Result<Account> {
        self.ensure_new_address(&new.email, None)?;
        let mut picture = None;
        let (credential, secret, access) = match &new.auth {
            NewAuth::Password => (Credential::Password(new.password.clone()), new.password.clone(), None),
            NewAuth::OAuth2 { provider, ticket } => {
                // The token opens the whole mailbox (and the calendar): only to the provider's own servers,
                // encrypted.
                for srv in [&new.imap, &new.smtp] {
                    if !oauth_host(*provider, &srv.host) || srv.security == Security::None {
                        return Err(Error::Other(crate::tr!(
                            "la connexion {} ne vaut que pour ses serveurs, chiffrés : {} n'en est pas un",
                            "{} sign-in only works with its own servers, encrypted: {} is not one",
                            match provider {
                                OAuthProvider::Google => "Google",
                                OAuthProvider::Microsoft => "Microsoft",
                            },
                            srv.host
                        )));
                    }
                }
                let (tokens, _) = self
                    .pending_oauth
                    .lock()
                    .unwrap()
                    .remove(ticket)
                    .ok_or_else(|| Error::Other(crate::tr!("connexion expirée : recommencez la connexion au fournisseur", "sign-in expired: sign in to the provider again")))?;
                let refresh = tokens.refresh_token.clone().unwrap_or_default();
                picture = tokens.picture.clone();
                (Credential::Token(tokens.access_token.clone()), refresh, Some((tokens.access_token, tokens.expires_at)))
            }
        };
        let mut session = imap::connect(&new.imap, &new.username, &credential).await?;
        logout(&mut session).await;
        let id = self.store.insert_account(&new)?;
        // SQLite hands a removed account's id to the next one: nothing it left behind (a credential
        // refusal, a cached secret or token) may apply to this account.
        self.forget_account_state(id);
        if let Err(e) = self.store_secret(id, &secret).await {
            self.store.delete_account(id)?;
            return Err(e);
        }
        if let Some(access) = access {
            self.tokens.lock().unwrap().insert(id, access);
        }
        if let Some(url) = picture {
            self.store_picture(id, &url).await;
        }
        self.store.account(id)
    }

    /// Only OAuth gives a photo; password accounts on their own domain get its logo instead (once
    /// per session; webmail domains have none).
    async fn fill_account_logos(&self) {
        if self.store.setting("sender_logos").ok().flatten().as_deref() == Some("off") {
            return;
        }
        for account in self.store.list_accounts().unwrap_or_default() {
            if account.avatar.is_some() || account.avatar_custom || !self.logo_tried.lock().unwrap().insert(account.id) {
                continue;
            }
            let Some(domain) = crate::avatars::logo_domain(&account.email) else { continue };
            let known = self.store.sender_logos(std::slice::from_ref(&domain)).ok().and_then(|k| k.get(&domain).cloned());
            let logo = match known {
                Some((logo, _)) => logo,
                None => {
                    let logo = crate::avatars::fetch_logo(&domain).await;
                    let _ = self.store.save_sender_logo(&domain, logo.as_deref());
                    logo
                }
            };
            if let Some(logo) = logo
                && self.store.set_avatar(account.id, &logo).is_ok()
            {
                self.emit(Event::Changed { account_id: account.id });
            }
        }
    }

    /// The Google photo for Google sign-in, else the domain's logo fetched anew. Replaces an
    /// uploaded picture. Returns whether one was found.
    pub async fn refresh_account_avatar(&self, account_id: i64) -> Result<bool> {
        let account = self.store.account(account_id)?;
        let mut found = None;
        if account.auth == (AuthMethod::OAuth2 { provider: OAuthProvider::Google })
            && let Credential::Token(token) = self.credential(&account).await?
            && let Some(url) = oauth::google_picture(&token).await?
        {
            found = Some(oauth::fetch_picture(&url).await?);
        }
        if found.is_none()
            && let Some(domain) = crate::avatars::logo_domain(&account.email)
        {
            let logo = crate::avatars::fetch_logo(&domain).await;
            let _ = self.store.save_sender_logo(&domain, logo.as_deref());
            found = logo;
        }
        self.logo_tried.lock().unwrap().insert(account_id);
        self.store.replace_avatar(account_id, found.as_deref(), false)?;
        self.emit(Event::Changed { account_id });
        Ok(found.is_some())
    }

    /// `None` for the initials. Kept until the user refreshes it.
    pub fn set_account_avatar(&self, account_id: i64, image: Option<&str>) -> Result<()> {
        let picture = match image {
            Some(data) => Some(crate::avatars::account_picture(data).ok_or_else(|| {
                Error::Other(crate::tr!("image illisible ou trop lourde", "unreadable or too large image"))
            })?),
            None => None,
        };
        self.store.replace_avatar(account_id, picture.as_deref(), true)?;
        self.emit(Event::Changed { account_id });
        Ok(())
    }

    /// Optional: an error here must not block anything.
    async fn store_picture(&self, account_id: i64, url: &str) {
        match oauth::fetch_picture(url).await {
            Ok(data) => {
                if self.store.set_avatar(account_id, &data).is_ok() {
                    self.emit(Event::Changed { account_id });
                }
            }
            Err(e) => tracing::info!("photo of account {account_id}: {e}"),
        }
    }

    /// Revoked access, or new permissions (profile photo).
    pub async fn reconnect(self: &Arc<Self>, account_id: i64, ticket: &str) -> Result<Account> {
        let account = self.store.account(account_id)?;
        let (tokens, _) = self
            .pending_oauth
            .lock()
            .unwrap()
            .remove(ticket)
            .ok_or_else(|| Error::Other(crate::tr!("connexion expirée : recommencez la connexion au fournisseur", "sign-in expired: sign in to the provider again")))?;
        // The sign-in name may differ from the account's address (see `OAuthIdentity`).
        if let Some(email) = &tokens.email
            && !email.eq_ignore_ascii_case(&account.email)
            && !email.eq_ignore_ascii_case(&account.username)
        {
            return Err(Error::Other(crate::tr!("connecté en tant que {email}, mais ce compte est {}", "signed in as {email}, but this account is {}", account.email)));
        }
        let refresh = tokens.refresh_token.clone().ok_or_else(|| Error::Other(crate::tr!("aucun jeton de rafraîchissement rendu", "no refresh token returned")))?;
        self.store_secret(account_id, &refresh).await?;
        self.tokens.lock().unwrap().insert(account_id, (tokens.access_token.clone(), tokens.expires_at));
        // The calendar's token comes from the new consent too.
        self.calendar_tokens.lock().unwrap().remove(&account_id);
        self.calendar_read_only.lock().unwrap().remove(&account_id);
        self.clear_refusal(account_id);
        self.slots.lock().unwrap().retain(|(a, _), _| *a != account_id);
        self.resume.notify_waiters();
        if let Some(url) = &tokens.picture {
            self.store_picture(account_id, url).await;
        }
        // The new authorization may cover the calendars; the mail refused before comes now.
        let this = self.clone();
        self.spawn(async move {
            if let Err(e) = this.sync(account_id).await {
                tracing::warn!("sync after reconnecting (account {account_id}): {e}");
            }
            let _ = this.sync_address_books(Some(account_id), true).await;
            this.sync_calendars(Some(account_id)).await
        });
        self.store.account(account_id)
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        self.store.list_accounts()
    }

    /// Empty: the address is shown.
    pub fn set_account_label(&self, account_id: i64, label: &str) -> Result<()> {
        let label = label.trim();
        self.store.set_label(account_id, (!label.is_empty()).then_some(label))?;
        self.emit(Event::Changed { account_id });
        Ok(())
    }

    /// Off, the waiting messages go back to the inbox (those still on their way to the folder
    /// follow after the next sync); the decisions are kept for when it is turned on again.
    pub fn set_screener(self: &Arc<Self>, account_id: i64, on: bool) -> Result<()> {
        let account = self.store.account(account_id)?;
        if on {
            self.store.set_setting(&release_key(account_id), "0")?;
            if account.screener_since.is_none() {
                self.store.set_screener_since(account_id, Some(crate::store::now()))?;
            }
        } else {
            let done = self.release_held(account_id)?;
            self.store.set_setting(&release_key(account_id), if done { "0" } else { "1" })?;
            // Last: when the move is refused, the screener stays on and can be turned off again.
            self.store.set_screener_since(account_id, None)?;
        }
        self.emit(Event::Changed { account_id });
        Ok(())
    }

    /// Messages still on their way to the folder (no UID yet) cannot move: they are left for after
    /// the next sync ([`Engine::after_sync`]). Returns whether nothing was left.
    fn release_held(self: &Arc<Self>, account_id: i64) -> Result<bool> {
        let Some(held) = self.store.mailbox_by_role(account_id, Role::Screener)? else { return Ok(true) };
        let ids = self.store.mailbox_message_ids(held.id)?;
        let (settled, moving): (Vec<_>, Vec<_>) = self.store.locations(&ids)?.into_iter().partition(|(_, l)| l.uid.is_some());
        if !settled.is_empty() {
            let inbox = self.role_mailbox(account_id, Role::Inbox)?;
            self.move_inner(&settled.into_iter().map(|(id, _)| id).collect::<Vec<_>>(), inbox, None)?;
        }
        Ok(moving.is_empty())
    }

    fn finish_release(self: &Arc<Self>, account_id: i64) {
        if self.store.setting(&release_key(account_id)).ok().flatten().as_deref() != Some("1")
            || self.store.account(account_id).map_or(true, |a| a.screener_since.is_some())
        {
            return;
        }
        match self.release_held(account_id) {
            Ok(true) => {
                let _ = self.store.set_setting(&release_key(account_id), "0");
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("screener of account {account_id}, messages back to the inbox: {e}"),
        }
    }

    /// Without `+tag`: mail from one of them is the user's own (see `Store::screen_verdicts`).
    fn own_addresses(&self) -> Result<HashSet<String>> {
        let mut own = HashSet::new();
        for account in self.store.list_accounts()? {
            own.insert(crate::store::base_address(&account.email));
            own.extend(account.identities.iter().map(|i| crate::store::base_address(&i.email)));
        }
        Ok(own)
    }

    /// Also records as accepted the senders whose message another device or client moved back to
    /// the inbox. Returns (held, refused): what did move; a failed move leaves messages in the inbox.
    fn screen(self: &Arc<Self>, account_id: i64, ids: &[i64]) -> Result<(HashSet<i64>, HashSet<i64>)> {
        let account = self.store.account(account_id)?;
        let Some(since) = account.screener_since else { return Ok(Default::default()) };
        if ids.is_empty() {
            return Ok(Default::default());
        }
        let own = self.own_addresses()?;
        let (mut held, mut refused) = (Vec::new(), Vec::new());
        for (id, verdict) in self.store.screen_verdicts(account_id, ids, since, &own)? {
            match verdict {
                Verdict::Hold => held.push(id),
                Verdict::Refuse => refused.push(id),
                Verdict::Accept(sender) => {
                    if let Err(e) = self.accept_moved_back(account_id, id, &sender) {
                        tracing::warn!("screener of account {account_id}, {sender} accepted elsewhere: {e}");
                    }
                }
            }
        }
        let held = self.screen_move(account_id, held, Role::Screener, &screener_folder());
        let refused = self.screen_move(account_id, refused, Role::Refused, &refused_folder());
        Ok((held, refused))
    }

    /// Created if missing. When the batch is refused (a message gone meanwhile), moves one by one so
    /// that one bad message does not keep the others in the inbox.
    fn screen_move(self: &Arc<Self>, account_id: i64, ids: Vec<i64>, role: Role, leaf: &str) -> HashSet<i64> {
        if ids.is_empty() {
            return HashSet::new();
        }
        let dest = match self.special_mailbox(account_id, &[role], role, leaf) {
            Ok(dest) => dest,
            Err(e) => {
                tracing::warn!("screener of account {account_id}, folder {leaf}: {e}");
                return HashSet::new();
            }
        };
        let moved: Vec<i64> = match self.move_inner(&ids, dest, None) {
            Ok(()) => ids,
            Err(e) => {
                tracing::warn!("screener of account {account_id}, to {leaf}: {e}; one by one");
                ids.into_iter().filter(|id| self.move_inner(&[*id], dest, None).is_ok()).collect()
            }
        };
        if let Err(e) = self.store.remember_screened(&moved) {
            tracing::warn!("screener of account {account_id}: {e}");
        }
        moved.into_iter().collect()
    }

    /// A message held or refused here came back to the inbox through another client (Gmail on the
    /// web, a phone): the user accepted its sender there. The decision is about the address (it
    /// outweighs a block of the domain), and the sender's other waiting messages follow.
    fn accept_moved_back(self: &Arc<Self>, account_id: i64, id: i64, sender: &str) -> Result<()> {
        if self.store.screening_decision(account_id, sender)?.as_deref() != Some("accept") {
            self.store.set_screening(account_id, sender, Some("accept"))?;
        }
        self.store.forget_screened(&[id])?;
        let waiting = self.store.held_leftovers(account_id, sender)?;
        if !waiting.is_empty() {
            let inbox = self.role_mailbox(account_id, Role::Inbox)?;
            self.move_inner(&waiting, inbox, None)?;
        }
        Ok(())
    }

    pub fn held_senders(&self, account_id: Option<i64>) -> Result<Vec<HeldSender>> {
        let held = self.store.held_senders(account_id)?;
        let latest: Vec<i64> = held.iter().map(|h| h.4).collect();
        let mut summaries: HashMap<i64, MessageSummary> = self.store.summaries(&latest, latest.len())?.into_iter().map(|s| (s.id, s)).collect();
        let trust = self.trust_parts_many(&latest);
        let mut out = Vec::new();
        for (account_id, email, name, count, latest) in held {
            let Some(summary) = summaries.remove(&latest) else { continue };
            let trust = trust.get(&latest).map_or(crate::trust::Level::Medium, |(s, c)| crate::trust::evaluate(s, c).level);
            let domainable = email.rsplit_once('@').is_some_and(|(_, host)| !crate::autoconfig::is_public_domain(host));
            out.push(HeldSender { account_id, email, name, count, latest: summary, trust, domainable });
        }
        Ok(out)
    }

    /// `domain`: about every address of the domain, never for a public provider's. A domain
    /// decision leaves alone addresses decided on their own and, when refused, the people the account
    /// wrote to. Nothing is recorded when the messages cannot move. Returns the undo handle.
    pub async fn screen_sender(self: &Arc<Self>, account_id: i64, email: &str, verdict: Screen, domain: bool) -> Result<Option<u64>> {
        let email = email.trim().trim_start_matches('@').to_lowercase();
        let host = match email.rsplit_once('@') {
            Some((_, host)) => host.to_string(),
            None if domain && email.contains('.') => email.clone(),
            None => return Err(Error::Other(crate::tr!("adresse invalide", "invalid address"))),
        };
        if domain && crate::autoconfig::is_public_domain(&host) {
            return Err(Error::Other(crate::tr!(
                "{host} est un fournisseur de messagerie public : décidez adresse par adresse",
                "{host} is a public mail provider: decide address by address"
            )));
        }
        let pattern = if domain { format!("@{host}") } else { email.clone() };
        let block = verdict == Screen::Block;
        let sources: &[Role] = if block { &[Role::Screener, Role::Inbox] } else { &[Role::Screener, Role::Refused] };
        let find = |pattern: &str| -> Result<Vec<i64>> {
            let mut ids = Vec::new();
            for role in sources {
                ids.extend(self.store.messages_from_pattern(account_id, pattern, *role)?);
            }
            Ok(ids)
        };
        // Messages just held or refused are still on their way to the folder: they get there first.
        // If one still has not, the move below says so.
        let _ = self.settle_bounded(&find(&pattern)?).await;
        let mut targets = find(&pattern)?;
        if domain {
            let mut spared = HashSet::new();
            for sender in self.store.senders_of(&targets)? {
                if self.store.spared_by_domain(account_id, &sender, block)? {
                    spared.extend(find(&sender)?);
                }
            }
            targets.retain(|id| !spared.contains(id));
        }
        // The messages move first: a refused move leaves the decision and categories untouched.
        let mut undo = Undo::default();
        let before = self.store.screening_decision(account_id, &pattern)?;
        if !targets.is_empty() {
            let dest = match verdict {
                Screen::Block => self.special_mailbox(account_id, &[Role::Refused], Role::Refused, &refused_folder())?,
                _ => self.role_mailbox(account_id, Role::Inbox)?,
            };
            self.move_inner(&targets, dest, Some(&mut undo))?;
        }
        undo.screening.push((account_id, pattern.clone(), before));
        self.store.set_screening(account_id, &pattern, Some(if block { "block" } else { "accept" }))?;
        if block {
            self.store.remember_screened(&targets)?;
        } else {
            self.store.forget_screened(&targets)?;
        }
        let category = match verdict {
            Screen::Inbox => Some(Category::People),
            Screen::Newsletter => Some(Category::Newsletter),
            Screen::Notification => Some(Category::Notification),
            Screen::Block => None,
        };
        if let Some(category) = category {
            let mut senders: Vec<String> = self.store.senders_of(&targets)?;
            if !domain && !senders.contains(&email) {
                senders.push(email.clone());
            }
            for sender in senders {
                undo.categories.push(self.store.sender_category_snapshot(&sender)?);
                self.store.set_sender_category(&sender, category)?;
            }
        }
        self.emit(Event::Changed { account_id });
        Ok(self.keep_undo(undo))
    }

    pub fn screening(&self, account_id: i64) -> Result<Vec<Screening>> {
        self.store.screening_list(account_id)
    }

    /// Forgetting a refusal also takes the messages out of "Screened out" (left there, they would
    /// keep refusing the sender). A domain's addresses decided on their own stay put.
    pub async fn forget_screening(self: &Arc<Self>, account_id: i64, pattern: &str) -> Result<()> {
        let pattern = pattern.trim().to_lowercase();
        if self.store.screening_decision(account_id, &pattern)?.as_deref() == Some("block") {
            let _ = self.settle_bounded(&self.store.messages_from_pattern(account_id, &pattern, Role::Refused)?).await;
            let mut ids = self.store.messages_from_pattern(account_id, &pattern, Role::Refused)?;
            if pattern.starts_with('@') {
                let mut spared = HashSet::new();
                for sender in self.store.senders_of(&ids)? {
                    if self.store.screening_decision(account_id, &sender)?.is_some() {
                        spared.extend(self.store.messages_from_pattern(account_id, &sender, Role::Refused)?);
                    }
                }
                ids.retain(|id| !spared.contains(id));
            }
            if !ids.is_empty() {
                let dest = match self.store.account(account_id)?.screener_since {
                    Some(_) => self.special_mailbox(account_id, &[Role::Screener], Role::Screener, &screener_folder())?,
                    None => self.role_mailbox(account_id, Role::Inbox)?,
                };
                self.move_inner(&ids, dest, None)?;
            }
        }
        self.store.set_screening(account_id, &pattern, None)?;
        self.emit(Event::Changed { account_id });
        Ok(())
    }

    pub fn reorder_accounts(&self, ids: &[i64]) -> Result<()> {
        self.store.reorder_accounts(ids)
    }

    pub fn set_account_color(&self, account_id: i64, color: &str) -> Result<()> {
        let color = color.trim();
        self.store.set_color(account_id, (!color.is_empty()).then_some(color))
    }

    /// One command per folder on the server, not one per message. Returns the number of folders.
    pub fn mark_account_read(self: &Arc<Self>, account_id: i64, inbox_only: bool) -> Result<usize> {
        let states = self.store.mailbox_states(account_id)?;
        let mut boxes = self.store.mark_account_seen(account_id, inbox_only)?;
        // Gmail's labels: the inbox's messages are All Mail's, which must not be marked whole.
        // Gmail's flags belong to the message: marking its INBOX folder read is enough.
        if inbox_only
            && self.store.labels_model(account_id)?
            && let Some(inbox) = states.iter().find(|s| s.role == Some(Role::Inbox))
        {
            if !boxes.is_empty() {
                let label = crate::i18n::both(|| crate::tr!("Tout marquer comme lu · {}", "Mark all as read · {}", crate::utf7::decode(&inbox.name)));
                self.enqueue(account_id, &Op::MarkAllSeen { mailbox: inbox.name.clone(), below: None, uidvalidity: None }, &label)?;
            }
            boxes.clear();
        }
        for mailbox in &boxes {
            let label = crate::i18n::both(|| crate::tr!("Tout marquer comme lu · {}", "Mark all as read · {}", crate::utf7::decode(mailbox)));
            // Only what already arrived: a message received before the op leaves (offline, queue
            // waiting) stays unread.
            let state = states.iter().find(|s| s.name == *mailbox);
            let (below, uidvalidity) = (state.and_then(|s| self.uid_cutoff(s)), state.and_then(|s| s.uidvalidity));
            self.enqueue(account_id, &Op::MarkAllSeen { mailbox: mailbox.clone(), below, uidvalidity }, &label)?;
        }
        self.after_local_change(vec![account_id]);
        Ok(boxes.len())
    }

    /// The folder's UIDNEXT from the last sync, or past the highest cached UID if higher (messages
    /// added since by a search or a listening connection). "Everything so far" ops stop there.
    fn uid_cutoff(&self, state: &crate::store::MailboxState) -> Option<u32> {
        let highest = self.store.uid_stats(state.id).ok().and_then(|(_, max)| max);
        cutoff(state.uidnext, highest)
    }

    pub fn coverage(&self) -> Result<Vec<crate::store::Coverage>> {
        self.store.list_accounts()?.into_iter().map(|a| self.store.coverage(a.id)).collect()
    }

    pub fn history_busy(&self) -> Vec<i64> {
        self.fetching_history.lock().unwrap().iter().copied().collect()
    }

    /// Each folder is walked back in batches to its first message, on a dedicated connection so
    /// reading stays possible meanwhile.
    pub async fn fetch_all_history(self: &Arc<Self>, account_id: i64) -> Result<usize> {
        let Some(_busy) = Busy::take(&self.fetching_history, account_id) else {
            return Err(Error::Other(crate::tr!("la récupération de l'historique de ce compte est déjà en cours", "this account's history is already being fetched")));
        };
        let res = async {
            let account = self.store.account(account_id)?;
            let mut session = self.dedicated_session(&account).await?;
            let caps = bounded(30, &crate::tr!("capacités du serveur", "server capabilities"), sync::capabilities(&mut session)).await?;
            let mut added = 0;
            // New inbox messages found on the way: the rules apply to them like after a sync.
            let mut fresh_inbox = Vec::new();
            let states: Vec<_> = self.store.mailbox_states(account_id)?.into_iter().filter(|s| s.selectable).collect();
            for state in states {
                // On an account limited to one connection, whoever waits for it goes first.
                backup::Lease::give_way(&mut session).await?;
                let label = crate::utf7::decode(&state.name);
                let total = state.server_count.unwrap_or(0).max(0) as usize;
                let what = crate::tr!("récupération de l'historique", "fetching history");
                // First the gaps inside the window already synced, through a sync that reads the folder's
                // whole UID list.
                sync::force_full_check(state.id);
                let before = self.store.uid_stats(state.id)?.0;
                let current = self.store.mailbox_state(state.id)?;
                let inbox = current.role == Some(Role::Inbox);
                // A folder the server refuses does not stop the others, as in a sync.
                let (ids, _) = match bounded(600, &what, sync::sync_mailbox(&self.store, account_id, &mut session, &caps, current, &mut |_| {})).await {
                    Err(Error::Imap(async_imap::error::Error::No(msg))) => {
                        tracing::warn!("history of {label} skipped: {msg}");
                        continue;
                    }
                    res => res?,
                };
                if inbox {
                    fresh_inbox.extend(ids);
                }
                added += self.store.uid_stats(state.id)?.0.saturating_sub(before);
                loop {
                    let state = self.store.mailbox_state(state.id)?;
                    let done = self.store.uid_stats(state.id)?.0;
                    self.emit(Event::SyncProgress { account_id, mailbox: label.clone(), done, total: total.max(done) });
                    backup::Lease::give_way(&mut session).await?;
                    let n = bounded(600, &what, sync::load_older(&self.store, &mut session, state, sync::WINDOW)).await?;
                    added += n;
                    if n == 0 {
                        break;
                    }
                    self.emit(Event::Changed { account_id });
                }
            }
            session.finish().await;
            self.after_inbox_arrivals(account_id, &fresh_inbox, false);
            Ok(added)
        }
        .await;
        drop(_busy);
        // Same signal as a sync: the interface clears its progress bar.
        self.emit(Event::SyncFinished { account_id, new_messages: 0, new_in_inbox: 0, error: res.as_ref().err().map(ToString::to_string) });
        self.emit(Event::Changed { account_id });
        res
    }

    pub fn account_stats(&self, account_id: i64) -> Result<AccountStats> {
        let account = self.store.account(account_id)?;
        self.store.account_stats(account_id, &account.email, chrono::Utc::now().timestamp())
    }

    pub fn set_identities(&self, account_id: i64, identities: Vec<Identity>) -> Result<()> {
        let account = self.store.account(account_id)?;
        let mut clean: Vec<Identity> = Vec::new();
        for mut i in identities {
            i.email = i.email.trim().to_string();
            i.name = i.name.trim().to_string();
            if i.email.is_empty() {
                continue;
            }
            if !i.email.contains('@') || i.email.parse::<lettre::Address>().is_err() {
                return Err(Error::Other(crate::tr!("adresse invalide : {}", "invalid address: {}", i.email)));
            }
            if i.email.eq_ignore_ascii_case(&account.email) || clean.iter().any(|c| c.email.eq_ignore_ascii_case(&i.email)) {
                continue;
            }
            clean.push(i);
        }
        self.store.set_identities(account_id, &clean)
    }

    pub fn set_signature(&self, account_id: i64, signature: &str) -> Result<()> {
        self.store.set_signature(account_id, signature)
    }

    pub fn remove_account(&self, id: i64) -> Result<()> {
        // Its listening connection closes now, not at the next IDLE renewal.
        if let Some(stop) = self.idle_stops.lock().unwrap().remove(&id) {
            let _ = stop.send(true);
        }
        self.slots.lock().unwrap().retain(|(a, _), _| *a != id);
        self.flush_locks.lock().unwrap().remove(&id);
        self.fetching_history.lock().unwrap().remove(&id);
        self.flush_requests.lock().unwrap().remove(&id);
        self.oauth_locks.lock().unwrap().remove(&id);
        self.forget_account_state(id);
        self.secrets.delete(id)?;
        self.store.delete_account(id)
    }

    /// Done on removal and again when an account is added, since SQLite may give the new one the
    /// removed account's id. In-flight guards clear themselves when their task ends.
    fn forget_account_state(&self, id: i64) {
        self.clear_refusal(id);
        self.tokens.lock().unwrap().remove(&id);
        self.secret_cache.lock().unwrap().remove(&id);
        self.unsaved_secrets.lock().unwrap().remove(&id);
        self.authservs.lock().unwrap().remove(&id);
        self.logo_tried.lock().unwrap().remove(&id);
        self.queue_errors.lock().unwrap().remove(&id);
        self.calendar_checked.lock().unwrap().remove(&id);
        self.idle_renew.lock().unwrap().remove(&id);
        self.draft_prints.lock().unwrap().retain(|(account, _), _| *account != id);
    }

    pub fn mailboxes(&self, account_id: i64) -> Result<Vec<Mailbox>> {
        self.store.list_mailboxes(account_id)
    }

    /// `category`: smart inbox (people, notifications or newsletters only).
    pub fn threads(
        &self,
        mailbox_id: i64,
        filter: ThreadFilter,
        category: Option<Category>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ThreadSummary>> {
        // Pinned threads stay on top in an inbox.
        let inbox = match self.store.mailbox_state(mailbox_id) {
            Ok(state) => state.role == Some(Role::Inbox),
            // Just deleted, its list still shown while the view moves away: empty, not an error.
            Err(Error::NotFound(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        self.store.list_threads(&[mailbox_id], &[], filter, inbox, category, limit, offset)
    }

    pub fn unified_threads(&self, filter: ThreadFilter, category: Option<Category>, limit: usize, offset: usize) -> Result<Vec<ThreadSummary>> {
        let inboxes = self.store.inbox_ids()?;
        if inboxes.is_empty() {
            return Ok(Vec::new());
        }
        self.store.list_threads(&inboxes, &[], filter, true, category, limit, offset)
    }

    pub fn category_unread(&self, mailbox_id: Option<i64>) -> Result<HashMap<Category, i64>> {
        let boxes = match mailbox_id {
            Some(id) => vec![id],
            None => self.store.inbox_ids()?,
        };
        self.store.category_unread(&boxes)
    }

    /// Applies to its cached and future messages.
    pub fn set_sender_category(&self, email: &str, category: Category) -> Result<()> {
        self.store.set_sender_category(email, category)?;
        for account in self.store.list_accounts()? {
            self.emit(Event::Changed { account_id: account.id });
        }
        Ok(())
    }

    pub fn sender_category(&self, email: &str) -> Result<Option<Category>> {
        self.store.sender_category(email)
    }

    /// What left the inboxes, across accounts: each account's archive folder, or Gmail's All Mail,
    /// without the threads back in an inbox.
    pub fn done_threads(&self, filter: ThreadFilter, limit: usize, offset: usize) -> Result<Vec<ThreadSummary>> {
        let mut archives = Vec::new();
        for account in self.store.list_accounts()? {
            let archive = self.store.mailbox_by_role(account.id, Role::Archive)?.or(self.store.mailbox_by_role(account.id, Role::All)?);
            archives.extend(archive.map(|m| m.id));
        }
        if archives.is_empty() {
            return Ok(Vec::new());
        }
        self.store.list_threads(&archives, &self.store.inbox_ids()?, filter, false, None, limit, offset)
    }

    /// `since`: start of the day, in the user's time zone.
    pub fn dashboard(&self, since: i64, flagged_limit: usize) -> Result<Dashboard> {
        let (flagged_count, flagged) = self.store.flagged_messages(flagged_limit)?;
        Ok(Dashboard { received_today: self.store.received_since(since)?, flagged_count, flagged })
    }

    /// `prefer`: the folder the thread is opened from (see `Store::thread_messages`).
    pub fn thread(&self, account_id: i64, thread_id: i64, prefer: Option<i64>) -> Result<Vec<MessageSummary>> {
        self.store.thread_messages(account_id, thread_id, prefer)
    }

    /// The user's own addresses are left out. An empty query gives the recent recipients.
    pub fn contacts(&self, query: &str) -> Result<Vec<Address>> {
        let own: Vec<String> = self
            .store
            .list_accounts()?
            .into_iter()
            .flat_map(|a| std::iter::once(a.email).chain(a.identities.into_iter().map(|i| i.email)))
            .collect();
        if query.trim().is_empty() {
            return self.store.recent_recipients(&own, 8);
        }
        self.store.search_contacts(query, &own, 8)
    }

    pub fn account_of(&self, message_id: i64) -> Result<i64> {
        Ok(self.store.location(message_id)?.account_id)
    }

    pub fn search(&self, account_id: i64, query: &str, bins: bool) -> Result<Vec<MessageSummary>> {
        let started = Instant::now();
        let res = self.store.search(account_id, query, 200, bins);
        let took = started.elapsed();
        if took > Duration::from_millis(300) {
            // The query's length only: what the user searches for stays out of the log file.
            tracing::warn!("search of {} characters (account {account_id}) took {} ms", query.chars().count(), took.as_millis());
        }
        res
    }

    async fn raw(&self, id: i64) -> Result<Vec<u8>> {
        let (raw, downloaded) = self.raw_unkept(id).await?;
        if let Some((mailbox, uid, validity)) = downloaded {
            crate::rt::offload(|| sync::keep_body(&self.store, id, &mailbox, uid, validity, &raw))?;
        }
        Ok(raw)
    }

    /// Like [`Engine::raw`], but a body just downloaded is left for the caller to cache, with its
    /// folder, UID and UIDVALIDITY then (see [`sync::keep_body`]).
    async fn raw_unkept(&self, id: i64) -> Result<(Vec<u8>, Option<(String, u32, Option<u32>)>)> {
        if let Some(raw) = crate::rt::offload(|| self.store.body(id))? {
            return Ok((raw, None));
        }
        let loc = self.store.location(id)?;
        let uid = loc.uid.ok_or_else(in_flight)?;
        // Two minutes plus a second per 50 KB: a 40 MB message on a slow line must not be cut off
        // every time it is opened.
        let size = self.store.message_size(id).unwrap_or(0);
        let secs = 120 + size / 50_000;
        // A kept connection the server dropped meanwhile: once more on a new one.
        let mut attempt = 0;
        let res = loop {
            attempt += 1;
            let mut guard = self.session(loc.account_id, Purpose::Interactive).await?;
            let session = &mut guard.as_mut().expect("session open").session;
            let res = bounded(secs, &crate::tr!("téléchargement du message", "downloading the message"), sync::download_body(&self.store, session, id, &loc.mailbox, uid)).await;
            // A cut EXAMINE or FETCH must not leave the dead connection in the slot for the next message.
            if matches!(res, Err(Error::Io(_) | Error::Imap(_) | Error::Other(_) | Error::Dropped(_) | Error::Refused(_))) {
                *guard = None;
            }
            match res {
                Err(e) if attempt == 1 && e.is_connection_drop() => {
                    tracing::info!("message {id}: {e}; trying again on a new connection");
                    self.note_cut(loc.account_id, false);
                }
                res => break res,
            }
        };
        let (raw, validity) = res?;
        Ok((raw, Some((loc.mailbox, uid, validity))))
    }

    /// Moved, and its new folder has not confirmed it yet (no UID).
    pub fn is_moving(&self, id: i64) -> bool {
        self.store.location(id).is_ok_and(|l| l.uid.is_none())
    }

    /// Settling queues behind a running sync (a first sync can hold it for hours) or a slow server,
    /// and a user's action must not hang with it. Past `SETTLE_WAIT` the action goes on and settling
    /// carries on in the background (the dropped handle detaches the task, it is not cancelled).
    pub async fn settle_bounded(self: &Arc<Self>, ids: &[i64]) -> Result<()> {
        let (this, ids) = (self.clone(), ids.to_vec());
        let work = self.spawn(async move { this.settle(&ids).await });
        match tokio::time::timeout(SETTLE_WAIT, work).await {
            Ok(joined) => joined.unwrap_or_else(|e| Err(Error::Other(e.to_string()))),
            Err(_) => Err(Error::Other(format!("still waiting after {}s", SETTLE_WAIT.as_secs()))),
        }
    }

    /// Messages still on their way to another folder get there now: their move is sent at once, its
    /// undo window cut short, then the destination folders are read, so an action on them does not
    /// wait for the next sync.
    pub async fn settle(self: &Arc<Self>, ids: &[i64]) -> Result<()> {
        let mut dests: HashMap<i64, Vec<String>> = HashMap::new();
        let mut moving = HashSet::new();
        for (id, loc) in self.store.locations(ids)? {
            if loc.uid.is_none() {
                moving.insert(id);
                let folders = dests.entry(loc.account_id).or_default();
                if !folders.contains(&loc.mailbox) {
                    folders.push(loc.mailbox);
                }
            }
        }
        if moving.is_empty() {
            return Ok(());
        }
        // Only the moves that carry these messages lose their undo window, not every move into the folder.
        let sources: HashSet<(i64, u32)> = self
            .undos
            .lock()
            .unwrap()
            .values()
            .flat_map(|undo| undo.moves.iter())
            .filter(|(id, ..)| moving.contains(id))
            .filter_map(|(_, mailbox_id, uid)| uid.map(|uid| (*mailbox_id, uid)))
            .collect();
        let mut names: HashMap<i64, Option<(i64, String)>> = HashMap::new();
        let mut moved: HashMap<i64, HashSet<(String, u32)>> = HashMap::new();
        for (mailbox_id, uid) in sources {
            let folder = names.entry(mailbox_id).or_insert_with(|| self.store.mailbox_state(mailbox_id).ok().map(|s| (s.account_id, s.name)));
            if let Some((account_id, name)) = folder {
                moved.entry(*account_id).or_default().insert((name.clone(), uid));
            }
        }
        for (account_id, folders) in dests {
            // A move made without an undo window (screener, rules) has no source noted: the destination
            // folders find it, and what holds it back in the queue goes.
            self.store.release_moves_of(account_id, moved.get(&account_id).unwrap_or(&HashSet::new()), &folders)?;
            self.sync_scoped(account_id, sync::Scope::Folders(folders)).await?;
        }
        Ok(())
    }

    /// Without a usable DKIM verdict from the provider, facteur checks the signatures itself with
    /// already known keys only: looking keys up when the message is opened would tell the sender
    /// when it was read (lookups happen on arrival, see [`Engine::check_dkim`]). A check still under
    /// way may change the trust afterwards ([`Event::TrustChanged`]).
    pub async fn message(self: &Arc<Self>, id: i64) -> Result<MessageDetail> {
        let (raw, downloaded) = self.raw_unkept(id).await?;
        let raw = Arc::new(raw);
        // Cached in the background: the message shows without waiting for the writer (a sync's batch
        // may hold it) nor for a second parse of the body.
        if let Some((mailbox, uid, validity)) = downloaded {
            let (this, raw) = (self.clone(), raw.clone());
            tokio::task::spawn_blocking(move || {
                if let Err(e) = sync::keep_body(&this.store, id, &mailbox, uid, validity, &raw) {
                    tracing::info!("body of message {id} not cached: {e}");
                }
            });
        }
        let summary = self.store.summary(id)?;
        // Off the async workers with the parse: an invitation's rules and a long text are read
        // here (a crafted invite must not hold a worker). An invitation has its own card;
        // otherwise a day and time in the sender's own words (the quoted thread left out) offer to
        // make an event.
        let (parsed, invite, appointment) = {
            let raw = raw.clone();
            let (date, subject) = (summary.date, summary.subject.clone());
            blocking(move || {
                let parsed = mime::parse_body(&raw)?;
                let invite = parsed.calendar.as_deref().and_then(crate::calendar::parse);
                let appointment = match invite {
                    Some(_) => None,
                    None => {
                        let text = parsed.text.clone().unwrap_or_else(|| crate::mime::html_to_text(&parsed.html));
                        let received = chrono::DateTime::from_timestamp(date, 0).map(|d| d.with_timezone(&chrono::Local).date_naive());
                        received.and_then(|day| crate::ai::appointment(&crate::ai::fresh(&text), &subject_core(&subject), day, crate::store::now()))
                    }
                };
                Ok((parsed, invite, appointment))
            })
            .await?
        };
        let (mut signals, context) = self.trust_parts(id, Some(&parsed.headers.signals))?;
        // Before the images are allowed, so that a forged signature never loads them.
        if crate::dkim::wanted(&signals, &context)
            && let Some(verdict) = self.dkim_verdict(id, &raw, summary.from.as_ref().map(|a| a.email.as_str()), summary.date, false).await
        {
            signals.dkim_local = Some(verdict);
            if let Err(e) = crate::rt::offload(|| self.store.set_signals(id, &signals)) {
                tracing::info!("DKIM verdict of message {id} not kept: {e}");
            }
        }
        let trust = crate::trust::evaluate(&signals, &context);
        let sender_key = parsed.headers.from.as_ref().and_then(|a| crate::avatars::sender_key(&a.email));
        let invite = invite.map(|mut i| {
            i.my_response = self.store.setting(&format!("rsvp:{}", i.uid)).ok().flatten();
            i
        });
        Ok(MessageDetail {
            summary,
            cc: parsed.headers.cc,
            reply_to: parsed.headers.reply_to,
            message_id: parsed.headers.message_id,
            references: parsed.headers.references,
            html: parsed.html,
            is_plain: parsed.is_plain,
            text: parsed.text,
            has_remote_content: parsed.has_remote_content,
            attachments: parsed.attachments,
            unsubscribe: parsed.unsubscribe,
            bulk: parsed.headers.bulk,
            images_allowed: match &sender_key {
                Some(key) => !trust.is_low() && self.store.images_trusted(key)?,
                None => false,
            },
            sender_key,
            trust,
            invite,
            appointment,
        })
    }

    /// `partstat`: `ACCEPTED`, `TENTATIVE` or `DECLINED`. Sent to the organizer from the invited
    /// address (an alias if needed).
    pub async fn respond_invite(self: &Arc<Self>, id: i64, partstat: &str) -> Result<()> {
        let words = answer_words(partstat)?;
        let account = self.store.account(self.store.location(id)?.account_id)?;
        let ics = parse_body(self.raw(id).await?).await?
            .calendar
            .ok_or_else(|| Error::Other(crate::tr!("ce message ne contient pas d'invitation", "this message contains no invitation")))?;
        self.mail_invite_reply(&account, &ics, partstat, words, Some(id)).await
    }

    /// The iMIP REPLY (RFC 6047) to the organizer of `ics`, from the invited address (an alias if
    /// needed). `words`: the reply's verb and status, in its subject and text.
    async fn mail_invite_reply(
        self: &Arc<Self>,
        account: &Account,
        ics: &str,
        partstat: &str,
        (verb, status, status_en): (String, &str, &str),
        in_reply_to: Option<i64>,
    ) -> Result<()> {
        let invite = crate::calendar::parse(ics).ok_or_else(|| Error::Other(crate::tr!("invitation illisible", "unreadable invitation")))?;
        let organizer = invite.organizer.clone().ok_or_else(|| Error::Other(crate::tr!("l'invitation n'indique pas d'organisateur", "the invitation does not name an organizer")))?;
        let own: Vec<(String, String)> = std::iter::once((account.email.clone(), account.name.clone()))
            .chain(account.identities.iter().map(|i| (i.email.clone(), if i.name.is_empty() { account.name.clone() } else { i.name.clone() })))
            .collect();
        let (email, name) = own
            .iter()
            .find(|(e, _)| invite.attendees.iter().any(|a| a.address.email.eq_ignore_ascii_case(e)))
            .cloned()
            .unwrap_or_else(|| own[0].clone());
        let me = Address { name: Some(name.clone()).filter(|n| !n.is_empty()), email: email.clone() };
        let reply = crate::calendar::reply(ics, &me, partstat, crate::store::now())
            .ok_or_else(|| Error::Other(crate::tr!("invitation illisible", "unreadable invitation")))?;
        let who = if name.is_empty() { email.clone() } else { name };
        let draft = Draft {
            account_id: account.id,
            to: vec![crate::store::format_address(&organizer)],
            subject: crate::tr!("{verb} : {}", "{verb}: {}", invite.summary),
            body: crate::tr!("{who} {status} l'invitation « {} ».", "{who} {status_en} the invitation “{}”.", invite.summary),
            in_reply_to,
            from: (!email.eq_ignore_ascii_case(&account.email)).then_some(email),
            calendar: Some(reply),
            ..Default::default()
        };
        // A retry after an uncertain send of the same answer reuses the Message-ID: the Sent copy is
        // not filed twice and the organizer's server can drop the duplicate. Any other answer, or the
        // same one given again later, gets a new one so it is not taken for a copy of a changed answer.
        let pending_key = format!("rsvp-pending:{}", invite.uid);
        let message_id = self
            .store
            .setting(&pending_key)?
            .and_then(|v| v.strip_prefix(&format!("{partstat} ")).map(str::to_string))
            .unwrap_or_else(|| new_message_id(&account.email));
        match self.deliver_as(draft, None, Some(message_id.clone())).await {
            Ok(()) => {}
            Err(Error::Uncertain(text)) => {
                self.store.set_setting(&pending_key, &format!("{partstat} {message_id}"))?;
                return Err(Error::Uncertain(crate::tr!("{text} ; {}", "{text}; {}", smtp::maybe_sent_advice(&account.smtp.host))));
            }
            Err(e) => return Err(e),
        }
        if self.store.setting(&pending_key)?.is_some_and(|v| !v.is_empty()) {
            self.store.set_setting(&pending_key, "")?;
        }
        self.store.set_setting(&format!("rsvp:{}", invite.uid), partstat)?;
        Ok(())
    }

    pub async fn invite_ics(&self, id: i64) -> Result<String> {
        parse_body(self.raw(id).await?).await?.calendar.ok_or_else(|| Error::Other(crate::tr!("ce message ne contient pas d'invitation", "this message contains no invitation")))
    }

    pub async fn message_source(&self, id: i64) -> Result<Vec<u8>> {
        self.raw(id).await
    }

    /// (file name, MIME type, content)
    pub async fn attachment(&self, id: i64, index: usize) -> Result<(String, String, Vec<u8>)> {
        // Checked, then shown or saved: the same attachment asked again at once is not decoded
        // from the whole message a second time.
        if let Some((key, at, kept)) = self.last_attachment.lock().unwrap().as_ref()
            && *key == (id, index)
            && at.elapsed() < Duration::from_secs(60)
        {
            return Ok((kept.0.clone(), kept.1.clone(), kept.2.clone()));
        }
        let raw = self.raw(id).await?;
        let found = blocking(move || mime::attachment(&raw, index)).await?;
        if found.2.len() <= 32 << 20 {
            *self.last_attachment.lock().unwrap() = Some(((id, index), Instant::now(), Arc::new(found.clone())));
        }
        Ok(found)
    }

    pub async fn inspect_attachment(&self, id: i64, index: usize) -> Result<crate::attachments::AttachmentReport> {
        let (filename, declared, data) = self.attachment(id, index).await?;
        let trust = self.trust(id, None).ok().map(|t| t.level);
        Ok(crate::attachments::inspect(&filename, &declared, &data, trust))
    }

    /// Refused for anything but an image, a PDF or text; the type is sniffed from the content.
    pub async fn attachment_view(&self, id: i64, index: usize) -> Result<(&'static str, Vec<u8>)> {
        let (filename, _, data) = self.attachment(id, index).await?;
        let mime = crate::attachments::view_mime(&filename, &data)
            .ok_or_else(|| Error::Other(crate::tr!("cette pièce jointe ne peut pas être affichée dans facteur", "this attachment cannot be displayed in facteur")))?;
        Ok((mime, data))
    }

    fn enqueue(&self, account_id: i64, op: &Op, label: &str) -> Result<i64> {
        self.store.push_op(account_id, &serde_json::to_string(op)?, label, None)
    }

    fn enqueue_undoable(&self, account_id: i64, op: &Op, label: &str, undo: &mut Undo) -> Result<()> {
        let text = serde_json::to_string(op)?;
        let id = self.store.push_op(account_id, &text, label, Some(crate::store::now() + UNDO_SECS))?;
        undo.ops.push((account_id, id));
        undo.op_texts.push(text);
        Ok(())
    }

    /// The undo's operations go to the server at the end of the window.
    fn keep_undo(self: &Arc<Self>, undo: Undo) -> Option<u64> {
        if undo.ops.is_empty() {
            return None;
        }
        let token = self.next_undo.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut accounts: Vec<i64> = undo.ops.iter().map(|(a, _)| *a).collect();
        accounts.sort_unstable();
        accounts.dedup();
        self.undos.lock().unwrap().insert(token, undo);
        let this = self.clone();
        self.spawn(async move {
            // Not a second more: once replayed, the operations' ids are free, and a new operation
            // taking one would be what an undo deletes.
            tokio::time::sleep(Duration::from_secs(UNDO_SECS as u64)).await;
            this.undos.lock().unwrap().remove(&token);
            for account_id in accounts {
                this.request_flush(account_id, &[]);
            }
        });
        Some(token)
    }

    /// Possible while the action's operations have not left the queue.
    pub async fn undo(self: &Arc<Self>, token: u64) -> Result<()> {
        let too_late = || Error::Other(crate::tr!("trop tard : l'action est déjà faite sur le serveur", "too late: the action has already been done on the server"));
        let undo = self.undos.lock().unwrap().remove(&token).ok_or_else(too_late)?;
        let mut accounts: Vec<i64> = undo.ops.iter().map(|(a, _)| *a).collect();
        accounts.sort_unstable();
        accounts.dedup();
        let ids: Vec<(i64, String)> = undo.ops.iter().map(|(_, id)| *id).zip(undo.op_texts.iter().cloned()).collect();
        // No replay may take these operations while they are removed. While they are all still held
        // (well before the end of their undo window), no replay takes them, so no need to wait behind
        // one (or behind its connection attempts).
        let now = crate::store::now();
        let mut still_held = true;
        for account_id in &accounts {
            let pending = self.store.pending_ops(*account_id)?;
            still_held &= undo
                .ops
                .iter()
                .filter(|(a, _)| a == account_id)
                .all(|(_, id)| pending.iter().any(|p| p.id == *id && p.not_before.is_some_and(|t| t > now + 1)));
        }
        let locks: Vec<_> = accounts.iter().map(|a| self.flush_lock(*a)).collect();
        let mut held = Vec::new();
        if !still_held {
            for lock in &locks {
                held.push(lock.lock().await);
            }
        }
        // Checked again inside the store's transaction: a settle may release them meanwhile, and a
        // replay then take them. That case is undone again behind the replays.
        let mut outcome = self.store.undo(&ids, &undo.moves, &undo.labels, &undo.snoozes, still_held)?;
        if outcome == crate::store::Undone::Released {
            for lock in &locks {
                held.push(lock.lock().await);
            }
            outcome = self.store.undo(&ids, &undo.moves, &undo.labels, &undo.snoozes, false)?;
        }
        if outcome != crate::store::Undone::Done {
            return Err(too_late());
        }
        for (account_id, pattern, before) in &undo.screening {
            self.store.set_screening(*account_id, pattern, before.as_deref())?;
        }
        for snapshot in &undo.categories {
            self.store.restore_sender_category(snapshot)?;
        }
        drop(held);
        for account_id in accounts {
            self.emit(Event::Changed { account_id });
        }
        self.emit(Event::OpsChanged);
        Ok(())
    }

    fn label(&self, action: &str, message_id: i64) -> String {
        match self.store.summary(message_id) {
            Ok(m) if !m.subject.is_empty() => format!("{action} · {}", m.subject),
            _ => action.to_string(),
        }
    }

    pub fn ops_status(&self) -> Result<OpsStatus> {
        let counts = self.store.ops_counts()?;
        Ok(OpsStatus {
            pending: counts.iter().map(|c| c.1).sum(),
            failed: self.store.failed_ops()?,
        })
    }

    pub fn ops_details(&self) -> Result<OpsDetails> {
        let now = crate::store::now();
        let mut ops = Vec::new();
        // Replay order per account: a held operation holds back what follows, except flag changes.
        let mut held: HashMap<i64, i64> = HashMap::new();
        for mut op in self.store.op_rows()? {
            op.mailbox = op.mailbox.map(|m| crate::utf7::decode(&m));
            op.dest = op.dest.map(|m| crate::utf7::decode(&m));
            let flags_only = matches!(op.kind.as_str(), "store" | "store_set" | "mark_all_seen");
            let (state, blocked_by) = if op.failed {
                (OpState::Failed, None)
            } else if op.not_before.is_some_and(|t| t > now) {
                held.entry(op.account_id).or_insert(op.id);
                (OpState::Held, None)
            } else if let Some(&first) = held.get(&op.account_id).filter(|_| !flags_only) {
                (OpState::Blocked, Some(first))
            } else if op.attempts > 0 {
                (OpState::Retrying, None)
            } else {
                (OpState::Waiting, None)
            };
            ops.push(OpView { op, state, blocked_by });
        }
        Ok(OpsDetails { ops, errors: self.queue_errors.lock().unwrap().clone() })
    }

    fn note_queue_error(&self, account_id: i64, error: Option<String>) {
        let mut errors = self.queue_errors.lock().unwrap();
        match error {
            Some(e) => errors.insert(account_id, e),
            None => errors.remove(&account_id),
        };
    }

    pub async fn retry_op(self: &Arc<Self>, id: i64) -> Result<()> {
        let account_id = self.store.retry_op(id)?;
        let op = self.store.op_payload(id)?.and_then(|p| serde_json::from_str::<Op>(&p).ok());
        let res = self.flush(account_id).await.map(|_| ());
        // Gone through: once refused, its messages were shown back where they were, so the folders it
        // touched are read again in full.
        if let Some(op) = op
            && self.store.op_row(id)?.is_none()
        {
            crate::ops::recheck_folders(&self.store, account_id, &op);
            let states = self.store.mailbox_states(account_id)?;
            let ids = op.folders().iter().filter_map(|name| states.iter().find(|s| s.name == *name).map(|s| s.id)).collect();
            self.flush_then_refresh(account_id, ids);
        }
        self.emit(Event::OpsChanged);
        res
    }

    /// The next sync realigns the cache with the server.
    pub fn discard_op(&self, id: i64) -> Result<()> {
        let row = self.store.op_row(id)?;
        let op = self.store.op_payload(id)?.and_then(|p| serde_json::from_str::<Op>(&p).ok());
        self.store.finish_op(id)?;
        // What it hid (moved or deleted locally) shows again at the next sync, in full.
        if let (Some(row), Some(op)) = (row, op) {
            crate::ops::recheck_folders(&self.store, row.account_id, &op);
        }
        self.emit(Event::OpsChanged);
        Ok(())
    }

    /// Noted in the operations that target UIDs.
    fn uidvalidity(&self, mailbox_id: i64) -> Option<u32> {
        self.store.mailbox_state(mailbox_id).ok().and_then(|s| s.uidvalidity)
    }

    pub fn set_flag(self: &Arc<Self>, ids: &[i64], flag: FlagKind, value: bool) -> Result<()> {
        let mut accounts = Vec::new();
        // One command per folder on the server: (folder, name, account, UIDs, first message).
        let mut batches: Vec<(i64, String, i64, Vec<u32>, i64)> = Vec::new();
        let locations = self.store.locations(ids)?;
        let mut by_account: HashMap<i64, Vec<i64>> = HashMap::new();
        for (id, loc) in &locations {
            by_account.entry(loc.account_id).or_default().push(*id);
        }
        let all: Vec<i64> = locations.iter().map(|(id, _)| *id).collect();
        self.store.set_flags_local(&all, flag, value)?;
        for (id, loc) in locations {
            if let Some(uid) = loc.uid {
                match batches.iter_mut().find(|b| b.0 == loc.mailbox_id) {
                    Some(b) => b.3.push(uid),
                    None => batches.push((loc.mailbox_id, loc.mailbox.clone(), loc.account_id, vec![uid], id)),
                }
            }
            accounts.push(loc.account_id);
        }
        let action = || match (flag, value) {
            (FlagKind::Seen, true) => crate::tr!("Marquer comme lu", "Mark as read"),
            (FlagKind::Seen, false) => crate::tr!("Marquer comme non lu", "Mark as unread"),
            (FlagKind::Flagged, true) => crate::tr!("Épingler", "Pin"),
            (FlagKind::Flagged, false) => crate::tr!("Désépingler", "Unpin"),
        };
        for (mailbox_id, mailbox, account_id, uids, first) in batches {
            let label = crate::i18n::both(|| match uids.len() {
                1 => self.label(&action(), first),
                n => format!("{} · {n} messages", action()),
            });
            let op = Op::StoreSet { mailbox, uids, flag: flag.imap().into(), value, uidvalidity: self.uidvalidity(mailbox_id) };
            self.enqueue(account_id, &op, &label)?;
        }
        if matches!(flag, FlagKind::Flagged) && !value {
            for account_id in by_account.keys() {
                self.store.drop_unpinned(*account_id)?;
            }
        }
        self.after_local_change(accounts);
        Ok(())
    }

    /// One server command per source folder. Returns the undo handle. `from`: the folder the user
    /// moves them from (Gmail: the label they leave).
    pub fn move_to(self: &Arc<Self>, ids: &[i64], dest_mailbox_id: i64, from: Option<i64>) -> Result<Option<u64>> {
        let mut undo = Undo::default();
        self.move_from(ids, dest_mailbox_id, from, Some(&mut undo))?;
        Ok(self.keep_undo(undo))
    }

    /// `undo`: a user's action, undoable; without: a move made by facteur (rule, snooze return),
    /// sent at once.
    fn move_inner(self: &Arc<Self>, ids: &[i64], dest_mailbox_id: i64, undo: Option<&mut Undo>) -> Result<()> {
        self.move_from(ids, dest_mailbox_id, None, undo)
    }

    fn move_from(self: &Arc<Self>, ids: &[i64], dest_mailbox_id: i64, from: Option<i64>, mut undo: Option<&mut Undo>) -> Result<()> {
        let dest = self.store.mailbox_state(dest_mailbox_id)?;
        // Check everything before touching anything: a refusal midway would leave messages moved
        // locally with no operation for the server.
        let mut plan = Vec::with_capacity(ids.len());
        let locations = self.store.locations(ids)?;
        if locations.len() != ids.iter().collect::<HashSet<_>>().len() {
            return Err(Error::NotFound(crate::tr!("message disparu entre-temps", "message no longer exists")));
        }
        // With Gmail's labels, a message already in the destination (All Mail, when archiving) still
        // has labels to lose: the plan decides.
        let labels_model = self.store.labels_model(dest.account_id)?;
        for (id, loc) in locations {
            if loc.mailbox_id == dest_mailbox_id && !labels_model {
                continue;
            }
            if loc.account_id != dest.account_id {
                return Err(Error::Other(crate::tr!("déplacement entre comptes non pris en charge", "moving between accounts is not supported")));
            }
            let Some(uid) = loc.uid else {
                return Err(in_flight());
            };
            plan.push((id, loc, uid));
        }
        if labels_model {
            return self.file_labelled(plan, &dest, from, undo);
        }
        // Gmail before its first sync turned folders into labels: its folders are not folders.
        if self.store.provider(dest.account_id)? == crate::provider::Provider::Gmail {
            return Err(Error::Other(crate::tr!(
                "compte en cours de préparation, réessayez après la synchronisation",
                "account still being set up, try again after the sync"
            )));
        }
        let items: Vec<crate::provider::Moving> = plan
            .into_iter()
            .map(|(id, loc, uid)| crate::provider::Moving { id, mailbox_id: loc.mailbox_id, mailbox: loc.mailbox, uid })
            .collect();
        let plan = crate::provider::plan_move(&items, &dest, |m| self.uidvalidity(m));
        self.store.move_many_local(&plan.moved, dest_mailbox_id)?;
        if let Some(undo) = undo.as_deref_mut() {
            let moved: HashSet<i64> = plan.moved.iter().copied().collect();
            undo.moves.extend(items.iter().filter(|m| moved.contains(&m.id)).map(|m| (m.id, m.mailbox_id, Some(m.uid))));
        }
        let target = crate::utf7::decode(&dest.name);
        for command in plan.commands {
            let label = crate::i18n::both(|| match command.count() {
                1 => self.label(&crate::tr!("Déplacer vers {target}", "Move to {target}"), command.first),
                n => crate::tr!("Déplacer {n} messages vers {target}", "Move {n} messages to {target}"),
            });
            match undo.as_deref_mut() {
                Some(undo) => self.enqueue_undoable(dest.account_id, &command.op, &label, undo)?,
                None => {
                    self.enqueue(dest.account_id, &command.op, &label)?;
                }
            }
        }
        if !ids.is_empty() {
            self.after_local_change(vec![dest.account_id]);
        }
        Ok(())
    }

    /// [`move_from`] for Gmail converted to labels: labels change, and only a move into or out of
    /// the bin or spam changes a message's own folder.
    fn file_labelled(
        self: &Arc<Self>,
        located: Vec<(i64, crate::store::Location, u32)>,
        dest: &crate::store::MailboxState,
        from: Option<i64>,
        mut undo: Option<&mut Undo>,
    ) -> Result<()> {
        let Some(all_mail) = self.store.mailbox_by_role(dest.account_id, Role::All)? else {
            return Err(Error::Other(crate::tr!("dossier « Tous les messages » introuvable", "All Mail folder not found")));
        };
        // A folder created here since the last listing (a new folder, the snooze folder) is a label
        // too: marked now, so that its messages show there at once.
        let mut dest = dest.clone();
        if !dest.is_label && !matches!(dest.role, Some(Role::All | Role::Trash | Role::Junk)) {
            self.store.set_label_folder(dest.id, true)?;
            dest.is_label = true;
        }
        let dest = &dest;
        let ids: Vec<i64> = located.iter().map(|(id, ..)| *id).collect();
        let mut labels = self.store.message_labels(&ids)?;
        let mut roles: HashMap<i64, Option<Role>> = HashMap::new();
        let mut items = Vec::with_capacity(located.len());
        for (id, loc, uid) in located {
            let role = match roles.get(&loc.mailbox_id) {
                Some(r) => *r,
                None => {
                    let r = self.store.mailbox_state(loc.mailbox_id)?.role;
                    roles.insert(loc.mailbox_id, r);
                    r
                }
            };
            let labels = labels.remove(&id).unwrap_or_default();
            items.push(crate::provider::Labelled { id, handle: loc.mailbox_id, handle_name: loc.mailbox, handle_role: role, uid, labels });
        }
        let folders = self.store.label_folders(dest.account_id)?;
        let plan = crate::provider::plan_labels(&items, dest, from, &folders, &all_mail, |m| self.uidvalidity(m));
        if let Some(undo) = undo.as_deref_mut() {
            let before: HashMap<i64, &crate::provider::Labelled> = items.iter().map(|i| (i.id, i)).collect();
            undo.labels.extend(plan.labels.iter().filter_map(|(id, _)| before.get(id).map(|i| (*id, i.labels.clone()))));
            undo.moves.extend(plan.rehome.iter().filter_map(|(id, _)| before.get(id).map(|i| (*id, i.handle, Some(i.uid)))));
        }
        for (home, group) in group_by_home(&plan.rehome) {
            self.store.move_many_local(&group, home)?;
        }
        self.store.relabel(&plan.labels)?;
        let target = crate::utf7::decode(&dest.name);
        for command in plan.commands {
            let label = crate::i18n::both(|| match command.count() {
                1 => self.label(&crate::tr!("Déplacer vers {target}", "Move to {target}"), command.first),
                n => crate::tr!("Déplacer {n} messages vers {target}", "Move {n} messages to {target}"),
            });
            match undo.as_deref_mut() {
                Some(undo) => self.enqueue_undoable(dest.account_id, &command.op, &label, undo)?,
                None => {
                    self.enqueue(dest.account_id, &command.op, &label)?;
                }
            }
        }
        if !ids.is_empty() {
            self.after_local_change(vec![dest.account_id]);
        }
        Ok(())
    }

    /// Gmail user labels (see [`crate::provider::user_label`]); the messages stay where they are.
    pub fn set_labels(self: &Arc<Self>, ids: &[i64], add: &[i64], remove: &[i64]) -> Result<Option<u64>> {
        let mut undo = Undo::default();
        let mut touched = Vec::new();
        // Everything planned before anything changes: a refusal halfway would leave labels changed
        // here without their command for the server.
        let mut plans = Vec::new();
        for (account_id, ids) in self.by_account(ids)? {
            if !self.store.labels_model(account_id)? {
                return Err(Error::Other(crate::tr!("ce compte n'a pas de libellés", "this account has no labels")));
            }
            let mut labels = self.store.message_labels(&ids)?;
            let mut items = Vec::with_capacity(ids.len());
            for (id, loc) in self.located(&ids)? {
                let Some(uid) = loc.uid else {
                    return Err(in_flight());
                };
                let role = self.store.mailbox_state(loc.mailbox_id)?.role;
                let labels = labels.remove(&id).unwrap_or_default();
                items.push(crate::provider::Labelled { id, handle: loc.mailbox_id, handle_name: loc.mailbox, handle_role: role, uid, labels });
            }
            let folders = self.store.label_folders(account_id)?;
            let plan = crate::provider::plan_relabel(&items, add, remove, &folders, |m| self.uidvalidity(m));
            plans.push((account_id, items, folders, plan));
        }
        for (account_id, items, folders, plan) in plans {
            if plan.labels.is_empty() {
                continue;
            }
            let before: HashMap<i64, &crate::provider::Labelled> = items.iter().map(|i| (i.id, i)).collect();
            undo.labels.extend(plan.labels.iter().filter_map(|(id, _)| before.get(id).map(|i| (*id, i.labels.clone()))));
            self.store.relabel(&plan.labels)?;
            let names = |ids: &[i64]| {
                ids.iter().filter_map(|id| folders.iter().find(|f| f.id == *id)).map(|f| crate::utf7::decode(&f.name)).collect::<Vec<_>>().join(", ")
            };
            for command in plan.commands {
                let label = crate::i18n::both(|| match (command.count(), add.len(), remove.len()) {
                    (1, 1, 0) => self.label(&crate::tr!("Libellé {}", "Label {}", names(add)), command.first),
                    (1, _, 0) => self.label(&crate::tr!("Libellés {}", "Labels {}", names(add)), command.first),
                    (1, 0, 1) => self.label(&crate::tr!("Retirer le libellé {}", "Remove label {}", names(remove)), command.first),
                    (1, 0, _) => self.label(&crate::tr!("Retirer les libellés {}", "Remove labels {}", names(remove)), command.first),
                    (1, _, _) => self.label(&crate::tr!("Libellés modifiés", "Labels changed"), command.first),
                    (n, _, _) => crate::tr!("Libellés de {n} messages", "Labels of {n} messages"),
                });
                self.enqueue_undoable(account_id, &command.op, &label, &mut undo)?;
            }
            touched.push(account_id);
        }
        if !touched.is_empty() {
            self.after_local_change(touched);
        }
        Ok(self.keep_undo(undo))
    }

    pub fn message_labels(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<i64>>> {
        self.store.message_labels(ids)
    }

    fn role_mailbox(&self, account_id: i64, role: Role) -> Result<i64> {
        self.store
            .mailbox_by_role(account_id, role)?
            .map(|m| m.id)
            .ok_or_else(|| Error::NotFound(crate::tr!("aucun dossier « {} » sur ce compte", "no “{}” folder on this account", role.as_str())))
    }

    /// A selection in the unified inbox can mix accounts.
    fn by_account(&self, ids: &[i64]) -> Result<Vec<(i64, Vec<i64>)>> {
        let mut groups: Vec<(i64, Vec<i64>)> = Vec::new();
        for (id, loc) in self.located(ids)? {
            match groups.iter_mut().find(|(a, _)| *a == loc.account_id) {
                Some((_, list)) => list.push(id),
                None => groups.push((loc.account_id, vec![id])),
            }
        }
        Ok(groups)
    }

    fn located(&self, ids: &[i64]) -> Result<Vec<(i64, crate::store::Location)>> {
        let locations = self.store.locations(ids)?;
        if locations.len() != ids.iter().collect::<HashSet<_>>().len() {
            return Err(Error::NotFound(crate::tr!("message disparu entre-temps", "message no longer exists")));
        }
        Ok(locations)
    }

    /// Archive, else All Mail (Gmail). With neither, "Archive" is created like Spark and Thunderbird do.
    fn archive_mailbox(&self, account_id: i64) -> Result<i64> {
        self.special_mailbox(account_id, &[Role::Archive, Role::All], Role::Archive, "Archive")
    }

    /// Without any of `roles`, `leaf` is created (locally at once, on the server through the queue)
    /// beside the existing special folders ("INBOX.Sent" → "INBOX.Archive"), as Spark and Thunderbird do.
    fn special_mailbox(&self, account_id: i64, roles: &[Role], role: Role, leaf: &str) -> Result<i64> {
        for r in roles {
            if let Some(m) = self.store.mailbox_by_role(account_id, *r)? {
                return Ok(m.id);
            }
        }
        let boxes = self.store.list_mailboxes(account_id)?;
        // Gmail refuses folders created under "[Gmail]": they go to the root.
        let sibling = boxes
            .iter()
            .filter(|m| !m.name.starts_with("[Gmail]") && !m.name.starts_with("[Google Mail]"))
            .find(|m| matches!(m.role, Some(Role::Sent | Role::Trash | Role::Drafts)));
        let delimiter = boxes.iter().find_map(|m| m.delimiter.clone());
        let name = match (sibling, &delimiter) {
            (Some(m), Some(d)) if !d.is_empty() && m.name.contains(d.as_str()) => {
                format!("{}{d}{leaf}", &m.name[..m.name.rfind(d.as_str()).unwrap_or(0)])
            }
            _ => leaf.to_string(),
        };
        let id = self.store.upsert_mailbox(account_id, &name, delimiter.as_deref(), Some(role), true)?;
        let label = crate::i18n::both(|| crate::tr!("Créer le dossier {}", "Create folder {}", crate::utf7::decode(&name)));
        self.enqueue(account_id, &Op::Create { mailbox: name }, &label)?;
        Ok(id)
    }

    /// With Gmail and most hosts, this move also trains the server's spam filter.
    pub fn report_spam(self: &Arc<Self>, ids: &[i64]) -> Result<Option<u64>> {
        let mut undo = Undo::default();
        self.report_spam_inner(ids, Some(&mut undo))?;
        Ok(self.keep_undo(undo))
    }

    fn report_spam_inner(self: &Arc<Self>, ids: &[i64], mut undo: Option<&mut Undo>) -> Result<()> {
        let plan = self
            .by_account(ids)?
            .into_iter()
            .map(|(account_id, ids)| Ok((ids, self.special_mailbox(account_id, &[Role::Junk], Role::Junk, "Junk")?)))
            .collect::<Result<Vec<_>>>()?;
        for (ids, dest) in plan {
            self.move_inner(&ids, dest, undo.as_deref_mut())?;
        }
        Ok(())
    }

    pub fn to_inbox(self: &Arc<Self>, ids: &[i64]) -> Result<Option<u64>> {
        self.not_spam(ids)
    }

    pub fn not_spam(self: &Arc<Self>, ids: &[i64]) -> Result<Option<u64>> {
        let mut undo = Undo::default();
        for (account_id, ids) in self.by_account(ids)? {
            let inbox = self.role_mailbox(account_id, Role::Inbox)?;
            self.move_inner(&ids, inbox, Some(&mut undo))?;
        }
        Ok(self.keep_undo(undo))
    }

    /// Its received and future messages go to Junk.
    pub fn block_sender(self: &Arc<Self>, email: &str) -> Result<()> {
        self.store.block_sender(email)?;
        for account in self.store.list_accounts()? {
            self.apply_blocklist(account.id)?;
        }
        Ok(())
    }

    pub fn unblock_sender(&self, email: &str) -> Result<()> {
        self.store.unblock_sender(email)
    }

    pub fn blocked_senders(&self) -> Result<Vec<String>> {
        self.store.blocked_senders()
    }

    fn apply_blocklist(self: &Arc<Self>, account_id: i64) -> Result<Vec<i64>> {
        let mut ids = self.store.inbox_from_blocked(account_id)?;
        // Already moved once and refused for good by the server: moving them again at every sync would
        // only be refused again. They stay until the user decides.
        if !ids.is_empty() {
            let stuck = self.store.failed_op_ids(&ids)?;
            ids.retain(|id| !stuck.contains(id));
        }
        if !ids.is_empty() {
            self.report_spam_inner(&ids, None)?;
        }
        Ok(ids)
    }

    /// One-click request if the sender offers it, else a message to the given address, else a web page.
    pub async fn unsubscribe(self: &Arc<Self>, id: i64, confirmed: bool) -> Result<Unsubscribed> {
        let raw = self.raw(id).await?;
        let parsed = parse_body(raw).await?;
        let (signals, context) = self.trust_parts(id, Some(&parsed.headers.signals))?;
        if crate::trust::evaluate(&signals, &context).is_low() {
            return Err(Error::Other(crate::tr!(
                "message peu fiable : se désabonner confirmerait votre adresse à l'expéditeur. Signalez-le plutôt comme indésirable",
                "untrustworthy message: unsubscribing would confirm your address to the sender. Report it as junk instead"
            )));
        }
        let unsub = parsed.unsubscribe.ok_or_else(|| Error::Other(crate::tr!("ce message ne propose pas de désabonnement", "this message offers no way to unsubscribe")))?;
        // The one-click POST rests on a DKIM verdict (RFC 8058): only the account's usual server's
        // counts, the sender could have written any other.
        if let Some(url) = unsub.one_click.as_ref().filter(|_| context.foreign_results.is_none()) {
            match crate::http::post_one_click(url).await {
                Ok(()) => return Ok(Unsubscribed::OneClick),
                Err(e) => tracing::info!("one-click unsubscribe refused ({e}), trying another way"),
            }
        }
        if let Some(mailto) = &unsub.mailto {
            let (address, subject, body) = parse_mailto(mailto);
            if !confirmed {
                return Ok(Unsubscribed::Confirm { address, subject: subject.unwrap_or_else(|| "unsubscribe".into()) });
            }
            let draft = Draft {
                account_id: self.store.location(id)?.account_id,
                to: vec![address.clone()],
                subject: subject.unwrap_or_else(|| "unsubscribe".into()),
                body: body.unwrap_or_else(|| "unsubscribe".into()),
                ..Default::default()
            };
            // No undo delay: the user already confirmed.
            self.deliver(draft, None).await?;
            return Ok(Unsubscribed::Mailto { address });
        }
        match unsub.web {
            Some(url) => Ok(Unsubscribed::Web { url }),
            None => Err(Error::Other(crate::tr!("aucun moyen de désabonnement utilisable", "no usable way to unsubscribe"))),
        }
    }

    /// `text`: what the link displays.
    pub async fn inspect_link(&self, message_id: i64, href: &str, text: &str) -> Result<crate::links::LinkReport> {
        use crate::links::{AnnouncedDomain, LinkContext, LinkReport, analyze, evaluate};
        let facts = analyze(href, text).ok_or_else(|| Error::Other(crate::tr!("lien illisible", "unreadable link")))?;
        let summary = self.store.summary(message_id)?;
        let sender_host = summary.from.as_ref().and_then(|a| a.email.rsplit_once('@')).map(|(_, host)| host.to_ascii_lowercase());
        let sender_domain = sender_host.as_deref().is_some_and(|host| crate::autoconfig::registrable(host) == facts.domain_ascii);
        let (seen_messages, seen_senders, wrote_to) = self.store.link_domain_context(&facts.domain_ascii, message_id)?;
        // The text announces another domain (a click tracker's included): where the user expects to
        // land is looked at too.
        let announced = facts
            .shown_host
            .as_deref()
            .filter(|_| facts.deceptive)
            .and_then(|shown| idna::domain_to_ascii(shown).ok())
            .map(|ascii| crate::autoconfig::registrable(&ascii))
            .filter(|ascii| ascii.contains('.') && *ascii != facts.domain_ascii);
        let link_age = async { if facts.ip { None } else { self.domain_registered(&facts.domain_ascii).await } };
        let announced_age = async {
            match &announced {
                Some(ascii) => self.domain_registered(ascii).await,
                None => None,
            }
        };
        let parts = self.trust_parts(message_id, None).ok();
        // Asking the sender's domain tells it the mailbox is read: not for an untrusted message, in
        // junk or from a blocked sender (as for logos).
        let may_ask = parts.as_ref().is_some_and(|(signals, context)| {
            !crate::trust::evaluate(signals, context).is_low() && !context.blocked && !context.in_junk
        });
        let records = async {
            match &sender_host {
                Some(host) if may_ask => Some(self.sender_records(host).await),
                _ => None,
            }
        };
        let (registered, announced_registered, records) = tokio::join!(link_age, announced_age, records);
        let sender_auth = match (&sender_host, records, &parts) {
            (Some(host), Some((dmarc, spf)), Some((signals, _))) => Some(crate::links::SenderAuth {
                domain: idna::domain_to_unicode(host).0,
                dmarc,
                spf,
                dkim: crate::links::dkim_state(signals),
            })
            .filter(crate::links::SenderAuth::incomplete),
            _ => None,
        };
        // The From address proven, by the provider (DMARC, from the account's usual server) or by
        // facteur's own DKIM check: otherwise a forged From (a domain without a DMARC policy, sent
        // through a click tracker's mailing service) would vouch for the domain its text announces.
        let from_proven = parts.as_ref().is_some_and(|(signals, context)| {
            (signals.dmarc == Some(true) && context.foreign_results.is_none()) || signals.dkim_local.as_ref().is_some_and(|d| d.pass && d.aligned)
        });
        let announced = match announced {
            Some(ascii) => Some(AnnouncedDomain {
                domain: idna::domain_to_unicode(&ascii).0,
                registered: announced_registered,
                seen_messages: self.store.domain_seen(&ascii, message_id)?,
                sender_domain: from_proven
                    && sender_host
                        .as_deref()
                        .and_then(|host| idna::domain_to_ascii(host).ok())
                        .is_some_and(|host| crate::autoconfig::registrable(&host) == ascii),
            }),
            None => None,
        };
        let ctx = LinkContext {
            message_trust: parts.map(|(signals, context)| crate::trust::evaluate(&signals, &context).level),
            // Same rule for the link's own domain: a forged From does not vouch for it either.
            sender_domain: from_proven && sender_domain,
            sender_domain_unproven: (!from_proven && sender_domain).then(|| facts.domain.clone()),
            seen_messages,
            seen_senders,
            wrote_to,
            registered,
            announced,
            now: crate::store::now(),
        };
        let trust = evaluate(&facts, &ctx);
        Ok(LinkReport { facts, registered, seen_messages, announced: ctx.announced, sender_auth, trust })
    }

    /// Whether `host` publishes a DMARC policy (its own or its registrable domain's) and an SPF
    /// record. Kept for the session; a failed lookup is asked again next time.
    async fn sender_records(&self, host: &str) -> (Option<bool>, Option<bool>) {
        if let Some(known) = self.sender_records.lock().unwrap().get(host) {
            return *known;
        }
        let ascii = idna::domain_to_ascii(host).unwrap_or_else(|_| host.to_string());
        let org = crate::autoconfig::registrable(&ascii);
        let has = |records: Option<Vec<String>>, tag: &str| records.map(|r| r.iter().any(|t| t.trim_start().to_ascii_lowercase().starts_with(tag)));
        let policy = format!("_dmarc.{ascii}");
        let (dmarc, spf) = tokio::join!(crate::autoconfig::txt(&policy), crate::autoconfig::txt(&ascii));
        let mut dmarc = has(dmarc, "v=dmarc1");
        if dmarc == Some(false) && org != ascii {
            dmarc = has(crate::autoconfig::txt(&format!("_dmarc.{org}")).await, "v=dmarc1");
        }
        let found = (dmarc, has(spf, "v=spf1"));
        if found.0.is_some() && found.1.is_some() {
            let mut records = self.sender_records.lock().unwrap();
            if records.len() >= 2000 {
                records.clear();
            }
            records.insert(host.to_string(), found);
        }
        found
    }

    /// Kept in memory: one request per domain and session.
    async fn domain_registered(&self, domain: &str) -> Option<i64> {
        if let Some(known) = self.domain_ages.lock().unwrap().get(domain) {
            return *known;
        }
        match crate::http::domain_registered(domain).await {
            Ok(at) => {
                let mut ages = self.domain_ages.lock().unwrap();
                // A session of several days hovers over many links: bounded memory.
                if ages.len() >= 2000 {
                    ages.clear();
                }
                ages.insert(domain.to_string(), at);
                at
            }
            // Offline or registry unreachable: tried again at the next hover.
            Err(e) => {
                tracing::info!("registration date of {domain}: {e}");
                None
            }
        }
    }

    fn usual_authserv(&self, account_id: i64) -> Option<String> {
        // Not known yet: asked again soon (the first sync brings the evidence within minutes).
        if let Some((known, at)) = self.authservs.lock().unwrap().get(&account_id)
            && at.elapsed() < Duration::from_secs(if known.is_some() { 600 } else { 30 })
        {
            return known.clone();
        }
        let known = self.store.usual_authserv(account_id).ok().flatten();
        self.authservs.lock().unwrap().insert(account_id, (known.clone(), Instant::now()));
        known
    }

    /// `fresh`: signals just read from the full message; they replace the cached ones.
    pub fn trust(&self, id: i64, fresh: Option<&crate::trust::Signals>) -> Result<crate::trust::Trust> {
        let (signals, context) = self.trust_parts(id, fresh)?;
        Ok(crate::trust::evaluate(&signals, &context))
    }

    /// Checks in the background the DKIM signatures of just-arrived messages whose provider verdict
    /// is of no use. Keys are looked up at arrival, never on opening: a lookup then tells the sender
    /// nothing a delivery did not already. The verdict is stored for good (a key replaced later must
    /// not turn it into a failure). Keys are looked up only for messages received at `arrived_since`
    /// or later (INTERNALDATE, which a move keeps): a copy moved by another device is no arrival,
    /// and a lookup then would tell the sender when it was triaged.
    fn check_dkim(self: &Arc<Self>, ids: &[i64], arrived_since: i64) {
        let ids: Vec<i64> = {
            let mut tried = self.dkim_tried.lock().unwrap();
            if tried.len() >= 5000 {
                tried.clear();
            }
            ids.iter()
                .copied()
                .filter(|id| {
                    let fresh = tried.get(id).is_none_or(|at| at.elapsed() >= DKIM_RETRY);
                    if fresh {
                        tried.insert(*id, Instant::now());
                    }
                    fresh
                })
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            let arrived = crate::rt::offload(|| this.store.arrived_since(&ids, arrived_since)).unwrap_or_default();
            for id in ids {
                if !this.trust_parts(id, None).is_ok_and(|(signals, context)| crate::dkim::wanted(&signals, &context)) {
                    continue;
                }
                let Ok(summary) = this.store.summary(id) else { continue };
                let Ok(Some(raw)) = crate::rt::offload(|| this.store.body(id)) else { continue };
                let from = summary.from.as_ref().map(|a| a.email.as_str());
                let Some(verdict) = this.dkim_verdict(id, &raw, from, summary.date, arrived.contains(&id)).await else { continue };
                let res = this.trust_parts(id, None).and_then(|(mut signals, _)| {
                    signals.dkim_local = Some(verdict);
                    this.trust(id, Some(&signals))
                });
                match res {
                    Ok(trust) => this.emit(Event::TrustChanged { message_id: id, trust }),
                    // Deleted meanwhile, most likely.
                    Err(e) => tracing::info!("DKIM verdict of message {id} not kept: {e}"),
                }
            }
        });
    }

    /// `lookup`: keys not known yet may be looked up (a message that just arrived).
    async fn dkim_verdict(&self, id: i64, raw: &[u8], from: Option<&str>, sent_at: i64, lookup: bool) -> Option<crate::trust::DkimLocal> {
        let account = self.store.location(id).and_then(|loc| self.store.account(loc.account_id)).ok()?;
        let own: Vec<String> = std::iter::once(account.email).chain(account.identities.into_iter().map(|i| i.email)).collect();
        let domain = from.and_then(|f| f.rsplit_once('@')).map_or("", |(_, d)| d);
        self.dkim.verify(raw, domain, &own, sent_at, lookup).await
    }

    fn trust_parts(&self, id: i64, fresh: Option<&crate::trust::Signals>) -> Result<(crate::trust::Signals, crate::trust::Context)> {
        let crate::store::TrustInputs { account_id, from, role, signals: stored } = self.store.trust_inputs(id)?;
        let signals = match (fresh, stored) {
            (Some(f), stored) => {
                let mut f = f.clone();
                // facteur's own DKIM verdict is not in the headers: kept from the stored signals.
                if f.dkim_local.is_none() {
                    f.dkim_local = stored.as_ref().and_then(|s| s.dkim_local.clone());
                }
                if stored.as_ref() != Some(&f) {
                    // Takes the writer, which a sync may hold: off the async worker (a message being opened).
                    crate::rt::offload(|| self.store.set_signals(id, &f))?;
                }
                f
            }
            (None, Some(s)) => s,
            (None, None) => crate::trust::Signals::default(),
        };
        let sender = match &from {
            Some(email) => {
                let (wrote, seen) = self.store.sender_context(account_id, email, id)?;
                Some((wrote, seen, self.store.is_blocked(email)?))
            }
            None => None,
        };
        let context = self.trust_context(&signals, account_id, role, sender);
        Ok((signals, context))
    }

    /// Stored signals only, in one statement. Unreadable messages are left out.
    fn trust_parts_many(&self, ids: &[i64]) -> HashMap<i64, (crate::trust::Signals, crate::trust::Context)> {
        let inputs = match self.store.trust_inputs_many(ids) {
            Ok(inputs) => inputs,
            Err(e) => {
                tracing::warn!("trust of {} message(s): {e}", ids.len());
                return HashMap::new();
            }
        };
        inputs
            .into_iter()
            .map(|(id, (crate::store::TrustInputs { account_id, role, signals, .. }, sender))| {
                let signals = signals.unwrap_or_default();
                let context = self.trust_context(&signals, account_id, role, sender);
                (id, (signals, context))
            })
            .collect()
    }

    /// `sender`: (written to, messages seen before, blocked), `None` without a sender address.
    fn trust_context(&self, signals: &crate::trust::Signals, account_id: i64, role: Option<Role>, sender: Option<(bool, i64, bool)>) -> crate::trust::Context {
        // A verdict stamped by another server than the account's usual one is not taken on trust.
        let foreign_results = match (&signals.authserv, self.usual_authserv(account_id)) {
            (Some(seen), Some(usual)) if *seen != usual => Some(seen.clone()),
            // The account's usual server is not known yet (new account, few messages): a "pass" cannot be
            // told from one written by the sender itself. Failures still count.
            (Some(seen), None) => Some(seen.clone()),
            _ => None,
        };
        let mut context = crate::trust::Context { in_junk: role == Some(Role::Junk), foreign_results, ..Default::default() };
        if let Some((wrote, seen, blocked)) = sender {
            context.known_correspondent = wrote;
            context.seen_before = seen;
            context.blocked = blocked;
        }
        context
    }

    /// `None`: the brand has none. Addresses missing from the answer are being looked up:
    /// [`Event::LogosChanged`] says to ask again.
    pub fn sender_logos(self: &Arc<Self>, emails: &[String]) -> Result<HashMap<String, Option<String>>> {
        // An answer for every address, "no logo" included: the interface keeps it and does not ask again.
        if self.store.setting("sender_logos")?.as_deref() == Some("off") {
            return Ok(emails.iter().map(|e| (e.clone(), None)).collect());
        }
        // No request to the domain of an untrustworthy sender (spam, spoofing, blocked): it would
        // contact a toxic domain and tell it the mailbox is active.
        let mut refused = HashMap::new();
        // Public webmail (gmail…): no company logo to look for.
        let (candidates, public): (Vec<(&String, Option<String>)>, Vec<_>) =
            emails.iter().map(|e| (e, crate::avatars::logo_domain(e))).partition(|(_, d)| d.is_some());
        refused.extend(public.into_iter().map(|(e, _)| (e.clone(), None)));
        let wanted: Vec<String> = candidates.iter().map(|(e, _)| (*e).clone()).collect();
        let latest = self.store.latest_from_many(&wanted)?;
        let ids: Vec<i64> = latest.values().copied().collect();
        let parts = self.trust_parts_many(&ids);
        let by_email: Vec<(String, String)> = candidates
            .into_iter()
            .filter_map(|(e, domain)| {
                let domain = domain?;
                let trusted = latest.get(e).and_then(|id| parts.get(id)).is_some_and(|(signals, context)| {
                    let trust = crate::trust::evaluate(signals, context);
                    crate::avatars::logo_allowed(&trust, signals, context)
                });
                if !trusted {
                    refused.insert(e.clone(), None);
                    return None;
                }
                Some((e.clone(), domain))
            })
            .collect();
        let mut domains: Vec<String> = by_email.iter().map(|(_, d)| d.clone()).collect();
        domains.sort();
        domains.dedup();
        let known = self.store.sender_logos(&domains)?;
        let now = chrono::Utc::now().timestamp();
        // Checked again after 30 days; an absence after 7 (the site may have published one).
        let fresh = |(data, at): &(Option<String>, i64)| now - at < if data.is_some() { 30 } else { 7 } * 86_400;
        let missing: Vec<String> = domains.into_iter().filter(|d| !known.get(d).is_some_and(fresh)).collect();
        self.fetch_logos(missing);
        let mut out: HashMap<String, Option<String>> = by_email
            .into_iter()
            .filter_map(|(email, domain)| known.get(&domain).map(|(data, _)| (email, data.clone())))
            .collect();
        out.extend(refused);
        Ok(out)
    }

    fn fetch_logos(self: &Arc<Self>, domains: Vec<String>) {
        let todo: Vec<String> = {
            let mut in_flight = self.logos_in_flight.lock().unwrap();
            domains.into_iter().filter(|d| in_flight.insert(d.clone())).collect()
        };
        if todo.is_empty() {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            use futures::StreamExt;
            futures::stream::iter(todo)
                .for_each_concurrent(4, |domain| {
                    let this = this.clone();
                    async move {
                        let logo = crate::avatars::fetch_logo(&domain).await;
                        if let Err(e) = this.store.save_sender_logo(&domain, logo.as_deref()) {
                            tracing::warn!("logo of {domain}: {e}");
                        }
                        this.logos_in_flight.lock().unwrap().remove(&domain);
                    }
                })
                .await;
            this.emit(Event::LogosChanged);
        });
    }

    pub async fn similar_facets(&self, id: i64) -> Result<SimilarFacets> {
        let summary = self.store.summary(id)?;
        let parsed = parse_body(self.raw(id).await?).await?;
        let from = parsed.headers.from.clone();
        let from_domain = from
            .as_ref()
            .and_then(|a| a.email.rsplit_once('@'))
            .map(|(_, d)| crate::autoconfig::registrable(&d.to_ascii_lowercase()));
        Ok(SimilarFacets {
            account_id: summary.account_id,
            from,
            from_domain,
            to: parsed.headers.to.iter().chain(&parsed.headers.cc).cloned().collect(),
            subject_core: subject_core(&parsed.headers.subject),
            subject: parsed.headers.subject,
            link_domains: parsed.links.into_iter().map(|(domain, links)| LinkDomain { domain, links }).collect(),
            list_id: parsed.headers.list_id,
            bulk: parsed.headers.bulk,
        })
    }

    pub fn find_similar(&self, criteria: &SimilarCriteria) -> Result<SimilarResult> {
        if criteria.is_empty() {
            return Err(Error::Other(crate::tr!("choisissez au moins un critère", "choose at least one criterion")));
        }
        self.store.similar(criteria, 300)
    }

    /// Messages outside the synced window matching the subject or a searched domain; their bodies
    /// are downloaded so their link domains are known. Returns how many were added or completed.
    pub async fn similar_from_server(&self, criteria: &SimilarCriteria) -> Result<usize> {
        let mut terms: Vec<String> = criteria.link_domains.clone();
        terms.extend(criteria.subject.clone().filter(|s| !s.trim().is_empty()));
        terms.extend(criteria.body.clone().filter(|s| !s.trim().is_empty()));
        terms.extend(criteria.from_address.clone());
        terms.extend(criteria.from_domain.clone());
        let accounts: Vec<i64> = match criteria.account_id {
            Some(a) => vec![a],
            None => self.store.list_accounts()?.into_iter().map(|a| a.id).collect(),
        };
        let mut completed = 0;
        for account_id in accounts {
            let mut ids = Vec::new();
            for term in &terms {
                ids.extend(self.search_server(account_id, term, false).await?.into_iter().map(|m| m.id));
            }
            ids.sort_unstable();
            ids.dedup();
            // Missing bodies (300 per account at most): needed for the link domains.
            for id in ids.into_iter().filter(|id| !self.store.has_body(*id).unwrap_or(true)).take(300) {
                if self.raw(id).await.is_ok() {
                    completed += 1;
                }
            }
        }
        Ok(completed)
    }

    /// Analyses the bodies cached before link extraction existed.
    fn backfill_links(self: &Arc<Self>) {
        // Once done, never again: every body stored since writes its links, and the empty query scans
        // the whole cache (no index on link_domains).
        const DONE: &str = "links_backfilled";
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            if this.store.setting(DONE).ok().flatten().is_some() {
                return;
            }
            loop {
                let batch = match this.store.ids_without_links(200) {
                    Ok(b) if !b.is_empty() => b,
                    Ok(_) => {
                        if let Err(e) = this.store.set_setting(DONE, "1") {
                            tracing::warn!("link analysis: {e}");
                        }
                        break;
                    }
                    Err(_) => break,
                };
                for id in batch {
                    let raw = this.store.body(id).ok().flatten().unwrap_or_default();
                    let info = mime::body_info(&raw);
                    if let Err(e) = this.store.set_links(id, &info.link_domains, info.list_id.as_deref()) {
                        tracing::warn!("link analysis of message {id}: {e}");
                        return;
                    }
                }
            }
        });
    }

    pub fn trusted_images(&self) -> Result<Vec<String>> {
        self.store.trusted_images()
    }

    /// A domain or an address; "www.example.com" or "https://example.com/…" comes down to the domain.
    pub fn add_trusted_images(&self, entry: &str) -> Result<String> {
        let entry = entry.trim().to_ascii_lowercase();
        let key = if entry.contains('@') {
            crate::avatars::sender_key(&entry)
        } else {
            let host = entry.trim_start_matches("https://").trim_start_matches("http://");
            let host = host.split(['/', '?', '#', ':']).next().unwrap_or_default();
            let domain = crate::autoconfig::registrable(host);
            domain.contains('.').then_some(domain)
        }
        .ok_or_else(|| Error::Other(crate::tr!("« {entry} » n'est ni un domaine ni une adresse", "“{entry}” is neither a domain nor an address")))?;
        self.store.add_trusted_images(&key)?;
        Ok(key)
    }

    pub fn remove_trusted_images(&self, key: &str) -> Result<()> {
        self.store.remove_trusted_images(key)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        self.store.setting(key)
    }

    pub fn set_setting(self: &Arc<Self>, key: &str, value: &str) -> Result<()> {
        let relanguaged = key == "language" && self.store.setting(key).ok().flatten().as_deref() != Some(value);
        if key == "language" {
            crate::i18n::set(value);
        }
        self.store.set_setting(key, value)?;
        if relanguaged {
            self.restate();
        }
        // At once; turned off, the listening starts again at the scheduler's next round.
        if let Some(id) = key.strip_prefix("single_connection:").and_then(|id| id.parse().ok())
            && value == "1"
        {
            self.leave_one_connection(id);
        }
        Ok(())
    }

    /// The calendar and address book states that carry a message, written again in the language
    /// just chosen: checked again (they come from errors, written once in the language of the
    /// moment).
    fn restate(self: &Arc<Self>) {
        let calendars: Vec<i64> = self.calendar_status().unwrap_or_default().into_iter().filter(|s| s.message.is_some() && !s.ignored).map(|s| s.account_id).collect();
        let books: Vec<i64> = self.address_book_status().unwrap_or_default().into_iter().filter(|s| s.message.is_some()).map(|s| s.account_id).collect();
        if calendars.is_empty() && books.is_empty() {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            for id in calendars {
                let _ = this.sync_calendars(Some(id)).await;
            }
            for id in books {
                let _ = this.sync_address_books(Some(id), true).await;
            }
        });
    }

    pub fn archive(self: &Arc<Self>, ids: &[i64]) -> Result<Option<u64>> {
        // Every destination first: nothing moves if one is missing.
        let plan = self
            .by_account(ids)?
            .into_iter()
            .map(|(account_id, ids)| Ok((ids, self.archive_mailbox(account_id)?)))
            .collect::<Result<Vec<_>>>()?;
        let mut undo = Undo::default();
        for (ids, dest) in plan {
            self.move_inner(&ids, dest, Some(&mut undo))?;
        }
        Ok(self.keep_undo(undo))
    }

    /// One undoable action (used for "Screened out": the refused mail, kept whole here). `None`
    /// when the folder was empty.
    pub fn trash_folder(self: &Arc<Self>, mailbox_id: i64) -> Result<Option<u64>> {
        let ids = self.store.mailbox_message_ids(mailbox_id)?;
        if ids.is_empty() {
            return Ok(None);
        }
        self.trash(&ids, false)
    }

    /// Messages already in the bin are purged only with `purge`, asked from the bin itself: a
    /// message trashed a moment ago and still shown elsewhere, deleted again, stays in the bin.
    pub fn trash(self: &Arc<Self>, ids: &[i64], purge: bool) -> Result<Option<u64>> {
        let mut undo = Undo::default();
        // One query for the whole selection (it can hold thousands of messages).
        let mut groups: Vec<(i64, Vec<(i64, crate::store::Location)>)> = Vec::new();
        for (id, loc) in self.located(ids)? {
            match groups.iter_mut().find(|(a, _)| *a == loc.account_id) {
                Some((_, list)) => list.push((id, loc)),
                None => groups.push((loc.account_id, vec![(id, loc)])),
            }
        }
        // Every bin first: nothing moves if one is missing.
        let plan = groups
            .into_iter()
            .map(|(account_id, list)| Ok((account_id, list, self.role_mailbox(account_id, Role::Trash)?)))
            .collect::<Result<Vec<_>>>()?;
        // A message still on its way to the bin has no UID: purging it now would touch nothing on the
        // server, and it would come back.
        if purge && plan.iter().any(|(_, list, trash)| list.iter().any(|(_, l)| l.mailbox_id == *trash && l.uid.is_none())) {
            return Err(Error::Other(crate::tr!(
                "message encore en route vers la corbeille : réessayez dans quelques secondes",
                "message still on its way to the trash: try again in a few seconds"
            )));
        }
        for (account_id, list, trash) in plan {
            let (in_trash, elsewhere): (Vec<_>, Vec<_>) = list.into_iter().partition(|(_, l)| l.mailbox_id == trash);
            if purge && !in_trash.is_empty() {
                let uids: Vec<u32> = in_trash.iter().filter_map(|(_, l)| l.uid).collect();
                let mailbox = in_trash[0].1.mailbox.clone();
                self.store.delete_uids(trash, &uids)?;
                let label = crate::i18n::both(|| crate::tr!("Supprimer définitivement {} message(s)", "Permanently delete {} message(s)", uids.len()));
                self.enqueue(account_id, &Op::DeleteSet { mailbox, uids, uidvalidity: self.uidvalidity(trash) }, &label)?;
            }
            // The move reports the change and starts the replay itself.
            if elsewhere.is_empty() {
                self.after_local_change(vec![account_id]);
            } else {
                let ids: Vec<i64> = elsewhere.iter().map(|(id, _)| *id).collect();
                self.move_inner(&ids, trash, Some(&mut undo))?;
            }
        }
        Ok(self.keep_undo(undo))
    }

    fn after_local_change(self: &Arc<Self>, mut accounts: Vec<i64>) {
        accounts.sort_unstable();
        accounts.dedup();
        let now = Instant::now();
        for account_id in accounts {
            self.local_changes.lock().unwrap().insert(account_id, now);
            self.emit(Event::Changed { account_id });
            self.request_flush(account_id, &[]);
        }
    }

    /// Until `BURST_QUIET` after the latest local action, nothing when there was none lately.
    fn burst_wait(&self, account_id: i64) -> Option<Duration> {
        let last = *self.local_changes.lock().unwrap().get(&account_id)?;
        BURST_QUIET.checked_sub(last.elapsed()).filter(|d| !d.is_zero())
    }

    /// `refresh`: folders whose content the interface should see updated (Drafts, Sent). Requests
    /// made while one is waiting (for its connection, typically) join it: a burst of actions, or of
    /// draft saves while offline, does not pile up one connection attempt each.
    fn request_flush(self: &Arc<Self>, account_id: i64, refresh: &[i64]) {
        {
            let mut requests = self.flush_requests.lock().unwrap();
            let joined = requests.contains_key(&account_id);
            requests.entry(account_id).or_default().extend(refresh.iter().copied());
            if joined {
                return;
            }
        }
        let this = self.clone();
        self.spawn(async move {
            match this.flush_inner(account_id, true).await {
                Ok((_, refresh)) if !refresh.is_empty() => {
                    let refresh: Vec<i64> = refresh.into_iter().collect();
                    if let Err(e) = this.refresh_mailboxes(account_id, &refresh).await {
                        tracing::info!("folder refresh: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::info!("deferred replay (account {account_id}): {e}"),
            }
        });
    }

    /// From now on, a new action needs a new replay. Returns the folders to refresh afterwards.
    fn take_flush_request(&self, account_id: i64) -> HashSet<i64> {
        self.flush_requests.lock().unwrap().remove(&account_id).unwrap_or_default()
    }

    fn has_due_ops(&self, account_id: i64) -> Result<bool> {
        let now = crate::store::now();
        Ok(self.store.pending_ops(account_id)?.iter().any(|p| p.not_before.is_none_or(|t| t <= now)))
    }

    pub async fn flush(&self, account_id: i64) -> Result<usize> {
        self.flush_inner(account_id, false).await.map(|(n, _)| n)
    }

    /// `requested`: run for [`Engine::request_flush`], whose request it takes over once it is about
    /// to read the queue (anything queued later asks for another replay).
    async fn flush_inner(&self, account_id: i64, requested: bool) -> Result<(usize, HashSet<i64>)> {
        let mut refresh = HashSet::new();
        // A request left pending after an error would make every later action join it, and no replay
        // would start again: it is taken over before giving up.
        let due = self.has_due_ops(account_id).inspect_err(|_| {
            if requested {
                self.take_flush_request(account_id);
            }
        })?;
        if !due {
            if !requested {
                return Ok((0, refresh));
            }
            // Taken over first, then checked again: an operation queued in between is seen.
            refresh = self.take_flush_request(account_id);
            if !self.has_due_ops(account_id)? {
                return Ok((0, refresh));
            }
        }
        // The connection first, then the replay lock: undoing an action, or the sync, does not wait
        // behind a connection attempt (up to a minute when the server is unreachable).
        let mut guard = match self.session(account_id, Purpose::Queue).await {
            Ok(guard) => guard,
            Err(e) => {
                if requested {
                    self.take_flush_request(account_id);
                }
                self.note_queue_error(account_id, Some(e.to_string()));
                self.emit(Event::OpsChanged);
                return Err(e);
            }
        };
        let lock = self.flush_lock(account_id);
        let _held = lock.lock().await;
        if requested {
            refresh.extend(self.take_flush_request(account_id));
        }
        let conn = guard.as_mut().expect("session open");
        let res = async {
            let caps = conn.caps().await?;
            crate::ops::flush(&self.store, account_id, &mut conn.session, &caps).await
        }
        .await;
        if res.is_err() {
            *guard = None;
        }
        self.note_queue_error(account_id, res.as_ref().err().map(ToString::to_string));
        self.emit(Event::OpsChanged);
        res.map(|n| (n, refresh))
    }

    async fn resolve_forwarded(&self, draft: &Draft) -> Result<Draft> {
        let mut full = draft.clone();
        for f in std::mem::take(&mut full.forwarded) {
            let (filename, mime, data) = self.attachment(f.message_id, f.index).await?;
            full.attachments.push(OutgoingAttachment { filename, mime, data: B64.encode(data) });
        }
        Ok(full)
    }

    fn reply_headers(&self, draft: &Draft) -> (Option<String>, Vec<String>) {
        match (&draft.reply_ids, draft.in_reply_to) {
            (Some((irt, refs)), _) => (Some(irt.clone()), refs.clone()),
            // Original message gone from the cache: the reply goes without threading headers.
            (None, Some(id)) => self.store.message_ids(id).unwrap_or_default(),
            (None, None) => (None, Vec::new()),
        }
    }

    /// Created (locally, then on the server) if missing.
    fn drafts_mailbox(&self, account_id: i64) -> Result<crate::store::MailboxState> {
        if let Some(m) = self.store.mailbox_by_role(account_id, Role::Drafts)? {
            return Ok(m);
        }
        let id = self.special_mailbox(account_id, &[Role::Drafts], Role::Drafts, "Drafts")?;
        self.store.mailbox_state(id)
    }

    /// Through the queue, so it works offline. Returns its id, to pass to the next saves.
    pub async fn save_draft(self: &Arc<Self>, mut draft: Draft) -> Result<String> {
        // The same content as the last save, still queued or in Drafts (an edit undone, a repeated
        // autosave): nothing to upload again.
        let print = draft_print(&mut draft)?;
        let account_id = draft.account_id;
        let unchanged = draft.draft_id.as_ref().is_some_and(|id| self.draft_prints.lock().unwrap().get(&(account_id, id.clone())) == Some(&print));
        if unchanged
            && let Some(id) = &draft.draft_id
            && self.store.draft_present(account_id, id)?
        {
            return Ok(id.clone());
        }
        let full = self.resolve_forwarded(&draft).await?;
        // The MIME (attachments encoded: megabytes) is built off the async workers.
        let this = self.clone();
        let id = blocking(move || this.save_resolved_draft(full)).await?;
        self.draft_prints.lock().unwrap().insert((account_id, id.clone()), print);
        Ok(id)
    }

    fn save_resolved_draft(self: &Arc<Self>, mut draft: Draft) -> Result<String> {
        let account = self.store.account(draft.account_id)?;
        let id = draft.draft_id.get_or_insert_with(|| new_message_id(&account.email)).clone();
        let (irt, refs) = self.reply_headers(&draft);
        let message = smtp::build(&account, &draft, irt.as_deref(), &refs, true)?;
        let drafts = self.drafts_mailbox(account.id)?;
        // Only the last version counts: earlier ones still queued are useless.
        self.store.drop_pending_draft(account.id, &id)?;
        let op = Op::SaveDraft { mailbox: drafts.name.clone(), message_id: id.clone(), raw: B64.encode(message.formatted()) };
        let large = matches!(&op, Op::SaveDraft { raw, .. } if raw.len() > LARGE_DRAFT);
        self.enqueue(account.id, &op, &crate::i18n::both(|| crate::tr!("Enregistrer le brouillon · {}", "Save draft · {}", draft.subject)))?;
        self.drop_draft_source(&draft);
        if large {
            self.flush_draft_later(account.id, drafts.id);
        } else {
            self.flush_then_refresh(account.id, vec![drafts.id]);
        }
        Ok(id)
    }

    /// Each save replaces the one still queued, so saves in a row upload the whole message once.
    /// The save is queued on disk meanwhile, and any other replay of the account sends it too.
    fn flush_draft_later(self: &Arc<Self>, account_id: i64, drafts: i64) {
        if !self.draft_flushes.lock().unwrap().insert(account_id) {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            tokio::time::sleep(DRAFT_APPEND_DELAY).await;
            this.draft_flushes.lock().unwrap().remove(&account_id);
            this.request_flush(account_id, &[drafts]);
        });
    }

    /// Deletes the Drafts copy a draft was reopened from when it had no Message-ID (written by
    /// another client): the saved version, under a Message-ID of its own, would not replace it.
    /// Does nothing once it is gone, so every save and the send may call it.
    fn drop_draft_source(&self, draft: &Draft) {
        let Some(source) = draft.source else { return };
        let res = (|| -> Result<()> {
            let Ok(loc) = self.store.location(source) else { return Ok(()) };
            let in_drafts = self.store.mailbox_state(loc.mailbox_id)?.role == Some(Role::Drafts);
            // Still a Drafts copy without a Message-ID: a local id can be reused once its message is gone,
            // and nothing else must ever be deleted through this.
            if loc.account_id != draft.account_id || !in_drafts || self.store.message_ids(source)?.0.is_some() {
                return Ok(());
            }
            let Some(uid) = loc.uid else { return Ok(()) };
            self.store.delete_uids(loc.mailbox_id, &[uid])?;
            let op = Op::Delete { mailbox: loc.mailbox, uid, uidvalidity: self.uidvalidity(loc.mailbox_id) };
            self.enqueue(loc.account_id, &op, &crate::i18n::both(|| crate::tr!("Supprimer l'ancien brouillon", "Delete the old draft")))?;
            self.emit(Event::Changed { account_id: loc.account_id });
            Ok(())
        })();
        if let Err(e) = res {
            tracing::warn!("old copy of the draft left in Drafts: {e}");
        }
    }

    pub fn discard_draft(self: &Arc<Self>, account_id: i64, draft_id: &str) -> Result<()> {
        self.draft_prints.lock().unwrap().remove(&(account_id, draft_id.to_string()));
        let Some(drafts) = self.store.mailbox_by_role(account_id, Role::Drafts)? else { return Ok(()) };
        self.store.drop_pending_draft(account_id, draft_id)?;
        self.store.delete_by_message_id(drafts.id, draft_id)?;
        let op = Op::DeleteDraft { mailbox: drafts.name.clone(), message_id: draft_id.to_string() };
        self.enqueue(account_id, &op, &crate::i18n::both(|| crate::tr!("Supprimer le brouillon", "Delete draft")))?;
        self.emit(Event::Changed { account_id });
        self.flush_then_refresh(account_id, vec![drafts.id]);
        Ok(())
    }

    pub async fn open_draft(&self, id: i64) -> Result<Draft> {
        let account_id = self.store.location(id)?.account_id;
        let parts = parse_draft(self.raw(id).await?).await?;
        let in_reply_to = match &parts.in_reply_to {
            Some(mid) => self.store.find_by_message_id(account_id, mid)?,
            None => None,
        };
        // Threading headers as saved: the message replied to may have left the cache since.
        let reply_ids = parts.in_reply_to.clone().map(|irt| (irt, parts.references.clone()));
        // The account's own address is no alias: the composer knows no "account|its own address" sender.
        let account_email = self.store.account(account_id)?.email;
        let from = parts.from.filter(|f| !f.eq_ignore_ascii_case(&account_email));
        // Without a Message-ID, the next save cannot replace this copy: it is deleted then.
        let source = parts.message_id.is_none().then_some(id);
        Ok(Draft {
            account_id,
            to: parts.to,
            cc: parts.cc,
            bcc: parts.bcc,
            subject: parts.subject,
            body: parts.body,
            html: parts.html,
            in_reply_to,
            attachments: parts.attachments,
            forwarded: Vec::new(),
            draft_id: parts.message_id,
            from,
            calendar: None,
            reply_ids,
            source,
        })
    }

    fn flush_then_refresh(self: &Arc<Self>, account_id: i64, mailboxes: Vec<i64>) {
        self.request_flush(account_id, &mailboxes);
    }

    async fn refresh_mailboxes(self: &Arc<Self>, account_id: i64, mailboxes: &[i64]) -> Result<()> {
        let mut guard = self.session(account_id, Purpose::Background).await?;
        let conn = guard.as_mut().expect("session open");
        let caps = match conn.caps().await {
            Ok(caps) => caps,
            Err(e) => {
                *guard = None;
                return Err(e);
            }
        };
        let mut fresh_inbox = Vec::new();
        for &id in mailboxes {
            let Ok(state) = self.store.mailbox_state(id) else { continue };
            let inbox = state.role == Some(Role::Inbox);
            // A Gmail label (Drafts and Sent too) is synced through All Mail: what it brings is
            // every new message, and those labelled inbox are arrivals, whichever label asked.
            let label = state.is_label;
            match bounded(300, &crate::tr!("synchronisation du dossier", "folder sync"), sync::sync_mailbox(&self.store, account_id, &mut conn.session, &caps, state, &mut |_| {})).await {
                Ok((ids, _)) if label => fresh_inbox.extend(self.store.with_label_role(&ids, &[Role::Inbox])?),
                Ok((ids, _)) if inbox => fresh_inbox.extend(ids),
                Ok(_) => {}
                Err(e) => {
                    *guard = None;
                    return Err(e);
                }
            }
        }
        drop(guard);
        self.after_inbox_arrivals(account_id, &fresh_inbox, true);
        self.emit(Event::Changed { account_id });
        Ok(())
    }

    /// New mail told to the interface (notification, counters). With Gmail's labels, an arrival
    /// is shown from the inbox, not from All Mail.
    fn tell_new_mail(&self, account_id: i64, ids: &[i64], held: &HashSet<i64>) {
        let inbox = match self.store.labels_model(account_id) {
            Ok(true) => self.store.mailbox_by_role(account_id, Role::Inbox).ok().flatten().map(|m| m.id),
            _ => None,
        };
        let messages: Vec<NewMail> = ids
            .iter()
            .filter_map(|&id| {
                let m = self.store.summary(id).ok()?;
                Some(NewMail {
                    id,
                    thread_id: m.thread_id,
                    mailbox_id: inbox.unwrap_or(m.mailbox_id),
                    from: m.from,
                    subject: m.subject,
                    preview: m.preview,
                    // "People only": what the smart inbox does not file with people.
                    bulk: self.store.message_category(id).map_or(true, |c| c != Category::People),
                    trust: self.trust(id, None).map(|t| t.level).unwrap_or(crate::trust::Level::Medium),
                    held: held.contains(&id),
                })
            })
            .collect();
        if !messages.is_empty() {
            self.emit(Event::NewMail { account_id, messages });
        }
    }

    /// Messages that reached the inbox cache outside a regular sync (history, folder refresh):
    /// blocked senders, rules and the screener apply to them as after a sync. Told as new mail
    /// only when `arrivals` (a folder refresh catching what just came); a history fetch fills gaps
    /// with old mail. Arrivals held back for the screener (`Store::insert_headers_held`) go
    /// through it here too, then show.
    fn after_inbox_arrivals(self: &Arc<Self>, account_id: i64, ids: &[i64], arrivals: bool) {
        let waiting = self.store.unscreened(account_id).unwrap_or_default();
        if ids.is_empty() && waiting.is_empty() {
            return;
        }
        let blocked = self.apply_blocklist(account_id).unwrap_or_default();
        let fresh: Vec<i64> = ids.iter().copied().filter(|id| !blocked.contains(id)).collect();
        let ruled = self.apply_rules(account_id, &fresh, None).unwrap_or_else(|e| {
            tracing::warn!("rules of account {account_id}: {e}");
            Ruled::default()
        });
        // Only what a rule moved out escapes the screener: marked read, a stranger still waits.
        let mut left: Vec<i64> = fresh.into_iter().filter(|id| !ruled.moved.contains(id)).collect();
        let more: Vec<i64> = waiting.iter().copied().filter(|id| !left.contains(id) && !blocked.contains(id) && !ruled.moved.contains(id)).collect();
        left.extend(more);
        let (held, refused) = self.screen(account_id, &left).unwrap_or_else(|e| {
            tracing::warn!("screener of account {account_id}: {e}");
            Default::default()
        });
        if let Err(e) = self.store.clear_unscreened(&waiting) {
            tracing::warn!("screener of account {account_id}: {e}");
        }
        if arrivals {
            let told: Vec<i64> = ids.iter().copied().filter(|id| !blocked.contains(id) && !ruled.contains(id) && !refused.contains(id)).collect();
            self.tell_new_mail(account_id, &told, &held);
        }
    }

    pub async fn load_older(&self, mailbox_id: i64) -> Result<usize> {
        let state = self.store.mailbox_state(mailbox_id)?;
        let account_id = state.account_id;
        // A kept connection may have died while unused (servers drop quiet ones): once on a new one.
        let mut attempt = 0;
        let res = loop {
            attempt += 1;
            let mut guard = self.session(account_id, Purpose::Background).await?;
            let session = &mut guard.as_mut().expect("session open").session;
            let res = bounded(300, &crate::tr!("historique", "history"), sync::load_older(&self.store, session, state.clone(), sync::WINDOW / 2)).await;
            if res.is_err() {
                *guard = None;
            }
            match res {
                Err(e) if attempt == 1 && e.is_connection_drop() => {
                    tracing::info!("history of account {account_id}: {e}; trying again on a new connection");
                    self.note_cut(account_id, false);
                }
                res => break res,
            }
        };
        if matches!(res, Ok(n) if n > 0) {
            self.emit(Event::Changed { account_id });
        }
        res
    }

    /// Beyond the cached messages. `bins`: search the bin and junk too.
    pub async fn search_server(&self, account_id: i64, query: &str, bins: bool) -> Result<Vec<MessageSummary>> {
        let q = crate::search::parse(query);
        // Nothing asked: no folder listed whole.
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let states: Vec<_> = self.store.mailbox_states(account_id)?.into_iter().filter(|s| s.selectable).collect();
        let targets = match &q.folder {
            Some(folder) => {
                let ids = self.store.folders_matching(account_id, folder)?;
                states.into_iter().filter(|s| ids.contains(&s.id)).collect()
            }
            None => {
                let states: Vec<_> = states.into_iter().filter(|s| bins || !matches!(s.role, Some(Role::Trash | Role::Junk))).collect();
                // Gmail: All Mail holds everything except Trash and Spam, kept when bins are included (the
                // filter above dropped them otherwise).
                match states.iter().find(|s| s.role == Some(Role::All)) {
                    Some(all) => {
                        let mut targets = vec![all.clone()];
                        targets.extend(states.iter().filter(|s| matches!(s.role, Some(Role::Trash | Role::Junk))).cloned());
                        targets
                    }
                    None => states,
                }
            }
        };
        let mut guard = self.session(account_id, Purpose::Background).await?;
        let conn = guard.as_mut().expect("session open");
        let caps = match conn.caps().await {
            Ok(caps) => caps,
            Err(e) => {
                *guard = None;
                return Err(e);
            }
        };
        let criteria = crate::search::imap_criteria(&q, caps.literal_plus);
        // One SELECT and one SEARCH per folder: on an account with many, the folders are shared
        // between a few more connections, opened for the search alone (never on an account limited
        // to one). A lane that cannot connect leaves its folders to the main one.
        let lanes = if self.single_connection(account_id) || targets.len() < SEARCH_LANE_MIN { 1 } else { SEARCH_LANES };
        let mut groups: Vec<Vec<crate::store::MailboxState>> = vec![Vec::new(); lanes];
        for (i, state) in targets.into_iter().enumerate() {
            groups[i % lanes].push(state);
        }
        let mut main_group = groups.remove(0);
        let account = self.store.account(account_id)?;
        let search = async {
            let others = futures::future::join_all(groups.into_iter().map(|group| {
                let (account, criteria) = (&account, &criteria);
                async move {
                    let mut session = match self.login(account).await {
                        Ok(session) => session,
                        Err(e) => return (group, Err(e)),
                    };
                    let res = sync::search_server(&self.store, &mut session, &group, criteria, 200).await;
                    let _ = session.logout().await;
                    (group, res)
                }
            }));
            let (main, others) = futures::join!(sync::search_server(&self.store, &mut conn.session, &main_group, &criteria, 200), others);
            let mut ids = main?;
            let mut left = Vec::new();
            for (group, res) in others {
                match res {
                    Ok(found) => ids.extend(found),
                    Err(e) => {
                        tracing::info!("server search of account {account_id}: another connection failed ({e}); its folders on the main one");
                        left.extend(group);
                    }
                }
            }
            if !left.is_empty() {
                main_group = left;
                ids.extend(sync::search_server(&self.store, &mut conn.session, &main_group, &criteria, 200).await?);
            }
            Ok(ids)
        };
        let ids = match bounded(300, &crate::tr!("recherche sur le serveur", "server search"), search).await {
            Ok(ids) => ids,
            Err(e) => {
                *guard = None;
                return Err(e);
            }
        };
        drop(guard);
        self.emit(Event::Changed { account_id });
        // IMAP cannot search "with attachment": filtered here. Read and pinned too, on the cache's
        // flags: the server may not know yet what was just done here. Capped like the local search: a
        // common word matches thousands of cached messages.
        let (mut found, no_body, labelled): (Vec<MessageSummary>, HashSet<i64>, HashSet<i64>) = crate::rt::offload(|| -> Result<_> {
            let mut seen = HashSet::new();
            let ids: Vec<i64> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
            // Attachments are only known from the body: a match not downloaded yet is kept on the
            // server's word (its multipart/mixed structure).
            let no_body = if q.has_attachment { self.store.without_body(&ids)? } else { HashSet::new() };
            // IMAP has no criterion for it either: told by the folders here.
            let labelled = if q.labelled.is_some() { self.store.labelled(&ids)? } else { HashSet::new() };
            Ok((self.store.summaries(&ids, ids.len())?, no_body, labelled))
        })?;
        found.retain(|m| {
            (!q.has_attachment || m.has_attachments || no_body.contains(&m.id))
                && q.unread.is_none_or(|unread| m.flags.seen != unread)
                && (!q.flagged || m.flags.flagged)
                && q.labelled.is_none_or(|want| labelled.contains(&m.id) == want)
        });
        found.sort_by_key(|m| std::cmp::Reverse(m.date));
        found.truncate(200);
        Ok(found)
    }

    /// With `send_at`, scheduled; otherwise it leaves after the undo delay (setting `undo_send`, in
    /// seconds). Every message goes through the outbox, where it can be followed and retried.
    /// Returns its outbox id, or `None` when it leaves right away: a failure is then reported by
    /// [`Event::SendFailed`]. `remind_after`: follow-up if nobody answered after so many seconds.
    pub async fn send(self: &Arc<Self>, draft: Draft, send_at: Option<i64>, remind_after: Option<i64>) -> Result<Option<i64>> {
        let account = self.store.account(draft.account_id)?;
        // An invalid address is reported now, not at send time.
        smtp::build(&account, &Draft { forwarded: Vec::new(), ..draft.clone() }, None, &[], false)?;
        let delay = self.undo_send_delay();
        let (at, scheduled) = match send_at {
            Some(at) => (at, true),
            None => (crate::store::now() + delay, false),
        };
        // It may leave much later: what depends on the cache (headers of the message replied to,
        // forwarded attachments) is frozen now.
        let mut draft = draft;
        if let Some(id) = draft.in_reply_to
            && draft.reply_ids.is_none()
            && let Ok((Some(irt), refs)) = self.store.message_ids(id)
        {
            draft.reply_ids = Some((irt, refs));
        }
        let mut draft = self.resolve_forwarded(&draft).await?;
        // Too large for any server: refused now rather than bounced once in the outbox.
        smtp::check_size(smtp::estimated_size(&draft))?;
        let saved = draft.draft_id.clone();
        // Message-ID fixed before queuing: every attempt sends the same one, so the Sent copy and the
        // follow-up recognise it, and a retry after an uncertain send can be checked against Sent.
        draft.draft_id.get_or_insert_with(|| new_message_id(&account.email));
        let id = self.store.queue_outgoing(&draft, at, scheduled, remind_after)?;
        // It leaves Drafts now: left there until delivery, it could be sent a second time from there
        // (or from another client). Cancelling puts it back (`cancel_send`).
        if let Some(saved) = saved
            && let Err(e) = self.discard_draft(account.id, &saved)
        {
            tracing::warn!("draft left in Drafts: {e}");
        }
        self.drop_draft_source(&draft);
        self.scheduler.notify_one();
        self.emit(Event::OutboxChanged);
        Ok((scheduled || delay > 0).then_some(id))
    }

    fn undo_send_delay(&self) -> i64 {
        self.setting("undo_send")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(UNDO_SEND_DEFAULT)
            .clamp(0, 60)
    }

    pub fn outbox(&self) -> Result<Vec<Outgoing>> {
        self.store.outbox()
    }

    /// It goes back to Drafts (it left it when queued) under the same Message-ID: closing the
    /// composer without a change does not lose it.
    pub fn cancel_send(self: &Arc<Self>, id: i64) -> Result<Draft> {
        let attempts = self.store.outgoing_attempts(id).unwrap_or(0);
        let mut draft = self
            .store
            .take_outgoing(id)?
            .ok_or_else(|| Error::Other(crate::tr!("trop tard : le message est déjà parti", "too late: the message has already been sent")))?;
        // Already tried: it may have gone (an uncertain send) with its copy in Sent under this
        // Message-ID. Edited and sent again it is another message: a new Message-ID, so that the
        // copies are not confused.
        if attempts > 0 {
            let account = self.store.account(draft.account_id)?;
            draft.draft_id = Some(new_message_id(&account.email));
        }
        match self.save_resolved_draft(draft.clone()) {
            Ok(draft_id) => draft.draft_id = Some(draft_id),
            Err(e) => tracing::warn!("message taken back but not put back in Drafts: {e}"),
        }
        self.emit(Event::OutboxChanged);
        Ok(draft)
    }

    pub fn send_now(&self, id: i64) -> Result<()> {
        if !self.store.reschedule_outgoing(id, crate::store::now())? {
            return Err(Error::Other(crate::tr!("ce message est déjà en cours d'envoi", "this message is already being sent")));
        }
        self.scheduler.notify_one();
        self.emit(Event::OutboxChanged);
        Ok(())
    }

    /// Not created before the folder list is known: a server's real Sent folder, not listed yet,
    /// would get a twin.
    fn sent_mailbox(&self, account_id: i64) -> Result<Option<crate::store::MailboxState>> {
        if let Some(m) = self.store.mailbox_by_role(account_id, Role::Sent)? {
            return Ok(Some(m));
        }
        if self.store.mailbox_by_role(account_id, Role::Inbox)?.is_none() {
            tracing::warn!("account {account_id}: folders not known yet, no copy in Sent");
            return Ok(None);
        }
        let id = self.special_mailbox(account_id, &[Role::Sent], Role::Sent, "Sent")?;
        Ok(Some(self.store.mailbox_state(id)?))
    }

    /// Through the queue, which survives a network cut.
    fn keep_sent_copy(&self, account: &Account, subject: &str, raw: &[u8]) -> Result<()> {
        if smtp::server_saves_sent(account) {
            return Ok(());
        }
        let Some(sent) = self.sent_mailbox(account.id)? else { return Ok(()) };
        let label = crate::i18n::both(|| crate::tr!("Copie dans Envoyés · {}", "Copy to Sent · {}", subject));
        self.enqueue(account.id, &Op::append(&sent.name, "\\Seen", raw), &label)?;
        Ok(())
    }

    async fn deliver(self: &Arc<Self>, draft: Draft, remind_after: Option<i64>) -> Result<()> {
        self.deliver_as(draft, remind_after, None).await
    }

    /// With the Message-ID to use when the draft has none.
    async fn deliver_as(self: &Arc<Self>, draft: Draft, remind_after: Option<i64>, message_id: Option<String>) -> Result<()> {
        let account = self.store.account(draft.account_id)?;
        // Message-ID known in advance: the follow-up uses it to recognise replies.
        let mut outgoing = self.resolve_forwarded(&draft).await?;
        let message_id = outgoing.draft_id.get_or_insert_with(|| message_id.unwrap_or_else(|| new_message_id(&account.email))).clone();
        let (irt, refs) = self.reply_headers(&outgoing);
        // The Sent copy keeps the Bcc recipients; the message handed to the server does not. `send`
        // built it when queued: failing now means something changed since (an alias removed, an
        // attachment gone), which no retry fixes.
        let (message, copy) = smtp::build_outgoing(&account, &outgoing, irt.as_deref(), &refs).map_err(|e| match e {
            Error::Smtp(text) | Error::Mime(text) => Error::Mime(text),
            e => e,
        })?;
        // No credential (the account waits to be signed in again, the keychain or the OAuth
        // provider out of reach): the message waits, no attempt spent.
        let waiting = |e: Error| match e {
            Error::Unreachable(_) => e,
            e => Error::Unreachable(e.to_string()),
        };
        let credential = self.credential(&account).await.map_err(waiting)?;
        let mut sent = smtp::send(&account, &credential, &message).await;
        // An access token refused although not expired (as for IMAP, `login`): a fresh one, once.
        if let (Err(Error::Auth(_)), Credential::Token(used)) = (&sent, &credential) {
            self.forget_token(account.id, Some(used.as_str()));
            let credential = self.credential(&account).await.map_err(waiting)?;
            sent = smtp::send(&account, &credential, &message).await;
        }
        match sent {
            Ok(()) => {}
            // Maybe gone: the copy goes to Sent all the same, as the record of a message that may have
            // left (proving nothing, see `smtp::maybe_sent_advice`). Sending again reuses this Message-ID,
            // and the copy is not added twice.
            Err(e @ Error::Uncertain(_)) => {
                match self.keep_sent_copy(&account, &draft.subject, &copy) {
                    Ok(()) => self.after_local_change(vec![account.id]),
                    Err(e) => tracing::warn!("copy to Sent: {e}"),
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        }
        // The message is gone. Nothing may look like a failure any more (the caller would send it
        // again): the next steps are best effort, their errors only logged.
        let after_send = || -> Result<Vec<i64>> {
            let mut changed = vec![account.id];
            if let Some(id) = &draft.draft_id
                && let Err(e) = self.discard_draft(account.id, id)
            {
                tracing::warn!("sent draft not deleted: {e}");
            }
            // The screener learns the recipients and this Message-ID now: a quick reply is not held while
            // the Sent copy has not been synced yet.
            let recipients: Vec<String> = draft
                .to
                .iter()
                .chain(&draft.cc)
                .filter_map(|s| s.trim().parse::<lettre::message::Mailbox>().ok())
                .map(|m| m.email.to_string())
                .collect();
            if let Err(e) = self.store.record_sent(account.id, &message_id, &recipients) {
                tracing::warn!("sent message not recorded for the screener: {e}");
            }
            if let Some(after) = remind_after {
                let now = crate::store::now();
                let recipients: Vec<String> = draft.to.iter().chain(&draft.cc).cloned().collect();
                self.store.add_reminder(account.id, &message_id, &draft.subject, &recipients, now, now + after)?;
                self.scheduler.notify_one();
            }
            self.keep_sent_copy(&account, &draft.subject, &copy)?;
            // The original message may have left the cache meanwhile: no flag then.
            if let Some(id) = draft.in_reply_to
                && let Ok(loc) = self.store.location(id)
            {
                self.store.set_answered(id)?;
                if let Some(uid) = loc.uid {
                    let label = crate::i18n::both(|| self.label(&crate::tr!("Marquer comme répondu", "Mark as answered"), id));
                    let uidvalidity = self.uidvalidity(loc.mailbox_id);
                    self.enqueue(loc.account_id, &Op::Store { mailbox: loc.mailbox, uid, flag: "\\Answered".into(), value: true, uidvalidity }, &label)?;
                }
                changed.push(loc.account_id);
            }
            Ok(changed)
        };
        match after_send() {
            Ok(changed) => self.after_local_change(changed),
            Err(e) => {
                tracing::error!("message sent, but the steps after it failed: {e}");
                self.after_local_change(vec![account.id]);
            }
        }
        // Sent is read again right after its copy is filed, so the message shows in its conversation
        // without waiting for the periodic sync.
        match self.store.mailbox_by_role(account.id, Role::Sent) {
            Ok(Some(sent)) => self.flush_then_refresh(account.id, vec![sent.id]),
            Ok(None) => {}
            Err(e) => tracing::warn!("Sent not read again after sending: {e}"),
        }
        Ok(())
    }

    pub fn rules(&self) -> Result<Vec<crate::rules::Rule>> {
        self.store.rules(None)
    }

    pub fn save_rule(&self, rule: crate::rules::Rule) -> Result<i64> {
        for action in &rule.actions {
            if let crate::rules::Action::Move { mailbox_id } = action
                && self.store.mailbox_state(*mailbox_id)?.account_id != rule.account_id
            {
                return Err(Error::Other(crate::tr!("le dossier de destination doit être du même compte", "the destination folder must belong to the same account")));
            }
        }
        self.store.save_rule(&rule)
    }

    pub fn delete_rule(&self, id: i64) -> Result<()> {
        self.store.delete_rule(id)
    }

    pub fn reorder_rules(&self, ids: &[i64]) -> Result<()> {
        self.store.reorder_rules(ids)
    }

    /// Each step stands on its own: a destination that refuses its messages is logged and the
    /// others still go. A rule moving to a folder that no longer exists is turned off, rather than
    /// failing every new message.
    fn apply_rules(self: &Arc<Self>, account_id: i64, ids: &[i64], only: Option<i64>) -> Result<Ruled> {
        use crate::rules::Action;
        let mut rules: Vec<_> = self.store.rules(Some(account_id))?.into_iter().filter(|r| only.is_none_or(|id| r.id == id)).collect();
        let mut ruled = Ruled::default();
        // Muted conversations first (not for a single rule run): their new messages are read and
        // archived, and the rules leave them alone.
        let muted = if only.is_none() { self.store.muted_among(ids)? } else { HashSet::new() };
        if !muted.is_empty() {
            let list: Vec<i64> = muted.iter().copied().collect();
            match self.set_flag(&list, FlagKind::Seen, true) {
                Ok(_) => ruled.read.extend(list.iter().copied()),
                Err(e) => tracing::warn!("muted conversations of account {account_id}, mark as read: {e}"),
            }
            match self.archive_mailbox(account_id).and_then(|dest| self.move_inner(&list, dest, None)) {
                Ok(()) => ruled.moved.extend(list),
                Err(e) => tracing::warn!("muted conversations of account {account_id}, archive: {e}"),
            }
        }
        let ids: Vec<i64> = ids.iter().copied().filter(|id| !muted.contains(id)).collect();
        let ids = &ids[..];
        if rules.is_empty() || ids.is_empty() {
            return Ok(ruled);
        }
        for rule in rules.iter_mut().filter(|r| r.enabled) {
            let gone = rule.actions.iter().any(|a| {
                matches!(a, Action::Move { mailbox_id } if matches!(self.store.mailbox_state(*mailbox_id), Err(Error::NotFound(_))))
            });
            if gone {
                tracing::warn!("rule {} ({}): its folder no longer exists, rule turned off", rule.id, rule.name);
                rule.enabled = false;
                if let Err(e) = self.store.save_rule(rule) {
                    tracing::warn!("rule {}: {e}", rule.id);
                }
            }
        }
        let (mut read, mut pin) = (Vec::new(), Vec::new());
        let mut moves: Vec<(Action, Vec<i64>)> = Vec::new();
        for m in self.store.rule_candidates(ids)? {
            let Some(d) = crate::rules::decide(&rules, &m) else { continue };
            if d.mark_read {
                read.push(m.id);
            }
            if d.pin {
                pin.push(m.id);
            }
            if let Some(dest) = d.destination {
                match moves.iter_mut().find(|(a, _)| *a == dest) {
                    Some((_, list)) => list.push(m.id),
                    None => moves.push((dest, vec![m.id])),
                }
            }
        }
        // Flags first: they target the UID in the inbox, before any move.
        if !read.is_empty() {
            match self.set_flag(&read, FlagKind::Seen, true) {
                Ok(_) => ruled.read.extend(read),
                Err(e) => tracing::warn!("rules of account {account_id}, mark as read: {e}"),
            }
        }
        if !pin.is_empty()
            && let Err(e) = self.set_flag(&pin, FlagKind::Flagged, true)
        {
            tracing::warn!("rules of account {account_id}, pin: {e}");
        }
        for (action, list) in moves {
            let dest = match action {
                Action::Move { mailbox_id } => Ok(mailbox_id),
                Action::Archive => self.archive_mailbox(account_id),
                Action::Trash => self.role_mailbox(account_id, Role::Trash),
                Action::MarkRead | Action::Pin => continue,
            };
            match dest.and_then(|dest| self.move_inner(&list, dest, None)) {
                Ok(()) => ruled.moved.extend(list),
                Err(e) => tracing::warn!("rules of account {account_id}, {action:?}: {e}"),
            }
        }
        Ok(ruled)
    }

    /// Muting also archives `ids` (its messages in the folder shown), undoably; the mute itself is
    /// undone by unmuting.
    pub fn mute_thread(self: &Arc<Self>, account_id: i64, thread_id: i64, muted: bool, ids: &[i64]) -> Result<Option<u64>> {
        self.store.set_thread_muted(account_id, thread_id, muted)?;
        if !muted || ids.is_empty() {
            self.emit(Event::Changed { account_id });
            return Ok(None);
        }
        self.archive(ids)
    }

    pub fn thread_muted(&self, account_id: i64, thread_id: i64) -> Result<bool> {
        self.store.thread_muted(account_id, thread_id)
    }

    /// Inbox messages this rule (saved or not) would take now, with a few figures. Changes nothing.
    pub fn preview_rule(&self, rule: crate::rules::Rule) -> Result<crate::rules::Preview> {
        let rule = crate::rules::Rule { enabled: true, ..rule };
        let ids = self.store.inbox_messages(rule.account_id)?;
        let matched: Vec<crate::rules::Candidate> = self
            .store
            .rule_candidates(&ids)?
            .into_iter()
            .filter(|c| crate::rules::decide(std::slice::from_ref(&rule), c).is_some())
            .collect();
        let ids: Vec<i64> = matched.iter().map(|c| c.id).collect();
        let messages = self.store.summaries(&ids, 3000)?;
        let mut senders: HashMap<String, (Option<String>, i64)> = HashMap::new();
        for c in &matched {
            if let Some(from) = &c.from {
                let e = senders.entry(from.email.to_lowercase()).or_insert((from.name.clone(), 0));
                e.1 += 1;
            }
        }
        let mut top_senders: Vec<CountedAddress> =
            senders.into_iter().map(|(email, (name, count))| CountedAddress { email, name, count }).collect();
        top_senders.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.email.cmp(&b.email)));
        top_senders.truncate(6);
        Ok(crate::rules::Preview {
            count: matched.len(),
            // Over every matched message, not only those listed (3000 at most).
            unread: matched.iter().filter(|c| !c.seen).count(),
            with_attachments: matched.iter().filter(|c| c.has_attachments).count(),
            bulk: matched.iter().filter(|c| c.bulk).count(),
            oldest: matched.iter().map(|c| c.date).min(),
            newest: matched.iter().map(|c| c.date).max(),
            top_senders,
            messages,
        })
    }

    pub fn run_rule(self: &Arc<Self>, rule_id: i64) -> Result<usize> {
        let rule = self.store.rules(None)?.into_iter().find(|r| r.id == rule_id).ok_or_else(|| Error::NotFound(crate::tr!("règle {rule_id}", "rule {rule_id}")))?;
        let ids = self.store.inbox_messages(rule.account_id)?;
        let ruled = self.apply_rules(rule.account_id, &ids, Some(rule_id))?;
        Ok(ruled.moved.union(&ruled.read).count())
    }

    fn folder_leaf(name: &str, delimiter: Option<&str>) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Other(crate::tr!("donnez un nom au dossier", "give the folder a name")));
        }
        if let Some(d) = delimiter.filter(|d| !d.is_empty())
            && name.contains(d)
        {
            return Err(Error::Other(crate::tr!("le nom ne peut pas contenir « {d} »", "the name cannot contain “{d}”")));
        }
        Ok(crate::utf7::encode(name))
    }

    /// Locally at once, on the server through the queue.
    pub fn create_folder(self: &Arc<Self>, account_id: i64, parent: Option<i64>, name: &str) -> Result<i64> {
        let boxes = self.store.list_mailboxes(account_id)?;
        let parent = parent.map(|id| boxes.iter().find(|m| m.id == id).cloned().ok_or_else(|| Error::NotFound(crate::tr!("dossier {id}", "folder {id}")))).transpose()?;
        let delimiter = parent.as_ref().and_then(|p| p.delimiter.clone()).or_else(|| boxes.iter().find_map(|m| m.delimiter.clone()));
        let leaf = Self::folder_leaf(name, delimiter.as_deref())?;
        let full = match (&parent, &delimiter) {
            (Some(p), Some(d)) => format!("{}{d}{leaf}", p.name),
            (Some(_), None) => return Err(Error::Other(crate::tr!("ce serveur n'accepte pas de sous-dossiers", "this server does not support subfolders"))),
            (None, _) => leaf,
        };
        if boxes.iter().any(|m| m.name.eq_ignore_ascii_case(&full)) {
            return Err(Error::Other(crate::tr!("un dossier porte déjà ce nom", "a folder with this name already exists")));
        }
        let id = self.store.upsert_mailbox(account_id, &full, delimiter.as_deref(), None, true)?;
        self.enqueue(account_id, &Op::Create { mailbox: full.clone() }, &crate::i18n::both(|| crate::tr!("Créer le dossier {}", "Create folder {}", name.trim())))?;
        self.after_local_change(vec![account_id]);
        Ok(id)
    }

    /// Not a special folder; it stays at the same place in the tree.
    pub fn rename_folder(self: &Arc<Self>, mailbox_id: i64, name: &str) -> Result<()> {
        let state = self.store.mailbox_state(mailbox_id)?;
        if state.role.is_some() {
            return Err(Error::Other(crate::tr!("un dossier spécial ne se renomme pas", "a special folder cannot be renamed")));
        }
        let account_id = state.account_id;
        let boxes = self.store.list_mailboxes(account_id)?;
        let mailbox = boxes.iter().find(|m| m.id == mailbox_id).ok_or_else(|| Error::NotFound(crate::tr!("dossier {mailbox_id}", "folder {mailbox_id}")))?;
        let delimiter = mailbox.delimiter.clone().filter(|d| !d.is_empty());
        let leaf = Self::folder_leaf(name, delimiter.as_deref())?;
        let to = match delimiter.as_deref().and_then(|d| mailbox.name.rsplit_once(d).map(|(parent, _)| (parent, d))) {
            Some((parent, d)) => format!("{parent}{d}{leaf}"),
            None => leaf,
        };
        if to == mailbox.name {
            return Ok(());
        }
        if boxes.iter().any(|m| m.name.eq_ignore_ascii_case(&to)) {
            return Err(Error::Other(crate::tr!("un dossier porte déjà ce nom", "a folder with this name already exists")));
        }
        self.store.rename_mailbox_tree(account_id, &mailbox.name, &to, delimiter.as_deref().unwrap_or("\u{0}"))?;
        let label = crate::i18n::both(|| crate::tr!("Renommer le dossier {} en {}", "Rename folder {} to {}", mailbox.display_name, name.trim()));
        self.enqueue(account_id, &Op::Rename { from: mailbox.name.clone(), to }, &label)?;
        self.after_local_change(vec![account_id]);
        Ok(())
    }

    /// With its subfolders ("Clients" takes "Clients/2024"), the deepest first.
    fn folder_tree(&self, mailbox_id: i64) -> Result<(Mailbox, Vec<Mailbox>)> {
        let state = self.store.mailbox_state(mailbox_id)?;
        let all = self.store.list_mailboxes(state.account_id)?;
        let mailbox = all.iter().find(|m| m.id == mailbox_id).cloned().ok_or_else(|| Error::NotFound(crate::tr!("dossier {mailbox_id}", "folder {mailbox_id}")))?;
        let mut children: Vec<Mailbox> = match mailbox.delimiter.as_deref().filter(|d| !d.is_empty()) {
            Some(d) => {
                let prefix = format!("{}{d}", mailbox.name);
                all.into_iter().filter(|m| m.name.starts_with(&prefix)).collect()
            }
            None => Vec::new(),
        };
        children.sort_by_key(|m| std::cmp::Reverse(m.name.len()));
        Ok((mailbox, children))
    }

    /// `recent`: how many recent messages are listed, 50 by default.
    pub fn folder_summary(&self, mailbox_id: i64, recent: Option<usize>) -> Result<FolderSummary> {
        let (mailbox, children) = self.folder_tree(mailbox_id)?;
        let gmail = self.store.provider(mailbox.account_id)? == crate::provider::Provider::Gmail;
        self.store.folder_summary(mailbox, children, gmail, recent.unwrap_or(50).min(5000))
    }

    /// Sub-folders included, as in `folder_summary`. `limit`: 50 by default.
    pub fn folder_messages_from(&self, mailbox_id: i64, from: &str, limit: Option<usize>) -> Result<Vec<MessageSummary>> {
        let (mailbox, children) = self.folder_tree(mailbox_id)?;
        let ids: Vec<i64> = std::iter::once(mailbox.id).chain(children.iter().map(|c| c.id)).collect();
        self.store.folder_messages_from(&ids, from, limit.unwrap_or(50).min(5000))
    }

    /// Bin or junk only: purges on the server everything the folder holds at the time of the request,
    /// even what is not cached. Returns the number of messages removed from the cache.
    pub fn empty_folder(self: &Arc<Self>, mailbox_id: i64) -> Result<usize> {
        let state = self.store.mailbox_state(mailbox_id)?;
        if !matches!(state.role, Some(Role::Trash | Role::Junk)) {
            return Err(Error::Other(crate::tr!(
                "seuls la corbeille et les indésirables peuvent être vidés",
                "only the trash and junk folders can be emptied"
            )));
        }
        // Before the cache is emptied: it tells how far the folder is known.
        let below = self.uid_cutoff(&state);
        // Arrived on the server before the folder's sync saw their UID.
        let arriving = self.store.placeholder_message_ids(mailbox_id)?;
        let removed = self.store.purge_local(mailbox_id)?;
        // What was still on its way to this folder would land after the purge (with a UID it does not
        // target) and come back: deleted directly where it is. Not on Gmail: an expunge from a label
        // view only takes the label off, and the message would stay in All Mail. There the moves go
        // first (the purge waits behind them) and the purge finds them by `arriving`.
        if self.store.provider(state.account_id)? != crate::provider::Provider::Gmail {
            self.store.moves_into_become_deletes(state.account_id, &state.name)?;
        }
        let label = crate::i18n::both(|| crate::tr!("Vider {}", "Empty {}", crate::utf7::decode(&state.name)));
        let op = Op::Purge { mailbox: state.name.clone(), below, uidvalidity: state.uidvalidity, arriving };
        self.enqueue(state.account_id, &op, &label)?;
        self.after_local_change(vec![state.account_id]);
        Ok(removed)
    }

    /// Their messages, including those never downloaded, first go to `dest` or the bin: nothing is
    /// lost at once (and with Gmail, deleting a label would otherwise leave the messages in place).
    pub fn delete_folder(self: &Arc<Self>, mailbox_id: i64, dest: Option<i64>) -> Result<()> {
        let (mailbox, children) = self.folder_tree(mailbox_id)?;
        let account_id = mailbox.account_id;
        if let Some(role) = mailbox.role.or_else(|| children.iter().find_map(|c| c.role)) {
            return Err(Error::Other(crate::tr!("le dossier spécial « {} » ne peut pas être supprimé", "the special folder “{}” cannot be deleted", role.as_str())));
        }
        let dest = match dest {
            Some(id) => id,
            None => self.role_mailbox(account_id, Role::Trash)?,
        };
        let target = self.store.mailbox_state(dest)?;
        let doomed: Vec<&Mailbox> = children.iter().chain(std::iter::once(&mailbox)).collect();
        if target.account_id != account_id || doomed.iter().any(|m| m.id == dest) {
            return Err(Error::Other(crate::tr!("destination invalide : choisissez un dossier du même compte, hors de celui supprimé", "invalid destination: choose a folder of the same account, outside the one being deleted")));
        }
        // Gmail's labels: deleting a label leaves its messages in All Mail; another destination gives
        // them its label (or the bin), as a move from the deleted one.
        let labels_model = self.store.labels_model(account_id)?;
        // Deepest first: some servers refuse to delete a folder that has children.
        for m in &doomed {
            if labels_model && self.store.mailbox_state(m.id)?.is_label {
                if target.role != Some(Role::All) {
                    let ids = self.store.mailbox_message_ids(m.id)?;
                    self.move_from(&ids, dest, Some(m.id), None)?;
                }
                let label = crate::i18n::both(|| crate::tr!("Supprimer le dossier {}", "Delete folder {}", m.display_name));
                self.enqueue(account_id, &Op::DeleteMailbox { mailbox: m.name.clone(), dest: None }, &label)?;
                self.store.delete_mailbox(m.id)?;
                continue;
            }
            if m.selectable {
                self.store.move_all_local(m.id, dest)?;
            }
            let op = Op::DeleteMailbox { mailbox: m.name.clone(), dest: m.selectable.then(|| target.name.clone()) };
            let label = crate::i18n::both(|| crate::tr!("Supprimer le dossier {} (messages vers {})", "Delete folder {} (messages to {})", m.display_name, crate::utf7::decode(&target.name)));
            self.enqueue(account_id, &op, &label)?;
            self.store.delete_mailbox(m.id)?;
        }
        self.flush_then_refresh(account_id, vec![dest]);
        self.emit(Event::Changed { account_id });
        Ok(())
    }

    /// Messages go to the snooze folder (created if missing), then come back to the inbox, unread,
    /// on top of the list.
    pub fn snooze(self: &Arc<Self>, ids: &[i64], until: i64) -> Result<Option<u64>> {
        let mut undo = Undo::default();
        let plan = self
            .by_account(ids)?
            .into_iter()
            .map(|(account_id, ids)| {
                Ok((account_id, ids, self.special_mailbox(account_id, &[Role::Snoozed], Role::Snoozed, SNOOZE_FOLDER)?))
            })
            .collect::<Result<Vec<_>>>()?;
        for (account_id, ids, dest) in plan {
            let mut threads: HashMap<i64, Vec<String>> = HashMap::new();
            for &id in &ids {
                let thread_id = self.store.summary(id)?.thread_id;
                let mid = self.store.message_ids(id)?.0;
                threads.entry(thread_id).or_default().extend(mid);
            }
            self.move_inner(&ids, dest, Some(&mut undo))?;
            for (thread_id, mids) in threads {
                self.store.snooze(account_id, thread_id, &mids, until)?;
                undo.snoozes.push((account_id, thread_id));
            }
        }
        self.scheduler.notify_one();
        Ok(self.keep_undo(undo))
    }

    pub fn unsnooze(self: &Arc<Self>, account_id: i64, thread_id: i64) -> Result<()> {
        let entry = self.store.snoozed()?.into_iter().find(|(a, t, _, _)| *a == account_id && *t == thread_id);
        match entry {
            Some((_, _, mids, _)) if !self.wake(account_id, thread_id, &mids)? => {
                Err(Error::Other(crate::tr!("mise de côté en cours sur le serveur, réessayez dans un instant", "snooze in progress on the server, try again in a moment")))
            }
            _ => Ok(()),
        }
    }

    /// The last message becomes unread again. `false` if a message still waits for its move to be
    /// confirmed (tried again later).
    fn wake(self: &Arc<Self>, account_id: i64, thread_id: i64, message_ids: &[String]) -> Result<bool> {
        let Some(snoozed) = self.store.mailbox_by_role(account_id, Role::Snoozed)? else {
            self.store.drop_snooze(account_id, thread_id)?;
            return Ok(true);
        };
        let ids = self.store.snoozed_messages(snoozed.id, thread_id, message_ids)?;
        if ids.is_empty() {
            // Moved elsewhere meanwhile by the user: nothing left to bring back.
            self.store.drop_snooze(account_id, thread_id)?;
            self.emit(Event::Changed { account_id });
            return Ok(true);
        }
        let mut located = Vec::new();
        for &id in &ids {
            located.push(self.store.location(id)?);
        }
        if located.iter().any(|l| l.uid.is_none()) {
            return Ok(false);
        }
        let inbox = self.role_mailbox(account_id, Role::Inbox)?;
        // Unread first: the op targets the UID in the snooze folder, before the move.
        if let Some(&last) = ids.last() {
            self.set_flag(&[last], FlagKind::Seen, false)?;
        }
        let thread_now = self.store.summary(ids[0])?.thread_id;
        self.move_inner(&ids, inbox, None)?;
        if thread_now != thread_id {
            self.store.drop_snooze(account_id, thread_id)?;
            self.store.snooze(account_id, thread_now, message_ids, crate::store::now())?;
        }
        self.store.woke(account_id, thread_now, crate::store::now())?;
        self.emit(Event::Changed { account_id });
        Ok(true)
    }

    pub fn follow_ups(&self) -> Result<Vec<FollowUp>> {
        self.store.follow_ups()
    }

    pub fn dismiss_follow_up(&self, id: i64) -> Result<()> {
        self.store.dismiss_reminder(id)?;
        self.emit(Event::FollowUpsDue { follow_ups: Vec::new() });
        Ok(())
    }

    /// Sends what is due, wakes snoozed threads, reports unanswered messages; sleeps until the next
    /// due time (a minute at most).
    /// A database with much free space is compacted a while after the start, off the window's
    /// way. A failure (a full disk: VACUUM needs about twice the base's size) is not tried again
    /// for a week.
    fn compact_later(self: &Arc<Self>) {
        const FAILED_AT: &str = "compaction_failed_at";
        let this = self.clone();
        self.spawn(async move {
            tokio::time::sleep(Duration::from_secs(120)).await;
            let failed_at: Option<i64> = this.store.setting(FAILED_AT).ok().flatten().and_then(|t| t.parse().ok());
            if failed_at.is_some_and(|t| crate::store::now() - t < 7 * 86400) {
                return;
            }
            match crate::rt::offload(|| this.store.compact_if_wasteful()) {
                Ok(Some(freed)) => tracing::info!("database compacted: {} MiB given back", freed / (1024 * 1024)),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("database compaction: {e}");
                    let _ = this.store.set_setting(FAILED_AT, &crate::store::now().to_string());
                }
            }
        });
    }

    fn start_scheduler(self: &Arc<Self>) {
        self.compact_later();
        match self.store.interrupted_outgoing() {
            Ok(0) => {}
            Ok(n) => {
                tracing::warn!("{n} send(s) interrupted at the last shutdown");
                self.emit(Event::OutboxChanged);
            }
            Err(e) => tracing::error!("outbox unreadable: {e}"),
        }
        let this = self.clone();
        self.spawn(async move {
            loop {
                // An internal error in one round must not stop the scheduler (snoozes, follow-ups and sends
                // would silently stop for the rest of the session).
                if std::panic::AssertUnwindSafe(this.run_due()).catch_unwind().await.is_err() {
                    tracing::error!("scheduler: internal error, carrying on at the next round");
                }
                let now = crate::store::now();
                let next = [this.store.next_outgoing(), this.store.next_snooze(), this.store.next_reminder()]
                    .into_iter()
                    .filter_map(|r| r.ok().flatten())
                    .filter(|&t| t > now)
                    .min();
                let wait = next.map_or(60, |t| (t - now).clamp(1, 60)) as u64;
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(wait)) => {}
                    _ = this.scheduler.notified() => {}
                }
            }
        });
    }

    async fn run_due(self: &Arc<Self>) {
        let now = crate::store::now();
        // Accounts whose two sending lanes are taken: their messages stay pending (still
        // cancellable, not "interrupted" by a quit) until a lane frees and runs this again.
        let mut busy: Vec<i64> = Vec::new();
        loop {
            let (id, draft, remind) = match self.store.claim_outgoing_except(now, &busy) {
                Ok(Some(claimed)) => claimed,
                Ok(None) => break,
                Err(e) => {
                    tracing::error!("outbox: {e}");
                    break;
                }
            };
            let lane = self.send_lane(draft.account_id);
            let Ok(permit) = lane.try_acquire_owned() else {
                if let Err(e) = self.store.unclaim_outgoing(id) {
                    tracing::error!("outbox: {e}");
                }
                busy.push(draft.account_id);
                continue;
            };
            // Each delivery in its own task: a slow or stuck server delays neither the other messages nor
            // the snoozes and follow-ups below.
            self.spawn(self.clone().deliver_outgoing(id, draft, remind, permit));
        }

        for (account_id, thread_id, mids, until) in self.store.snoozed().unwrap_or_default() {
            if until <= now
                && let Err(e) = self.wake(account_id, thread_id, &mids)
            {
                tracing::warn!("return of snoozed thread {thread_id}: {e}");
            }
        }

        self.check_follow_ups(now);
        self.check_event_alarms(now);
        // The local model's memory (more than a gigabyte) is given back soon: loading it again
        // costs a few seconds only.
        if crate::ai::unload_if_idle(Duration::from_secs(2 * 60)) {
            tracing::info!("local AI: model unloaded after 2 minutes unused");
        }
    }

    /// Reminders due since the last check, a quarter of an hour back at most (facteur closed or
    /// asleep meanwhile: an old reminder is no use). Setting `event_reminders` ("off"): none.
    fn check_event_alarms(&self, now: i64) {
        const UNTIL: &str = "event_alarms_until";
        let last = self.setting(UNTIL).ok().flatten().and_then(|v| v.parse::<i64>().ok()).unwrap_or(now - 60);
        let from = last.max(now - 15 * 60);
        if from >= now {
            return;
        }
        if let Err(e) = self.store.set_setting(UNTIL, &now.to_string()) {
            tracing::warn!("event reminders: {e}");
            return;
        }
        if self.setting("event_reminders").ok().flatten().as_deref() == Some("off") {
            return;
        }
        match self.store.event_alarms(from, now) {
            Ok(due) => {
                for (event, at) in due {
                    let minutes = ((event.start - at) / 60).max(0);
                    self.emit(Event::EventReminder { event, minutes });
                }
            }
            Err(e) => tracing::warn!("event reminders: {e}"),
        }
    }

    /// Two sends at a time per account: a batch of scheduled messages falling due together must
    /// not open as many connections (servers limit them, and a 4xx would spend an attempt).
    fn send_lane(&self, account_id: i64) -> Arc<tokio::sync::Semaphore> {
        self.sending.lock().unwrap().entry(account_id).or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(2))).clone()
    }

    /// `_lane`: the account's sending lane, held until the send ends; the messages that waited
    /// for it are looked at again then.
    async fn deliver_outgoing(self: Arc<Self>, id: i64, draft: Draft, remind: Option<i64>, _lane: tokio::sync::OwnedSemaphorePermit) {
        self.emit(Event::OutboxChanged);
        let (account_id, subject) = (draft.account_id, draft.subject.clone());
        let res = std::panic::AssertUnwindSafe(self.deliver(draft, remind)).catch_unwind().await.unwrap_or_else(|_| {
            // Not known how far it went: treated like a cut in the middle of the transfer.
            Err(Error::Uncertain(crate::tr!("erreur interne pendant l'envoi", "internal error while sending")))
        });
        match res {
            Ok(()) => {
                // A row left behind would be sent again: the write is retried (database busy).
                if let Err(e) = persist(|| self.store.delete_outgoing(id)).await {
                    tracing::error!("sent message left in the outbox: {e}");
                }
            }
            Err(e) => {
                // Maybe gone: what tells whether it did depends on who files the Sent copy.
                let e = match e {
                    Error::Uncertain(text) => {
                        let host = self.store.account(account_id).map(|a| a.smtp.host).unwrap_or_default();
                        Error::Uncertain(crate::tr!("{text} ; {}", "{text}; {}", smtp::maybe_sent_advice(&host)))
                    }
                    e => e,
                };
                tracing::warn!("send {id}: {e}");
                let offline = matches!(e, Error::Unreachable(_));
                let (attempts, tries) = self.store.outgoing_tries(id).unwrap_or((0, 0));
                let (attempts, tries) = if offline { (attempts, tries + 1) } else { (attempts + 1, tries) };
                let retry = retry_at(&e, attempts, tries, crate::store::now());
                // Otherwise the message would stay "sending" for the rest of the session.
                if let Err(db) = persist(|| self.store.outgoing_failed(id, &e.to_string(), retry, offline)).await {
                    tracing::error!("send {id}: failure not recorded, the message may stay as being sent: {db}");
                }
                // Tell on the first failure and on giving up, not at every attempt.
                if attempts + tries == 1 || retry.is_none() {
                    self.emit(Event::SendFailed { id, account_id, subject, error: e.to_string(), retrying: retry.is_some() });
                }
            }
        }
        self.emit(Event::OutboxChanged);
        // The lane frees: messages that waited for it go; and the scheduler may be asleep for
        // longer than a retry.
        drop(_lane);
        self.scheduler.notify_one();
    }

    fn check_follow_ups(&self, now: i64) {
        match self.store.check_reminders(now) {
            Ok(due) if !due.is_empty() => self.emit(Event::FollowUpsDue { follow_ups: due }),
            Ok(_) => {}
            Err(e) => tracing::warn!("follow-ups: {e}"),
        }
    }

    /// A reply in a snoozed thread brings it back, and answered follow-ups go away.
    fn after_sync(self: &Arc<Self>, account_id: i64) {
        let snoozed = self.store.snoozed().unwrap_or_default();
        for thread_id in self.store.snoozes_back_in_inbox(account_id).unwrap_or_default() {
            if let Some((_, _, mids, _)) = snoozed.iter().find(|(a, t, _, _)| *a == account_id && *t == thread_id)
                && let Err(e) = self.wake(account_id, thread_id, mids)
            {
                tracing::warn!("return of thread {thread_id} after a reply: {e}");
            }
        }
        self.check_follow_ups(crate::store::now());
        self.finish_release(account_id);
    }

    /// A second account with the same address means something is wrong underneath (a sign-in name
    /// taken for the mailbox's address), never two mailboxes.
    fn ensure_new_address(&self, email: &str, except: Option<i64>) -> Result<()> {
        if let Some(other) = self.store.list_accounts()?.into_iter().find(|a| Some(a.id) != except && a.email.eq_ignore_ascii_case(email)) {
            return Err(Error::Other(crate::tr!(
                "{email} est déjà dans facteur (compte « {} ») : une même adresse ne peut pas être ajoutée deux fois",
                "{email} is already in facteur (account “{}”): one address cannot be added twice",
                other.label.as_deref().unwrap_or(&other.name)
            )));
        }
        Ok(())
    }

    /// One already under way is not waited for (a first sync can take hours, and the periodic round
    /// awaits every account): it runs once more when done.
    pub async fn sync(self: &Arc<Self>, account_id: i64) -> Result<Report> {
        let Some(mut run) = CalendarRun::start(&self.full_syncs, account_id) else {
            tracing::info!("sync of account {account_id}: one already under way, it will run again");
            return Ok(Report::default());
        };
        loop {
            let res = self.sync_scoped(account_id, sync::Scope::All).await;
            if !run.again() {
                if let Err(e) = &res
                    && e.is_connection_drop()
                    && self.drop_follow_ups.lock().unwrap().insert(account_id)
                {
                    let this = self.clone();
                    self.spawn(async move {
                        tokio::time::sleep(SYNC_FOLLOW_UP).await;
                        this.drop_follow_ups.lock().unwrap().remove(&account_id);
                        // As `sync` does (not called itself: a future cannot hold itself).
                        let Some(mut run) = CalendarRun::start(&this.full_syncs, account_id) else { return };
                        loop {
                            let _ = this.sync_scoped(account_id, sync::Scope::All).await;
                            if !run.again() {
                                break;
                            }
                        }
                    });
                }
                return res;
            }
        }
    }

    /// `Scope::Inbox`: the inbox only, when IDLE reports something new.
    pub async fn sync_scoped(self: &Arc<Self>, account_id: i64, scope: sync::Scope) -> Result<Report> {
        loop {
            let report = self.sync_scoped_once(account_id, scope.clone()).await?;
            // Stopped between two folders for a message to open or an action to send (one connection
            // for everything): taken up again behind them, the connection going to whoever waited
            // first; the folders done already are left unopened then.
            if !report.handed_over {
                return Ok(report);
            }
        }
    }

    async fn sync_scoped_once(self: &Arc<Self>, account_id: i64, scope: sync::Scope) -> Result<Report> {
        // The replay lock keeps an interactive flush from replaying the same operations in parallel.
        let lock = self.flush_lock(account_id);
        let run = self.diag.sync_started(account_id, &scope);
        self.emit(Event::SyncStarted { account_id });
        // Whatever happens below (a panic, the task dropped halfway), the interface hears that the sync
        // is over: its spinner never stays on.
        let mut finished = SyncFinishedGuard { engine: self, account_id, armed: true };
        let mut flush_error: Option<String>;
        // Filled folder by folder, across attempts: what INBOX brought before a later folder failed
        // still goes through the rules, the screener and the notifications below.
        let mut report = Report::default();
        if self.single_connection(account_id) {
            report.waiting = Some(self.waiting_on(account_id));
        }
        // Before this sync moves it: what reached the server since then just arrived.
        let previous_sync = self.store.account(account_id).ok().and_then(|a| a.last_sync);
        let mut steps = crate::perf::Steps::new(|| format!("account {account_id} sync ({scope:?})"));
        // A connection dropped by the server under the sync (some hosts cut sessions when the mailbox
        // has too many) gets one more try on a new one before the user hears of it.
        let mut attempt = 0;
        let res = loop {
            attempt += 1;
            // Only the last replay counts: one that went through on the new connection clears it.
            flush_error = None;
            let res = async {
                let mut guard = self.session(account_id, Purpose::Sync).await?;
                steps.mark("session");
                let account = self.store.account(account_id)?;
                // The replay lock is held only while replaying: holding it for the whole sync would block
                // actions and their undo. A failing operation must not deprive the account of new mail: noted,
                // and the sync goes on. A replay holding the lock for long (a large purge) is not waited for:
                // the sync copes with operations still queued.
                let flushed = match tokio::time::timeout(FLUSH_LOCK_WAIT, lock.lock()).await {
                    Err(_) => {
                        tracing::info!("sync of account {account_id}: queue replay busy, sync without it");
                        0
                    }
                    Ok(held) => {
                        let conn = guard.as_mut().expect("session open");
                        let res = async {
                            let caps = conn.caps().await?;
                            crate::ops::flush(&self.store, account_id, &mut conn.session, &caps).await
                        }
                        .await;
                        // Released before any reconnection below: a connection slot is never awaited with the replay
                        // lock held. Every other path takes the slot first, then the lock; the reverse order
                        // deadlocks with a sync queued on this slot.
                        drop(held);
                        match res {
                            Ok(n) => n,
                            Err(e) => {
                                tracing::warn!("queue replay (account {account_id}): {e}");
                                flush_error = Some(e.to_string());
                                self.emit(Event::OpsChanged);
                                // Interrupted command: the connection is in an uncertain state, take a new one.
                                *guard = None;
                                drop(guard);
                                guard = self.session(account_id, Purpose::Sync).await?;
                                0
                            }
                        }
                    }
                };
                steps.mark("flush");
                let conn = guard.as_mut().expect("session open");
                let res = async {
                    let caps = conn.caps().await?;
                    if let Err(e) = self.store.set_provider(account_id, crate::provider::Provider::from_caps(&caps)) {
                        tracing::info!("provider of account {account_id} not kept: {e}");
                    }
                    let session = &mut conn.session;
                    let mut on_progress = |p: sync::Progress| {
                        self.emit(Event::SyncProgress { account_id, mailbox: p.mailbox, done: p.done, total: p.total });
                    };
                    // Each changed folder shows without waiting for the next ones.
                    let mut on_folder = || self.emit(Event::Changed { account_id });
                    // Each command has its own bound: this one only catches a sync that never ends. A first sync
                    // of a large account can take hours; what it stored is kept for the next one.
                    tokio::time::timeout(
                        Duration::from_secs(6 * 3600),
                        sync::sync_account_into(&self.store, &account, session, &caps, flushed, scope.clone(), &mut on_progress, &mut on_folder, &mut report),
                    )
                    .await
                    .map_err(|_| {
                        Error::Other(crate::tr!(
                            "la synchronisation a pris trop de temps : elle continuera à la prochaine",
                            "the sync took too long: it will continue at the next one"
                        ))
                    })?
                }
                .await;
                steps.mark("folders");
                if res.is_err() {
                    *guard = None;
                }
                res
            }
            .await;
            match res {
                Err(e) if attempt == 1 && e.is_connection_drop() => {
                    tracing::info!("sync of account {account_id}: {e}; trying again on a new connection");
                    self.note_cut(account_id, false);
                    tokio::time::sleep(SYNC_RETRY_AFTER).await;
                }
                res => break res,
            }
        };
        if res.as_ref().is_err_and(|e| e.is_connection_drop()) {
            self.note_cut(account_id, false);
        }
        let (n, inbox, error) = match &res {
            Ok(()) => (report.new_messages, report.new_in_inbox, None),
            Err(e) => (0, 0, Some(e.to_string())),
        };
        self.note_queue_error(account_id, error.clone().or(flush_error));
        // Blocked senders: their new messages go to junk, without notification.
        let blocked = self.apply_blocklist(account_id).unwrap_or_default();
        if res.is_ok() {
            self.after_sync(account_id);
        }
        // What the rules file or mark read gets no notification. Even after an error: the arrivals
        // stored before it will not be reported again.
        let ruled = {
            let fresh: Vec<i64> = report.new_inbox_ids.iter().copied().filter(|id| !blocked.contains(id)).collect();
            self.apply_rules(account_id, &fresh, None).unwrap_or_else(|e| {
                tracing::warn!("rules of account {account_id}: {e}");
                Ruled::default()
            })
        };
        // Screener: what is left in the inbox from unknown senders waits for a decision. The arrivals
        // were stored out of the lists until now; so were any a previous sync left undecided (facteur
        // closed meanwhile): they go through it too, then show.
        let waiting = self.store.unscreened(account_id).unwrap_or_default();
        let (held, refused) = {
            let mut left: Vec<i64> = report.new_inbox_ids.iter().copied().filter(|id| !blocked.contains(id) && !ruled.moved.contains(id)).collect();
            let more: Vec<i64> = waiting.iter().copied().filter(|id| !left.contains(id) && !blocked.contains(id) && !ruled.moved.contains(id)).collect();
            left.extend(more);
            self.screen(account_id, &left).unwrap_or_else(|e| {
                tracing::warn!("screener of account {account_id}: {e}");
                Default::default()
            })
        };
        if let Err(e) = self.store.clear_unscreened(&waiting) {
            tracing::warn!("screener of account {account_id}: {e}");
        }
        // Once rules and screener have filed them (junk and refused senders are left alone).
        let received: Vec<i64> = report.received_ids.iter().copied().filter(|id| !blocked.contains(id) && !refused.contains(id)).collect();
        let arrived_since = (crate::store::now() - DKIM_ARRIVAL).min(previous_sync.map_or(i64::MAX, |t| t - 600));
        self.check_dkim(&received, arrived_since);
        let arrived: Vec<i64> = report.new_inbox_ids.iter().copied().filter(|id| !blocked.contains(id) && !ruled.contains(id) && !refused.contains(id)).collect();
        self.tell_new_mail(account_id, &arrived, &held);
        // No change, no reload of the interface; after an error the sync may have stopped halfway, so
        // reload to be safe.
        let changed = res.is_err() || report.changed;
        finished.armed = false;
        steps.mark("after");
        drop(steps);
        run.finish(error.as_deref(), n);
        self.emit(Event::SyncFinished { account_id, new_messages: n, new_in_inbox: inbox, error });
        if changed {
            self.emit(Event::Changed { account_id });
        }
        res.map(|()| report)
    }

    /// So that a long backup blocks neither the sync nor reading. With one connection for
    /// everything, the account's own is taken instead (queued behind the sync): a second one
    /// would bring back the cut sessions that mode is there to avoid.
    async fn dedicated_session(&self, account: &Account) -> Result<Dedicated<'_>> {
        let held = if self.single_connection(account.id) {
            Held::Shared(self.session(account.id, Purpose::Background).await?)
        } else {
            Held::Own(self.login(account).await?)
        };
        Ok(Dedicated { engine: self, account_id: account.id, held: Some(held) })
    }

    /// The interface may have been reloaded during the operation: the engine is the reference.
    pub fn backup_busy(&self) -> Vec<i64> {
        self.archiving.lock().unwrap().iter().copied().collect()
    }

    async fn exclusive<T>(&self, account_id: i64, job: impl Future<Output = Result<T>>) -> Result<T> {
        let Some(_busy) = Busy::take(&self.archiving, account_id) else {
            return Err(Error::Other(crate::tr!("une sauvegarde ou une restauration de ce compte est déjà en cours", "a backup or restore of this account is already in progress")));
        };
        job.await
    }

    /// Full and incremental (see [`backup`]).
    pub async fn backup(&self, account_id: i64, dest: &Path) -> Result<backup::BackupStats> {
        let account = self.store.account(account_id)?;
        let res = self
            .exclusive(account_id, async {
                let mut session = self.dedicated_session(&account).await?;
                let root = backup::account_dir(dest, &account.email);
                let stats = backup::backup_account(&mut session, &account.email, &root, |p| {
                    self.emit(Event::BackupProgress { account_id, mailbox: p.mailbox, done: p.done, total: p.total });
                })
                .await;
                session.finish().await;
                stats
            })
            .await;
        let (downloaded, messages, error) = match &res {
            Ok(s) => (s.downloaded, s.messages, None),
            Err(e) => (0, 0, Some(e.to_string())),
        };
        self.emit(Event::BackupFinished { account_id, downloaded, messages, error });
        res
    }

    pub fn backup_status(&self, dest: &Path) -> Result<Vec<BackupStatus>> {
        Ok(self
            .store
            .list_accounts()?
            .into_iter()
            .map(|a| BackupStatus {
                account_id: a.id,
                manifest: backup::read_manifest(&backup::account_dir(dest, &a.email)),
                email: a.email,
            })
            .collect())
    }

    /// Including backups of accounts removed from facteur.
    pub fn list_backups(&self, dest: &Path) -> Result<Vec<backup::Manifest>> {
        let mut out: Vec<backup::Manifest> = match std::fs::read_dir(dest) {
            Ok(entries) => entries.flatten().filter_map(|e| backup::read_manifest(&e.path())).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        out.sort_by(|a, b| a.email.cmp(&b.email));
        Ok(out)
    }

    pub async fn backup_catalog(
        &self,
        dest: &Path,
        email: &str,
        filter: backup::RestoreFilter,
        limit: usize,
        offset: usize,
    ) -> Result<backup::Catalog> {
        let root = backup::account_dir(dest, email);
        // Reading the index (and maybe catching up): blocking disk work.
        tokio::task::spawn_blocking(move || backup::catalog(&root, &filter, limit, offset))
            .await
            .map_err(|e| Error::Other(e.to_string()))?
    }

    pub async fn backup_message(&self, dest: &Path, email: &str, id: i64) -> Result<BackupMessage> {
        let root = backup::account_dir(dest, email);
        let raw = tokio::task::spawn_blocking(move || backup::read_entry(&root, id))
            .await
            .map_err(|e| Error::Other(e.to_string()))??;
        let parsed = parse_body(raw).await?;
        Ok(BackupMessage {
            subject: parsed.headers.subject,
            from: parsed.headers.from,
            to: parsed.headers.to,
            cc: parsed.headers.cc,
            date: parsed.headers.date,
            html: parsed.html,
            is_plain: parsed.is_plain,
            has_remote_content: parsed.has_remote_content,
            attachments: parsed.attachments,
        })
    }

    /// The source may be the same account or another one (migration, recreated account).
    pub async fn restore(
        &self,
        dest: &Path,
        source_email: &str,
        account_id: i64,
        request: RestoreRequest,
    ) -> Result<backup::RestoreStats> {
        let account = self.store.account(account_id)?;
        let root = backup::account_dir(dest, source_email);
        if backup::read_manifest(&root).is_none() {
            return Err(Error::NotFound(crate::tr!("aucune sauvegarde de {source_email} dans {}", "no backup of {source_email} in {}", dest.display())));
        }
        let selection = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || backup::select(&root, &request.filter, request.ids.as_deref()))
                .await
                .map_err(|e| Error::Other(e.to_string()))??
        };
        if selection.is_empty() {
            return Err(Error::Other(crate::tr!("aucun message ne correspond à la sélection", "no message matches the selection")));
        }
        let stats = self
            .exclusive(account_id, async {
                let mut session = self.dedicated_session(&account).await?;
                let pattern = crate::tr!("Restauration %Y-%m-%d %Hh%M", "Restore %Y-%m-%d %Hh%M");
                let prefix = chrono::Local::now().format(&pattern).to_string();
                let stats = backup::restore(&mut session, &root, selection, request.target, &prefix, |p| {
                    self.emit(Event::RestoreProgress { account_id, mailbox: p.mailbox, done: p.done, total: p.total });
                })
                .await;
                session.finish().await;
                stats
            })
            .await?;
        // Restored messages and folders show at the next sync.
        self.emit(Event::Changed { account_id });
        Ok(stats)
    }

    pub fn calendars(&self) -> Result<Vec<CalendarInfo>> {
        self.store.calendars()
    }

    /// Occurrences overlapping [`from`, `to`) (Unix seconds) in the shown calendars, by start.
    pub fn calendar_events(&self, from: i64, to: i64) -> Result<Vec<CalendarEvent>> {
        self.store.calendar_events(from, to)
    }

    /// A calendar shown again has its events fetched at once.
    pub fn set_calendar_enabled(self: &Arc<Self>, id: i64, enabled: bool) -> Result<()> {
        let account_id = self.store.set_calendar_enabled(id, enabled)?;
        self.emit(Event::CalendarChanged);
        if enabled {
            let this = self.clone();
            self.spawn(async move { this.sync_calendars(Some(account_id)).await });
        }
        Ok(())
    }

    pub fn calendar_status(&self) -> Result<Vec<CalendarStatus>> {
        let mut settings = self.store.calendar_accounts()?;
        let fetches = self.store.calendar_fetches()?;
        Ok(self
            .accounts()?
            .into_iter()
            .map(|account| {
                let s = settings.remove(&account.id).unwrap_or_default();
                let caldav_url = s.caldav_url.clone().or(s.home_url.clone());
                let (state, message) = match (calendar_support(&account), s.state) {
                    (Err(why), _) => (CalendarState::Unavailable, Some(why)),
                    (Ok(_), Some(state)) => (state, s.message),
                    // Not synced yet.
                    (Ok(CalendarSource::Google | CalendarSource::Microsoft), None) => (CalendarState::Ok, None),
                    (Ok(CalendarSource::Caldav), None) if caldav_url.is_some() => (CalendarState::Ok, None),
                    (Ok(CalendarSource::Caldav), None) => (CalendarState::NotConfigured, None),
                };
                let (synced_from, synced_to) = synced_span(fetches.get(&account.id).copied());
                CalendarStatus { account_id: account.id, state, message, caldav_url, last_sync: s.last_sync, synced_from, synced_to, ignored: s.ignored }
            })
            .collect())
    }

    /// Ignored, its calendars leave the calendar view and it is no longer checked; used again, its
    /// calendars are looked for at once.
    pub fn set_calendar_ignored(self: &Arc<Self>, account_id: i64, ignored: bool) -> Result<()> {
        self.store.set_calendar_ignored(account_id, ignored)?;
        self.calendar_checked.lock().unwrap().remove(&account_id);
        self.emit(Event::CalendarChanged);
        if !ignored {
            let this = self.clone();
            self.spawn(async move { this.sync_calendars(Some(account_id)).await });
        }
        Ok(())
    }

    /// `None`: found by discovery again. A sync under way drops what it finds (the settings'
    /// generation moved on) and runs again.
    pub async fn set_caldav_url(self: &Arc<Self>, account_id: i64, url: Option<String>) -> Result<()> {
        let account = self.store.account(account_id)?;
        if calendar_support(&account) != Ok(CalendarSource::Caldav) {
            return Err(Error::Other(crate::tr!("ce compte n'utilise pas CalDAV", "this account does not use CalDAV")));
        }
        let url = match url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => Some(crate::caldav::check_url(u)?.to_string()),
            None => None,
        };
        self.store.set_caldav_url(account_id, url.as_deref())?;
        self.sync_calendars(Some(account_id)).await
    }

    /// Problems are kept in the account's calendar status, not returned. Asked for by the user, so
    /// every account is checked, even one waiting for its next recheck.
    pub async fn sync_calendars(self: &Arc<Self>, account_id: Option<i64>) -> Result<()> {
        self.sync_calendars_with(account_id, false).await
    }

    async fn sync_calendars_with(self: &Arc<Self>, account_id: Option<i64>, periodic: bool) -> Result<()> {
        let accounts = match account_id {
            Some(id) => vec![self.store.account(id)?],
            None => self.accounts()?,
        };
        let jobs: Vec<_> = accounts
            .into_iter()
            .map(|account| {
                let this = self.clone();
                // The periodic sync rechecks an account without calendar access only now and then; one asked
                // for (refresh, reconnected, an address saved) always runs.
                self.spawn(async move {
                    if periodic && this.calendar_check_waits(account.id) {
                        return false;
                    }
                    this.sync_account_calendars(&account).await
                })
            })
            .collect();
        let mut changed = false;
        for job in jobs {
            match job.await {
                Ok(c) => changed |= c,
                Err(e) => tracing::error!("calendar sync stopped by an internal error: {e}"),
            }
        }
        if changed {
            self.emit(Event::CalendarChanged);
        }
        Ok(())
    }

    /// No calendar access (to reconnect, no CalDAV server found) and checked less than
    /// CALENDAR_RECHECK ago.
    fn calendar_check_waits(&self, account_id: i64) -> bool {
        let blocked = self
            .store
            .calendar_account(account_id)
            .is_ok_and(|s| matches!(s.state, Some(CalendarState::NeedsReconnect | CalendarState::NotConfigured)));
        let mut checked = self.calendar_checked.lock().unwrap();
        if !blocked {
            checked.remove(&account_id);
            return false;
        }
        match checked.get(&account_id) {
            Some(at) if at.elapsed() < CALENDAR_RECHECK => true,
            _ => {
                checked.insert(account_id, Instant::now());
                false
            }
        }
    }

    /// Returns whether calendars, events or the account's state changed.
    async fn sync_account_calendars(&self, account: &Account) -> bool {
        let Ok(source) = calendar_support(account) else { return false };
        if self.store.calendar_account(account.id).is_ok_and(|s| s.ignored) {
            return false;
        }
        // Already under way: that sync runs once more when done.
        let Some(mut run) = CalendarRun::start(&self.calendar_syncing, account.id) else { return false };
        let mut changed = false;
        loop {
            changed |= self.sync_account_calendars_once(account, source).await;
            if !run.again() {
                return changed;
            }
        }
    }

    async fn sync_account_calendars_once(&self, account: &Account, source: CalendarSource) -> bool {
        use std::sync::atomic::{AtomicBool, Ordering};
        let settings = match self.store.calendar_account(account.id) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("calendar settings of account {}: {e}", account.id);
                return false;
            }
        };
        // Ignored since the sync was asked for (a pass run again, see `CalendarRun`): the server is not
        // contacted for it any more.
        if settings.ignored {
            return false;
        }
        let changed = AtomicBool::new(false);
        let what = crate::tr!("synchro des agendas", "calendar sync");
        let outcome = match source {
            CalendarSource::Google => {
                let mut res = bounded(CALENDAR_TIMEOUT, &what, self.sync_google_calendars(account, &changed)).await;
                // A 401 with a token not expired yet (revoked, the clock was off): one fresh token, once,
                // before asking the user to reconnect. Not when the account stands refused.
                if matches!(res, Err(Error::Auth(_))) && !self.auth_refused.lock().unwrap().contains_key(&account.id) {
                    self.forget_token(account.id, None);
                    res = bounded(CALENDAR_TIMEOUT, &what, self.sync_google_calendars(account, &changed)).await;
                }
                res
            }
            CalendarSource::Caldav => bounded(CALENDAR_TIMEOUT, &what, self.sync_caldav_calendars(account, &settings, &changed)).await,
            CalendarSource::Microsoft => {
                let mut res = bounded(CALENDAR_TIMEOUT, &what, self.sync_outlook_calendars(account, &changed)).await;
                if matches!(res, Err(Error::Auth(_))) {
                    self.calendar_tokens.lock().unwrap().remove(&account.id);
                    res = bounded(CALENDAR_TIMEOUT, &what, self.sync_outlook_calendars(account, &changed)).await;
                }
                res
            }
        };
        let (state, message) = match outcome {
            Ok(outcome) => outcome,
            Err(Error::Auth(m)) if matches!(source, CalendarSource::Google | CalendarSource::Microsoft) => (CalendarState::NeedsReconnect, Some(m)),
            // The CalDAV server refuses the mail password (an app password, another server's address):
            // retried only now and then (see `calendar_check_waits`), so a refusal every quarter of an hour
            // never gets the address banned. Saving an address or asking for a sync still tries at once.
            Err(Error::Auth(m)) => (CalendarState::NotConfigured, Some(m)),
            Err(e) => (CalendarState::Error, Some(e.to_string())),
        };
        // Written only while the settings are those the sync started with.
        let state_changed = self.store.set_calendar_state(account.id, state, message.as_deref(), settings.generation).unwrap_or_else(|e| {
            tracing::warn!("calendar state of account {}: {e}", account.id);
            false
        });
        // Logged when it changes, not on every sync.
        if let Some(m) = &message {
            if state_changed {
                tracing::warn!("calendars of account {}: {m}", account.id);
            } else {
                tracing::debug!("calendars of account {}: {m}", account.id);
            }
        }
        changed.load(Ordering::Relaxed) || state_changed
    }

    async fn sync_google_calendars(&self, account: &Account, changed: &std::sync::atomic::AtomicBool) -> Result<(CalendarState, Option<String>)> {
        use std::sync::atomic::Ordering;
        let Credential::Token(token) = self.credential(account).await? else {
            return Err(Error::Other("Google: OAuth2".into()));
        };
        let list = crate::gcal::calendars(&token).await?;
        let (rows, list_changed) = self.store.sync_calendar_list(account.id, CalendarSource::Google, &list)?;
        changed.fetch_or(list_changed, Ordering::Relaxed);
        let (from, to) = calendar_window();
        let fresh_after = crate::store::now() - CALENDAR_REFETCH;
        let mut outcome = CalendarFetches::default();
        for row in rows.iter().filter(|r| r.enabled) {
            // Nothing changed since the last fetch, and the window has not moved much since.
            if let Some(at) = row.synced_at.filter(|t| *t > fresh_after) {
                match crate::gcal::changed_since(&token, &row.remote_id, at - CHANGES_MARGIN).await {
                    Ok(false) => {
                        outcome.synced += 1;
                        continue;
                    }
                    Ok(true) => {}
                    Err(e @ Error::Auth(_)) => return Err(e),
                    Err(e) => tracing::debug!("Google calendar {} of account {}: changes not checked: {e}", row.id, account.id),
                }
            }
            match crate::gcal::events(&token, &row.remote_id, from, to).await {
                Ok(events) => {
                    changed.fetch_or(self.store.replace_calendar_events(row.id, account.id, &row.remote_id, &events, None)?, Ordering::Relaxed);
                    outcome.synced += 1;
                }
                Err(e @ Error::Auth(_)) => return Err(e),
                // One calendar refused (a subscription gone): the others are still synced.
                Err(e) => {
                    tracing::warn!("Google calendar {} of account {}: {e}", row.id, account.id);
                    changed.fetch_or(self.store.set_calendar_error(row.id, account.id, &row.remote_id, Some(&e.to_string()))?, Ordering::Relaxed);
                    outcome.failed.push((row.name.clone(), e));
                }
            }
        }
        outcome.result()
    }

    /// Through Graph, with a token of their own.
    async fn sync_outlook_calendars(&self, account: &Account, changed: &std::sync::atomic::AtomicBool) -> Result<(CalendarState, Option<String>)> {
        use std::sync::atomic::Ordering;
        let token = self.calendar_token(account, false).await?;
        let list = crate::mscal::calendars(&token).await?;
        let (rows, list_changed) = self.store.sync_calendar_list(account.id, CalendarSource::Microsoft, &list)?;
        changed.fetch_or(list_changed, Ordering::Relaxed);
        let (from, to) = calendar_window();
        let fresh_after = crate::store::now() - CALENDAR_REFETCH;
        let mut outcome = CalendarFetches::default();
        for row in rows.iter().filter(|r| r.enabled) {
            // Graph has no change token for any calendar but the main one: the ids and change times
            // of the window, compared with the last fetch's, say whether it must be read again.
            let fingerprint = match crate::mscal::fingerprint(&token, &row.remote_id, from, to).await {
                Ok(f) => Some(f),
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(e) => {
                    tracing::debug!("Outlook calendar {} of account {}: changes not checked: {e}", row.id, account.id);
                    None
                }
            };
            if fingerprint.is_some() && fingerprint == row.ctag && row.synced_at.is_some_and(|t| t > fresh_after) {
                outcome.synced += 1;
                continue;
            }
            match crate::mscal::events(&token, &row.remote_id, from, to).await {
                Ok(events) => {
                    changed.fetch_or(self.store.replace_calendar_events(row.id, account.id, &row.remote_id, &events, fingerprint.as_deref())?, Ordering::Relaxed);
                    outcome.synced += 1;
                }
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(e) => {
                    tracing::warn!("Outlook calendar {} of account {}: {e}", row.id, account.id);
                    changed.fetch_or(self.store.set_calendar_error(row.id, account.id, &row.remote_id, Some(&e.to_string()))?, Ordering::Relaxed);
                    outcome.failed.push((row.name.clone(), e));
                }
            }
        }
        outcome.result()
    }

    /// Kept apart from the mail's: a refusal only concerns the calendars, the mail goes on. An
    /// account signed in when facteur only read the calendars gets a read-only token; `write`
    /// then asks it to reconnect.
    async fn calendar_token(&self, account: &Account, write: bool) -> Result<String> {
        if let Some((token, expires_at, writable)) = self.calendar_tokens.lock().unwrap().get(&account.id).cloned()
            && expires_at > SystemTime::now() + Duration::from_secs(60)
            && (writable || !write)
        {
            return Ok(token);
        }
        // Behind the mail's refresh, which may rotate the refresh token.
        let lock = self.oauth_locks.lock().unwrap().entry(account.id).or_default().clone();
        let _refreshing = lock.lock().await;
        // Refreshed meanwhile by another request that held the lock.
        if let Some((token, expires_at, writable)) = self.calendar_tokens.lock().unwrap().get(&account.id).cloned()
            && expires_at > SystemTime::now() + Duration::from_secs(60)
            && (writable || !write)
        {
            return Ok(token);
        }
        let refresh_token = self.secret(account.id).await?;
        // An account known to read only is not asked for writing at every refresh, only for a write.
        let read_only = !write && self.calendar_read_only.lock().unwrap().contains(&account.id);
        let first = if read_only { oauth::refresh_calendar(&refresh_token, false).await.map(|t| (t, false)) } else {
            oauth::refresh_calendar(&refresh_token, true).await.map(|t| (t, true))
        };
        let (fresh, writable) = match first {
            Ok(fresh) => fresh,
            Err(Error::Auth(_)) if write => return Err(crate::mscal::needs_write_consent()),
            Err(Error::Auth(_)) if read_only => return Err(crate::mscal::needs_consent()),
            Err(Error::Auth(_)) => match oauth::refresh_calendar(&refresh_token, false).await {
                Ok(fresh) => (fresh, false),
                Err(Error::Auth(_)) => return Err(crate::mscal::needs_consent()),
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        };
        self.keep_rotated_refresh(account.id, &refresh_token, &fresh).await;
        if writable {
            self.calendar_read_only.lock().unwrap().remove(&account.id);
        } else {
            self.calendar_read_only.lock().unwrap().insert(account.id);
        }
        self.calendar_tokens.lock().unwrap().insert(account.id, (fresh.access_token.clone(), fresh.expires_at, writable));
        Ok(fresh.access_token)
    }

    async fn keep_rotated_refresh(&self, account_id: i64, refresh_token: &str, fresh: &oauth::Tokens) {
        if let Some(new_refresh) = fresh.refresh_token.as_deref().filter(|t| *t != refresh_token)
            && let Err(e) = self.store_secret(account_id, new_refresh).await
        {
            tracing::error!("account {account_id}: new refresh token not saved to the keychain (retried later): {e}");
            self.secret_cache.lock().unwrap().insert(account_id, new_refresh.to_string());
            self.unsaved_secrets.lock().unwrap().insert(account_id);
        }
    }

    /// Microsoft: a Graph token of its own, asked for at each (rare) address book sync.
    async fn contacts_token(&self, account: &Account) -> Result<String> {
        let lock = self.oauth_locks.lock().unwrap().entry(account.id).or_default().clone();
        let _refreshing = lock.lock().await;
        let refresh_token = self.secret(account.id).await?;
        let fresh = match oauth::refresh_contacts(&refresh_token).await {
            Ok(fresh) => fresh,
            Err(Error::Auth(_)) => return Err(crate::contacts::needs_consent()),
            Err(e) => return Err(e),
        };
        self.keep_rotated_refresh(account.id, &refresh_token, &fresh).await;
        Ok(fresh.access_token)
    }

    pub fn address_book_status(&self) -> Result<Vec<AddressBookStatus>> {
        self.store.address_book_status()
    }

    /// Address books change little: each is read again after `CONTACTS_EVERY`, unless `asked`
    /// (by the user, after a reconnection). Problems are kept in the account's status.
    pub async fn sync_address_books(self: &Arc<Self>, account_id: Option<i64>, asked: bool) -> Result<()> {
        let accounts = match account_id {
            Some(id) => vec![self.store.account(id)?],
            None => self.accounts()?,
        };
        let checks = self.store.address_book_checks()?;
        let now = crate::store::now();
        let what = crate::tr!("synchro des contacts", "contacts sync");
        let mut changed = false;
        for account in accounts {
            // A failure (offline, a server down) is tried again soon; an account without an address
            // book, or to reconnect, every few hours.
            let due = |(at, state): &(i64, CalendarState)| now - at >= if *state == CalendarState::Error { CONTACTS_RETRY } else { CONTACTS_EVERY };
            if !asked && checks.get(&account.id).is_some_and(|c| !due(c)) {
                continue;
            }
            let oauth = matches!(account.auth, AuthMethod::OAuth2 { .. });
            let (state, message, synced) = match bounded(CALENDAR_TIMEOUT, &what, self.fetch_address_book(&account)).await {
                Ok(Ok(contacts)) => match self.store.replace_address_book(account.id, &account.email, &contacts) {
                    Ok(()) => (CalendarState::Ok, None, true),
                    // Removed meanwhile.
                    Err(e) => {
                        tracing::debug!("address book of account {}: {e}", account.id);
                        continue;
                    }
                },
                Ok(Err((state, why))) => (state, Some(why), false),
                Err(Error::Auth(m)) if oauth => (CalendarState::NeedsReconnect, Some(m), false),
                Err(Error::Auth(m)) => (CalendarState::NotConfigured, Some(m), false),
                Err(e) => (CalendarState::Error, Some(e.to_string()), false),
            };
            if let Some(m) = &message {
                tracing::info!("address book of account {}: {m}", account.id);
            }
            if let Err(e) = self.store.set_address_book_state(account.id, state, message.as_deref(), synced) {
                tracing::debug!("address book of account {}: {e}", account.id);
                continue;
            }
            changed = true;
        }
        if changed {
            self.emit(Event::CalendarChanged);
        }
        Ok(())
    }

    /// `Ok(Err(..))`: the account has no address book facteur can read, and why.
    async fn fetch_address_book(&self, account: &Account) -> Result<std::result::Result<Vec<crate::contacts::Contact>, (CalendarState, String)>> {
        match account.auth {
            AuthMethod::OAuth2 { provider: OAuthProvider::Google } => Ok(Ok(crate::contacts::google(&self.google_token(account).await?).await?)),
            AuthMethod::OAuth2 { provider: OAuthProvider::Microsoft } => Ok(Ok(crate::contacts::outlook(&self.contacts_token(account).await?).await?)),
            AuthMethod::Password if crate::autoconfig::registrable(&account.imap.host) == "gmail.com" => Ok(Err((
                CalendarState::Unavailable,
                crate::tr!(
                    "contacts Google : connectez le compte avec Google plutôt qu'un mot de passe",
                    "Google contacts: sign the account in with Google instead of a password"
                ),
            ))),
            AuthMethod::Password => {
                let settings = self.store.calendar_account(account.id)?;
                let client = self.caldav_client(account, &settings).await?;
                // The calendar server, proven to be a DAV server, often serves the contacts too.
                let dav = match settings.caldav_url.as_deref() {
                    Some(u) => Some(crate::caldav::check_url(u)?),
                    None => settings.home_url.as_deref().and_then(|u| reqwest::Url::parse(u).ok()),
                };
                let candidates = crate::caldav::card_candidates(&account.email, &account.imap.host, dav.as_ref());
                let Some(home) = client.find_address_book(&candidates).await? else {
                    return Ok(Err((
                        CalendarState::NotConfigured,
                        crate::tr!("aucun carnet d'adresses CardDAV trouvé", "no CardDAV address book found"),
                    )));
                };
                // A book the server cannot give (too large, refused) does not keep the others out.
                let books = client.address_books(&home).await?;
                let mut contacts = Vec::new();
                let mut failed = None;
                for book in &books {
                    match client.cards(book).await {
                        Ok(cards) => contacts.extend(cards),
                        Err(e @ Error::Auth(_)) => return Err(e),
                        Err(e) => {
                            tracing::info!("address book {book} of account {}: {e}", account.id);
                            failed = Some(e);
                        }
                    }
                }
                match failed {
                    Some(e) if contacts.is_empty() => Err(e),
                    _ => Ok(Ok(contacts)),
                }
            }
        }
    }

    /// The password goes to the same sites as for the sync (see `sync_caldav_calendars`).
    async fn caldav_client(&self, account: &Account, settings: &crate::store::CalendarAccount) -> Result<crate::caldav::Client> {
        let Credential::Password(password) = self.credential(account).await? else {
            return Err(Error::Other("CalDAV: password".into()));
        };
        let mut client = crate::caldav::Client::new(&account.username, &password)?;
        for site in crate::caldav::auto_sites(&account.email, &account.imap.host) {
            client.trust_site(&site);
        }
        if let Some(url) = settings.caldav_url.as_deref().map(crate::caldav::check_url).transpose()? {
            client.trust(&url);
        }
        Ok(client)
    }

    fn event_writer(&self, account_id: i64, source: CalendarSource) -> Result<Account> {
        let account = self.store.account(account_id)?;
        if calendar_support(&account) != Ok(source) {
            return Err(Error::Other(crate::tr!("cet agenda n'est plus disponible", "this calendar is no longer available")));
        }
        Ok(account)
    }

    /// Creates an event in a writable calendar; returns its row. Shown at once, the sync then
    /// confirms it.
    /// `key`: chosen by the form when it opens (16 to 64 lowercase hex digits), the same for every
    /// attempt: an attempt whose answer was lost and is made again finds the event made the first
    /// time instead of making a second one.
    pub async fn create_event(self: &Arc<Self>, calendar_id: i64, draft: EventDraft, key: &str) -> Result<Option<i64>> {
        if !(16..=64).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(Error::Other(format!("event key {key}")));
        }
        let draft = checked_draft(draft)?;
        let calendar = self.store.calendar_target(calendar_id)?;
        if calendar.read_only {
            return Err(Error::Other(crate::tr!("cet agenda est en lecture seule", "this calendar is read only")));
        }
        let account = self.event_writer(calendar.account_id, calendar.source)?;
        let changes = crate::calendar::EventChanges::all(&draft).organized_by(organizer_of(&account));
        let what = crate::tr!("création de l'événement", "event creation");
        let mut told = Ok(());
        let event = match calendar.source {
            CalendarSource::Google => {
                let token = self.google_token(&account).await?;
                bounded(EVENT_TIMEOUT, &what, crate::gcal::create(&token, &calendar.remote_id, key, &changes, draft.repeat.as_ref())).await?
            }
            CalendarSource::Microsoft => {
                let token = self.calendar_token(&account, true).await?;
                bounded(EVENT_TIMEOUT, &what, crate::mscal::create(&token, &calendar.remote_id, key, &changes, draft.repeat.as_ref())).await?
            }
            CalendarSource::Caldav => {
                let settings = self.store.calendar_account(account.id)?;
                let client = self.caldav_client(&account, &settings).await?;
                let url = reqwest::Url::parse(&calendar.remote_id).map_err(|e| Error::Other(e.to_string()))?;
                let uid = format!("{key}@facteur");
                let ics = crate::calendar::new_event(&uid, &draft, Some(organizer_of(&account)), crate::store::now()).ok_or_else(|| Error::Other("iCalendar".into()))?;
                let href = bounded(EVENT_TIMEOUT, &what, client.create(&url, &uid, &ics)).await?;
                let guests: Vec<EventAttendee> = changes.guests().into_iter().cloned().collect();
                told = self.tell_guests(&account, &client, &url, &ics, &[], &guests).await;
                written_instance(&ics, href.as_str(), &draft)?
            }
        };
        // Made on the provider already: an error now would have the user make it twice. A cache that
        // cannot follow (the calendar removed meanwhile) is put right by the sync, as are the
        // dates of a new series.
        let id = if draft.repeat.is_some() {
            None
        } else {
            self.store.put_calendar_event(calendar_id, &event).inspect_err(|e| tracing::warn!("new event not cached: {e}")).ok()
        };
        self.after_event_change(account.id);
        told?;
        Ok(id)
    }

    /// Without server-side scheduling (RFC 6638), a CalDAV event's guests learn of it by mail:
    /// `ics` (as written) to those invited now, its cancellation to those `before` and no longer.
    async fn tell_guests(self: &Arc<Self>, account: &Account, client: &crate::caldav::Client, calendar: &reqwest::Url, ics: &str, before: &[EventAttendee], now: &[EventAttendee]) -> Result<()> {
        let gone: Vec<EventAttendee> = before.iter().filter(|b| !b.organizer && !now.iter().any(|n| n.email.eq_ignore_ascii_case(&b.email))).cloned().collect();
        let invited: Vec<EventAttendee> = now.iter().filter(|a| !a.organizer).cloned().collect();
        if (gone.is_empty() && invited.is_empty()) || client.auto_schedules(calendar).await {
            return Ok(());
        }
        let stamp = crate::store::now();
        let res = async {
            if !invited.is_empty() {
                let request = crate::calendar::invitation(ics, "REQUEST", stamp).ok_or_else(|| Error::Other("iCalendar".into()))?;
                self.mail_invitation(account, &request, &invited, !before.is_empty()).await?;
            }
            if !gone.is_empty() {
                let cancel = crate::calendar::invitation(ics, "CANCEL", stamp).ok_or_else(|| Error::Other("iCalendar".into()))?;
                self.mail_invitation(account, &cancel, &gone, false).await?;
            }
            Ok::<_, Error>(())
        }
        .await;
        res.map_err(|e| {
            Error::Other(crate::tr!(
                "événement enregistré, mais les invitations ne sont pas parties : {e}",
                "event saved, but the invitations were not sent: {e}"
            ))
        })
    }

    /// An iTIP message (`ics`, its METHOD set) mailed to guests. `update`: a change to an event
    /// they were invited to already.
    async fn mail_invitation(self: &Arc<Self>, account: &Account, ics: &str, to: &[EventAttendee], update: bool) -> Result<()> {
        let invite = crate::calendar::parse(ics).ok_or_else(|| Error::Other(crate::tr!("invitation illisible", "unreadable invitation")))?;
        let when = invite
            .start
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|d| {
                let d = d.with_timezone(&chrono::Local);
                if invite.all_day { d.format("%d/%m/%Y").to_string() } else { d.format("%d/%m/%Y %H:%M").to_string() }
            })
            .unwrap_or_default();
        let summary = invite.summary.clone();
        let (subject, body) = match (invite.method.as_str(), update) {
            ("CANCEL", _) => (
                crate::tr!("Annulé : {summary}", "Cancelled: {summary}"),
                crate::tr!("L'événement « {summary} » ({when}) est annulé.", "The event “{summary}” ({when}) is cancelled."),
            ),
            (_, true) => (
                crate::tr!("Invitation mise à jour : {summary}", "Updated invitation: {summary}"),
                crate::tr!("L'événement « {summary} » a changé : {when}.", "The event “{summary}” changed: {when}."),
            ),
            _ => (
                crate::tr!("Invitation : {summary}", "Invitation: {summary}"),
                crate::tr!("Vous êtes invité à « {summary} » : {when}.", "You are invited to “{summary}”: {when}."),
            ),
        };
        let body = match &invite.location {
            Some(place) => crate::tr!("{body}\nLieu : {place}", "{body}\nWhere: {place}"),
            None => body,
        };
        let draft = Draft {
            account_id: account.id,
            to: to.iter().map(|a| crate::store::format_address(&Address { name: a.name.clone(), email: a.email.clone() })).collect(),
            subject,
            body,
            calendar: Some(ics.to_string()),
            ..Default::default()
        };
        self.deliver_as(draft, None, None).await
    }

    /// `series`: the whole series an occurrence belongs to, else that occurrence alone.
    pub async fn update_event(self: &Arc<Self>, event: &EventRef, draft: EventDraft, series: bool) -> Result<()> {
        let draft = checked_draft(draft)?;
        let event_id = event.id;
        let target = self.event_target(event)?;
        let e = &target.event;
        if !e.editable {
            return Err(not_editable());
        }
        let changes = crate::calendar::EventChanges::between(e, &draft);
        // Outlook gives a short plain-text preview of the event's body: written back, it would
        // replace the whole body (the meeting link with it).
        if target.source == CalendarSource::Microsoft && changes.description.is_some() {
            return Err(Error::Other(crate::tr!(
                "la description d'un événement Outlook se modifie dans Outlook",
                "an Outlook event's description is changed in Outlook"
            )));
        }
        let series = series && e.recurring;
        let current = (e.start, e.end, e.all_day);
        let to_series = draft.repeat.as_ref().filter(|_| !e.recurring);
        // The repetition is compared with the series' own, not with the occurrence's row.
        if changes.is_empty() && !series && to_series.is_none() {
            return Ok(());
        }
        // The occurrence's new start and length: the series takes that time of day.
        let times = changes.times.map(|(start, end, _)| (start, (end - start).max(0)));
        let account = self.event_writer(e.account_id, target.source)?;
        let changes = changes.organized_by(organizer_of(&account));
        // Who is invited once written, for the guests' mails (CalDAV).
        let guests_after = changes.attendees.clone().unwrap_or_else(|| e.attendees.clone());
        let what = crate::tr!("modification de l'événement", "event change");
        let mut told = Ok(());
        // A single event made a series, from its own date: the dates are read back from the provider.
        if let Some(repeat) = to_series {
            let first = draft_first(&draft)?;
            match target.source {
                CalendarSource::Google => {
                    let token = self.google_token(&account).await?;
                    bounded(EVENT_TIMEOUT, &what, crate::gcal::rebuild_series(&token, &target.calendar_remote_id, &e.instance, &changes, first, Some(repeat))).await?
                }
                CalendarSource::Microsoft => {
                    let token = self.calendar_token(&account, true).await?;
                    bounded(EVENT_TIMEOUT, &what, crate::mscal::rebuild_series(&token, &e.instance, &changes, first, Some(repeat))).await?
                }
                CalendarSource::Caldav => {
                    let (client, url) = self.caldav_target(&account, &target).await?;
                    let now = crate::store::now();
                    let (start, end, all_day, _) = first;
                    let ics = bounded(EVENT_TIMEOUT, &what, client.rewrite(&url, |ics| crate::calendar::rebuild_series(ics, &changes, (start, end, all_day), Some(repeat), now)))
                        .await?
                        .ok_or_else(not_editable)?;
                    self.after_event_change(account.id);
                    let calendar = reqwest::Url::parse(&target.calendar_remote_id).map_err(|e| Error::Other(e.to_string()))?;
                    return self.tell_guests(&account, &client, &calendar, &ics, &e.attendees, &guests_after).await;
                }
            }
            self.after_event_change(account.id);
            return Ok(());
        }
        let written = match target.source {
            CalendarSource::Google => {
                let token = self.google_token(&account).await?;
                if series {
                    let id = target.series.as_deref().ok_or_else(not_editable)?;
                    let (rule, first) = bounded(EVENT_TIMEOUT, &what, crate::gcal::series_rule(&token, &target.calendar_remote_id, id)).await?;
                    let rule = series_repeat(rule.and_then(|r| crate::repeat::Repeat::from_rrule(&r, first)));
                    match series_plan(e, &draft, &changes, &rule, first)? {
                        Some(repeat) => {
                            let first = rebuilt_first(first, e, &draft)?;
                            bounded(EVENT_TIMEOUT, &what, crate::gcal::rebuild_series(&token, &target.calendar_remote_id, id, &changes, first, repeat.as_ref())).await?
                        }
                        None if changes.is_empty() => return Ok(()),
                        None => bounded(EVENT_TIMEOUT, &what, crate::gcal::update_series(&token, &target.calendar_remote_id, id, &changes, times)).await?,
                    }
                    None
                } else {
                    Some(bounded(EVENT_TIMEOUT, &what, crate::gcal::update(&token, &target.calendar_remote_id, &e.instance, &changes)).await?)
                }
            }
            CalendarSource::Microsoft => {
                let token = self.calendar_token(&account, true).await?;
                if series {
                    let id = target.series.as_deref().ok_or_else(not_editable)?;
                    let (rule, first) = bounded(EVENT_TIMEOUT, &what, crate::mscal::series_rule(&token, id)).await?;
                    let rule = series_repeat(crate::repeat::Repeat::from_graph(&rule, first));
                    match series_plan(e, &draft, &changes, &rule, first)? {
                        Some(repeat) => {
                            let first = rebuilt_first(first, e, &draft)?;
                            bounded(EVENT_TIMEOUT, &what, crate::mscal::rebuild_series(&token, id, &changes, first, repeat.as_ref())).await?
                        }
                        None if changes.is_empty() => return Ok(()),
                        None => bounded(EVENT_TIMEOUT, &what, crate::mscal::update_series(&token, id, &changes, times)).await?,
                    }
                    None
                } else {
                    Some(bounded(EVENT_TIMEOUT, &what, crate::mscal::update(&token, &e.instance, &changes)).await?)
                }
            }
            CalendarSource::Caldav => {
                let (client, url) = self.caldav_target(&account, &target).await?;
                let now = crate::store::now();
                let original = e.instance.parse::<i64>().ok();
                let (mut refused, mut unchanged) = (None, false);
                let ics = bounded(EVENT_TIMEOUT, &what, client.rewrite(&url, |ics| match (series, e.recurring, original) {
                    (true, _, _) => {
                        let (rule, first) = crate::calendar::series_rule(ics)?;
                        let rule = series_repeat(rule.and_then(|r| crate::repeat::Repeat::from_rrule(&r, first)));
                        match series_plan(e, &draft, &changes, &rule, first) {
                            Err(err) => {
                                refused = Some(err);
                                None
                            }
                            Ok(Some(repeat)) => {
                                let (start, end, all_day, _) = rebuilt_first(first, e, &draft).ok()?;
                                crate::calendar::rebuild_series(ics, &changes, (start, end, all_day), repeat.as_ref(), now)
                            }
                            Ok(None) if changes.is_empty() => {
                                unchanged = true;
                                None
                            }
                            Ok(None) => crate::calendar::edit_series(ics, &changes, current, now),
                        }
                    }
                    (false, true, Some(original)) => crate::calendar::edit_occurrence(ics, original, current, &changes, now),
                    _ => crate::calendar::edit_event(ics, &changes, now),
                }))
                .await?;
                if let Some(err) = refused {
                    return Err(err);
                }
                if unchanged {
                    return Ok(());
                }
                let ics = ics.ok_or_else(not_editable)?;
                let calendar = reqwest::Url::parse(&target.calendar_remote_id).map_err(|e| Error::Other(e.to_string()))?;
                told = self.tell_guests(&account, &client, &calendar, &ics, &e.attendees, &guests_after).await;
                (!e.recurring).then(|| written_instance(&ics, url.as_str(), &draft)).transpose()?
            }
        };
        match written {
            Some(event) => {
                // Written on the provider already: a cache that cannot follow is put right by the sync.
                // A provider may give the occurrence another key (a whole day turned into a timed event).
                let kept = if (event.uid.as_str(), event.instance.as_str()) != (e.uid.as_str(), e.instance.as_str()) {
                    self.store.rekey_calendar_event(event_id, e.calendar_id, &event)
                } else {
                    self.store.put_calendar_event(e.calendar_id, &event)
                };
                if let Err(err) = kept {
                    tracing::warn!("event {event_id} changed, not cached: {err}");
                }
                self.after_event_change(account.id);
            }
            // Occurrences of a series: read back from the provider.
            None => self.after_event_change(account.id),
        }
        told
    }

    /// How the series an occurrence belongs to repeats, for its form.
    pub async fn event_repeat(self: &Arc<Self>, event: &EventRef) -> Result<SeriesRepeat> {
        let target = self.event_target(event)?;
        let e = &target.event;
        if !e.recurring {
            return Ok(SeriesRepeat { repeat: None, custom: false });
        }
        let account = self.event_writer(e.account_id, target.source)?;
        let what = crate::tr!("lecture de la série", "series read");
        Ok(match target.source {
            CalendarSource::Google => {
                let token = self.google_token(&account).await?;
                let id = target.series.as_deref().ok_or_else(not_editable)?;
                let (rule, first) = bounded(EVENT_TIMEOUT, &what, crate::gcal::series_rule(&token, &target.calendar_remote_id, id)).await?;
                series_repeat(rule.and_then(|r| crate::repeat::Repeat::from_rrule(&r, first)))
            }
            CalendarSource::Microsoft => {
                let token = self.calendar_token(&account, true).await?;
                let id = target.series.as_deref().ok_or_else(not_editable)?;
                let (rule, first) = bounded(EVENT_TIMEOUT, &what, crate::mscal::series_rule(&token, id)).await?;
                series_repeat(crate::repeat::Repeat::from_graph(&rule, first))
            }
            CalendarSource::Caldav => {
                let (client, url) = self.caldav_target(&account, &target).await?;
                let (ics, _) = bounded(EVENT_TIMEOUT, &what, client.object(&url)).await?;
                let (rule, first) = crate::calendar::series_rule(&ics).ok_or_else(not_editable)?;
                series_repeat(rule.and_then(|r| crate::repeat::Repeat::from_rrule(&r, first)))
            }
        })
    }

    /// `series`: the whole series an occurrence belongs to, else that occurrence alone.
    pub async fn delete_event(self: &Arc<Self>, event: &EventRef, series: bool) -> Result<()> {
        let event_id = event.id;
        let target = self.event_target(event)?;
        let e = &target.event;
        if !e.editable {
            return Err(not_editable());
        }
        let series = series && e.recurring;
        let account = self.event_writer(e.account_id, target.source)?;
        let what = crate::tr!("suppression de l'événement", "event deletion");
        let series_id = || target.series.as_deref().ok_or_else(not_editable);
        let mut told = Ok(());
        match target.source {
            CalendarSource::Google => {
                let token = self.google_token(&account).await?;
                let id = if series { series_id()? } else { e.instance.as_str() };
                bounded(EVENT_TIMEOUT, &what, crate::gcal::delete(&token, &target.calendar_remote_id, id)).await?
            }
            CalendarSource::Microsoft => {
                let token = self.calendar_token(&account, true).await?;
                let id = if series { series_id()? } else { e.instance.as_str() };
                let guests = e.attendees.iter().any(|a| !a.organizer);
                bounded(EVENT_TIMEOUT, &what, crate::mscal::delete(&token, id, guests)).await?
            }
            CalendarSource::Caldav => {
                let (client, url) = self.caldav_target(&account, &target).await?;
                let calendar = reqwest::Url::parse(&target.calendar_remote_id).map_err(|e| Error::Other(e.to_string()))?;
                let guests = e.attendees.iter().any(|a| !a.organizer);
                match e.instance.parse::<i64>() {
                    Ok(original) if e.recurring && !series => {
                        let now = crate::store::now();
                        let ics = bounded(EVENT_TIMEOUT, &what, client.rewrite(&url, |ics| crate::calendar::delete_occurrence(ics, original, now)))
                            .await?
                            .ok_or_else(not_editable)?;
                        told = self.tell_guests(&account, &client, &calendar, &ics, &e.attendees, &e.attendees).await;
                    }
                    _ => {
                        // Its guests are told of the cancellation from the object as it was.
                        let before = if guests { Some(bounded(EVENT_TIMEOUT, &what, client.object(&url)).await?.0) } else { None };
                        bounded(EVENT_TIMEOUT, &what, client.delete(&url)).await?;
                        if let Some(ics) = before {
                            told = self.tell_guests(&account, &client, &calendar, &ics, &e.attendees, &[]).await;
                        }
                    }
                }
            }
        }
        if !series {
            self.store.delete_calendar_event(event_id)?;
        }
        self.after_event_change(account.id);
        told
    }

    /// Answers someone else's invitation from the calendar (`answer`: `accepted`, `tentative`
    /// or `declined`); the provider tells the organizer. `series`: for every date of a series.
    pub async fn respond_event(self: &Arc<Self>, event: &EventRef, answer: &str, series: bool) -> Result<()> {
        let event_id = event.id;
        let partstat = match answer {
            "accepted" => "ACCEPTED",
            "tentative" => "TENTATIVE",
            "declined" => "DECLINED",
            _ => return Err(Error::Other(format!("answer {answer}"))),
        };
        let target = self.event_target(event)?;
        let e = &target.event;
        if !e.respondable {
            return Err(Error::Other(crate::tr!("facteur ne peut pas répondre à cet événement", "facteur cannot answer this event")));
        }
        let series = series && e.recurring;
        let account = self.event_writer(e.account_id, target.source)?;
        let what = crate::tr!("réponse à l'invitation", "invitation answer");
        let id = if series { target.series.as_deref().unwrap_or(&e.instance) } else { e.instance.as_str() };
        match target.source {
            CalendarSource::Google => {
                let token = self.google_token(&account).await?;
                bounded(EVENT_TIMEOUT, &what, crate::gcal::respond(&token, &target.calendar_remote_id, id, answer)).await?
            }
            CalendarSource::Microsoft => {
                let token = self.calendar_token(&account, true).await?;
                bounded(EVENT_TIMEOUT, &what, crate::mscal::respond(&token, id, answer)).await?
            }
            CalendarSource::Caldav => {
                let (client, url) = self.caldav_target(&account, &target).await?;
                let me: Vec<String> = std::iter::once(account.email.clone()).chain(account.identities.iter().map(|i| i.email.clone())).collect();
                let occurrence = match e.instance.parse::<i64>() {
                    Ok(original) if e.recurring && !series => Some((original, (e.start, e.end, e.all_day))),
                    _ => None,
                };
                let now = crate::store::now();
                let written = bounded(EVENT_TIMEOUT, &what, client.rewrite(&url, |ics| crate::calendar::respond_event(ics, &me, partstat, occurrence, now)))
                    .await?
                    .ok_or_else(|| Error::Other(crate::tr!("vous n'êtes pas invité à cet événement", "you are not invited to this event")))?;
                // Without server-side scheduling (RFC 6638: Radicale, a bare sabre/dav), the organizer
                // learns of the answer only by mail.
                let calendar = reqwest::Url::parse(&target.calendar_remote_id).map_err(|e| Error::Other(e.to_string()))?;
                if !client.auto_schedules(&calendar).await {
                    let reply = match occurrence {
                        Some((original, _)) => crate::calendar::occurrence_ics(&written, original).unwrap_or(written),
                        None => written,
                    };
                    self.mail_invite_reply(&account, &reply, partstat, answer_words(partstat)?, None).await?;
                }
            }
        }
        self.store.set_event_response(event_id, answer, series)?;
        self.after_event_change(account.id);
        Ok(())
    }

    /// The event the user saw: a row id given back to another occurrence since (a sync rewrote
    /// the calendar) is refused.
    fn event_target(&self, event: &EventRef) -> Result<crate::store::EventTarget> {
        let target = self.store.event_target(event.id)?;
        if target.event.uid != event.uid || target.event.instance != event.instance {
            return Err(Error::NotFound(crate::tr!("l'événement a changé entre-temps : rouvrez-le", "the event changed meanwhile: open it again")));
        }
        Ok(target)
    }

    async fn caldav_target(&self, account: &Account, target: &crate::store::EventTarget) -> Result<(crate::caldav::Client, reqwest::Url)> {
        let href = target.href.as_deref().ok_or_else(not_editable)?;
        let url = reqwest::Url::parse(href).map_err(|e| Error::Other(e.to_string()))?;
        let settings = self.store.calendar_account(account.id)?;
        Ok((self.caldav_client(account, &settings).await?, url))
    }

    async fn google_token(&self, account: &Account) -> Result<String> {
        match self.credential(account).await? {
            Credential::Token(token) => Ok(token),
            _ => Err(Error::Other("Google: OAuth2".into())),
        }
    }

    /// Shown at once; the account's calendars are then synced, which settles what the provider
    /// made of the change.
    fn after_event_change(self: &Arc<Self>, account_id: i64) {
        self.emit(Event::CalendarChanged);
        let this = self.clone();
        self.spawn(async move { this.sync_calendars(Some(account_id)).await });
    }

    /// The password goes only to the sites of `caldav::auto_sites` and of the address the user
    /// entered (see `caldav`).
    async fn sync_caldav_calendars(&self, account: &Account, settings: &crate::store::CalendarAccount, changed: &std::sync::atomic::AtomicBool) -> Result<(CalendarState, Option<String>)> {
        use std::sync::atomic::Ordering;
        let client = self.caldav_client(account, settings).await?;
        let manual = settings.caldav_url.as_deref().map(crate::caldav::check_url).transpose()?;
        let home = match remembered_home(settings, &client) {
            Some(home) => home,
            None => {
                let found = match &manual {
                    Some(url) => client.home(url).await?.map_or(crate::caldav::Discovery::Nothing, crate::caldav::Discovery::Found),
                    None => client.discover(&crate::caldav::candidates(&account.email, &account.imap.host)).await?,
                };
                let home = match found {
                    crate::caldav::Discovery::Found(home) if client.trusts(&home) => home,
                    crate::caldav::Discovery::Found(other) | crate::caldav::Discovery::Elsewhere(other) => {
                        let host = other.host_str().unwrap_or_default().to_string();
                        return Ok((
                            CalendarState::NotConfigured,
                            Some(crate::tr!(
                                "serveur d'agenda trouvé sur {host}, qui n'est pas celui de votre messagerie : saisissez son adresse pour l'utiliser",
                                "calendar server found at {host}, which is not your mail server: enter its address to use it"
                            )),
                        ));
                    }
                    crate::caldav::Discovery::Nothing => {
                        return Ok((
                            CalendarState::NotConfigured,
                            Some(crate::tr!("aucun serveur CalDAV trouvé : indiquez son adresse", "no CalDAV server found: enter its address")),
                        ));
                    }
                };
                let site = crate::caldav::site_of(&home).unwrap_or_default();
                if !self.store.set_calendar_home(account.id, Some((home.as_str(), &site)), settings.generation)? {
                    // An address was saved meanwhile: the sync asked for then runs next.
                    return Ok((CalendarState::Ok, None));
                }
                home
            }
        };
        let list = match client.calendars(&home).await {
            Ok(list) => list,
            Err(e) => {
                // Moved or gone: looked for again at the next sync.
                if matches!(e, Error::NotFound(_)) {
                    self.store.set_calendar_home(account.id, None, settings.generation)?;
                }
                return Err(e);
            }
        };
        let (rows, list_changed) = self.store.sync_calendar_list(account.id, CalendarSource::Caldav, &list)?;
        changed.fetch_or(list_changed, Ordering::Relaxed);
        let me: Vec<String> = std::iter::once(account.email.clone()).chain(account.identities.iter().map(|i| i.email.clone())).collect();
        let (from, to) = calendar_window();
        let fresh_after = crate::store::now() - CALENDAR_REFETCH;
        let mut outcome = CalendarFetches::default();
        for row in rows.iter().filter(|r| r.enabled) {
            let ctag = list.iter().find(|c| c.remote_id == row.remote_id).and_then(|c| c.ctag.clone());
            // Unchanged on the server, and the window has not moved much since.
            if ctag.is_some() && ctag == row.ctag && row.synced_at.is_some_and(|t| t > fresh_after) {
                outcome.synced += 1;
                continue;
            }
            let Ok(url) = reqwest::Url::parse(&row.remote_id) else { continue };
            // Window moved since: every object read again (in batches all the same).
            let whole = row.synced_at.is_none_or(|t| t <= fresh_after);
            match self.read_caldav_calendar(&client, &url, row.id, whole, (from, to), &me).await {
                Ok(Some((listed, fetched, events))) => {
                    changed.fetch_or(
                        self.store.update_calendar_objects(row.id, account.id, &row.remote_id, &listed, &fetched, &events, ctag.as_deref())?,
                        Ordering::Relaxed,
                    );
                    outcome.synced += 1;
                }
                // The server cannot list its objects' ETags: read whole, as one answer.
                Ok(None) => match client.events(&url, from, to, &me).await {
                    Ok(events) => {
                        changed.fetch_or(self.store.replace_calendar_events(row.id, account.id, &row.remote_id, &events, ctag.as_deref())?, Ordering::Relaxed);
                        outcome.synced += 1;
                    }
                    Err(e @ Error::Auth(_)) => return Err(e),
                    Err(e) => {
                        tracing::warn!("CalDAV calendar {} of account {}: {e}", row.id, account.id);
                        changed.fetch_or(self.store.set_calendar_error(row.id, account.id, &row.remote_id, Some(&e.to_string()))?, Ordering::Relaxed);
                        outcome.failed.push((row.name.clone(), e));
                    }
                },
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(e) => {
                    tracing::warn!("CalDAV calendar {} of account {}: {e}", row.id, account.id);
                    changed.fetch_or(self.store.set_calendar_error(row.id, account.id, &row.remote_id, Some(&e.to_string()))?, Ordering::Relaxed);
                    outcome.failed.push((row.name.clone(), e));
                }
            }
        }
        outcome.result()
    }

    /// A CalDAV calendar's objects in the window and the events of those to read again (all when
    /// `whole`, else those new or whose ETag changed); `None` when the server does not list
    /// ETags (an empty or refused listing), for the whole read instead.
    async fn read_caldav_calendar(
        &self,
        client: &crate::caldav::Client,
        url: &reqwest::Url,
        calendar_id: i64,
        whole: bool,
        (from, to): (i64, i64),
        me: &[String],
    ) -> Result<Option<(Vec<(String, String)>, HashSet<String>, Vec<crate::calendar::Instance>)>> {
        let listed = match client.etags(url, from, to).await {
            Ok(listed) => listed,
            Err(e @ Error::Auth(_)) => return Err(e),
            Err(_) => return Ok(None),
        };
        let known = if whole { HashMap::new() } else { self.store.caldav_etags(calendar_id)? };
        let fetched: Vec<String> = listed.iter().filter(|(href, etag)| known.get(href) != Some(etag)).map(|(href, _)| href.clone()).collect();
        let events = client.objects(url, &fetched, from, to, me).await?;
        Ok(Some((listed, fetched.into_iter().collect(), events)))
    }

    pub fn start_background(self: &Arc<Self>, interval: Duration) {
        self.backfill_links();
        self.start_scheduler();
        let pool = self.clone();
        self.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                pool.close_idle_connections().await;
            }
        });
        // Calendars on their own task and rhythm: never in the way of the mail.
        let calendars = self.clone();
        self.spawn(async move {
            tokio::time::sleep(CALENDAR_FIRST).await;
            loop {
                // Listening before the work: a wake-up during a long sync is not lost.
                let woken = calendars.resume.notified();
                tokio::pin!(woken);
                woken.as_mut().enable();
                let _ = calendars.sync_calendars_with(None, true).await;
                // On its own task: a slow address book must not hold the calendars back, nor miss a
                // wake-up signal.
                let books = calendars.clone();
                calendars.spawn(async move {
                    if let Err(e) = books.sync_address_books(None, false).await {
                        tracing::warn!("address books: {e}");
                    }
                });
                // Back from sleep or online again: a sync that timed out meanwhile is redone now, not a whole
                // round later (a few seconds first, for the network to come up).
                tokio::select! {
                    _ = tokio::time::sleep(CALENDAR_EVERY) => {}
                    _ = woken => tokio::time::sleep(CALENDAR_AFTER_RESUME).await,
                }
            }
        });
        let this = self.clone();
        self.spawn(async move {
            // When each kind of periodic sync last ran, by account (see `due_sync`).
            let mut last: HashMap<i64, [Option<Instant>; 3]> = HashMap::new();
            let mut housekeeping: Option<Instant> = None;
            loop {
                // Listening before the syncs: a wake-up while they run is not lost.
                let woken = this.resume.notified();
                tokio::pin!(woken);
                woken.as_mut().enable();
                let chores = housekeeping.is_none_or(|t| t.elapsed() >= interval);
                if chores {
                    // In the background: the network must not delay the sync.
                    let logos = this.clone();
                    tokio::spawn(async move { logos.fill_account_logos().await });
                }
                let accounts = this.accounts().unwrap_or_default();
                for account in &accounts {
                    if this.single_connection(account.id) {
                        this.leave_one_connection(account.id);
                    } else {
                        // Restarted if it stopped (panic included).
                        this.ensure_listener(account.id);
                    }
                }
                // Each account in its own task: a slow server does not delay the others, and an internal
                // error on one does not stop the others. An account whose periodic sync of a previous tick
                // still runs is left alone.
                let jobs: Vec<_> = accounts
                    .iter()
                    .filter_map(|account| {
                        let account_id = account.id;
                        let scope = this.due_sync(account_id, last.entry(account_id).or_default())?;
                        if !this.periodic.lock().unwrap().insert(account_id) {
                            return None;
                        }
                        let this = this.clone();
                        Some(tokio::spawn(async move {
                            // Released even when the sync panics: otherwise the account would never
                            // get a periodic sync again until a restart.
                            let _periodic = PeriodicRun(this.clone(), account_id);
                            let res = match scope {
                                sync::Scope::All => this.sync(account_id).await,
                                scope => this.sync_scoped(account_id, scope).await,
                            };
                            if let Err(e) = res {
                                tracing::warn!("sync of account {account_id}: {e}");
                            }
                        }))
                    })
                    .collect();
                // Waited for one tick at most: a sync started now can take hours (a first sync), and the other
                // accounts, a listener to (re)start and a wake-up must not wait with it. A handle dropped on
                // the timeout leaves its sync running.
                let _ = tokio::time::timeout(SYNC_TICK, async {
                    for job in jobs {
                        if let Err(e) = job.await {
                            tracing::error!("sync stopped by an internal error: {e}");
                        }
                    }
                })
                .await;
                if chores {
                    this.store.optimize();
                    this.persist_unsaved_secrets().await;
                    housekeeping = Some(Instant::now());
                }
                // Back from sleep or online again (see `resume`): everything is synced now.
                tokio::select! {
                    _ = tokio::time::sleep(SYNC_TICK) => {}
                    _ = woken => last.clear(),
                }
            }
        });
    }

    /// By kind: the inbox (only while its listening connection is down), the folders with a role,
    /// every folder with the folder list. Setting `sync_every:<account>`, in minutes:
    /// "inbox,system,custom".
    pub fn sync_every(&self, account_id: i64) -> [Duration; 3] {
        let given = self.setting(&format!("sync_every:{account_id}")).ok().flatten().unwrap_or_default();
        let mut every = SYNC_EVERY_DEFAULT;
        for (slot, value) in every.iter_mut().zip(given.split(',')) {
            if let Ok(minutes) = value.trim().parse::<u64>()
                && minutes > 0
            {
                *slot = minutes.min(24 * 60);
            }
        }
        every.map(|m| Duration::from_secs(m * 60))
    }

    /// Noted in `last` (inbox, folders with a role, everything): the widest due wins and counts for
    /// the narrower ones.
    fn due_sync(&self, account_id: i64, last: &mut [Option<Instant>; 3]) -> Option<sync::Scope> {
        let every = self.sync_every(account_id);
        let now = Instant::now();
        let due = |i: usize| last[i].is_none_or(|t| now.duration_since(t) >= every[i]);
        if due(2) {
            *last = [Some(now); 3];
            return Some(sync::Scope::All);
        }
        if due(1) {
            last[0] = Some(now);
            last[1] = Some(now);
            return Some(sync::Scope::System);
        }
        if due(0) {
            last[0] = Some(now);
            return (!self.diag.listening(account_id)).then_some(sync::Scope::Inbox);
        }
        None
    }

    fn idle_renew_of(&self, account_id: i64) -> Duration {
        self.idle_renew.lock().unwrap().get(&account_id).map(|r| r.every).unwrap_or(IDLE_RENEW)
    }

    /// The connection dropped `held` after IDLE started (see [`IdleRenew::cut`]).
    fn learn_idle_cut(&self, account_id: i64, held: Duration) {
        let mut map = self.idle_renew.lock().unwrap();
        let renew = map.entry(account_id).or_default();
        if renew.cut(held) {
            tracing::info!("account {account_id}: IDLE renewed every {} s (the server cuts silent connections)", renew.every.as_secs());
        }
    }

    /// An IDLE of this account ran its full term and the server answered DONE.
    fn idle_held_full(&self, account_id: i64) {
        if let Some(renew) = self.idle_renew.lock().unwrap().get_mut(&account_id) {
            renew.held_full();
        }
    }

    /// A new account listens at once, not at the next round of syncs.
    /// Setting `single_connection:<account>` ("1"): one connection for everything and no listening
    /// (the inbox is checked every minute instead), for servers that cut sessions as soon as a
    /// mailbox has several (an OVH MX Plan included with a web hosting).
    pub fn single_connection(&self, account_id: i64) -> bool {
        self.setting(&format!("single_connection:{account_id}")).ok().flatten().is_some_and(|v| v == "1")
    }

    /// The listening and the connections other than the sync's are closed.
    fn leave_one_connection(&self, account_id: i64) {
        if let Some(stop) = self.idle_stops.lock().unwrap().remove(&account_id) {
            let _ = stop.send(true);
        }
        self.slots.lock().unwrap().retain(|(a, purpose), _| *a != account_id || *purpose == Purpose::Sync);
    }

    /// A connection of this account dropped by its server (`listening`: the IDLE one). Once the
    /// drops are surely the server's habit, not a passing blip, the single-connection mode is
    /// offered, once a session, unless the user turned the offer down for good.
    fn note_cut(&self, account_id: i64, listening: bool) {
        if self.single_connection(account_id) {
            return;
        }
        let now = Instant::now();
        let chronic = {
            let mut cuts = self.cuts.lock().unwrap();
            while cuts.front().is_some_and(|c| now.duration_since(c.at) > 2 * CUTS_WINDOW) || cuts.len() > 2000 {
                cuts.pop_front();
            }
            cuts.push_back(Cut { at: now, account_id, listening });
            chronic_cuts(&cuts, account_id, now, *self.last_resume.lock().unwrap())
        };
        if !chronic || self.advised.lock().unwrap().contains(&account_id) {
            return;
        }
        if self.setting(&format!("single_connection_offer:{account_id}")).ok().flatten().is_some_and(|v| v == "declined") {
            return;
        }
        tracing::info!("account {account_id}: the server keeps dropping connections, single-connection mode offered");
        self.advised.lock().unwrap().insert(account_id);
        self.advice.lock().unwrap().insert(account_id);
        self.emit(Event::OneConnectionAdvised { account_id });
    }

    /// Accounts offered the single-connection mode, not answered yet.
    pub fn connection_advice(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.advice.lock().unwrap().iter().copied().collect();
        ids.retain(|id| self.store.account(*id).is_ok() && !self.single_connection(*id));
        ids.sort_unstable();
        ids
    }

    /// `Some(true)`: the mode turned on; `Some(false)`: never offered again for this account;
    /// `None`: later (offered again in another session, if the drops go on).
    pub fn answer_connection_advice(self: &Arc<Self>, account_id: i64, accept: Option<bool>) -> Result<()> {
        self.advice.lock().unwrap().remove(&account_id);
        match accept {
            Some(true) => self.set_setting(&format!("single_connection:{account_id}"), "1"),
            Some(false) => self.set_setting(&format!("single_connection_offer:{account_id}"), "declined"),
            None => Ok(()),
        }
    }

    // ── Local AI ─────────────────────────────────────────────────────────────

    pub fn ai_status(&self) -> AiStatus {
        let model = self.setting(AI_MODEL).ok().flatten();
        let file = (model.as_deref() == Some("file")).then(|| self.setting(AI_FILE).ok().flatten()).flatten();
        let download = self.ai_download.lock().unwrap().clone();
        let memory = crate::ai::total_memory();
        AiStatus {
            available: crate::ai::available(),
            ready: self.ai_path().is_ok(),
            model,
            file,
            error: download.as_ref().and_then(|d| d.error.clone()),
            download: download.filter(|d| d.running),
            memory,
            fitting: crate::ai::fitting(memory).id.to_string(),
            models: crate::ai::MODELS.to_vec(),
            dictation: self.setting(DICTATION_MODEL).ok().flatten().is_some(),
            dictation_ready: self.dictation_path().is_ok(),
            dictation_model: crate::ai::DICTATION,
            graphics: self.setting(AI_GRAPHICS).ok().flatten().as_deref() != Some("off"),
            card: crate::ai::graphics_card(),
        }
    }

    /// The graphics card for the local AI, or the processor only (`false`).
    pub fn set_ai_graphics(&self, on: bool) -> Result<()> {
        if on {
            self.store.delete_setting(AI_GRAPHICS)?;
        } else {
            self.store.set_setting(AI_GRAPHICS, "off")?;
        }
        crate::ai::set_graphics(on);
        self.emit(Event::AiChanged);
        Ok(())
    }

    fn dictation_path(&self) -> Result<PathBuf> {
        let off = || Error::Other(crate::tr!("la dictée est désactivée (Réglages)", "dictation is off (Settings)"));
        let id = self.setting(DICTATION_MODEL)?.ok_or_else(off)?;
        let path = self.ai_dir.join(crate::ai::model(&id).ok_or_else(off)?.file);
        if !crate::ai::is_whisper(&path) {
            return Err(Error::Other(crate::tr!("le modèle de dictée est introuvable : activez-la de nouveau", "the dictation model is missing: turn it on again")));
        }
        Ok(path)
    }

    /// Dictation off: its model leaves the disk.
    pub fn dictation_off(&self) -> Result<()> {
        if self.ai_download.lock().unwrap().as_ref().is_some_and(|d| d.running && d.model == crate::ai::DICTATION.id) {
            self.ai_cancel();
        }
        self.store.delete_setting(DICTATION_MODEL)?;
        crate::ai::unload();
        self.ai_forget_others();
        self.emit(Event::AiChanged);
        Ok(())
    }

    /// What was said, recorded by the interface (16 kHz mono, 16-bit), by the local model.
    pub async fn ai_transcribe(&self, pcm: Vec<u8>, language: Option<String>) -> Result<String> {
        let path = self.dictation_path()?;
        let language = language.filter(|l| l.len() == 2 && l.chars().all(|c| c.is_ascii_lowercase()));
        let _one = self.ai_busy.lock().await;
        tokio::task::spawn_blocking(move || crate::ai::transcribe(&path, &crate::ai::samples(&pcm), language.as_deref()))
            .await
            .map_err(|e| Error::Other(e.to_string()))?
    }

    /// The model file in use; an error when the local AI is off or its file gone.
    fn ai_path(&self) -> Result<PathBuf> {
        let off = || Error::Other(crate::tr!("l'IA locale est désactivée (Réglages)", "the local AI is off (Settings)"));
        let path = match self.setting(AI_MODEL)?.as_deref() {
            Some("file") => PathBuf::from(self.setting(AI_FILE)?.ok_or_else(off)?),
            Some(id) => self.ai_dir.join(crate::ai::model(id).ok_or_else(off)?.file),
            None => return Err(off()),
        };
        if !crate::ai::is_gguf(&path) {
            return Err(Error::Other(crate::tr!("le fichier du modèle est introuvable : choisissez-le de nouveau", "the model's file is missing: choose it again")));
        }
        Ok(path)
    }

    /// Downloads an offered model and turns the local AI on with it, once checked; the model
    /// used before is deleted (one at a time).
    pub fn ai_install(self: &Arc<Self>, id: &str) -> Result<()> {
        let model = crate::ai::model(id).ok_or_else(|| Error::Other(format!("model {id}")))?;
        if !crate::ai::available() {
            return Err(Error::Other(crate::tr!("cette version de facteur n'a pas l'IA locale", "this build of facteur has no local AI")));
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut download = self.ai_download.lock().unwrap();
            if download.as_ref().is_some_and(|d| d.running) {
                return Err(Error::Other(crate::tr!("un modèle se télécharge déjà", "a model is downloading already")));
            }
            *download = Some(AiDownload { model: id.to_string(), done: 0, total: model.bytes, cancel: cancel.clone(), error: None, running: true });
        }
        self.emit(Event::AiChanged);
        let this = self.clone();
        self.spawn(async move {
            let dest = this.ai_dir.join(model.file);
            let res = async {
                std::fs::create_dir_all(&this.ai_dir)?;
                if !std::fs::metadata(&dest).is_ok_and(|m| m.len() == model.bytes) {
                    let mut told = Instant::now();
                    crate::ai::download(model, &dest, &cancel, |done| {
                        if let Some(d) = this.ai_download.lock().unwrap().as_mut() {
                            d.done = done;
                        }
                        if told.elapsed() >= Duration::from_millis(500) {
                            told = Instant::now();
                            this.emit(Event::AiChanged);
                        }
                    })
                    .await?;
                }
                if model.tier == "dictation" {
                    this.store.set_setting(DICTATION_MODEL, model.id)?;
                } else {
                    this.store.set_setting(AI_MODEL, model.id)?;
                    this.store.delete_setting(AI_FILE)?;
                    // Waits for a generation under way: not on an async worker.
                    let _ = tokio::task::spawn_blocking(crate::ai::unload).await;
                }
                this.ai_forget_others();
                Ok::<_, Error>(())
            }
            .await;
            if let Some(d) = this.ai_download.lock().unwrap().as_mut() {
                d.running = false;
                d.error = res.as_ref().err().map(ToString::to_string);
            }
            if let Err(e) = res {
                tracing::warn!("local AI model {}: {e}", model.id);
            }
            this.emit(Event::AiChanged);
        });
        Ok(())
    }

    pub fn ai_cancel(&self) {
        if let Some(d) = self.ai_download.lock().unwrap().as_ref() {
            d.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// A model file of the user's own (GGUF), not tested by facteur; the downloaded one goes.
    pub fn ai_use_file(&self, path: &str) -> Result<()> {
        if !crate::ai::available() {
            return Err(Error::Other(crate::tr!("cette version de facteur n'a pas l'IA locale", "this build of facteur has no local AI")));
        }
        if !crate::ai::is_gguf(Path::new(path)) {
            return Err(Error::Other(crate::tr!("ce fichier n'est pas un modèle GGUF", "this file is not a GGUF model")));
        }
        self.store.set_setting(AI_FILE, path)?;
        self.store.set_setting(AI_MODEL, "file")?;
        crate::ai::unload();
        self.ai_forget_others();
        self.emit(Event::AiChanged);
        Ok(())
    }

    /// Off: the model leaves memory, and the downloaded one the disk (a file of the user's own
    /// stays where it is).
    pub fn ai_turn_off(&self) -> Result<()> {
        self.ai_cancel();
        self.store.delete_setting(AI_MODEL)?;
        self.store.delete_setting(AI_FILE)?;
        crate::ai::unload();
        self.ai_forget_others();
        *self.ai_download.lock().unwrap() = None;
        self.emit(Event::AiChanged);
        Ok(())
    }

    /// The downloaded models no longer in use deleted (one text model and the dictation's at
    /// most); a download under way keeps its part file.
    fn ai_forget_others(&self) {
        let in_use: Vec<&str> = [AI_MODEL, DICTATION_MODEL]
            .iter()
            .filter_map(|key| self.setting(key).ok().flatten())
            .filter_map(|id| crate::ai::model(&id))
            .map(|m| m.file)
            .collect();
        let downloading = self.ai_download.lock().unwrap().as_ref().filter(|d| d.running).and_then(|d| crate::ai::model(&d.model)).map(|m| m.file);
        let Ok(entries) = std::fs::read_dir(&self.ai_dir) else { return };
        let part_of = |file: &str| Path::new(file).with_extension("part").to_string_lossy().into_owned();
        // Only what facteur downloads (and its unfinished downloads): a file of the user's own put
        // in this folder, chosen with "use a file", is theirs.
        let ours: Vec<String> = crate::ai::MODELS
            .iter()
            .chain([&crate::ai::DICTATION])
            .flat_map(|m| [m.file.to_string(), part_of(m.file)])
            .collect();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let kept = in_use.contains(&name.as_str()) || downloading.is_some_and(|d| name == d || name == part_of(d));
            if !kept
                && ours.contains(&name)
                && let Err(e) = std::fs::remove_file(entry.path())
            {
                tracing::warn!("local AI: {name} not deleted: {e}");
            }
        }
    }

    /// The model's answer, one at a time, off the async threads.
    async fn ai_answer(&self, turns: Vec<(String, String)>, max_tokens: usize, grammar: Option<&'static str>) -> Result<String> {
        let path = self.ai_path()?;
        let _one = self.ai_busy.lock().await;
        tokio::task::spawn_blocking(move || {
            let turns: Vec<crate::ai::Turn> = turns.iter().map(|(role, content)| crate::ai::Turn { role, content }).collect();
            crate::ai::generate(&path, &turns, max_tokens, grammar)
        })
        .await
        .map_err(|e| Error::Other(e.to_string()))?
        .map(|answer| crate::ai::without_thinking(&answer))
    }

    /// A thread summed up in a few points, by the local model.
    pub async fn ai_summarize(self: &Arc<Self>, account_id: i64, thread_id: i64) -> Result<String> {
        const BUDGET: usize = 12_000;
        let mut thread = self.thread(account_id, thread_id, None)?;
        thread.sort_by_key(|m| std::cmp::Reverse(m.date));
        // The latest first, until the text the model is given is full: a long thread's oldest
        // messages are neither downloaded nor parsed. One that cannot be read is left out.
        let (mut messages, mut used, mut failed) = (Vec::new(), 0, None);
        for m in thread {
            let detail = match self.message(m.id).await {
                Ok(detail) => detail,
                Err(e) => {
                    tracing::info!("summary: message {} left out: {e}", m.id);
                    failed.get_or_insert(e);
                    continue;
                }
            };
            let body = detail.text.clone().unwrap_or_else(|| crate::mime::html_to_text(&detail.html));
            let from = m.from.as_ref().map(|a| a.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| a.email.clone())).unwrap_or_default();
            used += crate::ai::fresh(&body).len().min(3000) + from.len() + 40;
            messages.push((from, m.date, body));
            if used >= BUDGET {
                break;
            }
        }
        if messages.is_empty()
            && let Some(e) = failed
        {
            return Err(e);
        }
        messages.sort_by_key(|(_, date, _)| *date);
        let text = crate::ai::thread_text(&messages, BUDGET);
        let french = !crate::i18n::english();
        let instructions = crate::ai::summary_instructions(french);
        let text = crate::ai::summary_request(&text, french);
        self.ai_answer(vec![("system".into(), instructions), ("user".into(), text)], 400, Some(crate::ai::SUMMARY_GRAMMAR)).await
    }

    /// The appointment a message proposes or confirms, found by the local model, its day and
    /// times read by facteur; `None` when there is none.
    pub async fn ai_find_event(self: &Arc<Self>, id: i64) -> Result<Option<crate::ai::FoundEvent>> {
        let detail = self.message(id).await?;
        let m = &detail.summary;
        let body = detail.text.clone().unwrap_or_else(|| crate::mime::html_to_text(&detail.html));
        let sent = chrono::DateTime::from_timestamp(m.date, 0).map(|d| d.with_timezone(&chrono::Local)).unwrap_or_else(chrono::Local::now);
        let from = m.from.as_ref().map(|a| a.email.clone()).unwrap_or_default();
        let fresh: String = crate::ai::fresh(&body).chars().take(6000).collect();
        let text = format!("From: {from}
Date: {}
Subject: {}

{fresh}", sent.format("%A %Y-%m-%d %H:%M"), m.subject);
        let answer = self
            .ai_answer(vec![("system".into(), crate::ai::EVENT_INSTRUCTIONS.into()), ("user".into(), text)], 200, Some(crate::ai::EVENT_GRAMMAR))
            .await?;
        Ok(crate::ai::found_event(&answer, sent.date_naive()))
    }

    pub fn ensure_listener(self: &Arc<Self>, account_id: i64) {
        if !self.idling.lock().unwrap().insert(account_id) {
            return;
        }
        let watcher = self.clone();
        let (stop_tx, stop) = tokio::sync::watch::channel(false);
        self.idle_stops.lock().unwrap().insert(account_id, stop_tx);
        tokio::spawn(async move {
            let res = std::panic::AssertUnwindSafe(watcher.clone().idle_loop(account_id, stop)).catch_unwind().await;
            if res.is_err() {
                tracing::error!("IDLE listener of account {account_id} stopped by an internal error");
            }
            watcher.idle_stops.lock().unwrap().remove(&account_id);
            watcher.idling.lock().unwrap().remove(&account_id);
            watcher.diag.listener_gone(account_id);
        });
    }

    /// After an error the wait doubles up to 30 minutes; refused credentials wait 30 minutes at
    /// once (otherwise the server ends up banning the address, and every account it hosts with it).
    async fn idle_loop(self: Arc<Self>, account_id: i64, mut stop: tokio::sync::watch::Receiver<bool>) {
        const MAX_WAIT: u64 = 30 * 60;
        let mut wait = 60;
        async fn stopped(stop: &mut tokio::sync::watch::Receiver<bool>) {
            let _ = stop.wait_for(|s| *s).await;
        }
        while !*stop.borrow() && self.store.account(account_id).is_ok() {
            let started = Instant::now();
            let mut listening = Listening::default();
            self.diag.listener(account_id, |l| l.connecting());
            let res = tokio::select! {
                res = self.idle_session(account_id, &mut listening) => res,
                _ = stopped(&mut stop) => return,
            };
            if let Some(held) = listening.cut {
                self.learn_idle_cut(account_id, held);
            }
            let pause = match res {
                Ok(()) => {
                    wait = 60;
                    // Closed by the server: a short pause before reconnecting, so as not to loop if it closes it
                    // again at once.
                    10
                }
                Err(e) => {
                    let refused = matches!(e, Error::Auth(_) | Error::Secret(_));
                    tracing::info!("IDLE (account {account_id}) interrupted: {e}");
                    if e.is_connection_drop() {
                        self.note_cut(account_id, true);
                    }
                    self.diag.listener(account_id, |l| l.failed(e.to_string()));
                    // A connection that held for a while was a working one: this error starts a new series
                    // rather than extending the last (a laptop back from sleep would otherwise wait up to half an
                    // hour). Only once it listened: a server stalling at login or SELECT keeps the backoff growing.
                    if listening.ever && started.elapsed() >= IDLE_HEALTHY {
                        wait = 60;
                    }
                    // Cut after listening a minute or more: the server may drop a silent connection (some after
                    // about two minutes; see `IdleRenew::cut`), so come back at once rather than after a minute
                    // without push. Time spent connecting or syncing says nothing about that.
                    let held = listening.cut.unwrap_or_default();
                    if !refused && held >= Duration::from_secs(60) {
                        5
                    } else {
                        let pause = if refused { MAX_WAIT } else { wait };
                        wait = (wait * 2).min(MAX_WAIT);
                        pause
                    }
                }
            };
            self.diag.listener(account_id, |l| l.waiting(Duration::from_secs(pause)));
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(pause)) => {}
                // Woken from sleep, back online, or new credentials: try again now.
                _ = self.resume.notified() => wait = 60,
                _ = stopped(&mut stop) => return,
            }
        }
    }

    /// Kept as long as it holds: after each signal or timeout, IDLE starts again on the same
    /// connection. With NOTIFY (RFC 5465) changes in every folder are reported here and only those
    /// folders are synced; otherwise only the inbox is watched. `Ok`: closed cleanly (to reopen).
    async fn idle_session(self: &Arc<Self>, account_id: i64, listening: &mut Listening) -> Result<()> {
        use async_imap::extensions::idle::IdleResponse;
        let account = self.store.account(account_id)?;
        let mut session = self.login(&account).await?;
        let caps = bounded(30, &crate::tr!("capacités du serveur", "server capabilities"), sync::capabilities(&mut session)).await?;
        if !caps.idle {
            // No IDLE: the periodic sync will do.
            self.diag.listener(account_id, |l| l.set("unsupported"));
            logout(&mut session).await;
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return Ok(());
        }
        bounded(60, &crate::tr!("ouverture de la boîte de réception", "opening the inbox"), async { Ok(session.select("INBOX").await?) }).await?;
        if caps.notify {
            // New and removed messages and flag changes in every folder, folder creation and renaming;
            // the selected inbox reports as usual. A refusal leaves plain IDLE.
            let set = "NOTIFY SET (selected MessageNew MessageExpunge FlagChange) (personal MessageNew MessageExpunge FlagChange MailboxName)";
            match bounded(30, "NOTIFY", async { Ok(session.run_command_and_check_ok(set).await?) }).await {
                Ok(()) => {
                    tracing::info!("account {account_id}: NOTIFY active, every folder is listened to");
                    self.diag.listener(account_id, |l| l.notify = Some(true));
                }
                Err(Error::Imap(async_imap::error::Error::No(e) | async_imap::error::Error::Bad(e))) => {
                    tracing::info!("account {account_id}: NOTIFY refused ({e}), listening to the inbox only");
                    self.diag.listener(account_id, |l| l.notify = Some(false));
                }
                Err(e) => return Err(e),
            }
        }
        let mut changes = IdleChanges::default();
        loop {
            if self.store.account(account_id).is_err() {
                logout(&mut session).await;
                return Ok(());
            }
            // What was reported outside IDLE (while ending it, during the NOOPs: NOTIFY keeps reporting)
            // is read before listening again, or it would wait for the next signal (and fill the channel,
            // whose overflow is dropped).
            drain_unsolicited(&mut session, &mut changes);
            // What the server reports while the user deletes or files messages one after another is
            // mostly the echo of those actions: synced once they stop, not after each (a sync per deletion,
            // each replaying the queue, held up opening a message by seconds on Outlook). New mail waits as
            // long, a few seconds at most. Then sync and collect what was reported meanwhile (a NOOP reads
            // it) until nothing is left; the wait comes before each round.
            loop {
                if changes.pending() {
                    let started = Instant::now();
                    while let Some(wait) = self.burst_wait(account_id) {
                        if started.elapsed() >= BURST_MAX {
                            break;
                        }
                        tokio::time::sleep(wait.min(BURST_MAX.saturating_sub(started.elapsed()))).await;
                    }
                    drain_unsolicited(&mut session, &mut changes);
                }
                let Some(scope) = changes.take_scope() else { break };
                self.diag.listener(account_id, |l| l.set("syncing"));
                if let Err(e) = self.sync_scoped(account_id, scope.clone()).await {
                    tracing::warn!("sync after IDLE: {e}");
                    // What the server reported is not lost while the listening goes on: tried again
                    // shortly, on its own.
                    let this = self.clone();
                    self.spawn(async move {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        if let Err(e) = this.sync_scoped(account_id, scope).await {
                            tracing::info!("sync after IDLE, again: {e}");
                        }
                    });
                    break;
                }
                crate::imap::still_open(&mut session).await?;
                drain_unsolicited(&mut session, &mut changes);
            }
            let mut handle = session.idle();
            bounded(60, &crate::tr!("mise en écoute", "starting to listen"), async { Ok(handle.init().await?) }).await?;
            let since = Instant::now();
            listening.ever = true;
            self.diag.listener(account_id, |l| {
                l.set("listening");
                l.notify.get_or_insert(false);
            });
            let (wait, _stop) = handle.wait_with_timeout(self.idle_renew_of(account_id));
            let woken = tokio::select! {
                res = wait => Some(res),
                _ = self.resume.notified() => None,
            };
            // The connection dropped during IDLE: async-imap reports a stream that ended or failed as
            // `ManualInterrupt`, which is never asked for here.
            if matches!(woken, Some(Err(_) | Ok(IdleResponse::ManualInterrupt))) {
                listening.cut = Some(since.elapsed());
            }
            let woken = woken.transpose()?;
            let Some(res) = woken else {
                // Back from sleep or online again: the connection may have died meanwhile. A prompt answer to
                // DONE says it holds; otherwise a new one is opened.
                match tokio::time::timeout(Duration::from_secs(5), handle.done()).await {
                    Ok(Ok(s)) => {
                        session = s;
                        continue;
                    }
                    _ => return Ok(()),
                }
            };
            session = bounded(30, &crate::tr!("fin de l'écoute", "ending listening"), async { Ok(handle.done().await?) }).await?;
            match &res {
                IdleResponse::NewData(data) => changes.note(data.parsed()),
                IdleResponse::Timeout => self.idle_held_full(account_id),
                // End of stream: the server closed the connection.
                IdleResponse::ManualInterrupt => return Ok(()),
            }
        }
    }
}

/// `ever`: it listened at all; `cut`: how long its last IDLE had lasted when the connection
/// dropped during it (not while syncing, at NOOP or DONE).
#[derive(Default)]
struct Listening {
    ever: bool,
    cut: Option<Duration>,
}

#[derive(Debug, Clone, Copy)]
struct Cut {
    at: Instant,
    account_id: i64,
    listening: bool,
}

/// Whether `account_id`'s server drops its connections as a habit (a cap on the sessions a
/// mailbox may have, as with some OVH offers), from the drops of the last hour. Drops shared with
/// another account within 15 s (the network, not the server) and those soon after a wake from
/// sleep do not count. Sure enough: ten of them over twenty minutes at least, three of them
/// outside the listening (a server cutting silent IDLE connections only is handled by renewing
/// IDLE).
fn chronic_cuts(cuts: &VecDeque<Cut>, account_id: i64, now: Instant, resumed: Option<Instant>) -> bool {
    let apart = |a: Instant, b: Instant| if a > b { a - b } else { b - a };
    let own: Vec<&Cut> = cuts
        .iter()
        .filter(|c| c.account_id == account_id && now.saturating_duration_since(c.at) <= CUTS_WINDOW)
        .filter(|c| resumed.is_none_or(|r| c.at < r || c.at - r > Duration::from_secs(120)))
        .filter(|c| !cuts.iter().any(|o| o.account_id != account_id && apart(o.at, c.at) <= Duration::from_secs(15)))
        .collect();
    let (Some(first), Some(last)) = (own.first(), own.last()) else { return false };
    own.len() >= 10 && own.iter().filter(|c| !c.listening).count() >= 3 && last.at - first.at >= Duration::from_secs(20 * 60)
}

#[derive(Debug, Clone, Copy)]
struct IdleRenew {
    every: Duration,
    /// A cut seen once, not yet confirmed by a second one: a single network blip (Wi-Fi switch,
    /// access point gone) says nothing about the server.
    unconfirmed: Option<Duration>,
}

impl Default for IdleRenew {
    fn default() -> Self {
        Self { every: IDLE_RENEW, unconfirmed: None }
    }
}

impl IdleRenew {
    /// Two cuts in a row before the term (a minute or more in) mean the server drops a silent
    /// connection: IDLE is renewed a good margin before the shorter of them (never under 45 s).
    /// Returns whether the delay changed.
    fn cut(&mut self, held: Duration) -> bool {
        if held < Duration::from_secs(60) || held >= self.every {
            return false;
        }
        let Some(before) = self.unconfirmed.replace(held) else { return false };
        self.unconfirmed = None;
        let renew = (before.min(held) * 3 / 5).max(Duration::from_secs(45));
        if renew < self.every {
            self.every = renew;
            return true;
        }
        false
    }

    /// The connection held longer than an unconfirmed cut, which was a blip.
    fn held_full(&mut self) {
        self.unconfirmed = None;
    }
}

fn drain_unsolicited(session: &mut Session, changes: &mut IdleChanges) {
    while let Ok(u) = session.unsolicited_responses.try_recv() {
        changes.note_unsolicited(u);
    }
}

/// For CPU-heavy work (MIME parsing of a large message): keeps it off the async workers that
/// drive the connections and the interface's commands.
async fn blocking<T: Send + 'static>(job: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(job).await.map_err(|e| Error::Other(e.to_string()))?
}

async fn parse_body(raw: Vec<u8>) -> Result<mime::ParsedBody> {
    blocking(move || mime::parse_body(&raw)).await
}

async fn parse_draft(raw: Vec<u8>) -> Result<mime::DraftParts> {
    blocking(move || mime::parse_draft(&raw)).await
}

/// Bounded: a server that does not answer must not keep the account marked busy (history,
/// backup, restore) for good.
async fn logout(session: &mut Session) {
    let _ = bounded(10, "logout", async { Ok(session.logout().await?) }).await;
}

/// See [`Engine::uid_cutoff`].
fn cutoff(uidnext: Option<u32>, highest_cached: Option<u32>) -> Option<u32> {
    let past_cache = highest_cached.map(|u| u.saturating_add(1));
    match (uidnext, past_cache) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// For writes that must not be lost (the outcome of a send): retried while the database is busy
/// or the disk hiccups.
async fn persist(mut write: impl FnMut() -> Result<()>) -> Result<()> {
    let mut last = None;
    for pause in [1, 2, 5, 10, 0] {
        match write() {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!("store write failed, retrying: {e}");
                last = Some(e);
            }
        }
        if pause > 0 {
            tokio::time::sleep(Duration::from_secs(pause)).await;
        }
    }
    Err(last.expect("at least one try"))
}

/// `None`: wait for the user. Tries 2, 3 and 4 come after 1, 2 and 4 minutes. A send that may
/// have gone (uncertain) is never repeated on its own, nor one the server refused for good.
/// `attempts`: those the server saw; `offline`: the tries that never reached it. Each counts this
/// failure already.
fn retry_at(error: &Error, attempts: i64, offline: i64, now: i64) -> Option<i64> {
    // Offline (on a train, the Wi-Fi gone, the account waiting to be signed in again): nothing
    // reached the server, so no attempt is spent; a try every 15 minutes at most, for a day.
    if matches!(error, Error::Unreachable(_)) {
        return (offline < UNREACHABLE_ATTEMPTS).then(|| now + (60 << (offline - 1).clamp(0, 4)).min(15 * 60));
    }
    // A message that cannot be built (see `deliver`) fails the same way every time. Refused
    // credentials too (with a fresh token already): each try is a failed sign-in, which servers
    // answer by locking the account out.
    let final_error = matches!(error, Error::Uncertain(_) | Error::Rejected(_) | Error::Mime(_) | Error::Auth(_));
    (attempts < SEND_ATTEMPTS && !final_error).then(|| now + 60 * (1 << (attempts - 1).clamp(0, 10)))
}

#[derive(Default)]
struct IdleChanges {
    /// Folders created, renamed or deleted: everything is synced.
    all: bool,
    folders: std::collections::BTreeSet<String>,
}

impl IdleChanges {
    fn note(&mut self, response: &async_imap::imap_proto::Response) {
        use async_imap::imap_proto::{MailboxDatum, Response};
        match response {
            // RFC 5465: the server dropped notifications it could not keep up with; nobody knows which
            // folders changed.
            Response::Data { outcome, .. } if outcome.information.as_deref().is_some_and(|i| i.to_ascii_uppercase().contains("NOTIFICATIONOVERFLOW")) => {
                self.all = true;
            }
            Response::MailboxData(MailboxDatum::Status { mailbox, .. }) => {
                self.folders.insert(mailbox.to_string());
            }
            Response::MailboxData(MailboxDatum::List(_)) => self.all = true,
            // Anything else concerns the selected folder, the inbox.
            _ => {
                self.folders.insert("INBOX".into());
            }
        }
    }

    fn note_unsolicited(&mut self, response: async_imap::types::UnsolicitedResponse) {
        use async_imap::types::UnsolicitedResponse as U;
        match response {
            U::Status { mailbox, .. } => {
                self.folders.insert(mailbox);
            }
            U::Other(data) => self.note(data.parsed()),
            _ => {
                self.folders.insert("INBOX".into());
            }
        }
    }

    fn pending(&self) -> bool {
        self.all || !self.folders.is_empty()
    }

    /// Clears the changes collected so far.
    fn take_scope(&mut self) -> Option<sync::Scope> {
        let scope = if std::mem::take(&mut self.all) {
            sync::Scope::All
        } else if self.folders.is_empty() {
            return None;
        } else if self.folders.len() == 1 && self.folders.contains("INBOX") {
            sync::Scope::Inbox
        } else {
            sync::Scope::Folders(self.folders.iter().cloned().collect())
        };
        self.folders.clear();
        Some(scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_time_change_across_the_computers_midnight_is_no_move() {
        // 15:00 in Los Angeles, the next day in Europe or Asia: moved to 14:00 the same day.
        let tz: chrono_tz::Tz = "America/Los_Angeles".parse().unwrap();
        let at = |h| chrono::TimeZone::with_ymd_and_hms(&tz, 2026, 10, 20, h, 0, 0).earliest().unwrap().timestamp();
        let e = CalendarEvent { start: at(15), end: at(16), zone: Some("America/Los_Angeles".into()), recurring: true, ..Default::default() };
        let rule = SeriesRepeat { repeat: None, custom: true };
        let first = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let draft = EventDraft { start: at(14), end: at(15), zone: e.zone.clone(), ..Default::default() };
        let changes = crate::calendar::EventChanges::between(&e, &draft);
        assert!(changes.times.is_some());
        // A custom rule left alone and no date moved: the times move, the series is not rebuilt.
        assert_eq!(series_plan(&e, &draft, &changes, &rule, first).unwrap(), None);
        // The next day in Los Angeles is a move, even on the same day elsewhere.
        let later = EventDraft { start: at(15) + 86400, end: at(16) + 86400, ..draft };
        let changes = crate::calendar::EventChanges::between(&e, &later);
        assert!(series_plan(&e, &later, &changes, &rule, first).is_err(), "a custom rule asks for its own calendar app");
    }
    use crate::mime::HeaderInfo;
    use crate::store::ServerFlags;

    #[test]
    fn a_series_is_rebuilt_only_when_its_days_or_rule_change() {
        use crate::repeat::{Freq, Repeat};
        let at = |d: u32, h: u32| chrono::TimeZone::with_ymd_and_hms(&chrono::Local, 2026, 10, d, h, 0, 0).earliest().unwrap().timestamp();
        let e = CalendarEvent {
            id: 1, calendar_id: 1, account_id: 1, uid: "s".into(), instance: "1".into(), summary: String::new(), location: None, description: None,
            start: at(19, 18), end: at(19, 19), all_day: false, status: None, organizer: None, url: None, busy: true, recurring: true,
            editable: true, response: None, respondable: false, attendees: Vec::new(), meeting: None, reminders: Vec::new(), zone: None,
        };
        let first = chrono::NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        let monday = Repeat { freq: Freq::Weekly, interval: 1, weekdays: vec![0], by_weekday: false, until: None, count: None };
        let now = series_repeat(Some(monday.clone()));
        let plan = |draft: &EventDraft, now: &SeriesRepeat| series_plan(&e, draft, &crate::calendar::EventChanges::between(&e, draft), now, first);
        let draft = |start: i64, end: i64, repeat: Option<Repeat>| EventDraft { start, end, repeat, ..Default::default() };

        // Same rule (no day named is the first day's), another hour: times only.
        let unnamed = Repeat { weekdays: vec![], ..monday.clone() };
        assert_eq!(plan(&draft(at(19, 20), at(19, 21), Some(unnamed)), &now).unwrap(), None);
        // Another day, another rule, or no more repetition: rebuilt.
        assert_eq!(plan(&draft(at(20, 18), at(20, 19), Some(monday.clone())), &now).unwrap(), Some(Some(monday.clone())));
        let daily = Repeat { freq: Freq::Daily, weekdays: vec![], ..monday.clone() };
        assert_eq!(plan(&draft(at(19, 18), at(19, 19), Some(daily.clone())), &now).unwrap(), Some(Some(daily.clone())));
        assert_eq!(plan(&draft(at(19, 18), at(19, 19), None), &now).unwrap(), Some(None));

        // A custom rule keeps its days unless another rule replaces it.
        let custom = series_repeat(None);
        assert_eq!(plan(&draft(at(19, 20), at(19, 21), None), &custom).unwrap(), None);
        assert!(plan(&draft(at(20, 18), at(20, 19), None), &custom).is_err());
        assert_eq!(plan(&draft(at(20, 18), at(20, 19), Some(daily.clone())), &custom).unwrap(), Some(Some(daily)));
    }

    #[test]
    fn single_connection_is_offered_only_for_lasting_drops() {
        let start = Instant::now();
        let at = |min: u64, sec: u64| start + Duration::from_secs(min * 60 + sec);
        let cut = |min: u64, account_id: i64, listening: bool| Cut { at: at(min, 0), account_id, listening };
        // Every two minutes for twenty minutes, syncs included.
        let mut cuts: VecDeque<Cut> = (0..=10).map(|i| cut(i * 2, 5, i % 3 != 0)).collect();
        assert!(chronic_cuts(&cuts, 5, at(21, 0), None));
        // Not for another account, nor an hour later.
        assert!(!chronic_cuts(&cuts, 4, at(21, 0), None));
        assert!(!chronic_cuts(&cuts, 5, at(75, 0), None));
        // Too short a stretch: a bad quarter of an hour.
        let quick: VecDeque<Cut> = (0..=12).map(|i| cut(i, 5, false)).collect();
        assert!(!chronic_cuts(&quick, 5, at(13, 0), None));
        // Only the listening cut: renewing IDLE deals with it.
        let idle: VecDeque<Cut> = (0..=10).map(|i| cut(i * 2, 5, true)).collect();
        assert!(!chronic_cuts(&idle, 5, at(21, 0), None));
        // The network: another account dropped at the same moments.
        let mut shared = cuts.clone();
        shared.extend((0..=10).filter(|i| i % 2 == 0).map(|i| Cut { at: at(i * 2, 5), account_id: 4, listening: true }));
        assert!(!chronic_cuts(&shared, 5, at(21, 0), None));
        // Drops right after a wake from sleep do not count.
        assert!(!chronic_cuts(&cuts, 5, at(21, 0), Some(at(19, 0))));
        cuts.push_back(cut(22, 5, false));
        assert!(chronic_cuts(&cuts, 5, at(23, 0), Some(at(19, 0))));
    }

    #[test]
    fn idle_renew_learns_only_repeated_cuts() {
        let secs = Duration::from_secs;
        let mut r = IdleRenew::default();
        assert!(!r.cut(secs(90)));
        r.held_full();
        assert!(!r.cut(secs(90)));
        assert_eq!(r.every, IDLE_RENEW);
        // Cut again before the term: renewed well before the shorter cut.
        assert!(r.cut(secs(120)));
        assert_eq!(r.every, secs(54));
        assert!(!r.cut(secs(30)));
        assert!(!r.cut(secs(30)));
        assert!(!r.cut(secs(60)));
        assert!(!r.cut(secs(60)));
        assert_eq!(r.every, secs(54));
        // Never under 45 s, never raised.
        let mut r = IdleRenew::default();
        r.cut(secs(61));
        r.cut(secs(65));
        assert_eq!(r.every, secs(45));
    }

    #[test]
    fn subject_core_drops_reply_prefixes() {
        assert_eq!(subject_core("Re: TR: Fwd: Réunion lundi"), "Réunion lundi");
        assert_eq!(subject_core("RE : Facture"), "Facture");
        assert_eq!(subject_core("Rendez-vous"), "Rendez-vous", "\"Re\" at the start of a word stays");
    }

    #[test]
    fn oauth_tokens_go_only_to_the_provider_servers() {
        assert!(oauth_host(OAuthProvider::Google, "imap.gmail.com"));
        assert!(oauth_host(OAuthProvider::Google, "SMTP.GMAIL.COM."));
        assert!(oauth_host(OAuthProvider::Microsoft, "outlook.office365.com"));
        assert!(oauth_host(OAuthProvider::Microsoft, "smtp-mail.outlook.com"));
        assert!(!oauth_host(OAuthProvider::Google, "imap.xgmail.com"));
        assert!(!oauth_host(OAuthProvider::Google, "imap.gmail.com.evil.example"));
        assert!(!oauth_host(OAuthProvider::Microsoft, "mail.myoutlook.com"));
        assert!(!oauth_host(OAuthProvider::Microsoft, "imap.gmail.com"));
    }

    #[tokio::test]
    async fn an_oauth_account_on_another_server_is_refused_before_its_token_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let new = |host: &str, security| NewAccount {
            name: "Y".into(), email: "y@xgmail.com".into(), username: "y@xgmail.com".into(), password: String::new(),
            imap: ServerConfig { host: host.into(), port: 993, security }, smtp: ServerConfig { host: "smtp.gmail.com".into(), port: 465, security: Security::Tls },
            auth: NewAuth::OAuth2 { provider: OAuthProvider::Google, ticket: "t".into() },
        };
        for account in [new("imap.xgmail.com", Security::Tls), new("imap.gmail.com", Security::None)] {
            let err = engine.add_account(account).await.unwrap_err().to_string();
            assert!(err.contains("Google"), "{err}");
        }
        // The provider's own servers: past the check (the unknown ticket stops it then).
        let err = engine.add_account(new("imap.gmail.com", Security::Tls)).await.unwrap_err().to_string();
        assert!(!err.contains("imap.gmail.com"), "{err}");
    }

    #[tokio::test]
    async fn an_address_already_in_facteur_is_not_added_again() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let new = |email: &str| NewAccount {
            name: "Y".into(), email: email.into(), username: email.into(), password: String::new(),
            imap: srv.clone(), smtp: srv.clone(), auth: NewAuth::Password,
        };
        engine.store.insert_account(&new("y@gmail.com")).unwrap();
        // Refused before anything else: no connection tried.
        let err = engine.add_account(new("Y@Gmail.com")).await.unwrap_err().to_string();
        assert!(err.contains("already in facteur") || err.contains("déjà dans facteur"), "{err}");
    }

    #[test]
    fn mailto_links() {
        assert_eq!(
            parse_mailto("mailto:a%40b.fr?subject=Bonjour%20toi&body=Ligne&cc=x@y.fr"),
            ("a@b.fr".into(), Some("Bonjour toi".into()), Some("Ligne".into()))
        );
        assert_eq!(parse_mailto("mailto:a@b.fr"), ("a@b.fr".into(), None, None));
    }

    #[tokio::test]
    async fn refused_credentials_are_not_tried_again_until_they_change() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let srv = ServerConfig { host: "localhost".into(), port: 1, security: Security::None };
        let acc = engine
            .store()
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let mut events = engine.subscribe();
        engine.refuse(acc, "mot de passe refusé");
        engine.refuse(acc, "mot de passe refusé");
        assert!(matches!(events.try_recv(), Ok(Event::AuthChanged { error: Some(_), .. })));
        assert!(events.try_recv().is_err(), "told once, not at every refusal");
        assert_eq!(engine.auth_problems().get(&acc).map(String::as_str), Some("mot de passe refusé"));
        // Refused at once: neither the keychain nor the server is asked.
        let account = engine.store().account(acc).unwrap();
        assert!(matches!(engine.credential(&account).await, Err(Error::Auth(_))));
        assert!(matches!(engine.session(acc, Purpose::Sync).await, Err(Error::Auth(_))));
        engine.clear_refusal(acc);
        assert!(matches!(events.try_recv(), Ok(Event::AuthChanged { error: None, .. })));
        assert!(engine.auth_problems().is_empty());
    }

    #[test]
    fn a_cached_token_is_forgotten_only_if_it_is_the_one_refused() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let later = SystemTime::now() + Duration::from_secs(3600);
        engine.tokens.lock().unwrap().insert(1, ("fresh".into(), later));
        engine.forget_token(1, Some("old"));
        assert!(engine.tokens.lock().unwrap().contains_key(&1));
        engine.forget_token(1, Some("fresh"));
        assert!(!engine.tokens.lock().unwrap().contains_key(&1));
    }

    #[test]
    fn a_damaged_database_is_set_aside_with_its_journal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("facteur.db"), b"not a database. ".repeat(512)).unwrap();
        std::fs::write(dir.path().join("facteur.db-wal"), b"x").unwrap();
        let err = Engine::open(dir.path()).err().expect("garbage is not a database");
        assert!(err.is_damaged_db(), "{err:?}");
        let aside = Engine::set_aside_database(dir.path()).unwrap();
        assert!(aside.file_name().unwrap().to_string_lossy().starts_with("facteur.db.corrupt-"));
        assert!(aside.exists() && !dir.path().join("facteur.db").exists() && !dir.path().join("facteur.db-wal").exists());
        Engine::open(dir.path()).expect("starts again on an empty database");
    }

    #[tokio::test]
    async fn resume_counts_once_for_calls_close_together() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        engine.resume();
        let first = *engine.last_resume.lock().unwrap();
        engine.resume();
        assert_eq!(*engine.last_resume.lock().unwrap(), first);
    }

    #[test]
    fn a_calendar_sync_asked_for_meanwhile_runs_again() {
        let map = Mutex::new(HashMap::new());
        let mut run = CalendarRun::start(&map, 7).unwrap();
        assert!(CalendarRun::start(&map, 7).is_none());
        assert!(CalendarRun::start(&map, 7).is_none());
        assert!(CalendarRun::start(&map, 8).is_some(), "another account goes its own way");
        assert!(run.again());
        assert!(!run.again());
        assert!(map.lock().unwrap().is_empty());
        let next = CalendarRun::start(&map, 7).unwrap();
        drop(run);
        assert!(map.lock().unwrap().contains_key(&7), "an older run never frees a newer one's slot");
        drop(next);
        assert!(map.lock().unwrap().is_empty());
    }

    #[test]
    fn one_calendar_failing_leaves_the_account_partial() {
        let fetches = |synced, failed: &[&str]| CalendarFetches {
            synced,
            failed: failed.iter().map(|n| (n.to_string(), Error::Other(format!("{n} gone")))).collect(),
        };
        assert_eq!(fetches(2, &[]).result().unwrap(), (CalendarState::Ok, None));
        assert_eq!(fetches(0, &[]).result().unwrap(), (CalendarState::Ok, None));
        let (state, message) = fetches(1, &["Fériés", "Club"]).result().unwrap();
        assert_eq!(state, CalendarState::Partial);
        assert!(message.unwrap().contains("Fériés, Club"));
        assert!(fetches(0, &["Fériés"]).result().unwrap_err().to_string().contains("Fériés gone"));
    }

    #[test]
    fn calendar_status_says_what_span_is_synced() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let srv = ServerConfig { host: "mail.example.org".into(), port: 993, security: Security::Tls };
        let acc = engine
            .store()
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let status = |e: &Engine| e.calendar_status().unwrap().into_iter().find(|s| s.account_id == acc).unwrap();
        assert_eq!((status(&engine).synced_from, status(&engine).synced_to), (None, None));
        let cal = crate::calendar::RemoteCalendar { remote_id: "https://example.org/c/".into(), name: "c".into(), color: None, read_only: false, enabled: true, ctag: None };
        let (rows, _) = engine.store().sync_calendar_list(acc, CalendarSource::Caldav, &[cal]).unwrap();
        engine.store().replace_calendar_events(rows[0].id, acc, &rows[0].remote_id, &[], None).unwrap();
        let s = status(&engine);
        let now = crate::store::now();
        assert!(s.synced_from.unwrap() <= now - CALENDAR_PAST && s.synced_from.unwrap() > now - CALENDAR_PAST - 60);
        assert!(s.synced_to.unwrap() >= now + CALENDAR_AHEAD - 60);
        assert_eq!(synced_span(Some((100, 200))), (Some(200 - CALENDAR_PAST), Some(100 + CALENDAR_AHEAD)));
    }

    #[test]
    fn a_remembered_caldav_home_needs_its_trusted_site() {
        // me@shop.fr reads its mail at OVH: the password goes to ovh.net only.
        let mut client = crate::caldav::Client::new("me@shop.fr", "secret").unwrap();
        for site in crate::caldav::auto_sites("me@shop.fr", "ssl0.ovh.net") {
            client.trust_site(&site);
        }
        let settings = |home: &str, site: Option<&str>| crate::store::CalendarAccount {
            home_url: Some(home.into()),
            home_site: site.map(str::to_string),
            ..Default::default()
        };
        assert!(remembered_home(&settings("https://dav.ovh.net/home/", Some("ovh.net")), &client).is_some());
        // Stored by an older version (no site), on another site than the one stored, or on a site the
        // password does not go to: looked for again instead.
        assert!(remembered_home(&settings("https://dav.ovh.net/home/", None), &client).is_none());
        assert!(remembered_home(&settings("https://dav.ovh.net/home/", Some("shop.fr")), &client).is_none());
        assert!(remembered_home(&settings("https://attacker.example/home/", Some("attacker.example")), &client).is_none());
        assert!(remembered_home(&settings("http://dav.ovh.net/home/", Some("ovh.net")), &client).is_none());
    }

    #[tokio::test]
    async fn archive_creates_missing_folder_next_to_special_ones() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let store = engine.store();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = store
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        // Courier/Cyrus-style server: no archive folder, folders under "INBOX.".
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("."), Some(Role::Inbox), true).unwrap();
        store.upsert_mailbox(acc, "INBOX.Sent", Some("."), Some(Role::Sent), true).unwrap();
        let flags = ServerFlags { seen: false, flagged: false, answered: false, draft: false };
        let header = HeaderInfo { message_id: Some("m@x".into()), subject: "Facture".into(), date: 1, ..Default::default() };
        let id = store.insert_header(acc, inbox, 7, &header, flags, 10).unwrap();

        engine.archive(&[id]).unwrap();

        let archive = store.mailbox_by_role(acc, Role::Archive).unwrap().expect("folder created");
        assert_eq!(archive.name, "INBOX.Archive");
        assert_eq!(store.location(id).unwrap().mailbox_id, archive.id);
        let kinds: Vec<String> = store
            .pending_ops(acc)
            .unwrap()
            .iter()
            .map(|p| p.kind.clone())
            .collect();
        assert_eq!(kinds, ["create", "move_set"], "the creation must come before the move");
    }

    #[tokio::test]
    async fn on_gmail_labels_done_moves_and_bin_change_labels() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let store = engine.store();
        let srv = ServerConfig { host: "imap.gmail.com".into(), port: 993, security: Security::None };
        let acc = store
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@gmail.com".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let all = store.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let trash = store.upsert_mailbox(acc, "[Gmail]/Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let mairie = store.upsert_mailbox(acc, "Mairie", Some("/"), None, true).unwrap();
        let flags = ServerFlags { seen: false, flagged: false, answered: false, draft: false };
        let header = HeaderInfo { message_id: Some("m@x".into()), subject: "Diaporama".into(), date: 1, ..Default::default() };
        let id = store.insert_header(acc, all, 7, &header, flags, 10).unwrap();
        assert!(store.convert_to_labels(acc).unwrap());
        store.set_labels(id, &[inbox]).unwrap();
        let labels = |id| store.message_labels(&[id]).unwrap().remove(&id).unwrap_or_default();
        let kinds = || store.pending_ops(acc).unwrap().into_iter().map(|o| o.kind).collect::<Vec<_>>();

        let token = engine.archive(&[id]).unwrap().expect("undoable");
        assert_eq!((labels(id), store.location(id).unwrap().mailbox_id), (vec![], all));
        assert_eq!(kinds(), ["labels"]);
        engine.undo(token).await.unwrap();
        assert_eq!(labels(id), vec![inbox]);
        assert!(kinds().is_empty());

        engine.move_to(&[id], mairie, Some(inbox)).unwrap();
        assert_eq!(labels(id), vec![mairie]);

        engine.trash(&[id], false).unwrap();
        assert_eq!((labels(id), store.location(id).unwrap().mailbox_id), (vec![], trash));
        let listed = |b: i64| store.list_threads(&[b], &[], ThreadFilter::All, false, None, 50, 0).unwrap().len();
        assert_eq!((listed(all), listed(trash)), (0, 1), "gone from All Mail, shown in the bin");
    }

    #[tokio::test]
    async fn archive_can_be_undone_and_folders_managed() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let store = engine.store();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = store
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        store.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let flags = ServerFlags { seen: false, flagged: false, answered: false, draft: false };
        let from = |email: &str| Some(Address { name: None, email: email.into() });
        let header = HeaderInfo { message_id: Some("m@x".into()), subject: "Avis".into(), date: 1, from: from("avis@impots.gouv.fr"), ..Default::default() };
        let id = store.insert_header(acc, inbox, 7, &header, flags, 10).unwrap();

        // Archived then undone: the message comes back with its UID, the op never leaves.
        let token = engine.archive(&[id]).unwrap().expect("undoable");
        assert_ne!(store.location(id).unwrap().mailbox_id, inbox);
        assert!(store.pending_ops(acc).unwrap()[0].not_before.is_some(), "held for the undo window");
        engine.undo(token).await.unwrap();
        let loc = store.location(id).unwrap();
        assert_eq!((loc.mailbox_id, loc.uid), (inbox, Some(7)));
        assert!(store.pending_ops(acc).unwrap().is_empty());
        assert!(engine.undo(token).await.is_err(), "only once");

        let impots = engine.create_folder(acc, None, "Impôts").unwrap();
        let year = engine.create_folder(acc, Some(impots), "2026").unwrap();
        assert!(engine.create_folder(acc, None, "impôts").is_err(), "name already taken");
        assert!(engine.create_folder(acc, None, "a/b").is_err(), "delimiter not allowed");
        engine.rename_folder(impots, "Administration").unwrap();
        assert_eq!(store.mailbox_state(year).unwrap().name, "Administration/2026");
        assert_eq!(store.mailbox_state(impots).unwrap().name, "Administration");

        // Rule: filed and marked read, without undo delay.
        let rule = crate::rules::Rule {
            id: 0,
            account_id: acc,
            name: "Impôts".into(),
            enabled: true,
            match_all: true,
            conditions: vec![crate::rules::Condition { field: crate::rules::Field::From, op: crate::rules::Op::EndsWith, value: "impots.gouv.fr".into() }],
            actions: vec![crate::rules::Action::Move { mailbox_id: impots }, crate::rules::Action::MarkRead],
            stop: false,
        };
        let preview = engine.preview_rule(rule.clone()).unwrap();
        assert_eq!((preview.count, preview.unread, preview.messages.len()), (1, 1, 1));
        assert_eq!(preview.top_senders[0].email, "avis@impots.gouv.fr");
        assert_eq!(store.location(id).unwrap().mailbox_id, inbox);
        let rule_id = engine.save_rule(rule).unwrap();
        assert_eq!(engine.run_rule(rule_id).unwrap(), 1);
        assert_eq!(store.location(id).unwrap().mailbox_id, impots);
        assert!(store.summary(id).unwrap().flags.seen);
        assert!(store.pending_ops(acc).unwrap().iter().all(|p| p.not_before.is_none()));
    }

    #[tokio::test]
    async fn emptying_the_trash_and_existing_special_folders() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let store = engine.store();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = store
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("."), Some(Role::Inbox), true).unwrap();
        // Special folders named in French, without SPECIAL-USE: never recreated in English.
        let drafts = store.upsert_mailbox(acc, "INBOX.Brouillons", Some("."), Some(Role::Drafts), true).unwrap();
        let trash = store.upsert_mailbox(acc, "INBOX.Corbeille", Some("."), Some(Role::Trash), true).unwrap();
        assert_eq!(engine.drafts_mailbox(acc).unwrap().id, drafts);
        assert!(store.pending_ops(acc).unwrap().is_empty(), "no folder created");

        let flags = ServerFlags { seen: true, flagged: false, answered: false, draft: false };
        for uid in 1..=3 {
            let h = HeaderInfo { message_id: Some(format!("{uid}@x")), subject: "x".into(), date: uid as i64, ..Default::default() };
            store.insert_header(acc, trash, uid, &h, flags, 10).unwrap();
        }
        let mut state = store.mailbox_state(trash).unwrap();
        state.uidnext = Some(4);
        store.save_mailbox_state(&state).unwrap();

        assert!(engine.empty_folder(inbox).is_err(), "only the bin and junk");
        assert_eq!(engine.empty_folder(trash).unwrap(), 3);
        assert_eq!(store.folder_summary(store.list_mailboxes(acc).unwrap().into_iter().find(|m| m.id == trash).unwrap(), Vec::new(), false, 10).unwrap().local, 0);
        let op: serde_json::Value = serde_json::from_str(&store.op_payload(store.pending_ops(acc).unwrap()[0].id).unwrap().unwrap()).unwrap();
        assert_eq!((op["kind"].as_str(), op["below"].as_u64()), (Some("purge"), Some(4)));
        // Until the purge is done on the server, the sync does not bring these messages back.
        assert_eq!(store.purge_pending_below(acc, "INBOX.Corbeille").unwrap(), Some(4));

        // Trashed then at once purged: refused while it is on its way.
        let h = HeaderInfo { message_id: Some("9@x".into()), subject: "x".into(), date: 9, ..Default::default() };
        let id = store.insert_header(acc, inbox, 9, &h, flags, 10).unwrap();
        engine.trash(&[id], false).unwrap();
        assert!(engine.trash(&[id], true).is_err(), "message still without a UID in the bin");
        // Deleted again from where it was: left in the bin, not deleted for good.
        assert!(engine.trash(&[id], false).is_ok());
        assert_eq!(store.location(id).unwrap().mailbox_id, trash);
        // The bin emptied meanwhile: the move becomes a delete at the source, and the purge also
        // looks for the message in case it already arrived.
        engine.empty_folder(trash).unwrap();
        let kinds: Vec<String> = store.pending_ops(acc).unwrap().iter().map(|p| p.kind.clone()).collect();
        assert!(kinds.contains(&"delete_set".to_string()) && !kinds.contains(&"move_set".to_string()), "{kinds:?}");
        let last: serde_json::Value = serde_json::from_str(&store.op_payload(store.pending_ops(acc).unwrap().last().unwrap().id).unwrap().unwrap()).unwrap();
        assert_eq!(last["arriving"], serde_json::json!(["9@x"]));
    }

    #[tokio::test]
    async fn queue_details_explain_why_each_action_waits() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let store = engine.store();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = store
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let now = crate::store::now();
        let held = store.push_op(acc, r#"{"kind":"move_set","mailbox":"INBOX","uids":[1],"dest":"Archive"}"#, "Archive", Some(now + 60)).unwrap();
        store.push_op(acc, r#"{"kind":"move_set","mailbox":"INBOX","uids":[2],"dest":"Trash"}"#, "Trash", None).unwrap();
        store.push_op(acc, r#"{"kind":"store_set","mailbox":"INBOX","uids":[3],"flag":"\\Seen","value":true}"#, "Read", None).unwrap();
        let refused = store.push_op(acc, r#"{"kind":"append","mailbox":"Sent","flags":"","raw":""}"#, "Sent copy", None).unwrap();
        for _ in 0..5 {
            store.fail_op(refused, "NO [OVERQUOTA]", 5).unwrap();
        }
        let d = engine.ops_details().unwrap();
        let states: Vec<String> = d.ops.iter().map(|o| serde_json::to_value(o.state).unwrap().as_str().unwrap().to_string()).collect();
        assert_eq!(states, ["held", "blocked", "waiting", "failed"], "flag changes pass held actions, moves do not");
        assert_eq!(d.ops[1].blocked_by, Some(held));
        assert_eq!(d.ops[3].op.error.as_deref(), Some("NO [OVERQUOTA]"));
    }

    #[test]
    fn listening_reports_only_the_folders_that_changed() {
        let mut c = IdleChanges::default();
        assert!(c.take_scope().is_none());
        c.folders.insert("INBOX".into());
        assert_eq!(c.take_scope(), Some(sync::Scope::Inbox));
        c.note_unsolicited(async_imap::types::UnsolicitedResponse::Status { mailbox: "Sent".into(), attributes: Vec::new() });
        c.folders.insert("INBOX".into());
        assert_eq!(c.take_scope(), Some(sync::Scope::Folders(vec!["INBOX".into(), "Sent".into()])));
        c.all = true;
        c.folders.insert("Sent".into());
        assert_eq!(c.take_scope(), Some(sync::Scope::All), "a folder appeared or vanished: everything");
        assert!(c.take_scope().is_none());
    }

    #[tokio::test]
    async fn deleting_a_folder_moves_its_messages_first() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let store = engine.store();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = store
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let trash = store.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let clients = store.upsert_mailbox(acc, "Clients", Some("/"), None, true).unwrap();
        let old = store.upsert_mailbox(acc, "Clients/2024", Some("/"), None, true).unwrap();
        store.upsert_mailbox(acc, "Clientèle", Some("/"), None, true).unwrap();
        let flags = ServerFlags { seen: false, flagged: false, answered: false, draft: false };
        let header = HeaderInfo { message_id: Some("m@x".into()), subject: "Devis".into(), date: 1, ..Default::default() };
        let id = store.insert_header(acc, old, 3, &header, flags, 10).unwrap();

        let summary = engine.folder_summary(clients, None).unwrap();
        assert_eq!(summary.children.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Clients/2024"]);
        assert_eq!((summary.local, summary.unread, summary.recent.len()), (1, 1, 1));
        assert!(engine.delete_folder(clients, Some(old)).is_err(), "destination inside the deleted folder");
        assert!(engine.delete_folder(trash, None).is_err(), "special folder");

        engine.delete_folder(clients, None).unwrap();
        assert_eq!(store.location(id).unwrap().mailbox_id, trash, "message kept, in the bin");
        let names: Vec<String> = store.list_mailboxes(acc).unwrap().into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["Trash", "Clientèle"]);
        let ops: Vec<serde_json::Value> =
            store.pending_ops(acc).unwrap().iter().map(|p| serde_json::from_str(&store.op_payload(p.id).unwrap().unwrap()).unwrap()).collect();
        assert_eq!(ops[0]["mailbox"], "Clients/2024", "the subfolder first");
        assert_eq!((ops[1]["mailbox"].as_str(), ops[1]["dest"].as_str()), (Some("Clients"), Some("Trash")));

        let projects = store.upsert_mailbox(acc, "Projects", Some("/"), None, true).unwrap();
        let archive = store.upsert_mailbox(acc, "Projects/Archive", Some("/"), Some(Role::Archive), true).unwrap();
        assert!(engine.delete_folder(projects, Some(trash)).is_err(), "holds a special folder");
        assert!(engine.delete_folder(archive, Some(trash)).is_err(), "special folder, even with a destination");
        assert_eq!(store.list_mailboxes(acc).unwrap().len(), 4);
    }

    fn test_engine() -> (tempfile::TempDir, Arc<Engine>, i64) {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = engine
            .store()
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        (dir, engine, acc)
    }

    #[tokio::test]
    async fn a_muted_conversation_keeps_its_new_messages_out_of_the_inbox() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let archive = store.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let flags = ServerFlags { seen: false, flagged: false, answered: false, draft: false };
        let from = Some(Address { name: None, email: "liste@example.com".into() });
        let first = HeaderInfo { message_id: Some("a@x".into()), subject: "Fil bavard".into(), date: 1, from: from.clone(), ..Default::default() };
        let id = store.insert_header(acc, inbox, 1, &first, flags, 10).unwrap();
        let thread = store.summary(id).unwrap().thread_id;

        engine.mute_thread(acc, thread, true, &[id]).unwrap();
        assert!(engine.thread_muted(acc, thread).unwrap());
        assert_eq!(store.location(id).unwrap().mailbox_id, archive);

        // A reply arrives: read, archived, and no notification (handled like a rule's).
        let reply = HeaderInfo {
            message_id: Some("b@x".into()), in_reply_to: Some("a@x".into()), references: vec!["a@x".into()],
            subject: "Re: Fil bavard".into(), date: 2, from, ..Default::default()
        };
        let next = store.insert_header(acc, inbox, 2, &reply, flags, 10).unwrap();
        assert_eq!(store.summary(next).unwrap().thread_id, thread);
        let ruled = engine.apply_rules(acc, &[next], None).unwrap();
        assert!(ruled.contains(&next) && ruled.moved.contains(&next));
        assert_eq!(store.location(next).unwrap().mailbox_id, archive);
        assert!(store.summary(next).unwrap().flags.seen);

        engine.mute_thread(acc, thread, false, &[]).unwrap();
        let later = HeaderInfo {
            message_id: Some("c@x".into()), in_reply_to: Some("b@x".into()), references: vec!["a@x".into(), "b@x".into()],
            subject: "Re: Fil bavard".into(), date: 3, ..Default::default()
        };
        let last = store.insert_header(acc, inbox, 3, &later, flags, 10).unwrap();
        assert!(!engine.apply_rules(acc, &[last], None).unwrap().contains(&last));
        assert_eq!(store.location(last).unwrap().mailbox_id, inbox);
    }

    #[test]
    fn a_re_added_account_does_not_inherit_a_refusal() {
        let (_dir, engine, acc) = test_engine();
        engine.refuse(acc, "password refused");
        engine.unsaved_secrets.lock().unwrap().insert(acc);
        assert!(engine.auth_problems().contains_key(&acc));
        // Removed then added again: SQLite hands out the same id.
        engine.store().delete_account(acc).unwrap();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let again = engine
            .store()
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        assert_eq!(again, acc);
        engine.forget_account_state(again);
        assert!(engine.auth_problems().is_empty());
        assert!(!engine.unsaved_secrets.lock().unwrap().contains(&again));
    }

    fn op_kinds(store: &Store, acc: i64) -> Vec<String> {
        store.pending_ops(acc).unwrap().into_iter().map(|p| p.kind).collect()
    }

    #[test]
    fn cutoffs_and_send_retries() {
        assert_eq!(cutoff(Some(4), Some(10)), Some(11), "a message cached since the last sync counts");
        assert_eq!(cutoff(Some(12), Some(10)), Some(12));
        assert_eq!(cutoff(None, Some(3)), Some(4));
        assert_eq!(cutoff(Some(5), None), Some(5));
        assert_eq!(cutoff(None, None), None);
        let now = 1000;
        assert_eq!(retry_at(&Error::Smtp("451 greylisted".into()), 1, 0, now), Some(now + 60));
        assert_eq!(retry_at(&Error::Smtp("451".into()), 3, 0, now), Some(now + 240));
        assert_eq!(retry_at(&Error::Smtp("451".into()), SEND_ATTEMPTS, 0, now), None, "then the user decides");
        assert_eq!(retry_at(&Error::Rejected("550".into()), 1, 0, now), None, "refused for good");
        assert_eq!(retry_at(&Error::Uncertain("cut".into()), 1, 0, now), None, "maybe sent: never repeated on its own");
        assert_eq!(retry_at(&Error::Mime("alias removed".into()), 1, 0, now), None, "cannot be built: the user edits it");
        // A refused SMTP sign-in is not tried again on its own: servers lock accounts out for it.
        assert_eq!(retry_at(&Error::Auth("535 5.7.139 SmtpClientAuthentication is disabled".into()), 1, 0, now), None);
        let offline = Error::Unreachable("no route to host".into());
        assert_eq!(retry_at(&offline, 0, SEND_ATTEMPTS + 1, now), Some(now + 15 * 60), "offline: no attempt spent");
        assert_eq!(retry_at(&offline, 0, 1, now), Some(now + 60));
        assert_eq!(retry_at(&offline, 0, UNREACHABLE_ATTEMPTS, now), None, "a day later, the user decides");
        // Three tries offline, then one greylisting once back: not the last attempt.
        assert_eq!(retry_at(&Error::Smtp("451".into()), 1, 3, now), Some(now + 60));
    }

    #[test]
    fn notification_overflow_syncs_everything() {
        let mut c = IdleChanges::default();
        let (_, response) = async_imap::imap_proto::Response::parse(b"* OK [NOTIFICATIONOVERFLOW] too many changes\r\n").unwrap();
        c.note(&response);
        assert_eq!(c.take_scope(), Some(sync::Scope::All));
    }

    #[tokio::test]
    async fn unchanged_drafts_are_not_saved_again() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        store.upsert_mailbox(acc, "Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        let draft = Draft { account_id: acc, subject: "Notes".into(), body: "a".into(), ..Default::default() };
        let id = engine.save_draft(draft.clone()).await.unwrap();
        // By payload: a replaced row may get the same number again.
        let ops = || store.op_rows().unwrap().into_iter().map(|r| store.op_payload(r.id).unwrap().unwrap()).collect::<Vec<_>>();
        let first = ops();
        assert_eq!(first.len(), 1);
        let again = engine.save_draft(Draft { draft_id: Some(id.clone()), ..draft.clone() }).await.unwrap();
        assert_eq!(again, id);
        assert_eq!(ops(), first, "the queued save stands");
        engine.save_draft(Draft { draft_id: Some(id.clone()), body: "b".into(), ..draft }).await.unwrap();
        let now = ops();
        assert!(now.len() == 1 && now != first, "replaced by the new version");
    }

    #[tokio::test]
    async fn queued_send_leaves_drafts_and_cancelling_puts_it_back() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        store.upsert_mailbox(acc, "Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        engine.set_setting("undo_send", "10").unwrap();
        let draft = Draft { account_id: acc, to: vec!["bob@example.org".into()], subject: "Devis".into(), body: "Ci-joint".into(), ..Default::default() };
        let saved = engine.save_draft(draft.clone()).await.unwrap();
        assert_eq!(op_kinds(store, acc), ["save_draft"]);

        let id = engine.send(Draft { draft_id: Some(saved.clone()), ..draft.clone() }, None, None).await.unwrap().expect("undo window");
        assert_eq!(op_kinds(store, acc), ["delete_draft"], "out of Drafts while it waits in the outbox");
        let back = engine.cancel_send(id).unwrap();
        assert_eq!(back.draft_id.as_deref(), Some(saved.as_str()));
        assert_eq!(op_kinds(store, acc), ["delete_draft", "save_draft"], "back in Drafts, same Message-ID");
        assert!(engine.outbox().unwrap().is_empty());

        // No undo window: through the outbox all the same, with its Message-ID fixed already.
        engine.set_setting("undo_send", "0").unwrap();
        assert_eq!(engine.send(draft, None, None).await.unwrap(), None);
        let (_, queued, _) = store.claim_outgoing(crate::store::now() + 5).unwrap().expect("in the outbox");
        assert!(queued.draft_id.is_some());
    }

    #[tokio::test]
    async fn editing_after_a_failed_attempt_gets_a_new_message_id() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        store.upsert_mailbox(acc, "Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        engine.set_setting("undo_send", "10").unwrap();
        let draft = Draft { account_id: acc, to: vec!["bob@example.org".into()], subject: "Devis".into(), body: "x".into(), draft_id: Some("first@example.org".into()), ..Default::default() };
        let id = engine.send(draft, None, None).await.unwrap().expect("undo window");
        store.outgoing_failed(id, "uncertain", None, false).unwrap();
        let back = engine.cancel_send(id).unwrap();
        assert!(back.draft_id.is_some_and(|d| d != "first@example.org"));
    }

    #[tokio::test]
    async fn reopened_draft_keeps_its_thread_and_loses_its_copy_without_message_id() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let drafts = store.upsert_mailbox(acc, "Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        let flags = ServerFlags { seen: true, flagged: false, answered: false, draft: true };
        // Written by another client: no Message-ID, from the account's own address (any case).
        let raw = b"From: Y <Y@Example.org>\r\nTo: bob@example.org\r\nSubject: Re: Devis\r\n\
In-Reply-To: <parent@x>\r\nReferences: <root@x> <parent@x>\r\n\r\nBrouillon\r\n";
        let h = HeaderInfo { subject: "Re: Devis".into(), date: 1, ..Default::default() };
        let id = store.insert_header(acc, drafts, 7, &h, flags, raw.len() as u32).unwrap();
        store.store_body(id, raw, &crate::mime::body_info(raw)).unwrap();

        let draft = engine.open_draft(id).await.unwrap();
        assert_eq!(draft.from, None, "the account itself, not an alias");
        assert_eq!(draft.reply_ids, Some(("parent@x".to_string(), vec!["root@x".to_string()])));
        assert_eq!(draft.source, Some(id));
        assert!(draft.draft_id.is_none());

        engine.save_draft(draft.clone()).await.unwrap();
        assert!(store.location(id).is_err(), "the old copy is gone from the cache");
        assert_eq!(op_kinds(store, acc), ["save_draft", "delete"]);
        engine.save_draft(draft).await.unwrap();
        assert_eq!(op_kinds(store, acc).iter().filter(|k| *k == "delete").count(), 1);
    }

    #[tokio::test]
    async fn bulk_delete_from_the_trash_is_one_operation() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let trash = store.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let flags = ServerFlags { seen: true, flagged: false, answered: false, draft: false };
        let ids: Vec<i64> = (1..=3)
            .map(|uid| {
                let h = HeaderInfo { message_id: Some(format!("{uid}@x")), subject: "x".into(), date: uid as i64, ..Default::default() };
                store.insert_header(acc, trash, uid, &h, flags, 10).unwrap()
            })
            .collect();
        assert!(engine.trash(&ids, true).unwrap().is_none(), "a permanent delete cannot be undone");
        assert!(store.locations(&ids).unwrap().is_empty());
        let pending = store.pending_ops(acc).unwrap();
        assert_eq!(pending.len(), 1);
        let op: serde_json::Value = serde_json::from_str(&store.op_payload(pending[0].id).unwrap().unwrap()).unwrap();
        assert_eq!((op["kind"].as_str(), op["uids"].clone()), (Some("delete_set"), serde_json::json!([1, 2, 3])));
        assert!(engine.trash(&ids, true).is_err(), "already gone");
    }

    #[tokio::test]
    async fn periodic_syncs_follow_each_kind_of_rhythm() {
        let (_dir, engine, acc) = test_engine();
        let mut last = [None; 3];
        assert_eq!(engine.due_sync(acc, &mut last), Some(sync::Scope::All), "everything at first");
        assert_eq!(engine.due_sync(acc, &mut last), None);
        let ago = |minutes: u64| Some(Instant::now() - Duration::from_secs(minutes * 60 + 1));
        // No listening connection: the inbox every minute, the folders with a role every five.
        last = [ago(1), ago(1), ago(1)];
        assert_eq!(engine.due_sync(acc, &mut last), Some(sync::Scope::Inbox));
        assert_eq!(engine.due_sync(acc, &mut last), None);
        last = [ago(5), ago(5), ago(5)];
        assert_eq!(engine.due_sync(acc, &mut last), Some(sync::Scope::System));
        // A listening connection reports the inbox itself.
        engine.diag.listener(acc, |l| l.set("listening"));
        last = [ago(1), ago(1), ago(1)];
        assert_eq!(engine.due_sync(acc, &mut last), None);
        engine.set_setting(&format!("sync_every:{acc}"), "2, 10,x").unwrap();
        assert_eq!(engine.sync_every(acc), [Duration::from_secs(120), Duration::from_secs(600), Duration::from_secs(900)]);
    }

    #[tokio::test]
    async fn emptying_gmail_trash_lets_queued_moves_land_first() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let all = store.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let trash = store.upsert_mailbox(acc, "[Gmail]/Trash", Some("/"), Some(Role::Trash), true).unwrap();
        assert!(store.convert_to_labels(acc).unwrap());
        let mut state = store.mailbox_state(trash).unwrap();
        state.uidnext = Some(4);
        store.save_mailbox_state(&state).unwrap();
        let flags = ServerFlags { seen: true, flagged: false, answered: false, draft: false };
        let h = HeaderInfo { message_id: Some("9@x".into()), subject: "x".into(), date: 9, ..Default::default() };
        let id = store.insert_header(acc, all, 5, &h, flags, 10).unwrap();
        store.relabel(&[(id, vec![inbox])]).unwrap();
        engine.trash(&[id], false).unwrap();
        engine.empty_folder(trash).unwrap();
        // An expunge in All Mail would delete it before its time: the move stays, ahead of the purge
        // that looks for the message once it is in the bin.
        let pending = store.pending_ops(acc).unwrap();
        let kinds: Vec<&str> = pending.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(kinds, ["move_set", "purge"]);
        let last: serde_json::Value = serde_json::from_str(&store.op_payload(pending[1].id).unwrap().unwrap()).unwrap();
        assert_eq!(last["arriving"], serde_json::json!(["9@x"]));
    }

    #[tokio::test]
    async fn gmail_labels_are_added_and_taken_without_moving() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let all = store.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let mairie = store.upsert_mailbox(acc, "Mairie", Some("/"), None, true).unwrap();
        assert!(store.convert_to_labels(acc).unwrap());
        let flags = ServerFlags { seen: true, flagged: false, answered: false, draft: false };
        let h = HeaderInfo { message_id: Some("l@x".into()), subject: "x".into(), date: 9, ..Default::default() };
        let id = store.insert_header(acc, all, 5, &h, flags, 10).unwrap();
        store.relabel(&[(id, vec![inbox])]).unwrap();
        assert!(store.list_mailboxes(acc).unwrap().iter().any(|m| m.id == mairie && m.user_label));

        let undo = engine.set_labels(&[id], &[mairie], &[]).unwrap().expect("undoable");
        assert_eq!(store.message_labels(&[id]).unwrap()[&id], vec![inbox, mairie], "still in the inbox");
        let pending = store.pending_ops(acc).unwrap();
        assert_eq!(pending.iter().map(|p| p.kind.as_str()).collect::<Vec<_>>(), ["labels"]);
        // The inbox is not a label picked here: asking for it changes nothing.
        assert!(engine.set_labels(&[id], &[inbox], &[]).unwrap().is_none());

        engine.undo(undo).await.unwrap();
        assert_eq!(store.message_labels(&[id]).unwrap()[&id], vec![inbox]);
    }

    #[tokio::test]
    async fn emptying_stops_past_the_highest_cached_uid() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let trash = store.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let flags = ServerFlags { seen: true, flagged: false, answered: false, draft: false };
        let h = HeaderInfo { message_id: Some("late@x".into()), subject: "x".into(), date: 1, ..Default::default() };
        // Seen by a search after the last sync, which had noted UIDNEXT 4.
        store.insert_header(acc, trash, 10, &h, flags, 10).unwrap();
        let mut state = store.mailbox_state(trash).unwrap();
        state.uidnext = Some(4);
        store.save_mailbox_state(&state).unwrap();
        engine.empty_folder(trash).unwrap();
        let op: serde_json::Value = serde_json::from_str(&store.op_payload(store.pending_ops(acc).unwrap()[0].id).unwrap().unwrap()).unwrap();
        assert_eq!(op["below"].as_u64(), Some(11));
    }

    #[tokio::test]
    async fn snoozed_thread_comes_back_unread() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let store = engine.store();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = store
            .insert_account(&NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        // Gmail: special folders live under "[Gmail]", where nothing can be created.
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        store.upsert_mailbox(acc, "[Gmail]/Sent Mail", Some("/"), Some(Role::Sent), true).unwrap();
        let seen = ServerFlags { seen: true, flagged: false, answered: false, draft: false };
        let header = HeaderInfo { message_id: Some("m@x".into()), subject: "Facture".into(), date: 1, ..Default::default() };
        let id = store.insert_header(acc, inbox, 7, &header, seen, 10).unwrap();
        let thread = store.summary(id).unwrap().thread_id;

        engine.snooze(&[id], 10).unwrap();
        let later = store.mailbox_by_role(acc, Role::Snoozed).unwrap().expect("folder created");
        assert_eq!(later.name, "Plus tard");
        assert_eq!(store.location(id).unwrap().mailbox_id, later.id);
        assert!(!engine.wake(acc, thread, &["m@x".into()]).unwrap(), "move not confirmed yet");

        // The server confirmed: the message has a UID in the snooze folder.
        store.insert_header(acc, later.id, 1, &header, seen, 10).unwrap();
        assert!(engine.wake(acc, thread, &["m@x".into()]).unwrap());
        assert_eq!(store.location(id).unwrap().mailbox_id, inbox);
        assert!(!store.summary(id).unwrap().flags.seen, "back unread");
        let listed = store.list_threads(&[inbox], &[], ThreadFilter::All, true, None, 10, 0).unwrap();
        assert!(listed[0].woken && listed[0].activity > 1);
    }

    #[tokio::test]
    async fn only_a_proven_sender_vouches_for_links_to_its_domain() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let id = store.insert_header(acc, inbox, 1, &screened_header("securite@banque.example", "b@x", 10), UNSEEN, 10).unwrap();
        // Known age: no registry asked.
        engine.domain_ages.lock().unwrap().insert("banque.example".into(), None);
        let same_domain = |r: &crate::trust::Reason| r.text == "Même domaine que l'expéditeur" || r.text == "Same domain as the sender";
        let report = engine.inspect_link(id, "https://banque.example/r?u=https://evil.example", "Mon espace").await.unwrap();
        assert!(!report.trust.reasons.iter().any(same_domain), "{:?}", report.trust.reasons);
        // facteur's own aligned DKIM pass proves the From: the bonus applies.
        let local = crate::trust::DkimLocal { pass: true, domain: "banque.example".into(), aligned: true };
        store.set_signals(id, &crate::trust::Signals { dkim_local: Some(local), ..Default::default() }).unwrap();
        let report = engine.inspect_link(id, "https://banque.example/espace", "Mon espace").await.unwrap();
        assert!(report.trust.reasons.iter().any(same_domain), "{:?}", report.trust.reasons);
    }

    fn screened_header(email: &str, mid: &str, received: i64) -> HeaderInfo {
        HeaderInfo {
            message_id: Some(mid.into()),
            subject: mid.into(),
            date: 1,
            received_at: Some(received),
            from: Some(Address { name: None, email: email.into() }),
            ..Default::default()
        }
    }

    const UNSEEN: ServerFlags = ServerFlags { seen: false, flagged: false, answered: false, draft: false };

    #[tokio::test]
    async fn screener_decisions_move_messages_and_undo_puts_categories_back() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let held = store.upsert_mailbox(acc, "New senders", Some("/"), Some(Role::Screener), true).unwrap();
        let refused = store.upsert_mailbox(acc, "Screened out", Some("/"), Some(Role::Refused), true).unwrap();
        engine.set_screener(acc, true).unwrap();
        let arrive = |mailbox, uid, email: &str, mid: &str| store.insert_header(acc, mailbox, uid, &screened_header(email, mid, 10), UNSEEN, 10).unwrap();

        let ann = arrive(held, 1, "ann@gmail.com", "a@x");
        assert!(engine.screen_sender(acc, "ann@gmail.com", Screen::Inbox, true).await.is_err());
        assert_eq!(store.location(ann).unwrap().mailbox_id, held);
        assert_eq!(store.screening_decision(acc, "@gmail.com").unwrap(), None);

        let news = arrive(held, 2, "news@shop.example", "n@x");
        let before = store.message_category(news).unwrap();
        let token = engine.screen_sender(acc, "news@shop.example", Screen::Newsletter, false).await.unwrap().expect("undoable");
        assert_eq!(store.location(news).unwrap().mailbox_id, inbox);
        assert_eq!(store.message_category(news).unwrap(), Category::Newsletter);
        engine.undo(token).await.unwrap();
        assert_eq!(store.location(news).unwrap().mailbox_id, held);
        assert_eq!(store.sender_category("news@shop.example").unwrap(), None);
        assert_eq!(store.message_category(news).unwrap(), before);
        assert_eq!(store.screening_decision(acc, "news@shop.example").unwrap(), None);

        engine.screen_sender(acc, "ann@gmail.com", Screen::Inbox, false).await.unwrap();
        assert_eq!(store.sender_category("ann@gmail.com").unwrap(), Some(Category::People));

        // A domain refused: the person written to stays in the inbox.
        store.record_sent(acc, "m@example.org", &["alice@corp.example".into()]).unwrap();
        let alice = arrive(inbox, 3, "alice@corp.example", "al@x");
        let bob = arrive(inbox, 4, "bob@corp.example", "bo@x");
        engine.screen_sender(acc, "bob@corp.example", Screen::Block, true).await.unwrap();
        assert_eq!(store.location(alice).unwrap().mailbox_id, inbox);
        assert_eq!(store.location(bob).unwrap().mailbox_id, refused);
        arrive(refused, 5, "bob@corp.example", "bo@x");

        engine.screen_sender(acc, "@corp.example", Screen::Inbox, true).await.unwrap();
        assert_eq!(store.location(bob).unwrap().mailbox_id, inbox);
        assert_eq!(store.screening_decision(acc, "@corp.example").unwrap().as_deref(), Some("accept"));
        arrive(inbox, 6, "bob@corp.example", "bo@x");

        // Refused again, then forgotten: no longer refused by what lies in "Screened out", the
        // messages wait for a new decision.
        engine.screen_sender(acc, "bob@corp.example", Screen::Block, false).await.unwrap();
        arrive(refused, 7, "bob@corp.example", "bo@x");
        engine.forget_screening(acc, "bob@corp.example").await.unwrap();
        assert_eq!(store.location(bob).unwrap().mailbox_id, held);
        assert_eq!(store.screening_decision(acc, "bob@corp.example").unwrap(), None);
    }

    #[tokio::test]
    async fn screener_turned_off_releases_messages_once_they_arrive() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let held = store.upsert_mailbox(acc, "New senders", Some("/"), Some(Role::Screener), true).unwrap();
        engine.set_screener(acc, true).unwrap();
        let waiting = store.insert_header(acc, held, 1, &screened_header("a@example.net", "a@x", 10), UNSEEN, 10).unwrap();
        let moving = store.insert_header(acc, inbox, 2, &screened_header("b@example.net", "b@x", 10), UNSEEN, 10).unwrap();
        store.move_local(moving, held).unwrap();

        engine.set_screener(acc, false).unwrap();
        assert_eq!(store.account(acc).unwrap().screener_since, None);
        assert_eq!(store.location(waiting).unwrap().mailbox_id, inbox);
        assert_eq!(store.location(moving).unwrap().mailbox_id, held, "still on its way: left for later");

        store.insert_header(acc, held, 3, &screened_header("b@example.net", "b@x", 10), UNSEEN, 10).unwrap();
        engine.finish_release(acc);
        assert_eq!(store.location(moving).unwrap().mailbox_id, inbox);
        assert_eq!(store.setting(&release_key(acc)).unwrap().as_deref(), Some("0"));
    }

    #[tokio::test]
    async fn turning_the_ai_off_never_deletes_a_file_of_the_users_own() {
        let (_dir, engine, _) = test_engine();
        std::fs::create_dir_all(&engine.ai_dir).unwrap();
        let own = engine.ai_dir.join("my-model.gguf");
        let downloaded = engine.ai_dir.join(crate::ai::MODELS[0].file);
        std::fs::write(&own, b"x").unwrap();
        std::fs::write(&downloaded, b"x").unwrap();
        engine.ai_forget_others();
        assert!(own.exists(), "the user's own file stays");
        assert!(!downloaded.exists(), "a model facteur downloaded, no longer used, goes");
    }

    #[tokio::test]
    async fn a_sign_in_refused_for_now_too_long_asks_the_user() {
        let (_dir, engine, acc) = test_engine();
        let start = SystemTime::now();
        let why = "User is authenticated but not connected";
        let at = |minutes: u64| start + Duration::from_secs(minutes * 60);
        // Once before a night's sleep, once on waking: no run, nothing asked.
        engine.refused_for_now(acc, why, at(0));
        engine.refused_for_now(acc, why, at(600));
        assert!(!engine.auth_refused.lock().unwrap().contains_key(&acc), "two isolated refusals");
        // Refused every few minutes from then on, for half an hour: the user is asked.
        for minutes in [605, 612, 619, 626] {
            engine.refused_for_now(acc, why, at(minutes));
        }
        assert!(!engine.auth_refused.lock().unwrap().contains_key(&acc), "still passing");
        engine.refused_for_now(acc, why, at(630));
        let why = engine.auth_refused.lock().unwrap().get(&acc).cloned().expect("waits for the user");
        assert!(why.contains("authenticated but not connected"), "{why}");
        assert!(!engine.refused_since.lock().unwrap().contains_key(&acc));
    }

    #[tokio::test]
    async fn new_mail_shows_nowhere_before_the_screener_has_decided() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        engine.set_screener(acc, true).unwrap();
        let later = crate::store::now() + 60;
        let header = screened_header("stranger@unknown.example", "u@x", later);
        // Stored by a sync: held out of the lists in the same transaction.
        let id = store.insert_headers_held(acc, inbox, &[(1, header, UNSEEN, 10)], true).unwrap()[0].0;
        let listed = || store.list_threads(&[inbox], &[], crate::model::ThreadFilter::All, false, None, 50, 0).unwrap().len();
        assert_eq!(listed(), 0, "not in the inbox list");
        assert!(store.search(acc, "u@x", 10, false).unwrap().is_empty(), "nor in a search");
        assert_eq!(store.category_unread(&[inbox]).unwrap().values().sum::<i64>(), 0, "nor in the smart inbox counts");
        let unread = |id| store.list_mailboxes(acc).unwrap().into_iter().find(|m| m.id == id).map(|m| (m.unread, m.unread_threads)).unwrap();
        assert_eq!(unread(inbox), (0, 0), "nor in the inbox's unread count");
        assert_eq!(store.unscreened(acc).unwrap(), vec![id]);

        // Whatever path comes next (a folder refresh here, or a sync left halfway before), the
        // waiting message goes through the screener, then is released.
        engine.after_inbox_arrivals(acc, &[], false);
        let screener = store.mailbox_by_role(acc, Role::Screener).unwrap().expect("created").id;
        assert_eq!(store.location(id).unwrap().mailbox_id, screener);
        assert!(store.unscreened(acc).unwrap().is_empty());
        assert_eq!(listed(), 0, "held, not in the inbox");
        assert_eq!(store.list_threads(&[screener], &[], crate::model::ThreadFilter::All, false, None, 50, 0).unwrap().len(), 1);
        assert_eq!(unread(screener), (1, 1), "counted where it waits");
    }

    #[tokio::test]
    async fn rules_survive_a_missing_folder_and_read_marks_do_not_skip_the_screener() {
        let (_dir, engine, acc) = test_engine();
        let store = engine.store();
        let inbox = store.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let archive = store.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let old = store.upsert_mailbox(acc, "Old", Some("/"), None, true).unwrap();
        // Another account of the user's, written from with a +tag.
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        store
            .insert_account(&NewAccount {
                name: "Z".into(), email: "z@other.example".into(), username: "z".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        engine.set_screener(acc, true).unwrap();
        let rule = |name: &str, value: &str, action| crate::rules::Rule {
            id: 0,
            account_id: acc,
            name: name.into(),
            enabled: true,
            match_all: true,
            conditions: vec![crate::rules::Condition { field: crate::rules::Field::From, op: crate::rules::Op::Contains, value: value.into() }],
            actions: vec![action],
            stop: false,
        };
        let broken = store.save_rule(&rule("Old", "shop", crate::rules::Action::Move { mailbox_id: old })).unwrap();
        store.delete_mailbox(old).unwrap();
        store.save_rule(&rule("Bank", "bank", crate::rules::Action::Archive)).unwrap();
        store.save_rule(&rule("Promo", "promo", crate::rules::Action::MarkRead)).unwrap();
        let later = crate::store::now() + 60;
        let put = |uid, email: &str, mid: &str| store.insert_header(acc, inbox, uid, &screened_header(email, mid, later), UNSEEN, 10).unwrap();
        let shop = put(1, "orders@shop.example", "s@x");
        let bank = put(2, "alerts@bank.example", "b@x");
        let promo = put(3, "deals@promo.example", "p@x");
        // Authenticated by the account's usual server (learned from earlier mail): proven.
        for n in 0..5 {
            let mut h = screened_header(&format!("news@learned{n}.example"), &format!("l{n}@x"), 1);
            h.signals.authserv = Some("mx.example.org".into());
            store.insert_header(acc, archive, 100 + n, &h, UNSEEN, 10).unwrap();
        }
        let mine = {
            let mut h = screened_header("Z+lists@other.example", "z@x", later);
            h.signals.dmarc = Some(true);
            h.signals.authserv = Some("mx.example.org".into());
            store.insert_header(acc, inbox, 4, &h, UNSEEN, 10).unwrap()
        };

        // Caught by a folder refresh (Gmail: saving a draft syncs All Mail): told as new mail.
        let mut events = engine.subscribe();
        engine.after_inbox_arrivals(acc, &[shop, bank, promo, mine], true);
        let mut told: Vec<(i64, bool)> = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let Event::NewMail { messages, .. } = event {
                told.extend(messages.iter().map(|m| (m.id, m.held)));
            }
        }
        told.sort();
        assert_eq!(told, vec![(shop, true), (mine, false)], "filed or marked read by a rule: not told");

        assert!(!store.rules(Some(acc)).unwrap().iter().find(|r| r.id == broken).unwrap().enabled);
        assert_eq!(store.location(bank).unwrap().mailbox_id, archive);
        // Marked read by a rule, a stranger still waits for a decision.
        let screener = store.mailbox_by_role(acc, Role::Screener).unwrap().expect("created").id;
        assert!(store.summary(promo).unwrap().flags.seen);
        assert_eq!(store.location(promo).unwrap().mailbox_id, screener);
        assert_eq!(store.location(shop).unwrap().mailbox_id, screener);
        assert_eq!(store.location(mine).unwrap().mailbox_id, inbox);
    }
}
