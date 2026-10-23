//! Incremental IMAP sync into the local cache. Per folder: reset on UIDVALIDITY change, drop
//! UIDs gone from the server, fetch new headers (the newest `WINDOW` on a first pass), refresh
//! flags (CONDSTORE `CHANGEDSINCE` when available), prefetch recent bodies.

use std::collections::{HashMap, HashSet};

use async_imap::types::Fetch;
use futures::TryStreamExt;

use crate::error::Result;
use crate::imap::{self, Session, server_flags, uid_set};
use crate::mime;
use crate::model::{Account, Role};

use crate::store::{MailboxState, Store};

/// Messages fetched per folder on its first sync.
pub const WINDOW: usize = 1000;
const FETCH_CHUNK: usize = 200;
const PREFETCH_BODIES: usize = 50;
const PREFETCH_MAX_SIZE: u32 = 2 * 1024 * 1024;
/// Body bytes downloaded at most per folder and per sync.
const PREFETCH_BUDGET: u32 = 16 * 1024 * 1024;
const PREFETCH_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    pub condstore: bool,
    pub mv: bool,
    pub uidplus: bool,
    pub idle: bool,
    pub notify: bool,
    /// LITERAL+ (RFC 7888) or LITERAL- (up to 4096 bytes): literals sent without waiting for the
    /// server's go-ahead, so they fit in a single command line.
    pub literal_plus: bool,
    pub list_status: bool,
    /// Asked for together with LIST-STATUS.
    pub special_use: bool,
    /// X-GM-EXT-1: true for Google Workspace too, whatever the server's name.
    pub gmail: bool,
    pub quota: bool,
}

pub async fn capabilities(session: &mut Session) -> Result<Caps> {
    let c = session.capabilities().await?;
    Ok(Caps {
        condstore: c.has_str("CONDSTORE"),
        mv: c.has_str("MOVE"),
        uidplus: c.has_str("UIDPLUS"),
        idle: c.has_str("IDLE"),
        notify: c.has_str("NOTIFY"),
        literal_plus: c.has_str("LITERAL+") || c.has_str("LITERAL-"),
        list_status: c.has_str("LIST-STATUS"),
        special_use: c.has_str("SPECIAL-USE"),
        gmail: c.has_str("X-GM-EXT-1"),
        quota: c.has_str("QUOTA"),
    })
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub mailbox: String,
    pub done: usize,
    pub total: usize,
}

pub type OnProgress<'a> = dyn FnMut(Progress) + Send + 'a;

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub new_messages: usize,
    pub new_in_inbox: usize,
    pub flushed_ops: usize,
    /// The local cache changed: the UI must reload.
    pub changed: bool,
    /// New inbox messages (not on a first pass), for notifications.
    pub new_inbox_ids: Vec<i64>,
    /// New messages (not on a first pass) outside the bin, junk, sent, drafts, screened-out and
    /// Gmail's All Mail: those whose DKIM signatures facteur checks on arrival (and a copy moved by
    /// another device, which the check tells apart).
    pub received_ids: Vec<i64>,
    /// Given by the caller when the account has one connection for everything: how many other
    /// requests wait for it. Between two folders, the sync then gives it up for them.
    pub waiting: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// The sync stopped early for them: to be resumed.
    pub handed_over: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every folder, the folder list included.
    All,
    /// The inbox only: what IDLE reports, as fast as possible.
    Inbox,
    /// Folders with a role, without listing the folders again: the periodic sync between two full
    /// ones.
    System,
    /// IMAP names, as reported by NOTIFY.
    Folders(Vec<String>),
}

/// The caller has just replayed the ops queue (`flushed_ops` operations) under its lock.
/// `on_folder` runs after each folder whose content changed, so the UI shows arrivals without
/// waiting for the other folders.
#[allow(clippy::too_many_arguments)]
pub async fn sync_account(
    store: &Store,
    account: &Account,
    session: &mut Session,
    caps: &Caps,
    flushed_ops: usize,
    scope: Scope,
    progress: &mut OnProgress<'_>,
    on_folder: &mut (dyn FnMut() + Send),
) -> Result<Report> {
    let mut report = Report::default();
    sync_account_into(store, account, session, caps, flushed_ops, scope, progress, on_folder, &mut report).await?;
    Ok(report)
}

/// Adds to `report` as it goes: what folders synced before an error found (INBOX first, its new
/// state saved) is kept for the caller, whose rules, screener and notifications would otherwise
/// never see those messages.
#[allow(clippy::too_many_arguments)]
pub async fn sync_account_into(
    store: &Store,
    account: &Account,
    session: &mut Session,
    caps: &Caps,
    flushed_ops: usize,
    scope: Scope,
    progress: &mut OnProgress<'_>,
    on_folder: &mut (dyn FnMut() + Send),
    report: &mut Report,
) -> Result<()> {
    let caps = *caps;
    report.flushed_ops += flushed_ops;

    let mut steps = crate::perf::Steps::new(|| format!("account {} folder list", account.id));
    let mut statuses: HashMap<String, imap::FolderStatus> = HashMap::new();
    if scope == Scope::All {
        // Never empty: an empty listing is an error (a connection gone bad), and the folder answers
        // that would follow on it cannot be trusted either.
        let (listed, status) = if caps.list_status {
            let items = format!("STATUS {}", status_items(&caps));
            let ask = if caps.special_use { format!("SPECIAL-USE {items}") } else { items };
            crate::error::bounded(120, "LIST", imap::list_with_status(session, Some(&ask))).await?
        } else {
            (crate::error::bounded(120, "LIST", imap::list_mailboxes(session)).await?, HashMap::new())
        };
        statuses = status;
        let known = store.mailbox_listing(account.id)?;
        report.changed |= report.flushed_ops > 0 || listed.len() != known.len() || listed.iter().any(|l| !known.contains_key(&l.name));
        // Folders created, renamed or deleted in facteur whose op has not left yet: the server does
        // not show them that way yet, so neither delete the new ones (and their messages) nor bring
        // back the old ones. A rename moves the whole subtree.
        let mut arriving = HashSet::new();
        let mut leaving = HashSet::new();
        let mut renamed: Vec<(String, String)> = Vec::new();
        for (kind, mailbox, dest) in store.pending_folder_ops(account.id)? {
            match kind.as_str() {
                "create" => {
                    arriving.insert(mailbox);
                }
                "rename" => {
                    if let Some(dest) = &dest {
                        renamed.push((mailbox.clone(), dest.clone()));
                    }
                    leaving.insert(mailbox);
                    arriving.extend(dest);
                }
                _ => {
                    leaving.insert(mailbox);
                }
            }
        }
        let is_leaving = |name: &str, delimiter: Option<&str>| {
            leaving.contains(name) || renamed.iter().any(|(from, _)| is_under(name, from, delimiter))
        };
        let is_arriving = |name: &str, delimiter: Option<&str>| {
            arriving.contains(name) || renamed.iter().any(|(_, to)| is_under(name, to, delimiter))
        };
        let mut keep = Vec::with_capacity(listed.len());
        for l in listed.iter().filter(|l| !is_leaving(&l.name, l.delimiter.as_deref())) {
            // Written only when new or changed: this runs at every sync.
            match known.get(&l.name) {
                Some((id, delimiter, role, selectable)) if *delimiter == l.delimiter && *role == l.role && *selectable == l.selectable => {
                    keep.push(*id)
                }
                _ => keep.push(store.upsert_mailbox(account.id, &l.name, l.delimiter.as_deref(), l.role, l.selectable)?),
            }
        }
        // Read again: a folder created in facteur meanwhile must not be taken for a vanished one.
        let current = store.mailbox_listing(account.id)?;
        keep.extend(current.iter().filter(|(name, (_, d, ..))| is_arriving(name, d.as_deref())).map(|(_, (id, ..))| *id));
        // A listing without INBOX (empty, cut short) is not trusted to delete folders, which would
        // take their whole cache with them.
        if !listed.iter().any(|l| l.name.eq_ignore_ascii_case("INBOX")) {
            tracing::warn!("folder listing without INBOX ({} folders): no folder removed", listed.len());
        } else if current.values().any(|(id, ..)| !keep.contains(id)) {
            // Deleting a folder drops its cache: require a second listing to leave it out too, so one
            // cut at a line boundary (right after INBOX) never takes the rest.
            let again = crate::error::bounded(120, "LIST", imap::list_mailboxes(session)).await?;
            let relisted: HashSet<&str> = again.iter().map(|l| l.name.as_str()).collect();
            let still: Vec<i64> = current
                .iter()
                .filter(|(name, (id, d, ..))| !keep.contains(id) && relisted.contains(name.as_str()) && !is_leaving(name, d.as_deref()))
                .map(|(_, (id, ..))| *id)
                .collect();
            if !still.is_empty() {
                tracing::warn!("{} folders missing from one listing but not from the next: kept", still.len());
            }
            keep.extend(still);
            if current.values().any(|(id, ..)| !keep.contains(id)) {
                store.retain_mailboxes(account.id, &keep)?;
            }
        }
    } else {
        report.changed |= report.flushed_ops > 0;
    }
    steps.mark("list");
    drop(steps);

    // Gmail: folders become labels (see docs/design/providers.md). The conversion waits
    // for an empty queue, whose operations name the copies it removes.
    let mut labels_model = store.labels_model(account.id)?;
    if caps.gmail && !labels_model && scope == Scope::All && crate::rt::offload(|| store.convert_to_labels(account.id))? {
        labels_model = true;
        report.changed = true;
        if let Some(all) = store.mailbox_by_role(account.id, Role::All)? {
            force_full_flags(all.id);
        }
    }
    if labels_model {
        let known = store.label_names(account.id)?;
        store.mark_label_folders(account.id)?;
        // A label created since the last listing was dropped from the messages read meanwhile, and
        // CHANGEDSINCE will not name them again: All Mail's labels are all read once more.
        if store.label_names(account.id)?.keys().any(|n| !known.contains_key(n))
            && let Some(all) = store.mailbox_by_role(account.id, Role::All)?
        {
            force_full_flags(all.id);
        }
    }
    let all_states = store.mailbox_states(account.id)?;
    // Labels' server counts, applied once All Mail (where labels are read) has synced.
    let label_counts: Vec<(i64, u32)> = all_states
        .iter()
        .filter(|s| labels_model && s.is_label)
        .filter_map(|s| Some((s.id, statuses.get(&s.name)?.messages?)))
        .collect();
    // A label stands for All Mail, where its messages are synced.
    let wants_all_mail = labels_model
        && match &scope {
            Scope::All | Scope::Inbox | Scope::System => true,
            Scope::Folders(names) => all_states.iter().any(|s| s.is_label && names.contains(&s.name)),
        };
    let mut states: Vec<MailboxState> = all_states
        .into_iter()
        .filter(|s| {
            s.selectable
                && !s.is_label
                && match &scope {
                    Scope::All => true,
                    Scope::Inbox => s.role == Some(Role::Inbox),
                    Scope::System => s.role.is_some(),
                    Scope::Folders(names) => names.contains(&s.name),
                }
                || wants_all_mail && s.role == Some(Role::All)
        })
        .collect();
    // The folder the user looks at goes first (with labels, All Mail holds the inbox).
    let first = if labels_model { Role::All } else { Role::Inbox };
    states.sort_by_key(|s| (s.role != Some(first), s.name.clone()));

    // With labels, an inbox count differing here and on the server forces All Mail open: its check
    // (`complete_gmail`) sets the labels right.
    let inbox_drift = labels_model
        && all_states_inbox(store, account.id)?.is_some_and(|(name, here)| {
            // Only the newest WINDOW inbox messages are cached: a larger inbox is no drift.
            statuses.get(&name).and_then(|s| s.messages).is_some_and(|n| here > i64::from(n) || here < i64::from(n).min(WINDOW as i64))
        });
    let mut untouched = 0;
    let mut synced = 0;
    for state in states {
        let drift = inbox_drift && state.role == Some(Role::All);
        // Without LIST-STATUS, ask a known folder for its STATUS alone: one command instead of the three
        // or four of opening it (on Outlook, about 0.1 s against 0.4 s, for dozens of folders).
        let mut status = statuses.get(&state.name).copied();
        if status.is_none() && matches!(scope, Scope::All | Scope::System) && !caps.list_status && may_stay_unopened(&state, &caps) {
            status = crate::error::bounded(30, "STATUS", imap::folder_status_of(session, &state.name, status_items(&caps))).await.ok();
        }
        let saved_unseen = if caps.condstore { None } else { store.server_unseen(state.id)? };
        if !drift && status.is_some_and(|s| unchanged_on_server(&state, &s, &caps, saved_unseen)) && settled(store, account.id, &state, &caps)? {
            untouched += 1;
            continue;
        }
        let state_id = state.id;
        // First pass on this folder: nothing is "new" to the user.
        let first_pass = state.uidvalidity.is_none();
        // With labels, All Mail receives: arrivals are told apart by their labels.
        let labelled = labels_model && state.role == Some(Role::All);
        let notify = state.role == Some(Role::Inbox) && !first_pass;
        let received = !first_pass && !matches!(state.role, Some(Role::Trash | Role::Junk | Role::Sent | Role::Drafts | Role::All | Role::Refused));
        // One folder at least each time, so that the sync always gets further.
        if synced > 0 && report.waiting.as_ref().is_some_and(|w| w.load(std::sync::atomic::Ordering::Relaxed) > 0) {
            report.handed_over = true;
            return Ok(());
        }
        synced += 1;
        let name = state.name.clone();
        match watched(&name, sync_mailbox(store, account.id, session, &caps, state, progress)).await {
            Ok((ids, changed)) => {
                if !caps.condstore {
                    store.set_server_unseen(state_id, status.and_then(|s| s.unseen))?;
                }
                report.changed |= changed;
                report.new_messages += ids.len();
                if changed {
                    on_folder();
                }
                if labelled && !first_pass {
                    let inbox = store.with_label_role(&ids, &[Role::Inbox])?;
                    let elsewhere = store.with_label_role(&ids, &[Role::Sent, Role::Drafts, Role::Refused])?;
                    report.received_ids.extend(ids.iter().filter(|id| !elsewhere.contains(id)));
                    let arrived: Vec<i64> = ids.iter().copied().filter(|id| inbox.contains(id)).collect();
                    report.new_in_inbox += arrived.len();
                    report.new_inbox_ids.extend(arrived);
                    continue;
                }
                if received {
                    report.received_ids.extend(&ids);
                }
                if notify {
                    report.new_in_inbox += ids.len();
                    report.new_inbox_ids.extend(ids);
                }
            }
            // An inaccessible folder must not block the others.
            Err(crate::Error::Imap(async_imap::error::Error::No(msg))) => tracing::warn!("folder skipped: {msg}"),
            // Refused or unreadable command for this folder (odd name, server quirk): the others still
            // sync.
            Err(e @ crate::Error::Imap(async_imap::error::Error::Bad(_) | async_imap::error::Error::Parse(_) | async_imap::error::Error::Validate(_))) => {
                tracing::warn!("folder {} skipped: {e}", crate::utf7::decode(&name))
            }
            // Nor a message the database refuses (malformed header): retried at the next sync. A full
            // disk, or a busy, read-only or broken database is not about this folder: the sync stops.
            Err(e @ crate::Error::Mime(_)) => tracing::warn!("folder {} skipped: {e}", crate::utf7::decode(&name)),
            Err(e @ crate::Error::Db(_)) if message_db_error(&e) => tracing::warn!("folder {} skipped: {e}", crate::utf7::decode(&name)),
            Err(e) => return Err(e),
        }
    }
    if untouched > 0 {
        tracing::debug!("account {}: {untouched} folders unchanged on the server, left unopened", account.id);
    }
    for (id, count) in label_counts {
        report.changed |= store.set_label_count(id, count)?;
    }
    if scope == Scope::All && caps.quota && account.quota.is_none_or(|q| crate::store::now() - q.at >= QUOTA_EVERY) {
        match read_quota(session).await {
            Ok(Some((used, limit))) => store.set_quota(account.id, used, limit)?,
            Ok(None) => {}
            Err(e) if e.is_connection_drop() => return Err(e),
            // Display only: a refusal does not hold the sync up.
            Err(e) => tracing::info!("quota of account {}: {e}", account.id),
        }
    }
    if scope == Scope::All {
        store.set_last_sync(account.id)?;
    }
    Ok(())
}

fn all_states_inbox(store: &Store, account_id: i64) -> Result<Option<(String, i64)>> {
    let Some(inbox) = store.mailbox_by_role(account_id, Role::Inbox)? else { return Ok(None) };
    if !inbox.is_label {
        return Ok(None);
    }
    Ok(Some((inbox.name, store.label_count(inbox.id)?)))
}

/// Seconds.
const QUOTA_EVERY: i64 = 3600;

/// (used, limit) in bytes (the server counts in KiB), when the server sets a storage limit.
async fn read_quota(session: &mut Session) -> Result<Option<(i64, i64)>> {
    use async_imap::types::QuotaResourceName;
    let (_, quotas) = crate::error::bounded(30, "GETQUOTAROOT", async { Ok(session.get_quota_root("INBOX").await?) }).await?;
    Ok(quotas
        .iter()
        .flat_map(|q| &q.resources)
        .find(|r| r.name == QuotaResourceName::Storage && r.limit > 0)
        .map(|r| ((r.usage as i64).saturating_mul(1024), (r.limit as i64).saturating_mul(1024))))
}

/// With CONDSTORE, HIGHESTMODSEQ reveals any flag change; without, the unread count reveals at
/// least read/unread changes made elsewhere.
fn status_items(caps: &Caps) -> &'static str {
    if caps.condstore { "(MESSAGES UIDNEXT UIDVALIDITY HIGHESTMODSEQ)" } else { "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN)" }
}

/// Without CONDSTORE the inbox is always opened: it is the folder looked at most, where a pin
/// made elsewhere must show.
fn may_stay_unopened(state: &MailboxState, caps: &Caps) -> bool {
    state.sync_from_uid.is_some() && (caps.condstore || state.role != Some(Role::Inbox))
}

/// Without CONDSTORE only the unread count (`saved_unseen`) reveals flag changes: other flags wait
/// for the hourly full reread (`settled`).
fn unchanged_on_server(state: &MailboxState, status: &imap::FolderStatus, caps: &Caps, saved_unseen: Option<u32>) -> bool {
    let same = may_stay_unopened(state, caps)
        && state.uidvalidity.is_some()
        && status.uidvalidity == state.uidvalidity
        && state.uidnext.is_some()
        && status.uidnext == state.uidnext
        && status.messages.is_some_and(|n| state.server_count == Some(i64::from(n)));
    if caps.condstore {
        same && state.highestmodseq.is_some() && status.highestmodseq == state.highestmodseq
    } else {
        same && status.unseen.is_some() && status.unseen == saved_unseen
    }
}

/// Nothing local waits on a read of the folder: no periodic check due, no move or deletion
/// queued from or into it, no message to restore, no row awaiting its UID, no body to fetch.
fn settled(store: &Store, account_id: i64, state: &MailboxState, caps: &Caps) -> Result<bool> {
    if peek_due(store, FULL_LIST, state.id, FULL_LIST_EVERY) || peek_due(store, FULL_FLAGS, state.id, full_flags_every(caps)) {
        return Ok(false);
    }
    if restoring().contains_key(&state.id) || store.moves_pending_into(state.id)? || !store.in_flight_uids(account_id, &state.name)?.is_empty() {
        return Ok(false);
    }
    if !store.folder_settled(state.id, false, PREFETCH_MAX_SIZE, 0)? {
        return Ok(false);
    }
    // Bodies still to fetch, but for those the server would not send.
    if !prefetched(store, state)? {
        return Ok(true);
    }
    let since = crate::store::now() - PREFETCH_DAYS * 86_400;
    let missing = store.missing_bodies(state.id, PREFETCH_BODIES, PREFETCH_MAX_SIZE, since)?;
    let unsent = unsent_bodies().lock().unwrap_or_else(|e| e.into_inner()).get(&state.id).cloned().unwrap_or_default();
    Ok(missing.iter().all(|(_, uid, _)| unsent.contains(uid)))
}

/// Per folder, the bodies a prefetch asked for and the server did not send (a damaged message,
/// Gmail's "Some messages could not be FETCHed"): not asked again until a restart, and no reason
/// for the folder to be opened at each sync.
fn unsent_bodies() -> &'static std::sync::Mutex<HashMap<i64, HashSet<u32>>> {
    static UNSENT: std::sync::OnceLock<std::sync::Mutex<HashMap<i64, HashSet<u32>>>> = std::sync::OnceLock::new();
    UNSENT.get_or_init(Default::default)
}

/// Not the bin, junk, nor All Mail (which copies the other folders) unless Gmail's labels make
/// it the one holding them.
fn prefetched(store: &Store, state: &MailboxState) -> Result<bool> {
    Ok(match state.role {
        Some(Role::Trash | Role::Junk) => false,
        Some(Role::All) => store.labels_model(state.account_id)?,
        _ => true,
    })
}

/// A folder sync with no progress for this long is given up: the connection is presumed dead
/// and the account sync fails so the engine reconnects, instead of waiting for the system's TCP
/// timeout (half an hour or more).
const FOLDER_STALL: std::time::Duration = std::time::Duration::from_millis(if cfg!(test) { 300 } else { 300_000 });
const STALL_TICK: std::time::Duration = std::time::Duration::from_millis(if cfg!(test) { 20 } else { 15_000 });

tokio::task_local! {
    /// Watchdog's sign of life, per task so another account's progress does not count.
    static ACTIVITY: std::sync::Arc<std::sync::atomic::AtomicU64>;
}

fn alive() {
    let _ = ACTIVITY.try_with(|a| a.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

/// The deadline moves on at every batch, so a first sync of a large folder is never cut short
/// while it advances.
async fn watched<T>(name: &str, job: impl Future<Output = Result<T>>) -> Result<T> {
    use std::sync::atomic::Ordering::Relaxed;
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let job = ACTIVITY.scope(counter.clone(), job);
    tokio::pin!(job);
    let (mut seen, mut since) = (0, std::time::Instant::now());
    loop {
        tokio::select! {
            res = &mut job => return res,
            _ = tokio::time::sleep(STALL_TICK) => {
                let now = counter.load(Relaxed);
                if now != seen {
                    (seen, since) = (now, std::time::Instant::now());
                } else if since.elapsed() >= FOLDER_STALL {
                    let folder = crate::utf7::decode(name);
                    return Err(crate::Error::Other(crate::tr!(
                        "synchro de {folder} : le serveur ne répond plus",
                        "sync of {folder}: the server stopped responding"
                    )));
                }
            }
        }
    }
}

fn is_under(name: &str, parent: &str, delimiter: Option<&str>) -> bool {
    name == parent
        || delimiter
            .filter(|d| !d.is_empty())
            .is_some_and(|d| name.strip_prefix(parent).is_some_and(|rest| rest.starts_with(d)))
}

/// Flags of the whole folder are only reread this often; in between, only the newest messages'
/// (without CONDSTORE) or what changed (CONDSTORE). The full reread also heals divergences
/// CONDSTORE never reports (a change missed by an earlier sync), rare enough to look for less
/// often.
const FULL_FLAGS_EVERY: i64 = 3600;
const FULL_FLAGS_EVERY_CONDSTORE: i64 = 6 * 3600;

fn full_flags_every(caps: &Caps) -> i64 {
    if caps.condstore { FULL_FLAGS_EVERY_CONDSTORE } else { FULL_FLAGS_EVERY }
}
const RECENT_FLAGS: usize = 500;
const FLAGS_CHUNK: usize = 1000;

/// A message skipped once (being moved during a sync, say) is fetched at the latest then. Kept in
/// the database so a launch does not check every folder again (on a slow server, three commands
/// per folder instead of one).
const FULL_LIST_EVERY: i64 = 6 * 3600;

#[derive(Default)]
struct Check {
    last: Option<i64>,
    forced: bool,
    /// Bumped at each request: a check that started before it does not answer it.
    epoch: u64,
    /// `last` was read from the database, where it persists across launches.
    loaded: bool,
}

fn periodic() -> std::sync::MutexGuard<'static, HashMap<(u8, i64), Check>> {
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<HashMap<(u8, i64), Check>>> = OnceLock::new();
    LAST.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Recorded as done only by `done`, once it has really run: a sync failing halfway leaves it due.
#[derive(Debug, Clone, Copy)]
struct Ticket {
    kind: u8,
    mailbox_id: i64,
    epoch: u64,
}

fn due(store: &Store, kind: u8, mailbox_id: i64, every: i64) -> Option<Ticket> {
    let loaded = periodic().get(&(kind, mailbox_id)).is_some_and(|c| c.loaded);
    // Read outside the lock; an unreadable date leaves the check due.
    let kept = if loaded { None } else { store.full_check_at(mailbox_id, kind == FULL_LIST).ok().flatten() };
    let now = crate::store::now();
    let mut checks = periodic();
    let c = checks.entry((kind, mailbox_id)).or_default();
    if !c.loaded {
        c.loaded = true;
        c.last = c.last.max(kept);
    }
    if !c.forced && c.last.is_some_and(|t| now - t < every) {
        return None;
    }
    Some(Ticket { kind, mailbox_id, epoch: c.epoch })
}

fn peek_due(store: &Store, kind: u8, mailbox_id: i64, every: i64) -> bool {
    due(store, kind, mailbox_id, every).is_some()
}

fn done(store: &Store, t: Ticket) {
    let now = crate::store::now();
    if let Err(e) = store.set_full_check_at(t.mailbox_id, t.kind == FULL_LIST, now) {
        tracing::info!("full check of folder {} not kept: {e}", t.mailbox_id);
    }
    let mut checks = periodic();
    let c = checks.entry((t.kind, t.mailbox_id)).or_default();
    c.last = Some(now);
    // Asked again since this check started (fetch_all_history during a sync): the request still
    // stands for the next sync.
    if c.epoch == t.epoch {
        c.forced = false;
    }
}

const FULL_FLAGS: u8 = 0;
const FULL_LIST: u8 = 1;

pub fn force_full_check(mailbox_id: i64) {
    force(FULL_LIST, mailbox_id);
}

pub fn force_full_flags(mailbox_id: i64) {
    force(FULL_FLAGS, mailbox_id);
}

fn force(kind: u8, mailbox_id: i64) {
    let mut checks = periodic();
    let c = checks.entry((kind, mailbox_id)).or_default();
    c.forced = true;
    c.epoch += 1;
}

/// UIDs abandoned moves or deletes had taken out of a folder: fetched again at its next full
/// read even below the synced window, and not as new mail.
fn restoring() -> std::sync::MutexGuard<'static, HashMap<i64, HashSet<u32>>> {
    use std::sync::{Mutex, OnceLock};
    static MAP: OnceLock<Mutex<HashMap<i64, HashSet<u32>>>> = OnceLock::new();
    MAP.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// See `ops::recheck_folders`.
pub fn restore_uids(mailbox_id: i64, uids: impl IntoIterator<Item = u32>) {
    restoring().entry(mailbox_id).or_default().extend(uids);
}

/// More may have been added meanwhile, so only the handled UIDs leave.
fn restored(mailbox_id: i64, uids: &HashSet<u32>) {
    let mut map = restoring();
    if let Some(set) = map.get_mut(&mailbox_id) {
        set.retain(|u| !uids.contains(u));
        if set.is_empty() {
            map.remove(&mailbox_id);
        }
    }
}

/// For an abandoned operation: what it changed locally ahead of the server must come back, so
/// its folders are read in full at their next sync. Call before the op leaves the queue.
pub fn recheck_op_folders(store: &Store, op_id: i64) -> Result<()> {
    for mailbox_id in store.op_folders(op_id)? {
        force_full_check(mailbox_id);
    }
    Ok(())
}

/// Streams the answer instead of keeping every response (and its buffer) in memory.
/// `labels`: `query` asks for X-GM-LABELS too, and they are collected there.
/// Read up to the tagged end, which async-imap's FETCH stream does not tell. `Ok(false)`: the
/// server answered NO with part of the messages (Gmail: "Some messages could not be FETCHed"):
/// what it gave is kept, but it is not the whole.
async fn fetch_flags(
    session: &mut Session,
    set: &str,
    query: &str,
    out: &mut Vec<(u32, crate::store::ServerFlags)>,
    mut labels: Option<&mut Vec<(u32, Vec<String>)>>,
) -> Result<bool> {
    use async_imap::imap_proto::{AttributeValue, Response, Status};
    let tag = session.run_command(format!("UID FETCH {set} {query}")).await?;
    loop {
        let Some(answer) = session.read_response().await? else {
            return Err(crate::Error::Imap(async_imap::error::Error::ConnectionLost));
        };
        match answer.parsed() {
            Response::Fetch(_, attrs) => {
                let uid = attrs.iter().find_map(|a| if let AttributeValue::Uid(u) = a { Some(*u) } else { None });
                let flags = attrs.iter().find_map(|a| if let AttributeValue::Flags(f) = a { Some(f) } else { None });
                let (Some(uid), Some(flags)) = (uid, flags) else { continue };
                out.push((uid, server_flags(flags.iter().map(|f| async_imap::types::Flag::from(f.as_ref())))));
                let given = attrs.iter().find_map(|a| if let AttributeValue::GmailLabels(l) = a { Some(l) } else { None });
                if let (Some(labels), Some(given)) = (labels.as_deref_mut(), given) {
                    labels.push((uid, given.iter().map(|l| l.to_string()).collect()));
                }
            }
            Response::Done { tag: done, status, outcome } if *done == tag => {
                return match status {
                    Status::Ok => Ok(true),
                    Status::No => Ok(false),
                    _ => Err(crate::Error::Imap(async_imap::error::Error::Bad(outcome.information.as_deref().unwrap_or("FETCH").to_string()))),
                };
            }
            Response::Data { status: Status::Bye, .. } => return Err(crate::Error::Imap(async_imap::error::Error::ConnectionLost)),
            _ => {}
        }
    }
}

/// Label folders by X-GM-LABELS name, when the folder is All Mail of an account converted to
/// labels.
fn gmail_labels(store: &Store, state: &MailboxState) -> Result<Option<HashMap<String, i64>>> {
    if state.role != Some(Role::All) || !store.labels_model(state.account_id)? {
        return Ok(None);
    }
    Ok(Some(store.label_names(state.account_id)?))
}

/// Names with no folder (a label created since the last listing) are read again once the listing
/// makes one (see `sync_account_into`).
fn label_ids(names: &HashMap<String, i64>, given: &[String]) -> Vec<i64> {
    given
        .iter()
        .filter_map(|l| {
            let l = unescape_label(l);
            // System labels: `\Inbox`, sometimes given without the backslash or in other case.
            names
                .get(&l)
                .or_else(|| names.get(&format!("\\{l}")))
                .or_else(|| names.iter().find(|(n, _)| n.starts_with('\\') && n.eq_ignore_ascii_case(&l)).map(|(_, id)| id))
        })
        .copied()
        .collect()
}

/// Gmail quotes its system labels (`"\\Inbox"`), and the parser keeps the escapes.
pub(crate) fn unescape_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some(next @ ('\\' | '"'))) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// Skips messages with a queued change still on its way: a label change, or a pin (the Starred
/// label follows `\Flagged`).
fn keep_server_labels(store: &Store, state: &MailboxState, names: &HashMap<String, i64>, given: &[(u32, Vec<String>)]) -> Result<bool> {
    if given.is_empty() {
        return Ok(false);
    }
    let mut keep = store.pending_label_uids(state.account_id, &state.name)?;
    keep.extend(store.pending_flag_uids(state.account_id, &state.name)?);
    let labels: Vec<(u32, Vec<i64>)> = given.iter().map(|(uid, l)| (*uid, label_ids(names, l))).collect();
    store.set_server_labels(state.id, &labels, &keep)
}

/// Spreads the cache's backlog over a few syncs.
const GMAIL_THREADS_PER_SYNC: usize = 5000;

/// Gmail's thread id becomes the conversation here, for All Mail messages not read yet (new ones,
/// and the cache from before). All Mail must be selected.
async fn read_gmail_threads(store: &Store, session: &mut Session, state: &MailboxState) -> Result<bool> {
    let refused = refused_threads().get(&state.id).cloned().unwrap_or_default();
    let uids: Vec<u32> = store.uids_without_gmail_thread(state.id, GMAIL_THREADS_PER_SYNC)?.into_iter().filter(|u| !refused.contains(u)).collect();
    if uids.is_empty() {
        return Ok(false);
    }
    let mut given = Vec::with_capacity(uids.len());
    for chunk in uids.chunks(FLAGS_CHUNK) {
        let (threads, whole) = imap::gmail_threads(session, &uid_set(chunk)).await?;
        if !whole {
            // Refused ones are not asked for again on every sync (until a restart): one refusal
            // must not fail All Mail's sync, and with it the arrivals' notifications.
            let got: HashSet<u32> = threads.iter().map(|(uid, _)| *uid).collect();
            refused_threads().entry(state.id).or_default().extend(chunk.iter().filter(|u| !got.contains(u)));
        }
        given.extend(threads);
        alive();
    }
    let account_id = state.account_id;
    let id = state.id;
    crate::rt::offload(|| store.set_gmail_threads(account_id, id, &given))
}

/// Per All Mail folder, messages whose thread id Gmail refuses to give.
fn refused_threads() -> std::sync::MutexGuard<'static, HashMap<i64, HashSet<u32>>> {
    use std::sync::{Mutex, OnceLock};
    static MAP: OnceLock<Mutex<HashMap<i64, HashSet<u32>>>> = OnceLock::new();
    MAP.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The inbox, facteur's own folders (screener, refused, snoozed) and pinned messages are cached
/// whatever their age, as they were when those folders synced on their own. All Mail must be
/// selected.
async fn complete_gmail(store: &Store, session: &mut Session, state: &MailboxState, names: &HashMap<String, i64>) -> Result<bool> {
    let mut queries: Vec<(String, Option<i64>)> = vec![(crate::provider::gmail_label_search("INBOX", Some(Role::Inbox)), names.get("\\Inbox").copied()), ("FLAGGED".to_string(), None)];
    for id in names.values() {
        let folder = store.mailbox_state(*id)?;
        if matches!(folder.role, Some(Role::Screener | Role::Refused | Role::Snoozed)) {
            queries.push((crate::provider::gmail_label_search(&folder.name, folder.role), Some(*id)));
        }
    }
    let in_flight = store.in_flight_uids(state.account_id, &state.name)?;
    let mut pending = store.pending_label_uids(state.account_id, &state.name)?;
    pending.extend(store.pending_flag_uids(state.account_id, &state.name)?);
    let mut wanted: HashSet<u32> = HashSet::new();
    // Cached messages whose labels here disagree with the search: read again.
    let mut recheck: HashSet<u32> = HashSet::new();
    let mut drift: Vec<String> = Vec::new();
    for (q, label) in &queries {
        let found = imap::uid_search_checked(session, q, 0).await?;
        if let Some(label) = label {
            let here = store.label_uids(state.id, *label)?;
            let cached = store.mailbox_uids_among(state.id, &found.iter().copied().collect::<Vec<_>>())?;
            let there_only: Vec<u32> = found.iter().copied().filter(|u| cached.contains_key(u) && !here.contains(u)).collect();
            let here_only: Vec<u32> = here.iter().copied().filter(|u| !found.contains(u)).collect();
            if !there_only.is_empty() || !here_only.is_empty() {
                drift.push(format!("{q}: server only {there_only:?}, here only {here_only:?}"));
            }
            recheck.extend(there_only);
            recheck.extend(here_only);
        }
        // As those folders were synced: their newest messages, within the usual window.
        let mut found: Vec<u32> = found.into_iter().collect();
        found.sort_unstable_by(|a, b| b.cmp(a));
        found.truncate(WINDOW);
        wanted.extend(found);
    }
    let mut changed = false;
    // A local change still on its way is not undone by what the server says now.
    let recheck: Vec<u32> = recheck.into_iter().filter(|u| !in_flight.contains(u) && !pending.contains(u)).collect();
    if !recheck.is_empty() {
        let mut flags = Vec::new();
        let mut labels = Vec::new();
        for chunk in recheck.chunks(FLAGS_CHUNK) {
            fetch_flags(session, &uid_set(chunk), "(UID FLAGS X-GM-LABELS)", &mut flags, Some(&mut labels)).await?;
        }
        imap::still_open(session).await?;
        changed |= keep_server_labels(store, state, names, &labels)?;
        let read: Vec<String> = labels.iter().map(|(uid, l)| format!("{uid} {l:?}")).collect();
        tracing::info!(
            "labels of {} messages read again, they differed from the server's folders ({}); read: {}",
            recheck.len(),
            drift.join("; "),
            read.join(", ")
        );
    }
    let wanted: Vec<u32> = wanted.into_iter().collect();
    let local = store.mailbox_uids_among(state.id, &wanted)?;
    let mut missing: Vec<u32> = wanted.into_iter().filter(|u| !local.contains_key(u) && !in_flight.contains(u)).collect();
    if missing.is_empty() {
        return Ok(changed);
    }
    missing.sort_unstable_by(|a, b| b.cmp(a));
    let fetched = fetch_headers(store, session, state.account_id, state.id, &mut missing, &mut |_| {}, false).await?;
    Ok(changed || !fetched.ids.is_empty())
}

/// `known`: the folder's cached UIDs when already at hand; otherwise read only if needed.
async fn refresh_flags(
    store: &Store,
    session: &mut Session,
    caps: &Caps,
    state: &MailboxState,
    server_modseq: Option<u64>,
    known: Option<&[u32]>,
) -> Result<bool> {
    if known.is_some_and(|k| k.is_empty()) {
        return Ok(false);
    }
    // Read before the FETCH: a change leaving the queue after this point is already on the server
    // when its flags are read.
    let mut keep = store.pending_flag_uids(state.account_id, &state.name)?;
    let all_seen_below = store.mark_all_seen_below(state.account_id, &state.name)?;
    let full = due(store, FULL_FLAGS, state.id, full_flags_every(caps));
    let mut updates = Vec::new();
    let names = gmail_labels(store, state)?;
    let mut labels: Vec<(u32, Vec<String>)> = Vec::new();
    let items = if names.is_some() { "UID FLAGS X-GM-LABELS" } else { "UID FLAGS" };
    let mut sent = false;
    let mut whole = true;
    match (caps.condstore, state.highestmodseq) {
        (true, Some(modseq)) if full.is_none() && Some(modseq) == server_modseq => return Ok(false),
        // Whole folder, so messages older than the window that a search added follow too.
        (true, Some(modseq)) if full.is_none() => {
            let ask = names.is_some().then_some(&mut labels);
            whole &= fetch_flags(session, "1:*", &format!("({items}) (CHANGEDSINCE {modseq})"), &mut updates, ask).await?;
            sent = true;
        }
        _ => {
            let mut sorted = match known {
                Some(k) => k.to_vec(),
                None => store.mailbox_uid_list(state.id, full.is_none().then_some(RECENT_FLAGS))?,
            };
            sorted.sort_unstable();
            if full.is_none() {
                sorted = sorted.split_off(sorted.len().saturating_sub(RECENT_FLAGS));
            }
            for chunk in sorted.chunks(FLAGS_CHUNK) {
                let ask = names.is_some().then_some(&mut labels);
                whole &= fetch_flags(session, &uid_set(chunk), &format!("({items})"), &mut updates, ask).await?;
                sent = true;
                alive();
            }
        }
    }
    // A partial answer must not pass for the whole: the HIGHESTMODSEQ saved next would hide the
    // rest. The whole folder is read again at the next sync instead.
    if sent {
        imap::still_open(session).await?;
    }
    // A message the server always refuses (Gmail's "Some messages could not be FETCHed") would make
    // every sync read the whole folder again: after a few full reads that came back partial, the
    // folder is taken as read as far as it goes (until a restart).
    // Only full reads count, in a row: a whole one starts the count over, and a partial
    // CHANGEDSINCE read always asks for a full one.
    let given_up = full.is_some() && partial_full_reads(state.id, whole) >= PARTIAL_FULL_READS;
    if !whole && !given_up {
        force_full_flags(state.id);
    }
    // Changes queued while the FETCH ran predate nothing the server said: keep them too. Keeping
    // one that has left meanwhile is harmless.
    keep.extend(store.pending_flag_uids(state.account_id, &state.name)?);
    let all_seen_below = all_seen_below.max(store.mark_all_seen_below(state.account_id, &state.name)?);
    // Read locally already, "mark all as read" still on its way.
    if let Some(below) = all_seen_below {
        keep.extend(updates.iter().map(|(uid, _)| *uid).filter(|uid| *uid < below));
    }
    // A message whose server flags were set aside for a change on its way: its other flags may
    // have changed too, and CHANGEDSINCE will not name it again (a STORE that changes nothing
    // keeps its mod-sequence). The whole folder's flags are read at the next sync.
    if caps.condstore && updates.iter().any(|(uid, _)| keep.contains(uid)) {
        force_full_flags(state.id);
    }
    let mut changed = crate::rt::offload(|| store.update_flags(state.id, &updates, &keep))? > 0;
    if let Some(names) = &names {
        changed |= keep_server_labels(store, state, names, &labels)?;
    }
    if let Some(t) = full.filter(|_| whole || given_up) {
        done(store, t);
    }
    Ok(changed)
}

/// Full flag reads of a folder in a row that came back partial before it is taken as read.
const PARTIAL_FULL_READS: u8 = 3;

/// A full flag read of the folder: the count of those in a row that came back partial (0 once
/// one came back whole).
fn partial_full_reads(mailbox_id: i64, whole: bool) -> u8 {
    use std::sync::{Mutex, OnceLock};
    static COUNTS: OnceLock<Mutex<HashMap<i64, u8>>> = OnceLock::new();
    let mut counts = COUNTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    if whole {
        counts.remove(&mailbox_id);
        return 0;
    }
    let n = counts.entry(mailbox_id).or_default();
    *n = n.saturating_add(1);
    *n
}

/// The regular sync, a folder refresh and the history fetch use different connections; two of
/// them on one folder would each read the state then save their own (UIDNEXT, counts), or
/// report the same arrivals twice.
fn folder_sync_lock(account_id: i64) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::sync::{Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<i64, std::sync::Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).entry(account_id).or_default().clone()
}

/// Past the highest UID seen if that is higher than the server's (or it gave none).
fn next_uid(server: Option<u32>, highest_seen: Option<u32>) -> Option<u32> {
    let past = highest_seen.map(|h| h.saturating_add(1));
    match (server, past) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// Read-only (EXAMINE) without CONDSTORE.
async fn open_folder(session: &mut Session, caps: &Caps, name: &str) -> Result<async_imap::types::Mailbox> {
    Ok(if caps.condstore { session.select_condstore(name).await? } else { session.examine(name).await? })
}

/// A known folder whose answer leaves UIDVALIDITY out (or gives 0, which is invalid) was
/// answered by a failing connection: an error, never a change, which would wipe the cache.
/// The cached UIDs below `next` the server still has (`still`, its answer to a search of them),
/// when that is consistent with `departed` messages gone; `None` (the whole list is read instead)
/// for an empty answer, which may be a refused search, or more gone than the count says.
fn departures_only(cached: &[u32], still: HashSet<u32>, departed: i64, next: u32) -> Option<HashSet<u32>> {
    let asked: HashSet<u32> = cached.iter().copied().collect();
    let known: HashSet<u32> = still.into_iter().filter(|u| *u < next && asked.contains(u)).collect();
    let gone = asked.len().saturating_sub(known.len()) as i64;
    (!known.is_empty() && gone <= departed).then_some(known)
}

fn recreated(known: Option<u32>, read: Option<u32>) -> Result<bool> {
    match (known, read.filter(|v| *v != 0)) {
        (None, _) => Ok(false),
        (Some(_), None) => Err(crate::Error::Dropped(crate::tr!(
            "le serveur n'a pas donné l'UIDVALIDITY du dossier",
            "the server did not give the folder's UIDVALIDITY"
        ))),
        (Some(k), Some(r)) => Ok(k != r),
    }
}

/// Waits for any other folder sync of the account. `state` only names the folder: it is read
/// again once the turn has come.
pub async fn sync_mailbox(
    store: &Store,
    account_id: i64,
    session: &mut Session,
    caps: &Caps,
    state: MailboxState,
    progress: &mut OnProgress<'_>,
) -> Result<(Vec<i64>, bool)> {
    let mut steps = crate::perf::Steps::new(|| format!("account {account_id} {}", crate::utf7::decode(&state.name)));
    let lock = folder_sync_lock(account_id);
    let acquire = lock.lock_owned();
    tokio::pin!(acquire);
    // Waiting for the turn is not a stall: keep the watchdog informed.
    let _turn = loop {
        tokio::select! {
            turn = &mut acquire => break turn,
            _ = tokio::time::sleep(STALL_TICK) => alive(),
        }
    };
    steps.mark("turn");
    // What the previous sync saved, not what the caller read before waiting.
    let state = match store.mailbox_state(state.id) {
        Ok(s) => s,
        Err(crate::Error::NotFound(_)) => return Ok((Vec::new(), false)),
        Err(e) => return Err(e),
    };
    // A Gmail label is not synced on its own: its messages are All Mail's.
    let state = match state.is_label {
        true => match store.mailbox_by_role(state.account_id, Role::All)? {
            Some(all) => all,
            None => return Ok((Vec::new(), false)),
        },
        false => state,
    };
    sync_mailbox_locked(store, account_id, session, caps, state, progress, &mut steps).await
}

async fn sync_mailbox_locked(
    store: &Store,
    account_id: i64,
    session: &mut Session,
    caps: &Caps,
    mut state: MailboxState,
    progress: &mut OnProgress<'_>,
    steps: &mut crate::perf::Steps,
) -> Result<(Vec<i64>, bool)> {
    let mut changed = false;
    let before = state.clone();
    // Before reading the folder: a move into it finishing during the sync must not make its message
    // look orphaned.
    let moves_pending = store.moves_pending_into(state.id)?;
    let mb = open_folder(session, caps, &state.name).await?;
    steps.mark("select");
    // Name the folder, so a server answering its SELECT badly (damaged on its side) shows in the
    // log.
    let recreated = match recreated(state.uidvalidity, mb.uid_validity) {
        Ok(recreated) => recreated,
        Err(crate::Error::Dropped(why)) => {
            let said = imap::parting_words(session).map(|w| format!("; {w}")).unwrap_or_default();
            return Err(crate::Error::Dropped(format!("{} ({why}{said})", state.name)));
        }
        Err(e) => return Err(e),
    };

    // Same UIDVALIDITY, UIDNEXT and count: no arrival or deletion, so the UID list need not be
    // read again; only flags may have changed.
    let same_folder = state.uidvalidity.is_some() && state.uidvalidity == mb.uid_validity && state.sync_from_uid.is_some();
    // Now and then (and on demand), the whole UID list is read anyway, to fill any gap.
    let full_check = due(store, FULL_LIST, state.id, FULL_LIST_EVERY);
    let unchanged = full_check.is_none()
        && same_folder
        && state.uidnext.is_some()
        && state.uidnext == mb.uid_next
        && state.server_count == Some(mb.exists as i64);
    if unchanged {
        // The folder's UID map (tens of thousands on Gmail's All Mail) is not loaded: refresh_flags
        // reads only what it needs, often nothing (same HIGHESTMODSEQ).
        if mb.exists > 0 {
            changed |= refresh_flags(store, session, caps, &state, mb.highest_modseq, None).await?;
        }
        steps.mark("flags");
        changed |= prefetch_bodies(store, session, &state).await? > 0;
        steps.mark("bodies");
        if let Some(names) = gmail_labels(store, &state)? {
            changed |= complete_gmail(store, session, &state, &names).await?;
            changed |= read_gmail_threads(store, session, &state).await?;
            steps.mark("labels");
        }
        store.drop_orphan_placeholders(state.id, moves_pending)?;
        // In flight: still listed by the server, which has not changed since the last read.
        store.set_count_base(state.id, store.in_flight_uids(account_id, &state.name)?.len(), true)?;
        state.highestmodseq = mb.highest_modseq;
        if state != before {
            store.save_mailbox_state(&state)?;
        }
        steps.mark("store");
        return Ok((Vec::new(), changed));
    }

    if recreated {
        // Confirmed by a second answer before wiping the cache, so one garbled reply on a failing
        // connection never wipes it.
        let again = open_folder(session, caps, &state.name).await?;
        if again.uid_validity != mb.uid_validity {
            return Err(crate::Error::Other(crate::tr!(
                "{} : le serveur a donné deux UIDVALIDITY différentes",
                "{}: the server gave two different UIDVALIDITY values",
                crate::utf7::decode(&state.name)
            )));
        }
        tracing::info!("{}: UIDVALIDITY changed, folder cache reset", state.name);
        crate::rt::offload(|| store.clear_mailbox(state.id))?;
        unreadable().remove(&state.id);
        restoring().remove(&state.id);
        changed = true;
        state.highestmodseq = None;
        state.sync_from_uid = None;
        // Read again like a first pass: what comes back is the folder's old mail, not arrivals
        // (no notification, no rule, nothing held for the screener).
        state.uidvalidity = None;
    }
    let local = store.mailbox_uids(state.id)?;
    steps.mark("cache");

    // Only read the new UIDs. If the count adds up (old + new = total), nothing was deleted and the
    // full list is not needed: Gmail's All Mail holds tens of thousands.
    let previous_next = state.uidnext.filter(|_| same_folder);
    let mut server: Option<HashSet<u32>> = None;
    // Only the arrivals were read: below this UID, the cache stands for the server's list.
    let mut arrived_only: Option<u32> = None;
    // A queued purge (an emptied bin): read the whole list to count what it will take out of the
    // server's total (its rows are already gone, so the cache cannot stand for them).
    let purging = store.purge_pending_below(account_id, &state.name)?;
    if let (Some(next), Some(count)) = (previous_next, state.server_count.filter(|_| same_folder))
        && mb.exists > 0
        && full_check.is_none()
        && purging.is_none()
    {
        // `n:*` names at least the last UID: an empty answer is a refused search (or an odd server),
        // and the whole list is read below.
        let answer = imap::uid_search_checked(session, &format!("UID {next}:*"), 0).await?;
        let arrived: HashSet<u32> = answer.iter().copied().filter(|u| *u >= next).collect();
        let departed = count + arrived.len() as i64 - mb.exists as i64;
        if !answer.is_empty() && departed == 0 {
            let mut known: HashSet<u32> = local.keys().copied().filter(|u| *u < next).collect();
            known.extend(arrived);
            server = Some(known);
            arrived_only = Some(next);
        } else if !answer.is_empty() && departed > 0 {
            // Only departures (a delete, a move to spam: Gmail's All Mail loses it): which cached
            // messages are still there, rather than the folder's whole list (200,000 UIDs).
            let cached: Vec<u32> = local.keys().copied().filter(|u| *u < next).collect();
            if !cached.is_empty() {
                let still = imap::uid_search_checked(session, &format!("UID {}", imap::uid_set(&cached)), 0).await?;
                if let Some(known) = departures_only(&cached, still, departed, next) {
                    let mut known = known;
                    known.extend(arrived);
                    server = Some(known);
                    arrived_only = Some(next);
                }
            }
        }
    }
    // The list that decides what is gone: a refused search or cut connection must never read as
    // an empty folder (see `uid_search_checked`). Nor an empty SELECT answer, which is what one
    // cut after its UIDVALIDITY line looks like: the session must still answer.
    let server: HashSet<u32> = match server {
        Some(s) => s,
        // Checked even with nothing cached: a never-synced folder saved empty from a cut SELECT would
        // start its window at UID 1 for good (its whole history fetched next time).
        None if mb.exists == 0 => {
            imap::still_open(session).await?;
            HashSet::new()
        }
        None => imap::uid_search_checked(session, "ALL", mb.exists).await?,
    };
    steps.mark(if arrived_only.is_some() { "search new" } else { "search all" });
    // The list read after SELECT: a message arrived in between is in it, and counted.
    state.server_count = Some(if arrived_only.is_some() { mb.exists as i64 } else { server.len() as i64 });
    let gone: Vec<u32> = local.keys().filter(|u| !server.contains(u)).copied().collect();
    if !gone.is_empty() {
        crate::rt::offload(|| store.delete_uids(state.id, &gone))?;
        changed = true;
    }

    let mut sorted: Vec<u32> = server.iter().copied().collect();
    sorted.sort_unstable();
    let from = *state.sync_from_uid.get_or_insert_with(|| {
        sorted.len().checked_sub(WINDOW).map(|i| sorted[i]).unwrap_or(1)
    });

    // Newest first, so the UI fills from the top; skipping what the user just moved or deleted
    // (op not sent yet).
    let in_flight = store.in_flight_uids(account_id, &state.name)?;
    // Moved or deleted by facteur, refused for good by the server: back in the cache, but not new
    // mail, nor anything the screener, a rule or the blocklist should move again (refused again,
    // sync after sync).
    let refused = store.failed_op_uids(account_id, &state.name)?;
    // Given back by an abandoned move or delete (op gone from the queue): read only with the whole
    // UID list, which tells whether the server still has them.
    let restore: HashSet<u32> = match arrived_only {
        None => restoring().get(&state.id).cloned().unwrap_or_default(),
        Some(_) => HashSet::new(),
    };
    // Refused and given-back messages come back even from below the window (found by a server
    // search, then moved); a full read skips the rest of what lies there.
    let mut new: Vec<u32> = sorted
        .iter()
        .rev()
        .filter(|u| (**u >= from || refused.contains(u) || restore.contains(u)) && !local.contains_key(u) && !in_flight.contains(u) && !purging.is_some_and(|b| **u < b))
        .copied()
        .collect();
    let label = crate::utf7::decode(&state.name);
    let total = new.len();
    let mut on_chunk = |done: usize| progress(Progress { mailbox: label.clone(), done, total });
    steps.mark("gone");
    // Arrivals into the inbox (All Mail with Gmail labels) wait for the screener, out of the lists,
    // when it is on; never on a first pass, which nobody screens.
    let hold = state.uidvalidity.is_some()
        && (state.role == Some(Role::Inbox) || state.role == Some(Role::All) && store.labels_model(account_id)?)
        && store.account(account_id)?.screener_since.is_some();
    let mut fetched = fetch_headers(store, session, account_id, state.id, &mut new, &mut on_chunk, hold).await?;
    steps.mark("headers");
    // Sent or discarded drafts whose deletion is still queued (by Message-ID, so `in_flight`
    // cannot see them): fetched back with the whole UID list, they go again.
    let unsent = if fetched.fresh.is_empty() { HashSet::new() } else { store.pending_draft_deletes(account_id, &state.name)? };
    if !unsent.is_empty() {
        let mut back = HashSet::new();
        for id in &fetched.fresh {
            if store.message_ids(*id).ok().and_then(|m| m.0).is_some_and(|mid| unsent.contains(&mid)) {
                back.insert(*id);
            }
        }
        let uids: Vec<u32> = fetched.ids.iter().filter(|(_, id)| back.contains(id)).map(|(u, _)| *u).collect();
        if !uids.is_empty() {
            crate::rt::offload(|| store.delete_uids(state.id, &uids))?;
            fetched.fresh.retain(|id| !back.contains(id));
        }
    }
    changed |= !fetched.fresh.is_empty();
    let held_back: HashSet<i64> = fetched.ids.iter().filter(|(u, _)| refused.contains(u) || restore.contains(u)).map(|(_, id)| *id).collect();
    let mut new_ids: Vec<i64> = fetched.fresh.into_iter().filter(|id| !held_back.contains(id)).collect();
    // Arrived since the last sync but already cached by a server search meanwhile: still new
    // (notifications, rules).
    if let Some(next) = previous_next {
        new_ids.extend(local.iter().filter(|(u, _)| **u >= next && server.contains(u) && !refused.contains(u)).map(|(_, id)| *id));
    }
    if recreated {
        new_ids.clear();
    }

    let known: Vec<u32> = local.keys().filter(|u| server.contains(u)).copied().collect();
    steps.mark("drafts");
    changed |= refresh_flags(store, session, caps, &state, mb.highest_modseq, Some(&known)).await?;
    steps.mark("flags");

    // Prefetched bodies change the preview shown in the list.
    changed |= prefetch_bodies(store, session, &state).await? > 0;
    steps.mark("bodies");
    if let Some(names) = gmail_labels(store, &state)? {
        changed |= complete_gmail(store, session, &state, &names).await?;
        changed |= read_gmail_threads(store, session, &state).await?;
        steps.mark("labels");
    }
    store.drop_orphan_placeholders(state.id, moves_pending)?;
    // Moved or deleted locally, still on the server. When only the arrivals were read, those below
    // them are not in `server` (their rows left the folder) but the server, whose count matched,
    // still holds them.
    let leaving = leaving_count(&server, &in_flight, arrived_only, purging, |u| local.contains_key(&u) || refused.contains(&u));
    store.set_count_base(state.id, leaving, false)?;

    state.uidvalidity = mb.uid_validity.filter(|v| *v != 0);
    // Never below what this sync has seen: a UIDNEXT read before the last arrivals (or not given)
    // would report them as new again at the next sync.
    state.uidnext = next_uid(mb.uid_next, sorted.last().copied());
    state.highestmodseq = mb.highest_modseq;
    if state != before {
        store.save_mailbox_state(&state)?;
    }
    if let Some(t) = full_check {
        done(store, t);
    }
    if !restore.is_empty() {
        restored(state.id, &restore);
    }
    steps.mark("store");
    let changed = changed || !new_ids.is_empty();
    Ok((new_ids, changed))
}

/// `fresh`: truly new messages, not one already cached nor a user's move reaching its
/// destination.
pub struct Fetched {
    pub ids: HashMap<u32, i64>,
    pub fresh: Vec<i64>,
}

/// Messages the server could not send readably (or the cache refused): `suspect` ones are
/// fetched one at a time, `bad` ones are left out until restart. One malformed message must
/// never block a folder (INBOX above all) forever.
#[derive(Default)]
struct Unreadable {
    suspect: HashSet<u32>,
    bad: HashSet<u32>,
    /// Times fetched alone and left out of the answer without an error.
    absent: HashMap<u32, u8>,
}

/// Messages still on the server but on their way out, left out of the folder's total: moved or
/// deleted locally (`in_flight`), and those a queued purge below `purging` will expunge (rows
/// already gone). `kept`: still cached, or given back by a refused move, which the purge leaves
/// alone. With `arrived_only`, in-flight UIDs below it are missing from `server` but still on
/// the server.
fn leaving_count(server: &HashSet<u32>, in_flight: &HashSet<u32>, arrived_only: Option<u32>, purging: Option<u32>, kept: impl Fn(u32) -> bool) -> usize {
    let moving = in_flight.iter().filter(|u| server.contains(u) || arrived_only.is_some_and(|next| **u < next)).count();
    let purged = purging.map_or(0, |b| server.iter().filter(|u| **u < b && !in_flight.contains(u) && !kept(**u)).count());
    moving + purged
}

/// Tries before giving up (until restart) on a lone message the server keeps leaving out: each
/// try forces a full UID list of the folder.
const ABSENT_TRIES: u8 = 3;

impl Unreadable {
    /// async-imap ends the stream the same way on a tagged NO (Gmail's "Some messages could not be
    /// FETCHed"), so a missing header carries no error.
    fn absent(&mut self, uid: u32, alone: bool) {
        if !alone {
            self.suspect.insert(uid);
            return;
        }
        let tries = self.absent.entry(uid).or_default();
        *tries += 1;
        if *tries >= ABSENT_TRIES {
            tracing::warn!("message UID {uid} skipped: the server does not send it");
            self.absent.remove(&uid);
            self.suspect.remove(&uid);
            self.bad.insert(uid);
        } else {
            self.suspect.insert(uid);
        }
    }
}

fn unreadable() -> std::sync::MutexGuard<'static, HashMap<i64, Unreadable>> {
    use std::sync::{Mutex, OnceLock};
    static MAP: OnceLock<Mutex<HashMap<i64, Unreadable>>> = OnceLock::new();
    MAP.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn message_level(e: &crate::Error) -> bool {
    use async_imap::error::Error as Imap;
    match e {
        crate::Error::Imap(Imap::Parse(_) | Imap::Bad(_)) => true,
        // async-imap 0.12 reports an unparsable response as this I/O error (and closes the stream: the
        // session is lost, the next sync uses a new one).
        crate::Error::Imap(Imap::Io(io)) => io.kind() == std::io::ErrorKind::Other && io.to_string().contains("during parsing"),
        _ => false,
    }
}

/// Not a full disk, a locked or broken database: those are not the message's fault.
fn message_db_error(e: &crate::Error) -> bool {
    use rusqlite::ErrorCode::*;
    match e {
        crate::Error::Db(rusqlite::Error::SqliteFailure(f, _)) => !matches!(
            f.code,
            DiskFull | SystemIoFailure | DatabaseBusy | DatabaseLocked | ReadOnly | DatabaseCorrupt | CannotOpen | OutOfMemory | NotADatabase
        ),
        crate::Error::Db(_) => true,
        _ => false,
    }
}

const HEADER_QUERY: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER])";
const HEADER_QUERY_GMAIL: &str = "(UID FLAGS INTERNALDATE RFC822.SIZE X-GM-LABELS BODY.PEEK[HEADER])";

async fn fetch_header_batch(session: &mut Session, uids: &[u32], query: &str) -> (Vec<Fetch>, Option<crate::Error>) {
    let mut out = Vec::new();
    let mut stream = match session.uid_fetch(uid_set(uids), query).await {
        Ok(s) => s,
        Err(e) => return (out, Some(e.into())),
    };
    loop {
        match stream.try_next().await {
            Ok(Some(f)) => out.push(f),
            Ok(None) => return (out, None),
            Err(e) => return (out, Some(e.into())),
        }
    }
}

/// One transaction per batch (one disk write, not one per message). If the cache refuses the
/// batch, its messages go one by one and the refused ones are left out.
fn store_headers(store: &Store, account_id: i64, mailbox_id: i64, fetches: &[Fetch], fetched: &mut Fetched, hold: bool) -> Result<()> {
    // Moved or deleted while this sync ran (its list predates the action): not fetched back. Read
    // at each batch, as the action may come after the sync's own check.
    let moving = store.in_flight_uids(account_id, &store.mailbox_state(mailbox_id)?.name)?;
    let batch: Vec<_> = fetches
        .iter()
        .filter_map(|f| {
            let (uid, header) = (f.uid?, f.header()?);
            if moving.contains(&uid) {
                return None;
            }
            let received = f.internal_date().map(|d| d.timestamp());
            let mut info = mime::parse_headers(header, received.unwrap_or(0));
            info.received_at = received;
            Some((uid, info, server_flags(f.flags()), f.size.unwrap_or(0)))
        })
        .collect();
    let mut record = |uid: u32, (id, new): (i64, bool)| {
        fetched.ids.insert(uid, id);
        if new {
            fetched.fresh.push(id);
        }
    };
    match store.insert_headers_held(account_id, mailbox_id, &batch, hold) {
        Ok(res) => {
            for ((uid, ..), r) in batch.iter().zip(res) {
                record(*uid, r);
            }
            Ok(())
        }
        Err(e) if message_db_error(&e) => {
            tracing::warn!("header batch refused, retrying message by message: {e}");
            for item in &batch {
                match store.insert_headers_held(account_id, mailbox_id, std::slice::from_ref(item), hold) {
                    Ok(res) => record(item.0, res[0]),
                    Err(e) if message_db_error(&e) => {
                        tracing::warn!("message UID {} skipped: {e}", item.0);
                        unreadable().entry(mailbox_id).or_default().bad.insert(item.0);
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// The folder must be selected. A batch the server cannot send readably is retried one message
/// at a time (now, or at the next sync if the connection was lost), skipping the culprit.
/// `hold`: new messages wait for the screener (see `Store::insert_headers_held`).
pub async fn fetch_headers(
    store: &Store,
    session: &mut Session,
    account_id: i64,
    mailbox_id: i64,
    uids: &mut [u32],
    on_chunk: &mut (dyn FnMut(usize) + Send),
    hold: bool,
) -> Result<Fetched> {
    let mut fetched = Fetched { ids: HashMap::new(), fresh: Vec::new() };
    let state = store.mailbox_state(mailbox_id)?;
    let names = gmail_labels(store, &state)?;
    let query = if names.is_some() { HEADER_QUERY_GMAIL } else { HEADER_QUERY };
    let mut batches: std::collections::VecDeque<Vec<u32>> = {
        let map = unreadable();
        let (suspect, bad) = map.get(&mailbox_id).map_or((None, None), |u| (Some(&u.suspect), Some(&u.bad)));
        let wanted = uids.iter().copied().filter(|u| !bad.is_some_and(|b| b.contains(u)));
        let (single, grouped): (Vec<u32>, Vec<u32>) = wanted.partition(|u| suspect.is_some_and(|s| s.contains(u)));
        single.into_iter().map(|u| vec![u]).chain(grouped.chunks(FETCH_CHUNK).map(<[u32]>::to_vec)).collect()
    };
    let mut done = 0;
    // Requested, not sent, no error: (UID, fetched alone).
    let mut short: Vec<(u32, bool)> = Vec::new();
    while let Some(batch) = batches.pop_front() {
        let (fetches, err) = fetch_header_batch(session, &batch, query).await;
        // Header parsing and the store's writer (another account's sync may hold it) off the async
        // worker.
        crate::rt::offload(|| store_headers(store, account_id, mailbox_id, &fetches, &mut fetched, hold))?;
        if let Some(names) = &names {
            let given: Vec<(u32, Vec<String>)> = fetches
                .iter()
                .filter_map(|f| Some((f.uid?, f.gmail_labels()?.iter().map(|l| l.to_string()).collect())))
                .collect();
            crate::rt::offload(|| keep_server_labels(store, &state, names, &given))?;
        }
        done += batch.len();
        on_chunk(done);
        alive();
        let Some(e) = err else {
            let sent: HashSet<u32> = fetches.iter().filter(|f| f.header().is_some()).filter_map(|f| f.uid).collect();
            short.extend(batch.iter().filter(|u| !sent.contains(u)).map(|u| (*u, batch.len() == 1)));
            if let [uid] = batch[..]
                && sent.contains(&uid)
                && let Some(u) = unreadable().get_mut(&mailbox_id)
            {
                u.suspect.remove(&uid);
                u.absent.remove(&uid);
            }
            continue;
        };
        if !message_level(&e) {
            return Err(e);
        }
        let got: HashSet<u32> = fetches.iter().filter_map(|f| f.uid).collect();
        let rest: Vec<u32> = batch.iter().copied().filter(|u| !got.contains(u)).collect();
        {
            let mut map = unreadable();
            let u = map.entry(mailbox_id).or_default();
            if let [uid] = batch[..] {
                tracing::warn!("message UID {uid} skipped: unreadable server response ({e})");
                u.suspect.remove(&uid);
                u.bad.insert(uid);
            } else {
                u.suspect.extend(&rest);
            }
        }
        // A BAD reply leaves the connection usable: the rest of the batch goes one by one now.
        if matches!(e, crate::Error::Imap(async_imap::error::Error::Bad(_))) {
            if batch.len() > 1 {
                done -= rest.len();
                for uid in rest.into_iter().rev() {
                    batches.push_front(vec![uid]);
                }
            }
            continue;
        }
        return Err(e);
    }
    if !short.is_empty() {
        // A closed connection also ends the stream without error: the NOOP reports it before the caller
        // saves a UIDNEXT past these messages.
        imap::still_open(session).await?;
        tracing::warn!("{} requested headers not sent by the server: tried again at the next sync", short.len());
        {
            let mut map = unreadable();
            let u = map.entry(mailbox_id).or_default();
            for (uid, alone) in short {
                u.absent(uid, alone);
            }
        }
        // Below the UIDNEXT about to be saved: only a full UID list brings them back.
        force_full_check(mailbox_id);
    }
    Ok(fetched)
}

/// Fetches the `count` messages preceding the synced window. Returns how many were added (0: the
/// whole history is there).
pub async fn load_older(store: &Store, session: &mut Session, state: MailboxState, count: usize) -> Result<usize> {
    if state.is_label {
        return load_older_labelled(store, session, &state, count).await;
    }
    let mb = session.examine(&state.name).await?;
    if mb.exists == 0 {
        // A cut EXAMINE reads as an empty folder too: never "the whole history is here".
        imap::still_open(session).await?;
        return Ok(0);
    }
    // Recreated since the last sync: its UIDs name other messages; the sync must start over.
    if recreated(state.uidvalidity, mb.uid_validity)? {
        return Err(crate::Error::Stale(crate::tr!("le dossier a été recréé sur le serveur", "the folder was recreated on the server")));
    }
    let from = state.sync_from_uid.unwrap_or(1);
    if from <= 1 {
        return Ok(0);
    }
    // Only what is cached below the window, not the folder's whole UID map at every batch.
    let local = store.uids_below(state.id, from)?;
    let in_flight = store.in_flight_uids(state.account_id, &state.name)?;
    let purging = store.purge_pending_below(state.account_id, &state.name)?;
    // Checked: a search cut short is an error, not "nothing older".
    let mut older: Vec<u32> = imap::uid_search_checked(session, &format!("UID 1:{}", from - 1), 0)
        .await?
        .into_iter()
        .filter(|u| *u < from && !local.contains(u) && !in_flight.contains(u) && !purging.is_some_and(|b| *u < b))
        .collect();
    older.sort_unstable_by(|a, b| b.cmp(a));
    older.truncate(count);
    let Some(&oldest) = older.last() else { return Ok(0) };
    let fetched = fetch_headers(store, session, state.account_id, state.id, &mut older, &mut |_| {}, false).await?;
    // Already in the server's count as last read: the displayed total must not grow with them.
    store.raise_count_base(state.id, fetched.fresh.len())?;
    // Only the window: the rest of the state belongs to the sync, which may have moved it on.
    store.extend_window(state.id, oldest)?;
    Ok(older.len())
}

/// [`load_older`] for a Gmail label: the next `count` uncached messages carrying it, newest
/// first, fetched into All Mail.
async fn load_older_labelled(store: &Store, session: &mut Session, label: &MailboxState, count: usize) -> Result<usize> {
    let Some(all) = store.mailbox_by_role(label.account_id, Role::All)? else { return Ok(0) };
    session.examine(&all.name).await?;
    let carrying: Vec<u32> = imap::uid_search_checked(session, &crate::provider::gmail_label_search(&label.name, label.role), 0)
        .await?
        .into_iter()
        .collect();
    let local = store.mailbox_uids_among(all.id, &carrying)?;
    let in_flight = store.in_flight_uids(all.account_id, &all.name)?;
    // Those the server will not give are left out: asked again, they would never be cached and
    // the history fetch would go round for ever.
    let bad: HashSet<u32> = unreadable().get(&all.id).map(|u| u.bad.clone()).unwrap_or_default();
    let mut missing: Vec<u32> = carrying.into_iter().filter(|u| !local.contains_key(u) && !in_flight.contains(u) && !bad.contains(u)).collect();
    missing.sort_unstable_by(|a, b| b.cmp(a));
    missing.truncate(count);
    if missing.is_empty() {
        return Ok(0);
    }
    let fetched = fetch_headers(store, session, all.account_id, all.id, &mut missing, &mut |_| {}, false).await?;
    // What was actually cached: zero ends the fetch even when some could not be read.
    Ok(fetched.ids.len())
}

/// Searches the server, beyond the cached window too. Uncached matches are added (newest first,
/// at most `per_mailbox` per folder). Returns the local ids of the matches.
pub async fn search_server(
    store: &Store,
    session: &mut Session,
    mailboxes: &[MailboxState],
    criteria: &str,
    per_mailbox: usize,
) -> Result<Vec<i64>> {
    // `criteria` comes from `search::imap_criteria` (values sanitized). `ALL` (a folder-only query)
    // lists the folders' newest messages; the caller leaves out empty queries.
    let mut found = Vec::new();
    // A folder removed or recreated on the server since the last listing is skipped; an error only
    // when no folder could be searched.
    let mut skipped = None;
    let mut searched = 0;
    for state in mailboxes {
        let mb = match session.examine(&state.name).await {
            Ok(mb) => mb,
            Err(async_imap::error::Error::No(e)) => {
                tracing::warn!("server search: folder {} skipped: {e}", state.name);
                skipped = Some(crate::Error::from(async_imap::error::Error::No(e)));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        // Recreated since the last sync: its UIDs name other messages, never to be cached under the old
        // ones (the sync starts the folder over).
        let known = store.mailbox_state(state.id).map_or(state.uidvalidity, |s| s.uidvalidity);
        if recreated(known, mb.uid_validity)? {
            tracing::warn!("server search: folder {} skipped, recreated on the server", state.name);
            skipped = Some(crate::Error::Stale(crate::tr!("le dossier a été recréé sur le serveur", "the folder was recreated on the server")));
            continue;
        }
        searched += 1;
        if mb.exists == 0 {
            // A cut EXAMINE reads as an empty folder too.
            imap::still_open(session).await?;
            continue;
        }
        // Checked: a cut connection is an error, not "no results".
        let uids = match imap::uid_search_checked(session, &format!("CHARSET UTF-8 {criteria}"), 0).await {
            Ok(u) => u,
            // Server without CHARSET UTF-8: retry without it, only for ASCII criteria (accented text
            // without a charset would be read as something else).
            Err(crate::Error::Imap(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_))) if criteria.is_ascii() => {
                imap::uid_search_checked(session, criteria, 0).await?
            }
            Err(e) => return Err(e),
        };
        // Only the newest matches: a common word matches thousands of messages, more than a result
        // list shows.
        let mut uids: Vec<u32> = uids.into_iter().collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(per_mailbox);
        if uids.is_empty() {
            continue;
        }
        // Only the matches' rows, not the folder's whole UID map (a large archive's is big).
        let (local, in_flight) = crate::rt::offload(|| -> Result<_> {
            Ok((store.mailbox_uids_among(state.id, &uids)?, store.in_flight_uids(state.account_id, &state.name)?))
        })?;
        let mut missing: Vec<u32> = uids.iter().filter(|u| !local.contains_key(u) && !in_flight.contains(u)).copied().collect();
        let Fetched { ids: fetched, fresh } = fetch_headers(store, session, state.account_id, state.id, &mut missing, &mut |_| {}, false).await?;
        // New rows below the last-read UIDNEXT were already in the server's count; later arrivals were
        // not, and do raise the displayed total.
        let next = store.mailbox_state(state.id).ok().and_then(|s| s.uidnext);
        let fresh: HashSet<i64> = fresh.into_iter().collect();
        let counted = fetched.iter().filter(|(u, id)| fresh.contains(id) && next.is_some_and(|n| **u < n)).count();
        store.raise_count_base(state.id, counted)?;
        found.extend(uids.iter().filter_map(|u| local.get(u).or_else(|| fetched.get(u)).copied()));
    }
    match skipped {
        Some(e) if searched == 0 => Err(e),
        _ => Ok(found),
    }
}

/// Bounded to the last month, size-capped batches, one response at a time. The folder must be
/// selected.
async fn prefetch_bodies(store: &Store, session: &mut Session, state: &MailboxState) -> Result<usize> {
    // Copies of a body already stored are shared when their header is inserted.
    let mut stored = 0;
    if !prefetched(store, state)? {
        return Ok(stored);
    }
    let since = crate::store::now() - PREFETCH_DAYS * 86_400;
    let mut budget = PREFETCH_BUDGET;
    let unsent = unsent_bodies().lock().unwrap_or_else(|e| e.into_inner()).get(&state.id).cloned().unwrap_or_default();
    let missing: Vec<(i64, u32, u32)> = store
        .missing_bodies(state.id, PREFETCH_BODIES, PREFETCH_MAX_SIZE, since)?
        .into_iter()
        .filter(|(_, uid, _)| !unsent.contains(uid))
        .take_while(|(_, _, size)| {
            let fits = *size <= budget;
            budget = budget.saturating_sub(*size);
            fits
        })
        .collect();
    if missing.is_empty() {
        return Ok(stored);
    }
    let by_uid: HashMap<u32, i64> = missing.iter().map(|(id, uid, _)| (*uid, *id)).collect();
    let uids: Vec<u32> = by_uid.keys().copied().collect();
    let mut stream = session.uid_fetch(uid_set(&uids), "(UID BODY.PEEK[])").await?;
    let mut sent: HashSet<u32> = HashSet::new();
    while let Some(f) = stream.try_next().await? {
        if let (Some(uid), Some(raw)) = (f.uid, f.body())
            && let Some(id) = by_uid.get(&uid)
        {
            sent.insert(uid);
            // The FETCH can take long: a row gone meanwhile may have handed its id to another
            // message, which must not get this body.
            if !store.location(*id).is_ok_and(|l| l.mailbox_id == state.id && l.uid == Some(uid)) {
                continue;
            }
            crate::rt::offload(|| store.store_body(*id, raw, &mime::body_info(raw)))?;
            alive();
            stored += 1;
        }
    }
    drop(stream);
    // The stream ends quietly on a tagged NO: what was asked and not sent is remembered.
    let left: Vec<u32> = uids.iter().copied().filter(|u| !sent.contains(u)).collect();
    if !left.is_empty() {
        imap::still_open(session).await?;
        unsent_bodies().lock().unwrap_or_else(|e| e.into_inner()).entry(state.id).or_default().extend(left);
    }
    Ok(stored)
}

/// The UID must still name this message (same place in the cache, same UIDVALIDITY on the
/// server), otherwise another message's body would be stored under this header.
fn check_still_there(store: &Store, id: i64, mailbox: &str, uid: u32, validity: Option<u32>) -> Result<()> {
    let stale = || crate::Error::Stale(crate::tr!("le message a changé de place, réessayez après la synchro", "the message has moved, try again after the sync"));
    let loc = store.location(id)?;
    if loc.mailbox != mailbox || loc.uid != Some(uid) {
        return Err(stale());
    }
    let known = store.mailbox_state(loc.mailbox_id)?.uidvalidity;
    // A missing UIDVALIDITY is a cut EXAMINE (an error that drops the session), not a new folder.
    if recreated(known, validity)? {
        return Err(crate::Error::Stale(crate::tr!("le dossier a été recréé sur le serveur", "the folder was recreated on the server")));
    }
    Ok(())
}

/// `validity`: what [`download_body`] returned with the body.
pub fn keep_body(store: &Store, id: i64, mailbox: &str, uid: u32, validity: Option<u32>, raw: &[u8]) -> Result<()> {
    // Again after the download: a sync may have reset the folder meanwhile.
    check_still_there(store, id, mailbox, uid, validity)?;
    store.store_body(id, raw, &mime::body_info(raw))
}

/// Returns the folder's UIDVALIDITY as read then, for [`keep_body`].
pub async fn download_body(store: &Store, session: &mut Session, id: i64, mailbox: &str, uid: u32) -> Result<(Vec<u8>, Option<u32>)> {
    let validity = session.examine(mailbox).await?.uid_validity;
    check_still_there(store, id, mailbox, uid, validity)?;
    let fetches: Vec<Fetch> = session.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])").await?.try_collect().await?;
    let Some(raw) = fetches.iter().find(|f| f.uid == Some(uid)).and_then(|f| f.body()).map(<[u8]>::to_vec) else {
        // A FETCH cut short ends quietly too: a dead connection, not a message gone.
        imap::still_open(session).await?;
        return Err(crate::Error::NotFound(crate::tr!("message {uid} absent du serveur", "message {uid} missing from the server")));
    };
    Ok((raw, validity))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn departures_are_found_among_the_cached_messages_only() {
        let cached = [10, 11, 12, 20];
        // One message deleted (Gmail: moved out of All Mail): the others are still there.
        let still: HashSet<u32> = [10, 12, 20].into();
        assert_eq!(departures_only(&cached, still, 1, 30), Some([10, 12, 20].into()));
        // An empty answer is a refused search, never an emptied folder.
        assert_eq!(departures_only(&cached, HashSet::new(), 4, 30), None);
        // More gone than the count says: the whole list is read instead.
        assert_eq!(departures_only(&cached, [10].into(), 1, 30), None);
    }

    #[test]
    fn partial_full_reads_count_in_a_row() {
        let folder = -77;
        assert_eq!((partial_full_reads(folder, false), partial_full_reads(folder, false)), (1, 2));
        assert_eq!(partial_full_reads(folder, true), 0, "a whole read starts over");
        assert_eq!(partial_full_reads(folder, false), 1);
    }

    #[tokio::test]
    async fn stalled_folder_syncs_are_given_up() {
        // (Under test, the stall limit is 300 ms instead of 5 minutes.)
        let stalled = watched("INBOX", async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(())
        });
        assert!(matches!(stalled.await, Err(crate::Error::Other(_))));
        // Slow but advancing: never cut short.
        let advancing = watched("INBOX", async {
            for _ in 0..8 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                alive();
            }
            Ok(7)
        });
        assert_eq!(advancing.await.unwrap(), 7);
    }

    #[test]
    fn renamed_subtrees_are_recognised() {
        assert!(is_under("Clients", "Clients", Some("/")));
        assert!(is_under("Clients/2024", "Clients", Some("/")));
        assert!(!is_under("Clients2", "Clients", Some("/")), "a sibling sharing the prefix");
        assert!(!is_under("Clients/2024", "Clients", None));
        assert!(is_under("INBOX.Clients.2024", "INBOX.Clients", Some(".")));
    }

    fn store_with_folder() -> (Store, i64) {
        use crate::model::{NewAccount, NewAuth, Security, ServerConfig};
        let s = Store::open_in_memory().unwrap();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = s
            .insert_account(&NewAccount {
                auth: NewAuth::default(),
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv,
            })
            .unwrap();
        let id = s.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        (s, id)
    }

    #[test]
    fn periodic_checks_are_done_only_once_they_have_run() {
        let (s, _) = store_with_folder();
        // Folder numbers of their own: the map is shared with the other tests.
        let id = -4242;
        let t = due(&s, FULL_LIST, id, 3600).expect("never run: due");
        assert!(due(&s, FULL_LIST, id, 3600).is_some(), "still due until it has run");
        done(&s, t);
        assert!(due(&s, FULL_LIST, id, 3600).is_none());
        // Asked for while a check was running: the running check does not answer it.
        let id = -4243;
        let early = due(&s, FULL_LIST, id, 3600).unwrap();
        force_full_check(id);
        done(&s, early);
        let t = due(&s, FULL_LIST, id, 3600).expect("the request still stands");
        done(&s, t);
        assert!(due(&s, FULL_LIST, id, 3600).is_none());
    }

    #[test]
    fn periodic_checks_are_remembered_across_launches() {
        let (s, id) = store_with_folder();
        let t = due(&s, FULL_LIST, id, 3600).expect("never run: due");
        done(&s, t);
        // A new launch: the process-wide map starts empty, the base remembers.
        periodic().remove(&(FULL_LIST, id));
        assert!(due(&s, FULL_LIST, id, 3600).is_none());
        assert!(due(&s, FULL_FLAGS, id, 3600).is_some());
    }

    #[tokio::test]
    async fn the_mailbox_space_is_read_in_bytes() {
        let mut session = crate::imap::tests::scripted(vec![(
            "* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" (STORAGE 512 1024 MESSAGE 3 100)\r\n{tag} OK done",
            false,
        )])
        .await;
        assert_eq!(read_quota(&mut session).await.unwrap(), Some((512 * 1024, 1024 * 1024)));
        let mut session = crate::imap::tests::scripted(vec![("* QUOTAROOT INBOX\r\n{tag} OK done", false)]).await;
        assert_eq!(read_quota(&mut session).await.unwrap(), None);
    }

    #[test]
    fn gmail_labels_are_recognised_however_the_server_writes_them() {
        let names = HashMap::from([("\\Inbox".to_string(), 1), ("\\Important".to_string(), 2), ("Mairie".to_string(), 3), ("L&AOk-o".to_string(), 4)]);
        let given = |l: &[&str]| label_ids(&names, &l.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        // As Gmail sends them: system labels quoted, their backslash escaped.
        assert_eq!(given(&["\\\\Inbox", "\\\\Important", "Mairie", "L&AOk-o"]), vec![1, 2, 3, 4]);
        assert_eq!(given(&["\\Inbox", "Important", "\\INBOX"]), vec![1, 2, 1]);
        assert_eq!(given(&["Inconnu"]), Vec::<i64>::new());
        assert_eq!(unescape_label("a\\\"b"), "a\"b");
    }

    #[test]
    fn a_folder_is_left_unopened_only_when_nothing_changed() {
        let state = MailboxState {
            id: 1,
            account_id: 1,
            name: "INBOX".into(),
            role: Some(Role::Inbox),
            selectable: true,
            uidvalidity: Some(7),
            uidnext: Some(9),
            highestmodseq: Some(41),
            sync_from_uid: Some(1),
            server_count: Some(3),
            is_label: false,
        };
        let same = imap::FolderStatus { messages: Some(3), uidnext: Some(9), uidvalidity: Some(7), highestmodseq: Some(41), unseen: Some(2) };
        let caps = Caps { condstore: true, ..Default::default() };
        let check = |state: &MailboxState, status: &imap::FolderStatus| unchanged_on_server(state, status, &caps, None);
        assert!(check(&state, &same));
        assert!(!check(&state, &imap::FolderStatus { uidnext: Some(10), ..same }), "an arrival");
        assert!(!check(&state, &imap::FolderStatus { messages: Some(2), ..same }), "a deletion");
        assert!(!check(&state, &imap::FolderStatus { highestmodseq: Some(42), ..same }), "a flag change");
        assert!(!check(&state, &imap::FolderStatus { highestmodseq: None, ..same }), "no modseq given");
        assert!(!check(&state, &imap::FolderStatus { uidvalidity: Some(8), ..same }), "recreated");
        assert!(!check(&MailboxState { sync_from_uid: None, ..state.clone() }, &same), "never synced");
        // Without CONDSTORE: the unread count stands for the flags, and the inbox is always opened.
        let plain = Caps::default();
        let archive = MailboxState { name: "Archive".into(), role: Some(Role::Archive), ..state.clone() };
        assert!(unchanged_on_server(&archive, &same, &plain, Some(2)));
        assert!(!unchanged_on_server(&archive, &same, &plain, Some(1)), "read or unread elsewhere");
        assert!(!unchanged_on_server(&archive, &same, &plain, None), "never counted");
        assert!(!unchanged_on_server(&archive, &imap::FolderStatus { unseen: None, ..same }, &plain, Some(2)), "no count given");
        assert!(!unchanged_on_server(&state, &same, &plain, Some(2)), "the inbox");
    }

    #[test]
    fn restored_uids_leave_only_once_dealt_with() {
        // A folder number of its own: the map is shared with the other tests.
        let id = -4244;
        restore_uids(id, [120, 121]);
        let read: HashSet<u32> = restoring().get(&id).cloned().unwrap();
        // Given back meanwhile, by another abandoned operation: kept for the next full read.
        restore_uids(id, [300]);
        restored(id, &read);
        assert_eq!(restoring().get(&id).cloned(), Some(HashSet::from([300])));
        restored(id, &HashSet::from([300]));
        assert!(!restoring().contains_key(&id));
    }

    #[test]
    fn errors_are_classified() {
        use async_imap::error::Error as Imap;
        let parse = crate::Error::Imap(Imap::Io(std::io::Error::other("Error(...) during parsing of \"* 1 FETCH\"")));
        assert!(message_level(&parse));
        assert!(message_level(&crate::Error::Imap(Imap::Bad("bad".into()))));
        assert!(!message_level(&crate::Error::Imap(Imap::ConnectionLost)));
        assert!(!message_level(&crate::Error::Imap(Imap::Io(std::io::Error::other("broken pipe")))));
        let full = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL), None);
        assert!(!message_db_error(&crate::Error::Db(full)), "a full disk is not the message's fault");
        let big = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG), None);
        assert!(message_db_error(&crate::Error::Db(big)));
    }

    #[test]
    fn headers_the_server_leaves_out_are_retried_then_given_up() {
        let mut u = Unreadable::default();
        u.absent(5, false);
        assert!(u.suspect.contains(&5), "fetched alone next time");
        for _ in 1..ABSENT_TRIES {
            u.absent(5, true);
            assert!(!u.bad.contains(&5));
        }
        u.absent(5, true);
        assert!(u.bad.contains(&5) && !u.suspect.contains(&5) && u.absent.is_empty(), "given up after {ABSENT_TRIES} lone tries");
    }

    #[test]
    fn a_missing_uidvalidity_is_an_error_not_a_recreated_folder() {
        assert!(!recreated(None, None).unwrap(), "never synced: nothing to reset");
        assert!(!recreated(None, Some(7)).unwrap());
        assert!(!recreated(Some(7), Some(7)).unwrap());
        assert!(recreated(Some(7), Some(8)).unwrap());
        // A half-dead connection's answer: the cache must not be wiped.
        assert!(recreated(Some(7), None).is_err());
        assert!(recreated(Some(7), Some(0)).is_err(), "0 is not a valid UIDVALIDITY");
    }

    #[test]
    fn a_queued_purge_counts_as_leaving() {
        let set = |u: &[u32]| u.iter().copied().collect::<HashSet<u32>>();
        let server = set(&[1, 2, 3, 4, 10]);
        // Moved out (2), kept (3: a refused move the purge leaves), purged (1, 4), arrived (10).
        assert_eq!(leaving_count(&server, &set(&[2]), None, Some(10), |u| u == 3), 3);
        assert_eq!(leaving_count(&server, &set(&[2]), None, None, |u| u == 3), 1, "no purge queued");
        // A purge without a bound covers everything, the new arrival too.
        assert_eq!(leaving_count(&server, &HashSet::new(), None, Some(u32::MAX), |_| false), 5);
        // Arrivals only: an in-flight UID below them is still on the server.
        assert_eq!(leaving_count(&set(&[10]), &set(&[2]), Some(10), None, |_| false), 1);
    }

    #[test]
    fn uidnext_never_goes_below_what_was_seen() {
        assert_eq!(next_uid(Some(10), Some(4)), Some(10));
        assert_eq!(next_uid(Some(10), Some(12)), Some(13), "arrived after the SELECT");
        assert_eq!(next_uid(None, Some(12)), Some(13));
        assert_eq!(next_uid(Some(10), None), Some(10));
        assert_eq!(next_uid(None, None), None);
    }
}
