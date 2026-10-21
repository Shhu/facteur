//! Operations applied locally first (offline too), then replayed on the server.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use std::collections::HashSet;

use futures::TryStreamExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, bounded};
use crate::imap::Session;
use crate::store::Store;
use crate::sync::Caps;

/// UID-based operations record the folder's UIDVALIDITY when created: if the folder was
/// recreated since, those UIDs name other messages and the operation is dropped rather than
/// touching the wrong mail.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    Store { mailbox: String, uid: u32, flag: String, value: bool, #[serde(default)] uidvalidity: Option<u32> },
    StoreSet { mailbox: String, uids: Vec<u32>, flag: String, value: bool, #[serde(default)] uidvalidity: Option<u32> },
    Move { mailbox: String, uid: u32, dest: String, #[serde(default)] uidvalidity: Option<u32> },
    Delete { mailbox: String, uid: u32, #[serde(default)] uidvalidity: Option<u32> },
    Append { mailbox: String, flags: String, raw: String },
    Create { mailbox: String },
    /// Appends the draft, then removes its previous versions (same Message-ID).
    SaveDraft { mailbox: String, message_id: String, raw: String },
    DeleteDraft { mailbox: String, message_id: String },
    /// Covers the whole folder on the server, beyond the synced window, but only what had arrived
    /// when asked (UID < `below`).
    MarkAllSeen { mailbox: String, #[serde(default)] below: Option<u32>, #[serde(default)] uidvalidity: Option<u32> },
    MoveSet { mailbox: String, uids: Vec<u32>, dest: String, #[serde(default)] uidvalidity: Option<u32> },
    DeleteSet { mailbox: String, uids: Vec<u32>, #[serde(default)] uidvalidity: Option<u32> },
    /// Messages, including never-downloaded ones, are moved to `dest` first: if that fails, the
    /// folder stays.
    DeleteMailbox { mailbox: String, dest: Option<String> },
    /// Empties a folder (trash, junk): only what it held when asked (UID < `below`), cached or not.
    /// `arriving`: Message-IDs of messages that reached the folder before the sync saw their UID
    /// (above `below`); searched for and deleted too.
    Purge {
        mailbox: String,
        #[serde(default)]
        below: Option<u32>,
        #[serde(default)]
        uidvalidity: Option<u32>,
        #[serde(default)]
        arriving: Vec<String>,
    },
    Rename { from: String, to: String },
    /// Gmail: label names as X-GM-LABELS spells them (`provider::gmail_label`), applied to
    /// `mailbox` (All Mail). The messages stay where they are.
    Labels {
        mailbox: String,
        uids: Vec<u32>,
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
        #[serde(default)]
        uidvalidity: Option<u32>,
    },
}

impl Op {
    pub fn append(mailbox: &str, flags: &str, raw: &[u8]) -> Op {
        Op::Append { mailbox: mailbox.to_string(), flags: flags.to_string(), raw: B64.encode(raw) }
    }

    /// May jump ahead of a still-undoable action: ordering with it does not matter, since a move
    /// keeps the flags.
    fn flags_only(&self) -> bool {
        matches!(self, Op::Store { .. } | Op::StoreSet { .. } | Op::MarkAllSeen { .. })
    }

    /// A large APPEND (copy to Sent) over a slow connection needs more time than the rest.
    fn timeout_secs(&self) -> u64 {
        match self {
            Op::Append { raw, .. } | Op::SaveDraft { raw, .. } => 120 + (raw.len() as u64 * 3 / 4) / 50_000,
            Op::DeleteMailbox { .. } | Op::Purge { .. } | Op::MarkAllSeen { .. } => 600,
            // One command per chunk of UIDs, each with its own share of time (a retry without MOVE also
            // looks for copies already made).
            Op::StoreSet { uids, .. } | Op::MoveSet { uids, .. } | Op::DeleteSet { uids, .. } | Op::Labels { uids, .. } => {
                120 + 90 * uids.len().div_ceil(UID_CHUNK) as u64
            }
            _ => 120,
        }
    }

    /// Folders shown differently while this operation waits: when it fails or is dropped, they are
    /// checked in full so the cache matches the server again.
    pub fn folders(&self) -> Vec<&str> {
        match self {
            Op::Move { mailbox, dest, .. } | Op::MoveSet { mailbox, dest, .. } => vec![mailbox, dest],
            Op::DeleteMailbox { mailbox, dest } => std::iter::once(mailbox.as_str()).chain(dest.as_deref()).collect(),
            Op::Rename { from, to } => vec![from, to],
            Op::Store { mailbox, .. }
            | Op::StoreSet { mailbox, .. }
            | Op::Delete { mailbox, .. }
            | Op::DeleteSet { mailbox, .. }
            | Op::Append { mailbox, .. }
            | Op::Create { mailbox }
            | Op::SaveDraft { mailbox, .. }
            | Op::DeleteDraft { mailbox, .. }
            | Op::MarkAllSeen { mailbox, .. }
            | Op::Purge { mailbox, .. }
            | Op::Labels { mailbox, .. } => vec![mailbox],
        }
    }
}

/// Thousands of UIDs in one set make a line some servers refuse (BAD) or a command that runs
/// past its timeout.
const UID_CHUNK: usize = 500;

/// After that the operation fails for good and the user decides (retry or drop).
const MAX_ATTEMPTS: i64 = 5;
/// Network errors during the operation itself get more slack, but an operation that always
/// fails must not block the queue forever.
const MAX_NETWORK_ATTEMPTS: i64 = 10;

/// A lasting server refusal: the operation is counted and the next one is tried.
fn refused(e: &Error) -> Option<String> {
    use async_imap::error::Error as Imap;
    match e {
        // A passing refusal (RFC 5530) is retried like a network error: the queue stops there, which
        // keeps its order, and the next replay tries again within the larger budget.
        Error::Imap(Imap::No(msg) | Imap::Bad(msg)) if crate::imap::transient_refusal(msg) => None,
        Error::Imap(Imap::No(msg) | Imap::Bad(msg)) => Some(crate::imap::server_reason(msg)),
        // async-imap hides the server's NO on APPEND (quota, size, missing folder) behind this error.
        // `append` has already told it apart from a cut connection.
        Error::Imap(Imap::Append) => Some(e.to_string()),
        Error::Imap(Imap::Validate(v)) => Some(v.to_string()),
        _ => None,
    }
}

fn is_within(mailbox: &str, folder: &str, delimiter: Option<&str>) -> bool {
    mailbox == folder || delimiter.is_some_and(|d| mailbox.strip_prefix(folder).is_some_and(|rest| rest.starts_with(d)))
}

/// Stops at the first network error (retried later); a server refusal (NO/BAD) is counted and
/// the next operation is tried.
pub async fn flush(store: &Store, account_id: i64, session: &mut Session, caps: &Caps) -> Result<usize> {
    let mut done = 0;
    let now = crate::store::now();
    // A still-undoable action holds back everything after it (order matters), except plain flag
    // changes.
    let mut held = false;
    // Folders a held-back rename or creation brings into being: a flag change in or below them
    // would reach the server before the folder exists there.
    let mut unborn: Vec<(String, Option<String>)> = Vec::new();
    let mut listing = None;
    for pending in store.pending_ops(account_id)? {
        if pending.not_before.is_some_and(|t| t > now) {
            held = true;
            continue;
        }
        let op: Op = match serde_json::from_str(&store.op_payload(pending.id)?.unwrap_or_default()) {
            Ok(op) => op,
            Err(e) => {
                tracing::error!("unreadable operation dropped: {e}");
                store.finish_op(pending.id)?;
                continue;
            }
        };
        if held && !op.flags_only() {
            if let Op::Rename { to: name, .. } | Op::Create { mailbox: name } = &op {
                if listing.is_none() {
                    listing = Some(store.mailbox_listing(account_id)?);
                }
                let delimiter = listing.as_ref().and_then(|l| l.get(name)).and_then(|m| m.1.clone()).filter(|d| !d.is_empty());
                unborn.push((name.clone(), delimiter));
            }
            continue;
        }
        if held && op.folders().iter().any(|m| unborn.iter().any(|(name, d)| is_within(m, name, d.as_deref()))) {
            continue;
        }
        let what = crate::tr!("envoi des modifications", "sending changes");
        // A move out of this folder that the server refused, or that waits, must not see its message
        // destroyed by a later purge or permanent delete of the folder.
        let protected = match &op {
            Op::Purge { mailbox, .. } => protected_uids(store, account_id, mailbox, pending.id, false, false)?,
            // A permanent delete names messages one by one: one a failed move gave back is the user's to
            // delete. Only a move still waiting holds it back.
            Op::DeleteSet { mailbox, .. } => protected_uids(store, account_id, mailbox, pending.id, true, false)?,
            // Marking read skips messages a move held for undo already took out of the folder for the
            // user. A move queued after the request leaves with the message marked read.
            Op::MarkAllSeen { mailbox, .. } => protected_uids(store, account_id, mailbox, pending.id, true, true)?,
            _ => HashSet::new(),
        };
        // Without MOVE: copy, then expunge. A try cut in between (or a crash) must not copy twice,
        // even after the user's retry has reset the attempts.
        if !caps.mv && matches!(op, Op::Move { .. } | Op::MoveSet { .. } | Op::DeleteMailbox { dest: Some(_), .. }) && !pending.tried {
            store.mark_op_started(pending.id)?;
        }
        match bounded(op.timeout_secs(), &what, apply(session, caps, &op, pending.tried, &protected)).await {
            Ok(()) => {
                store.finish_op(pending.id)?;
                done += 1;
            }
            Err(Error::Stale(why)) => {
                tracing::warn!("operation dropped: {why}");
                store.finish_op(pending.id)?;
                recheck_folders(store, account_id, &op);
            }
            Err(e) => match refused(&e) {
                Some(msg) => {
                    tracing::warn!("operation \"{}\" refused: {msg}", pending.kind);
                    // A server keeps refusing to delete the folders it protects (Outlook's Notes,
                    // Outbox…): given up at once, the folder comes back with the next folder list.
                    let (msg, max) = match &op {
                        Op::DeleteMailbox { .. } => (
                            crate::i18n::both(|| crate::tr!("le serveur refuse de supprimer ce dossier (dossier protégé) : {msg}", "the server refuses to delete this folder (a protected one): {msg}")),
                            1,
                        ),
                        _ => (msg, MAX_ATTEMPTS),
                    };
                    store.fail_op(pending.id, &msg, max)?;
                    if pending.attempts + 1 >= max {
                        recheck_folders(store, account_id, &op);
                    }
                }
                None => {
                    // Kept for the queue details: in both languages, as far as the error's own words go.
                    store.fail_op(pending.id, &crate::i18n::both(|| e.to_string()), MAX_NETWORK_ATTEMPTS)?;
                    if pending.attempts + 1 >= MAX_NETWORK_ATTEMPTS {
                        recheck_folders(store, account_id, &op);
                    }
                    return Err(e);
                }
            },
        }
    }
    Ok(done)
}

/// The next sync reads the whole UID list and the flags of the folders the operation touched, so
/// what it hid or changed shows again as it is on the server.
pub fn recheck_folders(store: &Store, account_id: i64, op: &Op) {
    let Ok(states) = store.mailbox_states(account_id) else { return };
    for name in op.folders() {
        if let Some(state) = states.iter().find(|s| s.name == name) {
            crate::sync::force_full_check(state.id);
            // A flag change given up is undone only by reading the flags again (CHANGEDSINCE
            // reports nothing: the server never changed).
            crate::sync::force_full_flags(state.id);
        }
    }
    // Messages a move or delete took out come back even from below the synced window (found by a
    // server search): the full read alone skips those.
    let (mailbox, uids, uidvalidity) = match op {
        Op::Move { mailbox, uid, uidvalidity, .. } | Op::Delete { mailbox, uid, uidvalidity } => (mailbox, std::slice::from_ref(uid), uidvalidity),
        Op::MoveSet { mailbox, uids, uidvalidity, .. } | Op::DeleteSet { mailbox, uids, uidvalidity } => (mailbox, &uids[..], uidvalidity),
        _ => return,
    };
    if let Some(state) = states.iter().find(|s| s.name == *mailbox)
        && (uidvalidity.is_none() || *uidvalidity == state.uidvalidity)
    {
        crate::sync::restore_uids(state.id, uids.iter().copied());
    }
}

/// UIDs of `mailbox` that another queued move (waiting, refused, or failed; not failed with
/// `waiting_only`; queued before `except` with `earlier_only`) still has to take out of it.
fn protected_uids(store: &Store, account_id: i64, mailbox: &str, except: i64, waiting_only: bool, earlier_only: bool) -> Result<HashSet<u32>> {
    let mut out = HashSet::new();
    for payload in store.move_payloads(account_id, mailbox, except, waiting_only, earlier_only)? {
        match serde_json::from_str::<Op>(&payload).ok() {
            Some(Op::Move { uid, .. }) => {
                out.insert(uid);
            }
            Some(Op::MoveSet { uids, .. }) => out.extend(uids),
            _ => {}
        }
    }
    Ok(out)
}

fn chunked(uids: &[u32]) -> Vec<Vec<u32>> {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.chunks(UID_CHUNK).map(<[u32]>::to_vec).collect()
}

fn range_without(lo: u32, hi: u32, skip: &HashSet<u32>) -> Option<String> {
    let mut holes: Vec<u32> = skip.iter().copied().filter(|u| (lo..=hi).contains(u)).collect();
    holes.sort_unstable();
    let mut parts = Vec::new();
    let mut start = lo;
    for h in holes {
        if h > start {
            parts.push(if h - 1 == start { start.to_string() } else { format!("{start}:{}", h - 1) });
        }
        start = h + 1;
    }
    if start <= hi && hi >= lo {
        parts.push(if start == hi { start.to_string() } else { format!("{start}:{hi}") });
    }
    (!parts.is_empty()).then(|| parts.join(","))
}

/// async-imap reports a refusal, a cut connection and an untagged line before the continuation
/// all as `Error::Append`. A NOOP that answers means the server said NO; otherwise it is
/// treated as a network error.
async fn append(session: &mut Session, mailbox: &str, flags: Option<&str>, raw: Vec<u8>) -> Result<()> {
    match session.append(mailbox, flags, None, raw).await {
        Ok(()) => Ok(()),
        Err(async_imap::error::Error::Append) => match crate::imap::still_open(session).await {
            Ok(()) => Err(Error::Imap(async_imap::error::Error::Append)),
            Err(_) => Err(Error::Imap(async_imap::error::Error::ConnectionLost)),
        },
        Err(e) => Err(e.into()),
    }
}

/// After a cut between COPY and EXPUNGE, a `retry` copies only what the destination does not
/// hold yet (same Message-ID), so the message does not end up there twice.
async fn copy_then_expunge(session: &mut Session, caps: &Caps, mailbox: &str, uidvalidity: Option<u32>, uids: &[u32], dest: &str, retry: bool) -> Result<()> {
    for chunk in chunked(uids) {
        let to_copy = if retry { not_yet_copied(session, mailbox, uidvalidity, &chunk, dest).await? } else { chunk.clone() };
        if !to_copy.is_empty() {
            session.uid_copy(crate::imap::uid_set(&to_copy), dest).await?;
        }
        expunge_uids(session, caps, &chunk).await?;
    }
    Ok(())
}

/// Leaves `mailbox` selected again.
async fn not_yet_copied(session: &mut Session, mailbox: &str, uidvalidity: Option<u32>, chunk: &[u32], dest: &str) -> Result<Vec<u32>> {
    // Already expunged from the source (not fetched): nothing left to copy for them.
    let sources = identities(session, &crate::imap::uid_set(chunk)).await?;
    let mut out = Vec::new();
    session.examine(dest).await?;
    // Distinct messages can share a Message-ID: a copy also has the same size and date. Each copy
    // in `dest` answers for one source message only, so of two identical messages, one copied, the
    // other still gets copied.
    let mut taken = HashSet::new();
    for (uid, me) in sources {
        let copy = match &me.message_id {
            Some(id) => same_message_id(session, id).await?.into_iter().find(|(u, other)| !taken.contains(u) && *other == me),
            None => None,
        };
        match copy {
            Some((u, _)) => {
                taken.insert(u);
            }
            None => out.push(uid),
        }
    }
    select(session, mailbox, uidvalidity).await?;
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    /// Without the angle brackets.
    message_id: Option<String>,
    size: Option<u32>,
    /// Raw Date header.
    date: Option<Vec<u8>>,
}

fn bare_message_id(raw: &[u8]) -> Option<String> {
    let id = String::from_utf8_lossy(raw);
    let id = id.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!id.is_empty()).then(|| id.to_string())
}

async fn identities(session: &mut Session, set: &str) -> Result<Vec<(u32, Identity)>> {
    let mut out = Vec::new();
    let mut stream = session.uid_fetch(set, "(UID ENVELOPE RFC822.SIZE)").await?;
    while let Some(f) = stream.try_next().await? {
        let Some(uid) = f.uid else { continue };
        let envelope = f.envelope();
        out.push((
            uid,
            Identity {
                message_id: envelope.and_then(|e| e.message_id.as_deref()).and_then(bare_message_id),
                size: f.size,
                date: envelope.and_then(|e| e.date.as_deref()).map(<[u8]>::to_vec),
            },
        ));
    }
    Ok(out)
}

/// `HEADER Message-ID` matches a substring: results are checked for an exact match.
async fn same_message_id(session: &mut Session, message_id: &str) -> Result<Vec<(u32, Identity)>> {
    // No control characters: a CR/LF would end the IMAP command and inject another one.
    let id: String = message_id.chars().filter(|c| !matches!(c, '"' | '\\') && !c.is_control()).collect();
    let found: Vec<u32> = session.uid_search(format!("HEADER Message-ID \"{id}\"")).await?.into_iter().collect();
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let want = bare_message_id(message_id.as_bytes());
    Ok(identities(session, &crate::imap::uid_set(&found)).await?.into_iter().filter(|(_, i)| i.message_id.is_some() && i.message_id == want).collect())
}

/// Fails as stale if the folder was recreated since the operation was queued.
async fn select(session: &mut Session, mailbox: &str, uidvalidity: Option<u32>) -> Result<async_imap::types::Mailbox> {
    let mb = session.select(mailbox).await?;
    if let (Some(expected), Some(now)) = (uidvalidity, mb.uid_validity)
        && expected != now
    {
        return Err(Error::Stale(crate::tr!("le dossier {} a été recréé sur le serveur", "folder {} was recreated on the server", crate::utf7::decode(mailbox))));
    }
    Ok(mb)
}

/// `retry`: a previous try failed midway, so what must not happen twice (adding a message) is
/// checked first. `protected`: UIDs a purge, permanent delete or mark-all-read must leave alone.
async fn apply(session: &mut Session, caps: &Caps, op: &Op, retry: bool, protected: &HashSet<u32>) -> Result<()> {
    match op {
        Op::Store { mailbox, uid, flag, value, uidvalidity } => {
            select(session, mailbox, *uidvalidity).await?;
            let sign = if *value { '+' } else { '-' };
            crate::imap::run_checked(session, format!("UID STORE {uid} {sign}FLAGS.SILENT ({flag})")).await?;
        }
        Op::StoreSet { mailbox, uids, flag, value, uidvalidity } => {
            select(session, mailbox, *uidvalidity).await?;
            let sign = if *value { '+' } else { '-' };
            for chunk in chunked(uids) {
                crate::imap::run_checked(session, format!("UID STORE {} {sign}FLAGS.SILENT ({flag})", crate::imap::uid_set(&chunk))).await?;
            }
        }
        Op::Labels { mailbox, uids, add, remove, uidvalidity } => {
            select(session, mailbox, *uidvalidity).await?;
            for chunk in chunked(uids) {
                if !add.is_empty() {
                    add_labels(session, &chunk, add).await?;
                }
                if !remove.is_empty() {
                    crate::imap::run_checked(session, format!("UID STORE {} -X-GM-LABELS ({})", crate::imap::uid_set(&chunk), remove.join(" "))).await?;
                }
            }
        }
        Op::Move { mailbox, uid, dest, uidvalidity } => {
            select(session, mailbox, *uidvalidity).await?;
            if caps.mv {
                session.uid_mv(uid.to_string(), dest).await?;
            } else {
                copy_then_expunge(session, caps, mailbox, *uidvalidity, &[*uid], dest, retry).await?;
            }
        }
        Op::Delete { mailbox, uid, uidvalidity } => {
            select(session, mailbox, *uidvalidity).await?;
            expunge_uid(session, caps, *uid).await?;
        }
        Op::SaveDraft { mailbox, message_id, raw } => {
            session.select(mailbox).await?;
            let previous = draft_uids(session, message_id).await?;
            let raw = B64.decode(raw).map_err(|e| Error::Other(e.to_string()))?;
            // New version first: a cut in between leaves a duplicate, never nothing.
            append(session, mailbox, Some("(\\Draft \\Seen)"), raw).await?;
            expunge_uids(session, caps, &previous).await?;
        }
        Op::DeleteDraft { mailbox, message_id } => {
            session.select(mailbox).await?;
            let uids = draft_uids(session, message_id).await?;
            expunge_uids(session, caps, &uids).await?;
        }
        Op::MoveSet { mailbox, uids, dest, uidvalidity } => {
            select(session, mailbox, *uidvalidity).await?;
            if caps.mv {
                // A chunk already moved before a cut is gone from the source: moving its UIDs again is a
                // no-op.
                for chunk in chunked(uids) {
                    session.uid_mv(crate::imap::uid_set(&chunk), dest).await?;
                }
            } else {
                copy_then_expunge(session, caps, mailbox, *uidvalidity, uids, dest, retry).await?;
            }
        }
        Op::DeleteSet { mailbox, uids, uidvalidity } => {
            select(session, mailbox, *uidvalidity).await?;
            let uids: Vec<u32> = uids.iter().copied().filter(|u| !protected.contains(u)).collect();
            expunge_uids(session, caps, &uids).await?;
        }
        Op::Purge { mailbox, below, uidvalidity, arriving } => {
            let mb = select(session, mailbox, *uidvalidity).await?;
            // Without a mark (folder never synced), "everything" would also hit what arrives after the
            // request: refuse to guess.
            let range = match below {
                Some(0 | 1) => None,
                Some(n) => range_without(1, n - 1, protected),
                None => return Err(Error::Stale(crate::tr!("dossier jamais synchronisé", "folder never synchronised"))),
            };
            if let Some(range) = range
                && mb.exists > 0
            {
                crate::imap::run_checked(session, format!("UID STORE {range} +FLAGS.SILENT (\\Deleted)")).await?;
                if caps.uidplus {
                    crate::imap::run_checked(session, format!("UID EXPUNGE {range}")).await?;
                } else {
                    // Plain EXPUNGE removes every \Deleted message: unmark what a refused move still has to
                    // take out first.
                    let keep: Vec<u32> = protected.iter().copied().collect();
                    for chunk in chunked(&keep) {
                        crate::imap::run_checked(session, format!("UID STORE {} -FLAGS.SILENT (\\Deleted)", crate::imap::uid_set(&chunk))).await?;
                    }
                    crate::imap::run_checked(session, "EXPUNGE".into()).await?;
                }
            }
            // Arrived since the request: one search and one identity fetch per chunk, not a search per
            // Message-ID (thousands after a large delete, past the op's time limit).
            let mut late = Vec::new();
            if let Some(b) = *below
                && !arriving.is_empty()
            {
                let wanted: HashSet<String> = arriving.iter().filter_map(|id| bare_message_id(id.as_bytes())).collect();
                let b = b.max(1);
                // `b:*` also names the highest UID when none is at or above `b`.
                let since: Vec<u32> = session.uid_search(format!("UID {b}:*")).await?.into_iter().filter(|u| *u >= b && !protected.contains(u)).collect();
                for chunk in chunked(&since) {
                    let found = identities(session, &crate::imap::uid_set(&chunk)).await?;
                    late.extend(found.into_iter().filter(|(_, i)| i.message_id.as_ref().is_some_and(|m| wanted.contains(m))).map(|(u, _)| u));
                }
            }
            expunge_uids(session, caps, &late).await?;
        }
        Op::DeleteMailbox { mailbox, dest } => {
            // A previous try cut after the server deleted the folder: nothing is left to do, and the
            // server would refuse every replay.
            if retry && !listed(session, mailbox).await? {
                return Ok(());
            }
            // Emptied until a last look finds it empty: a message filed into it meanwhile (a server
            // rule, another client) would go with the folder.
            if let Some(dest) = dest {
                let mut tries = 0;
                loop {
                    let exists = session.select(mailbox).await?.exists;
                    if exists == 0 {
                        break;
                    }
                    tries += 1;
                    if tries > 3 {
                        return Err(Error::Other(crate::i18n::both(|| {
                            crate::tr!("le dossier reçoit encore des messages : suppression remise à plus tard", "the folder keeps receiving messages: its deletion waits")
                        })));
                    }
                    if caps.mv {
                        session.uid_mv("1:*", dest).await?;
                    } else {
                        // List first: a message arriving between copy and expunge would otherwise be deleted
                        // uncopied. Checked: a refused search must not read as an empty folder, deleted next.
                        let uids: Vec<u32> = crate::imap::uid_search_checked(session, "ALL", exists).await?.into_iter().collect();
                        if !uids.is_empty() {
                            copy_then_expunge(session, caps, mailbox, None, &uids, dest, retry || tries > 1).await?;
                        }
                    }
                }
            }
            // Some servers refuse to delete the selected folder.
            session.select("INBOX").await?;
            session.delete(mailbox).await?;
        }
        Op::MarkAllSeen { mailbox, below, uidvalidity } => {
            // What arrived after the request stays unread.
            let range = match below {
                Some(0 | 1) => None,
                Some(n) => range_without(1, n - 1, protected),
                None => return Err(Error::Stale(crate::tr!("dossier jamais synchronisé", "folder never synchronised"))),
            };
            if let Some(range) = range
                && select(session, mailbox, *uidvalidity).await?.exists > 0
            {
                crate::imap::run_checked(session, format!("UID STORE {range} +FLAGS.SILENT (\\Seen)")).await?;
            }
        }
        Op::Rename { from, to } => {
            session.select("INBOX").await?;
            match session.rename(from, to).await {
                Ok(()) => {}
                // A previous try cut after the server renamed the folder: the goal is reached.
                Err(async_imap::error::Error::No(msg)) => {
                    if !retry || listed(session, from).await? || !listed(session, to).await? {
                        return Err(Error::Imap(async_imap::error::Error::No(msg)));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Op::Create { mailbox } => {
            // Already there (created elsewhere meanwhile): the goal is reached. Any other refusal
            // (invalid name, quota, permissions) is one: the moves into the folder would all fail behind it.
            match session.create(mailbox).await {
                Ok(()) => {}
                Err(async_imap::error::Error::No(msg)) => {
                    if !msg.contains("[ALREADYEXISTS]") && !listed(session, mailbox).await? {
                        return Err(Error::Imap(async_imap::error::Error::No(msg)));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Op::Append { mailbox, flags, raw } => {
            let raw = B64.decode(raw).map_err(|e| Error::Other(e.to_string()))?;
            // Already there (a previous try was cut after sending it, or an uncertain send had gone
            // through): not added twice. Checked on every try: a second send queues a new operation.
            if let Some(id) = message_id_of(&raw) {
                session.select(mailbox).await?;
                if !draft_uids(session, &id).await?.is_empty() {
                    return Ok(());
                }
            }
            let flags = (!flags.is_empty()).then(|| format!("({flags})"));
            append(session, mailbox, flags.as_deref(), raw).await?;
        }
    }
    Ok(())
}

/// For a CREATE refused without `[ALREADYEXISTS]` (not every server sends it), or a RENAME or
/// DELETE retried after a try the server had carried out.
async fn listed(session: &mut Session, mailbox: &str) -> Result<bool> {
    let names: Vec<_> = session.list(Some(""), Some(mailbox)).await?.try_collect().await?;
    Ok(names.iter().any(|n| n.name() == mailbox))
}

fn message_id_of(raw: &[u8]) -> Option<String> {
    mail_parser::MessageParser::default().parse_headers(raw)?.message_id().map(str::to_string)
}

/// Exact Message-ID match: a message whose Message-ID merely contains it is left alone.
async fn draft_uids(session: &mut Session, message_id: &str) -> Result<Vec<u32>> {
    Ok(same_message_id(session, message_id).await?.into_iter().map(|(uid, _)| uid).collect())
}

async fn expunge_uids(session: &mut Session, caps: &Caps, uids: &[u32]) -> Result<()> {
    if uids.is_empty() {
        return Ok(());
    }
    let chunks = chunked(uids);
    if caps.uidplus {
        for chunk in &chunks {
            let set = crate::imap::uid_set(chunk);
            crate::imap::run_checked(session, format!("UID STORE {set} +FLAGS.SILENT (\\Deleted)")).await?;
            crate::imap::run_checked(session, format!("UID EXPUNGE {set}")).await?;
        }
        return Ok(());
    }
    // Without UIDPLUS, EXPUNGE removes every \Deleted message, including ones another client marked
    // but has not expunged yet: unmark those for the EXPUNGE, then mark them again.
    let mine: HashSet<u32> = uids.iter().copied().collect();
    let others: Vec<u32> = crate::imap::uid_search_strict(session, "DELETED").await?.into_iter().filter(|u| !mine.contains(u)).collect();
    let others = chunked(&others);
    for chunk in &others {
        crate::imap::run_checked(session, format!("UID STORE {} -FLAGS.SILENT (\\Deleted)", crate::imap::uid_set(chunk))).await?;
    }
    for chunk in &chunks {
        crate::imap::run_checked(session, format!("UID STORE {} +FLAGS.SILENT (\\Deleted)", crate::imap::uid_set(chunk))).await?;
    }
    let res = crate::imap::run_checked(session, "EXPUNGE".into()).await;
    for chunk in &others {
        crate::imap::run_checked(session, format!("UID STORE {} +FLAGS.SILENT (\\Deleted)", crate::imap::uid_set(chunk))).await?;
    }
    Ok(res?)
}

/// Checks the labels hold before the op removes any: Gmail has been seen accepting the addition
/// without applying it, so once the inbox label was taken the message had no label at all.
/// Tried twice; still missing, the op fails and the messages keep their labels. A UID the server
/// no longer answers for is gone.
async fn add_labels(session: &mut Session, uids: &[u32], add: &[String]) -> Result<()> {
    let mut missing = uids.to_vec();
    for attempt in 1..=2 {
        let set = crate::imap::uid_set(&missing);
        let answers = session.uid_store(&set, format!("+X-GM-LABELS ({})", add.join(" "))).await?.try_collect::<Vec<_>>().await?;
        let mut given = labels_by_uid(&answers);
        let silent: Vec<u32> = missing.iter().copied().filter(|u| !given.contains_key(u)).collect();
        if !silent.is_empty() {
            let fetched = session.uid_fetch(crate::imap::uid_set(&silent), "(UID X-GM-LABELS)").await?.try_collect::<Vec<_>>().await?;
            given.extend(labels_by_uid(&fetched));
        }
        missing.retain(|u| given.get(u).is_some_and(|labels| !carries_all(labels, add)));
        if missing.is_empty() {
            return Ok(());
        }
        let found: Vec<String> = missing.iter().map(|u| format!("{u} {:?}", given.get(u))).collect();
        tracing::warn!("Gmail did not add {add:?} (attempt {attempt}): {}", found.join(", "));
    }
    let names: Vec<String> = add.iter().map(|l| crate::utf7::decode(&crate::provider::unquote_label(l))).collect();
    Err(Error::Other(crate::i18n::both(|| {
        crate::tr!("Gmail n'a pas ajouté le libellé {} : les messages gardent leurs libellés", "Gmail did not add the label {}: the messages keep their labels", names.join(", "))
    })))
}

fn labels_by_uid(answers: &[async_imap::types::Fetch]) -> std::collections::HashMap<u32, Vec<String>> {
    answers.iter().filter_map(|f| Some((f.uid?, f.gmail_labels()?.iter().map(|l| crate::sync::unescape_label(l)).collect()))).collect()
}

/// `labels` as the server gives them, `wanted` as X-GM-LABELS commands name them; system labels
/// match whatever their case.
fn carries_all(labels: &[String], wanted: &[String]) -> bool {
    wanted.iter().all(|w| {
        let w = crate::provider::unquote_label(w);
        labels.iter().any(|l| *l == w || w.starts_with('\\') && l.trim_start_matches('\\').eq_ignore_ascii_case(w.trim_start_matches('\\')))
    })
}

async fn expunge_uid(session: &mut Session, caps: &Caps, uid: u32) -> Result<()> {
    expunge_uids(session, caps, &[uid]).await
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn gmail_labels_are_added_and_taken_in_one_command_each() {
        let select = "* 3 EXISTS\r\n* OK [UIDVALIDITY 7] UIDs valid\r\n{tag} OK [READ-WRITE] selected";
        let (mut session, log) = crate::imap::tests::scripted_with_log(vec![
            (select, false),
            // The answer gives UID 10's labels only: UID 11's are read.
            ("* 1 FETCH (UID 10 X-GM-LABELS (\"\\\\Inbox\" \"Mairie\"))\r\n{tag} OK stored", false),
            ("* 2 FETCH (UID 11 X-GM-LABELS (\"Mairie\"))\r\n{tag} OK done", false),
            ("{tag} OK stored", false),
        ])
        .await;
        let op = Op::Labels {
            mailbox: "[Gmail]/All Mail".into(),
            uids: vec![10, 11],
            add: vec!["\"Mairie\"".into()],
            remove: vec!["\\Inbox".into()],
            uidvalidity: Some(7),
        };
        apply(&mut session, &Caps::default(), &op, false, &HashSet::new()).await.unwrap();
        let sent = log.lock().unwrap().clone();
        assert_eq!(&sent[1..], ["UID STORE 10:11 +X-GM-LABELS (\"Mairie\")", "UID FETCH 11 (UID X-GM-LABELS)", "UID STORE 10:11 -X-GM-LABELS (\\Inbox)"]);
    }

    #[tokio::test]
    async fn a_label_gmail_does_not_add_keeps_the_inbox_label() {
        let select = "* 3 EXISTS\r\n* OK [UIDVALIDITY 7] UIDs valid\r\n{tag} OK [READ-WRITE] selected";
        let ignored = "* 1 FETCH (UID 10 X-GM-LABELS (\"\\\\Inbox\"))\r\n{tag} OK stored";
        let (mut session, log) = crate::imap::tests::scripted_with_log(vec![(select, false), (ignored, false), (ignored, false)]).await;
        let op = Op::Labels {
            mailbox: "[Gmail]/All Mail".into(),
            uids: vec![10],
            add: vec!["\"New senders\"".into()],
            remove: vec!["\\Inbox".into()],
            uidvalidity: Some(7),
        };
        let err = apply(&mut session, &Caps::default(), &op, false, &HashSet::new()).await.unwrap_err();
        assert!(err.to_string().contains("New senders"), "{err}");
        let sent = log.lock().unwrap().clone();
        assert_eq!(&sent[1..], ["UID STORE 10 +X-GM-LABELS (\"New senders\")", "UID STORE 10 +X-GM-LABELS (\"New senders\")"], "the inbox label stays");
    }

    #[test]
    fn labels_are_matched_as_commands_name_them() {
        let given = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(carries_all(&given(&["\\Inbox", "New senders"]), &["\"New senders\"".to_string()]));
        assert!(carries_all(&given(&["\\important"]), &["\\Important".to_string()]));
        assert!(!carries_all(&given(&["\\Inbox"]), &["\"New senders\"".to_string()]));
    }

    use super::*;

    #[test]
    fn a_passing_refusal_is_retried_not_counted() {
        use async_imap::error::Error as Imap;
        let no = |info: &str| Error::Imap(Imap::No(format!("code: None, info: Some(\"{info}\")")));
        assert_eq!(refused(&no("[TRYCREATE] No such mailbox")).as_deref(), Some("No such mailbox"));
        assert_eq!(refused(&no("[OVERQUOTA] Quota exceeded")).as_deref(), Some("Quota exceeded"));
        assert_eq!(refused(&no("[INUSE] Mailbox is locked")), None);
        assert_eq!(refused(&no("[UNAVAILABLE] Temporary System Error")), None);
        assert_eq!(refused(&Error::Imap(Imap::Bad("code: None, info: Some(\"[SERVERBUG] Internal error\")".into()))), None);
    }

    #[test]
    fn message_ids_are_compared_bare() {
        assert_eq!(bare_message_id(b" <abc@x> ").as_deref(), Some("abc@x"));
        assert_eq!(bare_message_id(b"abc@x").as_deref(), Some("abc@x"));
        assert_eq!(bare_message_id(b"<>"), None);
        let id = |mid: &str, size| Identity { message_id: bare_message_id(mid.as_bytes()), size: Some(size), date: None };
        assert_eq!(id("<abc@x>", 10), id("abc@x", 10));
        assert_ne!(id("abc@x", 10), id("abc@x", 11), "same Message-ID, another message");
        assert_ne!(id("1abc@x", 10), id("abc@x", 10));
    }

    #[test]
    fn folders_within_a_pending_rename() {
        assert!(is_within("Customers", "Customers", Some("/")));
        assert!(is_within("Customers/2024", "Customers", Some("/")));
        assert!(!is_within("Customers2", "Customers", Some("/")));
        assert!(!is_within("Customers/2024", "Customers", None));
        assert!(!is_within("INBOX", "Customers", Some("/")));
    }

    #[test]
    fn uid_sets_are_chunked_and_holed() {
        let many: Vec<u32> = (1..=1200).rev().collect();
        let chunks = chunked(&many);
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [500, 500, 200]);
        assert_eq!((chunks[0][0], chunks[2][199]), (1, 1200), "sorted");
        let skip = |u: &[u32]| u.iter().copied().collect::<HashSet<u32>>();
        assert_eq!(range_without(1, 9, &skip(&[5])).as_deref(), Some("1:4,6:9"));
        assert_eq!(range_without(1, 9, &skip(&[1, 9, 20])).as_deref(), Some("2:8"));
        assert_eq!(range_without(1, 3, &skip(&[2])).as_deref(), Some("1,3"));
        assert_eq!(range_without(1, 3, &skip(&[1, 2, 3])), None);
        assert_eq!(range_without(1, 1, &HashSet::new()).as_deref(), Some("1"));
    }

    #[test]
    fn refused_moves_protect_their_messages_from_a_purge() {
        let store = Store::open_in_memory().unwrap();
        let srv = crate::model::ServerConfig { host: "localhost".into(), port: 143, security: crate::model::Security::None };
        let acc = store
            .insert_account(&crate::model::NewAccount {
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: crate::model::NewAuth::Password,
            })
            .unwrap();
        let push = |op: &Op| store.push_op(acc, &serde_json::to_string(op).unwrap(), "x", None).unwrap();
        // Out of the trash, refused for good by the server.
        let out = push(&Op::MoveSet { mailbox: "Trash".into(), uids: vec![5, 6], dest: "INBOX".into(), uidvalidity: None });
        for _ in 0..MAX_ATTEMPTS {
            store.fail_op(out, "NO", MAX_ATTEMPTS).unwrap();
        }
        push(&Op::Move { mailbox: "Trash".into(), uid: 8, dest: "Archive".into(), uidvalidity: None });
        push(&Op::MoveSet { mailbox: "INBOX".into(), uids: vec![1], dest: "Trash".into(), uidvalidity: None });
        let purge = push(&Op::Purge { mailbox: "Trash".into(), below: Some(10), uidvalidity: None, arriving: Vec::new() });
        let protected = protected_uids(&store, acc, "Trash", purge, false, false).unwrap();
        assert_eq!(protected, [5, 6, 8].into_iter().collect::<HashSet<u32>>());
        assert_eq!(range_without(1, 9, &protected).as_deref(), Some("1:4,7,9"));
        // A permanent delete of messages the failed move gave back goes through: only the
        // waiting move holds its message.
        let delete = push(&Op::DeleteSet { mailbox: "Trash".into(), uids: vec![5, 8], uidvalidity: None });
        assert_eq!(protected_uids(&store, acc, "Trash", delete, true, false).unwrap(), [8].into_iter().collect::<HashSet<u32>>());
        // Marking the trash read leaves out the message a waiting move queued before takes out,
        // not one moved after it was asked for.
        let seen = push(&Op::MarkAllSeen { mailbox: "Trash".into(), below: Some(10), uidvalidity: None });
        push(&Op::Move { mailbox: "Trash".into(), uid: 3, dest: "Archive".into(), uidvalidity: None });
        assert_eq!(protected_uids(&store, acc, "Trash", seen, true, true).unwrap(), [8].into_iter().collect::<HashSet<u32>>());
    }
}
