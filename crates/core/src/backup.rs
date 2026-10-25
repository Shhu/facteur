//! Full account backups as Maildir, and restore.
//!
//! Layout: `<destination>/<address>/<folder>/<subfolder>/{cur,new,tmp}`, one file per message,
//! named with the arrival date, UID, UIDVALIDITY and flags: `1792166641.U42V7.facteur!2,FS`
//! (`:` instead of `!` where the disk allows it: Windows and FAT/exFAT drives refuse `:`).
//!
//! Incremental (only missing messages are downloaded, flags are updated by renaming), and never
//! deletes anything: a message erased from the server stays in the backup.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use futures::TryStreamExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, bounded};
use crate::imap::{self, Session, server_flags, uid_set};
use crate::mime;
use crate::store::ServerFlags;
use crate::utf7;

pub mod index;

pub use index::{Catalog, CatalogEntry, CatalogFolder, RestoreFilter};
use index::{Index, Item};

const CHUNK: usize = 25;
/// Holds the folder's original IMAP name, for the restore.
const FOLDER_FILE: &str = ".facteur-dossier.json";
const MANIFEST_FILE: &str = "facteur-sauvegarde.json";

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub mailbox: String,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FolderSummary {
    pub name: String,
    pub path: String,
    pub messages: usize,
}

/// Written at the end of each successful backup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub email: String,
    pub last_backup: i64,
    pub messages: usize,
    pub bytes: u64,
    pub folders: Vec<FolderSummary>,
    /// Messages the server did not send during that backup (0 for a complete one): the backup
    /// is shown incomplete and the automatic one asks for them again sooner.
    #[serde(default)]
    pub failed: usize,
    /// Name of the backup's directory: what a restore names it by (not stored, read from where
    /// the manifest lies).
    #[serde(default, skip_deserializing)]
    pub dir: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupStats {
    pub folders: usize,
    pub messages: usize,
    pub downloaded: usize,
    /// Messages already saved in another folder (a Gmail label, a moved message): copied from
    /// there on the disk instead of downloaded again.
    pub copied: usize,
    pub bytes: u64,
    /// Messages still on the server that it did not send (refused, a bandwidth limit reached):
    /// not saved this time, the next backup asks for them again.
    pub failed: usize,
    /// Folders listed but that the server will not open (shared without the read right): left
    /// out, named in the log, not counted as failed (it will not change by asking again).
    #[serde(default)]
    pub unreadable_folders: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RestoreStats {
    pub target: String,
    pub folders: usize,
    pub messages: usize,
    /// Already on the server, not sent again (restore into the original folders).
    pub skipped: usize,
    /// Messages not restored: refused by the server (too large, malformed) or missing from the
    /// backup. The restore goes on with the others.
    pub failed: usize,
    /// The first few of them: subject (or file) and reason.
    pub failures: Vec<String>,
}

const FAILURES_SHOWN: usize = 5;

impl RestoreStats {
    fn fail(&mut self, item: &Item, reason: String) {
        self.failed += 1;
        if self.failures.len() < FAILURES_SHOWN {
            let name = if item.subject.trim().is_empty() { item.file.as_str() } else { item.subject.trim() };
            self.failures.push(format!("{name} ({reason})"));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreTarget {
    /// A new « Restore <date> » folder reproducing the original tree.
    NewFolder,
    /// The original folders, skipping messages already there (same Message-ID).
    Original,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FolderInfo {
    name: String,
    delimiter: Option<String>,
}

/// By its name (`Manifest::dir`) or, for older callers, the address.
pub fn account_dir(dest: &Path, source: &str) -> PathBuf {
    dest.join(sanitize(source))
}

pub fn read_manifest(account_dir: &Path) -> Option<Manifest> {
    let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(account_dir.join(MANIFEST_FILE)).ok()?).ok()?;
    manifest.dir = account_dir.file_name()?.to_string_lossy().into_owned();
    Some(manifest)
}

/// A long job's session: on an account limited to one connection, given over between two folders
/// or batches to whoever waits for it (opening a message, a sync), then taken again.
#[allow(async_fn_in_trait)]
pub trait Lease {
    fn session(&mut self) -> &mut Session;
    /// Whether the connection was given over: another may have selected a folder meanwhile.
    async fn give_way(&mut self) -> Result<bool>;
}

impl Lease for Session {
    fn session(&mut self) -> &mut Session {
        self
    }

    async fn give_way(&mut self) -> Result<bool> {
        Ok(false)
    }
}

pub async fn backup_account(
    lease: &mut impl Lease,
    email: &str,
    root: &Path,
    mut progress: impl FnMut(Progress),
) -> Result<BackupStats> {
    let mut stats = BackupStats::default();
    let mut folders = Vec::new();
    // The disk as it was before this run: files a failed one left unindexed differ from it.
    let before = signature(root).ok();
    let listed: Vec<_> = imap::list_mailboxes(lease.session()).await?.into_iter().filter(|l| l.selectable).collect();
    std::fs::create_dir_all(root)?;
    let sep = info_sep(root);
    let paths = unique_paths(root, &listed.iter().map(|l| (l.name.as_str(), l.delimiter.as_deref())).collect::<Vec<_>>());
    for (listed, rel) in listed.iter().zip(paths) {
        let dir = root.join(&rel);
        let place = Place { root, dir: &dir, rel: &index::rel_path(root, &dir), sep };
        lease.give_way().await?;
        let count = backup_folder(lease, &listed.name, listed.delimiter.as_deref(), &place, &mut stats, &mut progress).await?;
        stats.folders += 1;
        stats.messages += count;
        folders.push(FolderSummary {
            name: utf7::decode(&listed.name),
            path: rel.to_string_lossy().into_owned(),
            messages: count,
        });
    }
    // Copies saved before they were linked (see `copy_saved_elsewhere`) are linked now: once.
    match crate::rt::offload(|| link_copies(root)) {
        Ok(0) => {}
        Ok(n) => tracing::info!("backup of {email}: {n} copies of saved messages linked to one file"),
        Err(e) => tracing::warn!("backup of {email}: copies not linked: {e}"),
    }
    let manifest = Manifest {
        email: email.to_string(),
        last_backup: chrono::Utc::now().timestamp(),
        messages: stats.messages,
        bytes: dir_size(root),
        folders,
        failed: stats.failed,
        dir: String::new(),
    };
    write_atomic(&root.join(MANIFEST_FILE), &serde_json::to_vec_pretty(&manifest)?)?;
    stats.bytes = manifest.bytes;
    // What was saved is kept (manifest, index), but the backup is not reported complete.
    if stats.failed > 0 {
        let n = stats.failed;
        return Err(Error::Other(crate::tr!(
            "{n} message(s) n'ont pas pu être sauvegardés (refusés par le serveur) : la prochaine sauvegarde les redemandera",
            "{n} message(s) could not be saved (refused by the server): the next backup will ask for them again"
        )));
    }
    mark_current(root, before);
    Ok(stats)
}

/// After a run whose index followed every file it wrote: a catalog already read in this session
/// need not read the whole backup again for it, unless the disk had changed before it (files a
/// failed run left unindexed), which the next catalog then reads.
fn mark_current(root: &Path, before: Option<u64>) {
    let Ok(mut known) = refreshed().lock() else { return };
    let Some(cached) = known.get(root).copied() else { return };
    match signature(root) {
        Ok(sig) if before == Some(cached) => {
            known.insert(root.to_path_buf(), sig);
        }
        _ => {
            known.remove(root);
        }
    }
}

/// Backup directory of each folder (relative to the account's directory). Two IMAP folders
/// that would share one (names that differ only by characters replaced by `sanitize`, or only
/// by case on a case-insensitive disk) are kept apart: the one already saved there (or the
/// first by name) keeps it, the others get a suffix made from a hash of their IMAP name.
fn unique_paths(root: &Path, folders: &[(&str, Option<&str>)]) -> Vec<PathBuf> {
    let plain: Vec<PathBuf> = folders.iter().map(|(name, delimiter)| folder_path(name, *delimiter)).collect();
    let folded = case_insensitive(root);
    let key = |p: &PathBuf| {
        let s = p.to_string_lossy().into_owned();
        if folded { s.to_lowercase() } else { s }
    };
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, p) in plain.iter().enumerate() {
        groups.entry(key(p)).or_default().push(i);
    }
    let mut out = plain.clone();
    for members in groups.values() {
        let saved: Option<String> = std::fs::read(root.join(&plain[members[0]]).join(FOLDER_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<FolderInfo>(&b).ok())
            .map(|info| info.name);
        // The directory stays its saver's, even once that folder is gone from the server: a twin
        // taking it over would mix the old folder's mail into its own (and restore it there).
        let owner = match saved {
            Some(saved) => members.iter().copied().find(|i| saved == folders[*i].0),
            None => members.iter().copied().min_by_key(|i| folders[*i].0),
        };
        if members.len() == 1 && owner.is_some() {
            continue;
        }
        for &i in members.iter().filter(|i| Some(**i) != owner) {
            let mut p = plain[i].clone();
            let leaf = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            p.set_file_name(format!("{leaf}~{:06x}", fnv1a(folders[i].0.as_bytes()) & 0xff_ffff));
            out[i] = p;
        }
    }
    out
}

/// The disk holding the backup ignores case (Windows, macOS): checked once per directory.
fn case_insensitive(root: &Path) -> bool {
    let probe = root.join(".facteur-casse");
    if std::fs::write(&probe, b"").is_err() {
        return cfg!(any(windows, target_os = "macos"));
    }
    let folded = root.join(".FACTEUR-CASSE").exists();
    let _ = std::fs::remove_file(&probe);
    folded
}

/// Separator before a message file's flags: ':' as Maildir wants, '!' where the disk refuses
/// ':' (Windows, and FAT/exFAT drives on any system). Both are read back.
fn info_sep(root: &Path) -> char {
    if cfg!(windows) {
        return '!';
    }
    let probe = root.join(".facteur-sep:2,");
    if std::fs::write(&probe, b"").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return ':';
    }
    // Only when a plain name can be written: a disk refusing everything fails the backup
    // anyway, and must not have every saved file renamed for it.
    let plain = root.join(".facteur-sep");
    if std::fs::write(&plain, b"").is_ok() {
        let _ = std::fs::remove_file(&plain);
        return '!';
    }
    ':'
}

/// FNV-1a, 32 bits: a short hash that stays the same across versions and systems.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| (h ^ *b as u32).wrapping_mul(0x0100_0193))
}

struct Place<'a> {
    root: &'a Path,
    dir: &'a Path,
    /// Relative to `root`: the folder's key in the index.
    rel: &'a str,
    sep: char,
}

/// Returns how many of the folder's messages are in the backup. The index is only opened between
/// network exchanges: a SQLite connection must not live across an `.await` (not `Send`).
async fn backup_folder(
    lease: &mut impl Lease,
    name: &str,
    delimiter: Option<&str>,
    place: &Place<'_>,
    stats: &mut BackupStats,
    progress: &mut impl FnMut(Progress),
) -> Result<usize> {
    let dir = place.dir;
    for sub in ["cur", "new", "tmp"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    let info = FolderInfo { name: name.to_string(), delimiter: delimiter.map(str::to_string) };
    write_atomic(&dir.join(FOLDER_FILE), &serde_json::to_vec_pretty(&info)?)?;
    Index::open(place.root)?.upsert_folder(place.rel, &info)?;

    // Every exchange is bounded: a connection dead during an automatic backup (sleep) must not
    // leave it "in progress" until a restart.
    let what = crate::tr!("sauvegarde", "backup");
    // A folder listed but not readable (shared without the read right): counted as failed, the
    // other folders still saved.
    let mb = match bounded(120, &what, async { Ok(lease.session().examine(name).await?) }).await {
        // Refused for good (no read right, gone): noted apart, so the backup is not retried every
        // hour for it. For a while (locked, busy, throttled): failed, tried again soon.
        Err(Error::Imap(async_imap::error::Error::No(why))) => {
            tracing::warn!("backup: folder {name} refused: {why}");
            let upper = why.to_ascii_uppercase();
            if ["[NOPERM]", "[NONEXISTENT]", "[NOACCESS]"].iter().any(|c| upper.contains(c)) {
                stats.unreadable_folders.push(utf7::decode(name));
            } else {
                stats.failed += 1;
            }
            let mut held = 0;
            for entry in std::fs::read_dir(dir.join("cur"))? {
                let entry = entry?;
                held += usize::from(index::is_message_file(&entry, &entry.file_name().to_string_lossy()));
            }
            return Ok(held);
        }
        mb => mb?,
    };
    let validity = mb.uid_validity.unwrap_or(0);
    let mut have: HashMap<u32, String> = HashMap::new();
    let mut total_files = 0;
    for entry in std::fs::read_dir(dir.join("cur"))? {
        let entry = entry?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if !index::is_message_file(&entry, &file) {
            continue;
        }
        total_files += 1;
        if let Some((uid, v, _)) = parse_name(&file)
            && v == validity
        {
            have.insert(uid, file);
        }
    }
    if mb.exists == 0 {
        return Ok(total_files);
    }

    // Checked: async-imap takes a refused or cut SEARCH for an empty folder, which would have
    // this one reported saved with none of its new messages.
    let server: HashSet<u32> = match bounded(300, &what, imap::uid_search_checked(lease.session(), "ALL", mb.exists)).await {
        Ok(server) => server,
        // The connection still answers: the server refuses this folder's search. Its messages
        // not saved yet count as failed (the backup is not reported complete), the other
        // folders go on.
        Err(Error::Imap(async_imap::error::Error::No(_))) => {
            stats.failed += (mb.exists as usize).saturating_sub(have.len()).max(1);
            return Ok(total_files);
        }
        Err(e) => return Err(e),
    };
    let mut missing: Vec<u32> = server.iter().filter(|u| !have.contains_key(u)).copied().collect();
    missing.sort_unstable();
    // Files saved under another UIDVALIDITY (the folder was recreated on the server): the
    // messages already there are taken over by their new UID instead of downloaded again.
    if total_files > have.len() && !missing.is_empty() {
        adopt_previous(lease.session(), place, validity, &mut missing, &mut have).await?;
    }
    if !missing.is_empty() {
        let copied = copy_saved_elsewhere(lease.session(), place, validity, &mut missing).await?;
        stats.copied += copied;
        total_files += copied;
    }
    let label = utf7::decode(name);
    let total = missing.len();
    progress(Progress { mailbox: label.clone(), done: 0, total });

    for (i, chunk) in missing.chunks(CHUNK).enumerate() {
        // The connection given over meanwhile: the folder selected again, unless recreated.
        if i > 0 && lease.give_way().await? {
            let again = bounded(120, &what, async { Ok(lease.session().examine(name).await?) }).await?;
            if again.uid_validity != mb.uid_validity {
                // Not saved: the backup is incomplete, tried again within the hour.
                stats.failed += missing.len().saturating_sub(i * CHUNK);
                return Ok(total_files);
            }
        }
        // One message at a time, written then dropped: a batch of large messages does not fit in
        // memory. The index follows in one transaction once the batch is on the disk.
        let mut written = Vec::new();
        {
            let mut stream = bounded(120, &what, async { Ok(lease.session().uid_fetch(uid_set(chunk), "(UID FLAGS INTERNALDATE BODY.PEEK[])").await?) }).await?;
            // Bounded per message rather than per chunk: 25 large messages on a slow link take
            // the time they need, a server gone silent still stops the backup.
            while let Some(f) = bounded(600, &what, async { Ok(stream.try_next().await?) }).await? {
                let (Some(uid), Some(raw)) = (f.uid, f.body()) else { continue };
                if !chunk.contains(&uid) {
                    continue;
                }
                let received = f.internal_date().map(|d| d.timestamp()).unwrap_or(0);
                let base = format!("{received}.U{uid}V{validity}.facteur");
                let file = format!("{base}{}2,{}", place.sep, flag_letters(server_flags(f.flags())));
                // Written to tmp/ and synced, then renamed into cur/: never a half-written file,
                // even after a power cut.
                let tmp = dir.join("tmp").join(&base);
                // The write, its fsync (slow on some drives) and the parsing off the async worker.
                let headers = crate::rt::offload(|| -> Result<_> {
                    write_synced(&tmp, raw)?;
                    std::fs::rename(&tmp, dir.join("cur").join(&file))?;
                    Ok(mime::parse_headers(raw, received))
                })?;
                written.push((uid, file, received, headers, raw.len() as u64));
            }
        }
        // async-imap ends the answer at the tagged reply without reading it: a NO (« some
        // messages could not be FETCHed », a bandwidth limit) looks like a short answer. Those
        // not sent and still on the server (not deleted meanwhile) are counted as failed.
        let unsaved: Vec<u32> = chunk.iter().copied().filter(|u| !written.iter().any(|(w, ..)| w == u)).collect();
        if !unsaved.is_empty() {
            let query = format!("UID {}", uid_set(&unsaved));
            let still = bounded(120, &what, imap::uid_search_checked(lease.session(), &query, 0)).await?;
            stats.failed += unsaved.iter().filter(|u| still.contains(u)).count();
        }
        // The renames themselves reach the disk before the index records the files.
        crate::rt::offload(|| -> Result<()> {
            sync_dir(&dir.join("cur"));
            let idx = Index::open(place.root)?;
            idx.begin()?;
            for (_, file, received, headers, size) in &written {
                idx.insert(place.rel, file, *received, headers, *size)?;
            }
            idx.commit()?;
            Ok(())
        })?;
        stats.downloaded += written.len();
        total_files += written.len();
        progress(Progress { mailbox: label.clone(), done: ((i + 1) * CHUNK).min(total), total });
    }

    // Flags of messages already saved: renamed when seen/flagged/answered changed.
    if !have.is_empty() {
        let mut renames: Vec<(&String, String)> = Vec::new();
        {
            // Bounded per response like the download: the flags of a huge folder on a slow
            // server take the time they need, a server gone silent still stops the backup.
            let mut stream = bounded(120, &what, async { Ok(lease.session().uid_fetch("1:*", "(UID FLAGS)").await?) }).await?;
            while let Some(f) = bounded(300, &what, async { Ok(stream.try_next().await?) }).await? {
                let Some(uid) = f.uid else { continue };
                let Some(current) = have.get(&uid) else { continue };
                let (base, _) = split_info(current);
                let wanted = format!("{base}{}2,{}", place.sep, flag_letters(server_flags(f.flags())));
                if &wanted != current {
                    renames.push((current, wanted));
                }
            }
        }
        crate::rt::offload(|| -> Result<()> {
            for (current, wanted) in &renames {
                std::fs::rename(dir.join("cur").join(current), dir.join("cur").join(wanted))?;
            }
            if renames.is_empty() {
                return Ok(());
            }
            sync_dir(&dir.join("cur"));
            let idx = Index::open(place.root)?;
            idx.begin()?;
            for (_, wanted) in &renames {
                idx.rename(place.rel, wanted)?;
            }
            idx.commit()?;
            Ok(())
        })?;
    }
    Ok(total_files)
}

/// After a UIDVALIDITY change: missing UIDs whose message is already saved in this folder under
/// the old UIDVALIDITY (same Message-ID, and same size or arrival date) take over that file,
/// renamed to their new UID, rather than being downloaded a second time.
async fn adopt_previous(
    session: &mut Session,
    place: &Place<'_>,
    validity: u32,
    missing: &mut Vec<u32>,
    have: &mut HashMap<u32, String>,
) -> Result<()> {
    let what = crate::tr!("sauvegarde", "backup");
    // (uid, Message-ID, size, arrival date), read before the index is opened (no connection
    // across an await).
    let mut seen: Vec<(u32, String, i64, i64)> = Vec::new();
    for chunk in missing.chunks(500) {
        bounded(300, &what, async {
            let mut stream = session.uid_fetch(uid_set(chunk), "(UID RFC822.SIZE INTERNALDATE ENVELOPE)").await?;
            while let Some(f) = stream.try_next().await? {
                let Some(uid) = f.uid else { continue };
                let Some(id) = f.envelope().and_then(|e| e.message_id.as_ref()) else { continue };
                let id = String::from_utf8_lossy(id).trim_matches(['<', '>', ' ']).to_string();
                let received = f.internal_date().map(|d| d.timestamp()).unwrap_or(0);
                seen.push((uid, id, f.size.unwrap_or(0) as i64, received));
            }
            Ok(())
        })
        .await?;
    }
    if seen.is_empty() {
        return Ok(());
    }
    let idx = Index::open(place.root)?;
    // Older files may not be indexed yet (saved by an earlier version).
    let info: FolderInfo = serde_json::from_slice(&std::fs::read(place.dir.join(FOLDER_FILE))?)?;
    idx.refresh_folder(place.root, place.dir, &info)?;
    let mut adopted = HashSet::new();
    for (uid, id, size, received) in seen {
        let old = idx.copies(place.rel, &id)?.into_iter().find(|(_, file, s, r)| {
            parse_name(file).is_some_and(|(_, v, _)| v != validity) && (*s == size || (received > 0 && *r == received))
        });
        let Some((old_base, old_file, ..)) = old else { continue };
        let Some(path) = index::find_file(place.dir, &old_file) else { continue };
        let (_, letters) = split_info(&old_file);
        let stamp = old_base.split('.').next().unwrap_or("0");
        let file = format!("{stamp}.U{uid}V{validity}.facteur{}2,{letters}", place.sep);
        std::fs::rename(&path, place.dir.join("cur").join(&file))?;
        idx.rebase(place.rel, &old_base, &file)?;
        have.insert(uid, file);
        adopted.insert(uid);
    }
    sync_dir(&place.dir.join("cur"));
    missing.retain(|u| !adopted.contains(u));
    Ok(())
}

/// Messages of `missing` already saved in another folder of the backup (same Message-ID, same
/// size): on Gmail, every label is a folder of its own, and labelling a hundred messages had
/// them all downloaded again. Linked to that file (copied where the disk has no hard links)
/// instead; returns how many, taken out of `missing`.
async fn copy_saved_elsewhere(session: &mut Session, place: &Place<'_>, validity: u32, missing: &mut Vec<u32>) -> Result<usize> {
    let what = crate::tr!("sauvegarde", "backup");
    // (uid, Message-ID, size, arrival date, flags), read before the index is opened.
    let mut seen: Vec<(u32, String, i64, i64, String)> = Vec::new();
    for chunk in missing.chunks(500) {
        bounded(300, &what, async {
            let mut stream = session.uid_fetch(uid_set(chunk), "(UID FLAGS RFC822.SIZE INTERNALDATE ENVELOPE)").await?;
            while let Some(f) = stream.try_next().await? {
                let (Some(uid), Some(size)) = (f.uid, f.size) else { continue };
                let Some(id) = f.envelope().and_then(|e| e.message_id.as_ref()) else { continue };
                let id = String::from_utf8_lossy(id).trim_matches(['<', '>', ' ']).to_string();
                if id.is_empty() || !chunk.contains(&uid) {
                    continue;
                }
                let received = f.internal_date().map(|d| d.timestamp()).unwrap_or(0);
                seen.push((uid, id, size as i64, received, flag_letters(server_flags(f.flags()))));
            }
            Ok(())
        })
        .await?;
    }
    if seen.is_empty() {
        return Ok(0);
    }
    let (root, dir, rel, sep) = (place.root.to_path_buf(), place.dir.to_path_buf(), place.rel.to_string(), place.sep);
    // Disk work only (links, index): off the async worker.
    let copied = crate::rt::offload(move || -> Result<HashSet<u32>> {
        let idx = Index::open(&root)?;
        let mut copied = HashSet::new();
        idx.begin()?;
        for (uid, id, size, received, letters) in seen {
            let Some((folder, source)) = idx.elsewhere(&rel, &id, size, received)? else { continue };
            let Some(path) = index::find_file(&root.join(&folder), &source) else { continue };
            let file = format!("{received}.U{uid}V{validity}.facteur{sep}2,{letters}");
            let target = dir.join("cur").join(&file);
            if std::fs::hard_link(&path, &target).is_err() {
                // Through tmp/, as a download: never a half-written file in cur/.
                let tmp = dir.join("tmp").join(&file);
                std::fs::copy(&path, &tmp)?;
                std::fs::rename(&tmp, &target)?;
            }
            idx.insert_copy(&folder, &source, &rel, &file, received)?;
            copied.insert(uid);
        }
        idx.commit()?;
        if !copied.is_empty() {
            sync_dir(&dir.join("cur"));
        }
        Ok(copied)
    })?;
    missing.retain(|u| !copied.contains(u));
    Ok(copied.len())
}

pub struct Selection {
    items: Vec<Item>,
    folders: HashMap<String, FolderInfo>,
}

impl Selection {
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Backups whose index was caught up on the disk in this session, with the signature of their
/// directories at that time.
fn refreshed() -> &'static std::sync::Mutex<HashMap<PathBuf, u64>> {
    static MAP: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, u64>>> = std::sync::OnceLock::new();
    MAP.get_or_init(Default::default)
}

/// Folders of the backup and the modification times of their cur/ and new/ directories (a file
/// added, removed or renamed changes them): reading this costs a few stats, not a listing of
/// every message file.
fn signature(root: &Path) -> Result<u64> {
    let mut folders = Vec::new();
    collect_folders(root, &mut folders)?;
    folders.sort_by(|a, b| a.0.cmp(&b.0));
    let mut bytes = Vec::new();
    for (dir, info) in &folders {
        bytes.extend(dir.to_string_lossy().as_bytes());
        bytes.extend(info.name.as_bytes());
        for sub in ["cur", "new"] {
            let mtime = std::fs::metadata(dir.join(sub))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            bytes.extend(mtime.to_le_bytes());
        }
    }
    Ok((fnv1a(&bytes) as u64) << 32 | (bytes.len() as u64 & 0xffff_ffff))
}

fn refresh_if_changed(idx: &Index, root: &Path) -> Result<()> {
    let sig = signature(root)?;
    if refreshed().lock().is_ok_and(|known| known.get(root) == Some(&sig)) {
        return Ok(());
    }
    idx.refresh(root)?;
    if let Ok(mut known) = refreshed().lock() {
        known.insert(root.to_path_buf(), sig);
    }
    Ok(())
}

pub fn catalog(root: &Path, filter: &RestoreFilter, limit: usize, offset: usize) -> Result<Catalog> {
    let idx = Index::open(root)?;
    refresh_if_changed(&idx, root)?;
    idx.catalog(filter, limit, offset)
}

/// The explicit selection `ids` when given, otherwise everything the filter matches.
pub fn select(root: &Path, filter: &RestoreFilter, ids: Option<&[i64]>) -> Result<Selection> {
    let idx = Index::open(root)?;
    refresh_if_changed(&idx, root)?;
    let items = idx.items(filter, ids)?;
    let mut folders = HashMap::new();
    for item in &items {
        if !folders.contains_key(&item.folder) {
            folders.insert(item.folder.clone(), idx.folder(&item.folder)?);
        }
    }
    Ok(Selection { items, folders })
}

pub fn read_entry(root: &Path, id: i64) -> Result<Vec<u8>> {
    let (folder, file) = Index::open(root)?.location(id)?;
    let path = index::find_file(&root.join(&folder), &file)
        .ok_or_else(|| Error::NotFound(crate::tr!("fichier {file} introuvable dans la sauvegarde", "file {file} not found in the backup")))?;
    Ok(std::fs::read(path)?)
}

/// `name` as one level of the target server: its hierarchy delimiter replaced.
fn without_delimiter(name: &str, delim: &str) -> String {
    if delim.is_empty() { name.to_string() } else { name.replace(delim, "-") }
}

pub async fn restore(
    lease: &mut impl Lease,
    root: &Path,
    selection: Selection,
    target: RestoreTarget,
    prefix: &str,
    mut progress: impl FnMut(Progress),
) -> Result<RestoreStats> {
    // Target server's delimiter and namespace (« INBOX.xxx » on Courier/Cyrus).
    let what = crate::tr!("restauration", "restore");
    let listed = bounded(120, &what, imap::list_mailboxes(lease.session())).await?;
    let delim = listed.iter().find_map(|l| l.delimiter.clone()).unwrap_or_else(|| "/".into());
    let under_inbox = listed
        .iter()
        .filter(|l| !l.name.eq_ignore_ascii_case("INBOX"))
        .all(|l| l.name.to_ascii_uppercase().starts_with(&format!("INBOX{delim}")))
        && listed.len() > 1;
    let inbox_prefixed = |name: String| {
        if under_inbox && !name.eq_ignore_ascii_case("INBOX") && !name.to_ascii_uppercase().starts_with(&format!("INBOX{delim}")) {
            format!("INBOX{delim}{name}")
        } else {
            name
        }
    };
    let top = match target {
        RestoreTarget::NewFolder => Some(inbox_prefixed(utf7::encode(&without_delimiter(prefix, &delim)))),
        RestoreTarget::Original => None,
    };
    let mut stats = RestoreStats {
        target: top.as_deref().map_or_else(|| crate::tr!("dossiers d'origine", "original folders"), utf7::decode),
        ..Default::default()
    };

    let mut groups: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    for item in selection.items {
        groups.entry(item.folder.clone()).or_default().push(item);
    }
    // Back where they were: a message moved since the backup is on the server in another folder,
    // and must not come back twice. Read once from the other folders (not the bin or junk, whose
    // messages are on their way out).
    let (mut elsewhere, mut elsewhere_anon) = (HashSet::new(), HashSet::new());
    if matches!(target, RestoreTarget::Original) {
        let restored: HashSet<&str> = groups.keys().filter_map(|f| selection.folders.get(f)).map(|i| i.name.as_str()).collect();
        // Gmail: All Mail holds every other folder's messages already.
        let all = listed.iter().find(|l| l.selectable && l.role == Some(crate::model::Role::All));
        let others: Vec<&imap::ListedMailbox> = match all {
            Some(all) => vec![all],
            None => listed
                .iter()
                .filter(|l| l.selectable && !matches!(l.role, Some(crate::model::Role::Trash | crate::model::Role::Junk)) && !restored.contains(l.name.as_str()))
                .collect(),
        };
        for other in &others {
            match message_ids(lease.session(), &other.name).await {
                Ok((ids, anon)) => {
                    elsewhere.extend(ids);
                    elsewhere_anon.extend(anon);
                }
                Err(e) => return Err(interrupted(e, &stats)),
            }
        }
        if !others.is_empty()
            && let Err(e) = bounded(60, &what, async { Ok(lease.session().close().await?) }).await
        {
            return Err(interrupted(e, &stats));
        }
    }
    // Sent in this run (Message-ID, size): a message saved twice, in two folders too, goes back
    // once. The size tells apart two versions of a draft sharing a Message-ID.
    let mut appended: HashSet<(String, i64)> = HashSet::new();
    // Without a Message-ID: (arrival date, size), on the server or sent in this run.
    let mut appended_anon: HashSet<(i64, i64)> = HashSet::new();
    // APPENDs refused before the continuation, one after the other.
    let mut refused_in_row = 0;
    for (folder, items) in groups {
        lease.give_way().await?;
        let session = lease.session();
        let info = &selection.folders[&folder];
        // A segment holding the target's delimiter ("v1.2" from a '/' server to a '.' one)
        // would otherwise gain a level.
        let segments: Vec<String> = match info.delimiter.as_deref() {
            Some(d) if !d.is_empty() => info.name.split(d).map(|s| without_delimiter(s, &delim)).collect(),
            _ => vec![without_delimiter(&info.name, &delim)],
        };
        // Each level is created: not every server creates the parents itself.
        let levels: Vec<String> = (1..=segments.len())
            .map(|n| {
                let path = segments[..n].join(&delim);
                match &top {
                    Some(top) => format!("{top}{delim}{path}"),
                    None => inbox_prefixed(path),
                }
            })
            .collect();
        if let Some(top) = &top
            && let Err(e) = bounded(60, &what, create(session, top)).await
        {
            return Err(interrupted(e, &stats));
        }
        for level in &levels {
            if !level.eq_ignore_ascii_case("INBOX")
                && let Err(e) = bounded(60, &what, create(session, level)).await
            {
                return Err(interrupted(e, &stats));
            }
        }
        let mailbox = levels.last().expect("at least one segment");
        // Also in a new folder: nothing to read when it was just created, and a retry into the
        // same folder (within the same minute) does not send again what already went.
        let (present, present_anon) = match message_ids(session, mailbox).await {
            Ok(found) => found,
            Err(e) => return Err(interrupted(e, &stats)),
        };
        // The target no longer selected: an EXISTS sent for it before APPEND's continuation
        // would make async-imap give up on the message. CLOSE on a read-only (EXAMINEd)
        // mailbox expunges nothing.
        if let Err(e) = bounded(60, &what, async { Ok(session.close().await?) }).await {
            return Err(interrupted(e, &stats));
        }

        let label = utf7::decode(&info.name);
        let total = items.len();
        let dir = root.join(&folder);
        for (i, item) in items.iter().enumerate() {
            // The connection may go to another between two: back, it may have a folder selected
            // (an EXISTS before APPEND's continuation fails the message), closed again. Examined
            // first: CLOSE on a folder the sync selected read-write would expunge what other
            // clients only marked deleted.
            if i > 0
                && i % CHUNK == 0
                && lease.give_way().await?
                && let Err(e) = bounded(60, &what, deselect(lease.session(), mailbox)).await
                && !matches!(e, Error::Imap(async_imap::error::Error::Bad(_) | async_imap::error::Error::No(_)))
            {
                return Err(interrupted(e, &stats));
            }
            let session = lease.session();
            let again = item.message_id.as_ref().is_some_and(|m| appended.contains(&(m.clone(), item.size)));
            let anon = anon_key(item);
            let anon_there = anon.is_some_and(|k| present_anon.contains(&k) || elsewhere_anon.contains(&k) || appended_anon.contains(&k));
            if again || anon_there || item.message_id.as_ref().is_some_and(|m| present.contains(m) || elsewhere.contains(m)) {
                stats.skipped += 1;
            } else if let Some(path) = index::find_file(&dir, &item.file) {
                let raw = match std::fs::read(path) {
                    Ok(raw) => raw,
                    Err(e) => return Err(interrupted(e.into(), &stats)),
                };
                let (date, flags) = append_meta(item.received, &item.flags);
                // Time to send the message, even a large one, over a slow connection.
                let secs = 120 + raw.len() as u64 / 50_000;
                match bounded(secs, &what, async { Ok(session.append(mailbox, flags.as_deref(), date.as_deref(), &raw).await?) }).await {
                    Ok(()) => {
                        stats.messages += 1;
                        refused_in_row = 0;
                        if let Some(m) = &item.message_id {
                            appended.insert((m.clone(), item.size));
                        }
                        if let Some(k) = anon {
                            appended_anon.insert(k);
                        }
                    }
                    // A tagged NO refuses this message only (too large, malformed): the session
                    // is still fine, the others go on. A full mailbox or a missing folder would
                    // refuse every one of them: that stops the restore.
                    Err(Error::Imap(async_imap::error::Error::No(reason))) if !refuses_all(&reason) => stats.fail(item, reason.trim().to_string()),
                    // Refused before the continuation (over APPENDLIMIT, [TOOBIG], a quota checked
                    // on the announced size): async-imap gives no reason. A NOOP that answers
                    // tells it from a cut connection. Without the reason, a run of such refusals
                    // stands for one that applies to every message (a full mailbox).
                    Err(Error::Imap(async_imap::error::Error::Append)) => {
                        if imap::still_open(session).await.is_err() {
                            return Err(interrupted(Error::Imap(async_imap::error::Error::ConnectionLost), &stats));
                        }
                        refused_in_row += 1;
                        if refused_in_row >= REFUSALS_IN_ROW {
                            return Err(interrupted(
                                Error::Other(crate::tr!(
                                    "le serveur refuse chaque message envoyé (boîte pleine ?)",
                                    "the server refuses every message sent (mailbox full?)"
                                )),
                                &stats,
                            ));
                        }
                        stats.fail(item, crate::tr!("refusé par le serveur", "refused by the server"));
                    }
                    Err(e) => return Err(interrupted(e, &stats)),
                }
            } else {
                stats.fail(item, crate::tr!("absent de la sauvegarde", "missing from the backup"));
            }
            if (i + 1) % CHUNK == 0 || i + 1 == total {
                progress(Progress { mailbox: label.clone(), done: i + 1, total });
            }
        }
        stats.folders += 1;
    }
    Ok(stats)
}

const REFUSALS_IN_ROW: usize = 10;

/// Key telling apart a saved message without a Message-ID: its arrival date and size (only when
/// the date is known, or unrelated messages of the same size would be taken for it).
fn anon_key(item: &Item) -> Option<(i64, i64)> {
    (item.message_id.is_none() && item.received > 0).then_some((item.received, item.size))
}

/// An APPEND refusal that applies to every message, not just the one sent (RFC 9051 codes).
fn refuses_all(reason: &str) -> bool {
    let upper = reason.to_ascii_uppercase();
    ["[OVERQUOTA]", "[TRYCREATE]", "[NOPERM]"].iter().any(|code| upper.contains(code))
}

/// A restore stopped partway: the error says how much already went, so the user knows a retry
/// (which skips what is already there) is worth it.
fn interrupted(e: Error, stats: &RestoreStats) -> Error {
    if stats.messages == 0 {
        return e;
    }
    let n = stats.messages;
    Error::Other(crate::tr!("restauration interrompue après {n} message(s) renvoyé(s) : {e}", "restore interrupted after {n} message(s) uploaded: {e}"))
}

/// Leaves the folder selected without expunging anything: EXAMINE (read-only) first, so CLOSE
/// never removes messages another client only marked deleted. A server refusing either (none
/// selected) is no failure.
async fn deselect(session: &mut Session, mailbox: &str) -> Result<()> {
    session.examine(mailbox).await?;
    Ok(session.close().await?)
}

/// Message-IDs (without brackets) already in `mailbox`, and (arrival date, size) of those without
/// one. All of them or an error: a set cut short (a FETCH refused with a tagged NO, which
/// async-imap ends quietly) would have the restore upload again the messages it missed.
async fn message_ids(session: &mut Session, mailbox: &str) -> Result<(HashSet<String>, HashSet<(i64, i64)>)> {
    let what = crate::tr!("restauration", "restore");
    let exists = bounded(120, &what, async { Ok(session.examine(mailbox).await?.exists) }).await?;
    let mut anon = HashSet::new();
    if exists == 0 {
        return Ok((HashSet::new(), anon));
    }
    let mut uids: Vec<u32> = bounded(300, &what, imap::uid_search_checked(session, "ALL", exists)).await?.into_iter().collect();
    uids.sort_unstable();
    // By chunks, each with its own bound: a large folder on a slow server takes the time it needs.
    let mut ids = HashSet::new();
    for chunk in uids.chunks(2000) {
        let mut answered: HashSet<u32> = HashSet::new();
        {
            let mut stream = bounded(120, &what, async { Ok(session.uid_fetch(uid_set(chunk), "(UID RFC822.SIZE INTERNALDATE ENVELOPE)").await?) }).await?;
            while let Some(f) = bounded(300, &what, async { Ok(stream.try_next().await?) }).await? {
                if let Some(uid) = f.uid {
                    answered.insert(uid);
                }
                match f.envelope().and_then(|e| e.message_id.as_ref()) {
                    Some(id) => {
                        ids.insert(String::from_utf8_lossy(id).trim_matches(['<', '>', ' ']).to_string());
                    }
                    None => {
                        if let (Some(date), Some(size)) = (f.internal_date(), f.size) {
                            anon.insert((date.timestamp(), size as i64));
                        }
                    }
                }
            }
        }
        // A short answer is fine only for messages deleted meanwhile.
        let unanswered: Vec<u32> = chunk.iter().copied().filter(|u| !answered.contains(u)).collect();
        if !unanswered.is_empty() {
            let query = format!("UID {}", uid_set(&unanswered));
            let still = bounded(120, &what, imap::uid_search_checked(session, &query, 0)).await?;
            if unanswered.iter().any(|u| still.contains(u)) {
                return Err(Error::Dropped(crate::tr!(
                    "le serveur n'a pas renvoyé tous les messages déjà présents dans le dossier",
                    "the server did not return every message already in the folder"
                )));
            }
        }
    }
    Ok((ids, anon))
}

async fn create(session: &mut Session, name: &str) -> Result<()> {
    match session.create(name).await {
        // Already there: that is what we want.
        Ok(()) | Err(async_imap::error::Error::No(_)) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn collect_folders(dir: &Path, out: &mut Vec<(PathBuf, FolderInfo)>) -> Result<()> {
    if let Ok(bytes) = std::fs::read(dir.join(FOLDER_FILE))
        && let Ok(info) = serde_json::from_slice::<FolderInfo>(&bytes)
    {
        out.push((dir.to_path_buf(), info));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        // Case-insensitive: on NTFS, APFS or exFAT "New" is the maildir's own new/.
        if entry.file_type()?.is_dir() && !name.to_str().is_some_and(is_maildir_sub) {
            collect_folders(&entry.path(), out)?;
        }
    }
    Ok(())
}

/// (uid, uidvalidity, flags) of a file name written by facteur.
fn parse_name(file: &str) -> Option<(u32, u32, &str)> {
    let (base, info) = file.split_once([':', '!']).unwrap_or((file, ""));
    let ids = base.split('.').nth(1)?.strip_prefix('U')?;
    let (uid, validity) = ids.split_once('V')?;
    Some((uid.parse().ok()?, validity.parse().ok()?, info.strip_prefix("2,").unwrap_or("")))
}

/// Base (without flags) and flag letters of a Maildir file name.
fn split_info(file: &str) -> (&str, &str) {
    match file.split_once([':', '!']) {
        Some((base, info)) => (base, info.strip_prefix("2,").unwrap_or("")),
        None => (file, ""),
    }
}

/// Arrival date (IMAP format, quoted) and flag list for APPEND.
fn append_meta(received: i64, letters: &str) -> (Option<String>, Option<String>) {
    let date = chrono::DateTime::from_timestamp(received, 0)
        .filter(|_| received > 0)
        .map(|d| format!("\"{}\"", d.format("%d-%b-%Y %H:%M:%S +0000")));
    let flags: Vec<&str> = letters
        .chars()
        .filter_map(|c| match c {
            'S' => Some("\\Seen"),
            'F' => Some("\\Flagged"),
            'R' => Some("\\Answered"),
            'D' => Some("\\Draft"),
            _ => None,
        })
        .collect();
    (date, (!flags.is_empty()).then(|| format!("({})", flags.join(" "))))
}

/// In the alphabetical order the Maildir format requires.
fn flag_letters(f: ServerFlags) -> String {
    let mut s = String::new();
    if f.draft {
        s.push('D');
    }
    if f.flagged {
        s.push('F');
    }
    if f.answered {
        s.push('R');
    }
    if f.seen {
        s.push('S');
    }
    s
}

/// Decoded segments, sanitized for every system.
fn folder_path(name: &str, delimiter: Option<&str>) -> PathBuf {
    let segments: Vec<&str> = match delimiter {
        Some(d) if !d.is_empty() => name.split(d).collect(),
        _ => vec![name],
    };
    segments.iter().map(|s| sanitize(&utf7::decode(s))).collect()
}

/// cur/, new/ or tmp/ in any case: a folder of that name would share the parent's maildir
/// directory on a disk that ignores case.
fn is_maildir_sub(name: &str) -> bool {
    ["cur", "new", "tmp"].iter().any(|r| name.eq_ignore_ascii_case(r))
}

/// Valid on Windows, macOS and Linux.
fn sanitize(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    // Windows refuses names ending with a dot or a space, and a few reserved names.
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    // Reserved even with an extension (« CON.txt »).
    let stem = out.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if out.is_empty() || is_maildir_sub(&out) || reserved {
        out.insert(0, '_');
    }
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    write_synced(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// Written and flushed to the disk (before being renamed into place): a power cut can never
/// leave a truncated file that looks like a saved message. `sync_data` flushes the content and
/// the length, which is what matters here, for less than `sync_all` (once per message).
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_data()?;
    Ok(())
}

/// Makes the renames done in a directory durable (Unix; elsewhere the rename is enough).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Bytes the backup takes on the disk: a file linked from several folders counts once.
fn dir_size(dir: &Path) -> u64 {
    fn walk(dir: &Path, seen: &mut HashSet<same_file::Handle>) -> u64 {
        let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
        entries
            .flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => walk(&e.path(), seen),
                _ => {
                    let first = linked_id(&e.path()).is_none_or(|id| seen.insert(id));
                    if first { e.metadata().map(|m| m.len()).unwrap_or(0) } else { 0 }
                }
            })
            .sum()
    }
    walk(dir, &mut HashSet::new())
}

/// The file's identity on the disk when it may have other names (hard links); None for a file
/// known to have only this one.
fn linked_id(path: &Path) -> Option<same_file::Handle> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if std::fs::metadata(path).ok()?.nlink() <= 1 {
            return None;
        }
        same_file::Handle::from_path(path).ok()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Its attributes only, not its content: no antivirus scan on open.
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .ok()?;
        same_file::Handle::from_file(file).ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// Each message saved more than once becomes one file linked from every folder, when the copies
/// are the same bytes (checked, not taken from the Message-ID). Stops at the first link the disk
/// refuses (FAT, exFAT): nothing to gain there. Returns how many files were replaced by a link.
fn link_copies(root: &Path) -> Result<usize> {
    let groups = Index::open(root)?.saved_twice()?;
    let mut linked = 0;
    for group in groups {
        let mut files = group.iter().filter_map(|(folder, file)| index::find_file(&root.join(folder), file));
        let Some(first) = files.next() else { continue };
        let Ok(first_id) = same_file::Handle::from_path(&first) else { continue };
        let mut first_bytes: Option<Vec<u8>> = None;
        for other in files {
            if same_file::Handle::from_path(&other).is_ok_and(|id| id == first_id) {
                continue;
            }
            let bytes = match &first_bytes {
                Some(b) => b,
                None => first_bytes.insert(std::fs::read(&first)?),
            };
            if std::fs::read(&other)? != *bytes {
                continue;
            }
            // Made beside it, then put in its place: the folder never misses the message.
            let (Some(dir), Some(name)) = (other.parent().and_then(Path::parent), other.file_name()) else { continue };
            std::fs::create_dir_all(dir.join("tmp"))?;
            let tmp = dir.join("tmp").join(name);
            let _ = std::fs::remove_file(&tmp);
            if std::fs::hard_link(&first, &tmp).is_err() {
                return Ok(linked);
            }
            std::fs::rename(&tmp, &other)?;
            linked += 1;
        }
    }
    Ok(linked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        let f = ServerFlags { seen: true, flagged: true, answered: false, draft: false };
        let file = format!("1792166641.U42V7.facteur!2,{}", flag_letters(f));
        assert_eq!(parse_name(&file), Some((42, 7, "FS")));
        // Both separators are read, whatever system wrote the backup.
        assert_eq!(parse_name("1.U1V2.facteur:2,S"), Some((1, 2, "S")));
        assert_eq!(parse_name("1.U1V2.facteur!2,S"), Some((1, 2, "S")));
        let (base, letters) = split_info(&file);
        assert_eq!((base, letters), ("1792166641.U42V7.facteur", "FS"));
        let (date, flags) = append_meta(1792166641, letters);
        assert_eq!(date.as_deref(), Some("\"16-Oct-2026 16:04:01 +0000\""));
        assert_eq!(flags.as_deref(), Some("(\\Flagged \\Seen)"));
    }

    #[test]
    fn restored_levels_never_hold_the_target_delimiter() {
        assert_eq!(without_delimiter("Restore 2026-10-18 09.30", "."), "Restore 2026-10-18 09-30");
        assert_eq!(without_delimiter("v1.2", "/"), "v1.2");
        assert_eq!(without_delimiter("a/b", "/"), "a-b");
        assert_eq!(without_delimiter("a.b", ""), "a.b");
    }

    #[test]
    fn folder_paths_are_portable() {
        assert_eq!(folder_path("[Gmail]/Messages envoy&AOk-s", Some("/")), PathBuf::from("[Gmail]").join("Messages envoyés"));
        assert_eq!(folder_path("INBOX.Clients.2024", Some(".")), PathBuf::from("INBOX").join("Clients").join("2024"));
        assert_eq!(sanitize("Projet: A/B?"), "Projet_ A_B_");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("notes. "), "notes");
        assert_eq!(sanitize("cur"), "_cur");
        assert_eq!(sanitize("New"), "_New");
        assert_eq!(folder_path("INBOX/Tmp", Some("/")), PathBuf::from("INBOX").join("_Tmp"));
    }

    #[test]
    fn colliding_folders_get_their_own_directory() {
        let root = tempfile::tempdir().unwrap();
        let folders = [("Projet:A", Some("/")), ("Projet?A", Some("/")), ("Autre", Some("/"))];
        let paths = unique_paths(root.path(), &folders);
        assert_eq!(paths[0], PathBuf::from("Projet_A"), "first by name keeps the plain directory");
        assert_ne!(paths[1], paths[0]);
        assert!(paths[1].to_string_lossy().starts_with("Projet_A~"));
        assert_eq!(paths[2], PathBuf::from("Autre"));
        assert_eq!(unique_paths(root.path(), &folders), paths, "the same at every run");
        // The folder already saved in the plain directory keeps it.
        std::fs::create_dir_all(root.path().join("Projet_A")).unwrap();
        let info = FolderInfo { name: "Projet?A".into(), delimiter: Some("/".into()) };
        std::fs::write(root.path().join("Projet_A").join(FOLDER_FILE), serde_json::to_vec(&info).unwrap()).unwrap();
        let paths = unique_paths(root.path(), &folders);
        assert_eq!(paths[1], PathBuf::from("Projet_A"));
        assert!(paths[0].to_string_lossy().starts_with("Projet_A~"));
        assert_eq!(fnv1a(b"a"), 0xe40c_292c, "the reference FNV-1a: the same suffix on every version");
        // Its saver deleted on the server: the twin keeps its own directory, not the saver's.
        let alone = unique_paths(root.path(), &[("Projet:A", Some("/"))]);
        assert!(alone[0].to_string_lossy().starts_with("Projet_A~"), "{:?}", alone[0]);
    }

    #[test]
    fn index_lists_a_message_saved_twice_once() {
        let root = tempfile::tempdir().unwrap();
        let idx = Index::open(root.path()).unwrap();
        idx.upsert_folder("INBOX", &FolderInfo { name: "INBOX".into(), delimiter: None }).unwrap();
        let h = |mid: &str| mime::parse_headers(format!("Message-ID: <{mid}>\r\nSubject: s\r\n\r\n").as_bytes(), 0);
        // Downloaded again after a UIDVALIDITY change; and two versions of a draft.
        idx.insert("INBOX", "10.U1V1.facteur:2,S", 10, &h("a@x"), 100).unwrap();
        idx.insert("INBOX", "10.U7V2.facteur:2,S", 10, &h("a@x"), 100).unwrap();
        idx.insert("INBOX", "20.U2V1.facteur:2,", 20, &h("d@x"), 100).unwrap();
        idx.insert("INBOX", "21.U3V1.facteur:2,", 21, &h("d@x"), 140).unwrap();
        let cat = idx.catalog(&RestoreFilter::default(), 50, 0).unwrap();
        assert_eq!((cat.matching, cat.folders[0].total), (3, 3));
        assert_eq!(idx.items(&RestoreFilter::default(), None).unwrap().len(), 3);
        // Taken over by a new UID: renamed in the index.
        assert_eq!(idx.copies("INBOX", "d@x").unwrap().len(), 2);
        idx.rebase("INBOX", "20.U2V1.facteur", "20.U9V2.facteur:2,S").unwrap();
        let bases: Vec<String> = idx.copies("INBOX", "d@x").unwrap().into_iter().map(|c| c.0).collect();
        assert!(bases.contains(&"20.U9V2.facteur".to_string()));
    }

    #[test]
    fn catalog_reads_the_disk_again_only_after_a_change() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("INBOX");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        write_atomic(&dir.join(FOLDER_FILE), &serde_json::to_vec(&FolderInfo { name: "INBOX".into(), delimiter: None }).unwrap()).unwrap();
        write_synced(&dir.join("cur").join("5.U1V1.facteur:2,S"), b"Message-ID: <a@x>\r\n\r\nx").unwrap();
        assert_eq!(catalog(root.path(), &RestoreFilter::default(), 10, 0).unwrap().matching, 1);
        let sig = signature(root.path()).unwrap();
        assert_eq!(refreshed().lock().unwrap().get(root.path()), Some(&sig));
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_synced(&dir.join("cur").join("6.U2V1.facteur:2,"), b"Message-ID: <b@x>\r\n\r\ny").unwrap();
        assert_ne!(signature(root.path()).unwrap(), sig, "a new file changes the signature");
        assert_eq!(catalog(root.path(), &RestoreFilter::default(), 10, 0).unwrap().matching, 2);
    }

    #[test]
    fn a_backup_does_not_hide_files_a_failed_run_left_unindexed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("INBOX");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        write_atomic(&dir.join(FOLDER_FILE), &serde_json::to_vec(&FolderInfo { name: "INBOX".into(), delimiter: None }).unwrap()).unwrap();
        write_synced(&dir.join("cur").join("5.U1V1.facteur:2,S"), b"Message-ID: <a@x>\r\n\r\nx").unwrap();
        assert_eq!(catalog(root.path(), &RestoreFilter::default(), 10, 0).unwrap().matching, 1);
        // A run cut after writing a file, before indexing it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_synced(&dir.join("cur").join("6.U2V1.facteur:2,"), b"Message-ID: <b@x>\r\n\r\ny").unwrap();
        // The next run succeeds.
        let before = signature(root.path()).ok();
        mark_current(root.path(), before);
        assert_eq!(catalog(root.path(), &RestoreFilter::default(), 10, 0).unwrap().matching, 2);
    }

    #[test]
    fn catalog_skips_what_is_not_a_message_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("INBOX");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        write_atomic(&dir.join(FOLDER_FILE), &serde_json::to_vec(&FolderInfo { name: "INBOX".into(), delimiter: None }).unwrap()).unwrap();
        write_synced(&dir.join("cur").join("5.U1V1.facteur:2,S"), b"Message-ID: <a@x>\r\n\r\nx").unwrap();
        // An older backup of "INBOX/New" on a case-insensitive disk, inside INBOX's own new/;
        // and what file managers leave behind.
        let nested = dir.join("new");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(nested.join(sub)).unwrap();
        }
        write_atomic(&nested.join(FOLDER_FILE), &serde_json::to_vec(&FolderInfo { name: "INBOX/New".into(), delimiter: Some("/".into()) }).unwrap()).unwrap();
        write_synced(&dir.join("cur").join(".DS_Store"), b"x").unwrap();
        write_synced(&dir.join("cur").join("Thumbs.db"), b"x").unwrap();
        let cat = catalog(root.path(), &RestoreFilter::default(), 10, 0).unwrap();
        assert_eq!((cat.folders.len(), cat.matching), (1, 1));
        assert_eq!(select(root.path(), &RestoreFilter::default(), None).unwrap().len(), 1);
    }

    #[test]
    fn index_forgets_folders_removed_from_the_disk() {
        let root = tempfile::tempdir().unwrap();
        for (name, mid) in [("INBOX", "a@x"), ("Spam", "b@x")] {
            let dir = root.path().join(name);
            for sub in ["cur", "new", "tmp"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
            write_atomic(&dir.join(FOLDER_FILE), &serde_json::to_vec(&FolderInfo { name: name.into(), delimiter: None }).unwrap()).unwrap();
            write_synced(&dir.join("cur").join("5.U1V1.facteur:2,S"), format!("Message-ID: <{mid}>\r\n\r\nx").as_bytes()).unwrap();
        }
        let cat = catalog(root.path(), &RestoreFilter::default(), 10, 0).unwrap();
        assert_eq!((cat.folders.len(), cat.matching), (2, 2));
        std::fs::remove_dir_all(root.path().join("Spam")).unwrap();
        let cat = catalog(root.path(), &RestoreFilter::default(), 10, 0).unwrap();
        assert_eq!((cat.folders.len(), cat.matching), (1, 1));
        assert_eq!(cat.folders[0].path, "INBOX");
        assert_eq!(select(root.path(), &RestoreFilter::default(), None).unwrap().len(), 1);
    }

    #[test]
    fn indexing_a_file_again_keeps_its_id() {
        let root = tempfile::tempdir().unwrap();
        let idx = Index::open(root.path()).unwrap();
        idx.upsert_folder("INBOX", &FolderInfo { name: "INBOX".into(), delimiter: None }).unwrap();
        let h = mime::parse_headers(b"Message-ID: <a@x>\r\nSubject: s\r\n\r\n", 0);
        // Indexed by a catalog read during the backup, then recorded by the backup itself.
        idx.insert("INBOX", "10.U1V1.facteur:2,", 10, &h, 100).unwrap();
        let id = idx.catalog(&RestoreFilter::default(), 10, 0).unwrap().entries[0].id;
        idx.insert("INBOX", "10.U1V1.facteur:2,S", 10, &h, 100).unwrap();
        let cat = idx.catalog(&RestoreFilter::default(), 10, 0).unwrap();
        assert_eq!(cat.entries.len(), 1);
        assert_eq!((cat.entries[0].id, cat.entries[0].flags.as_str()), (id, "S"));
    }

    #[test]
    fn only_some_append_refusals_stop_a_restore() {
        assert!(!refuses_all("[LIMIT] message too large"));
        assert!(!refuses_all("Invalid message"));
        assert!(refuses_all("[OVERQUOTA] Mailbox is full"));
        assert!(refuses_all("[TRYCREATE] no such mailbox"));
        let mut stats = RestoreStats::default();
        let item = |subject: &str| Item {
            folder: "INBOX".into(),
            file: "1.U1V1.facteur:2,".into(),
            received: 1,
            flags: String::new(),
            message_id: None,
            size: 1,
            subject: subject.into(),
        };
        for _ in 0..FAILURES_SHOWN + 2 {
            stats.fail(&item("Photos"), "too large".into());
        }
        stats.fail(&item(" "), "missing".into());
        assert_eq!((stats.failed, stats.failures.len()), (FAILURES_SHOWN + 3, FAILURES_SHOWN));
        assert_eq!(stats.failures[0], "Photos (too large)");
    }

    /// A session against a scripted server: one reply (with `{tag}` for the command's tag) per
    /// command after the login.
    async fn scripted(script: Vec<&'static str>) -> Session {
        recorded(script).await.0
    }

    /// As `scripted`, with the commands the server received.
    async fn recorded(script: Vec<&'static str>) -> (Session, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = heard.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            w.write_all(b"* OK ready\r\n").await.unwrap();
            for reply in std::iter::once("{tag} OK signed in").chain(script) {
                let Ok(Some(line)) = lines.next_line().await else { return };
                log.lock().unwrap().push(line.split_once(' ').map_or(line.clone(), |(_, c)| c.to_string()));
                let tag = line.split(' ').next().unwrap_or("*").to_string();
                let _ = w.write_all(format!("{}\r\n", reply.replace("{tag}", &tag)).as_bytes()).await;
            }
            while let Ok(Some(_)) = lines.next_line().await {}
        });
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut client = async_imap::Client::new(imap::Stream::Plain(tcp));
        client.read_response().await.unwrap();
        (client.login("u", "p").await.map_err(|(e, _)| e).unwrap(), heard)
    }

    #[tokio::test]
    async fn the_connection_given_back_is_deselected_without_expunging() {
        let examine = "* 0 EXISTS\r\n* OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-ONLY] done";
        let (mut session, heard) = recorded(vec![examine, "{tag} OK closed"]).await;
        deselect(&mut session, "Archives").await.unwrap();
        // The sync may have left INBOX selected read-write: CLOSE only once read-only.
        assert_eq!(heard.lock().unwrap()[1..], ["EXAMINE \"Archives\"".to_string(), "CLOSE".to_string()]);
    }

    #[tokio::test]
    async fn message_ids_cut_short_are_an_error_not_a_smaller_set() {
        let examine = "* 2 EXISTS\r\n* OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-ONLY] done";
        let search = "* SEARCH 3 4\r\n{tag} OK done";
        let envelope = |uid: u32, id: &str| format!("* {uid} FETCH (UID {uid} ENVELOPE (NIL NIL NIL NIL NIL NIL NIL NIL NIL \"<{id}>\"))");
        let whole: &'static str = format!("{}\r\n{}\r\n{{tag}} OK done", envelope(3, "a@x"), envelope(4, "b@x")).leak();
        let mut ok = scripted(vec![examine, search, "{tag} OK", whole]).await;
        assert_eq!(message_ids(&mut ok, "INBOX").await.unwrap().0, HashSet::from(["a@x".to_string(), "b@x".to_string()]));
        // Refused after the first message, the second one still there: no partial set.
        let cut: &'static str = format!("{}\r\n{{tag}} NO [LIMIT] throttled", envelope(3, "a@x")).leak();
        let mut refused = scripted(vec![examine, search, "{tag} OK", cut, "* SEARCH 4\r\n{tag} OK done", "{tag} OK"]).await;
        assert!(message_ids(&mut refused, "INBOX").await.is_err());
        // The second one deleted meanwhile: what came back is the whole set.
        let mut deleted = scripted(vec![examine, search, "{tag} OK", cut, "* SEARCH\r\n{tag} OK done", "{tag} OK"]).await;
        assert_eq!(message_ids(&mut deleted, "INBOX").await.unwrap().0, HashSet::from(["a@x".to_string()]));
    }

    #[tokio::test]
    async fn a_folder_the_server_refuses_to_open_does_not_stop_the_backup() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("Partage");
        let place = Place { root: root.path(), dir: &dir, rel: "Partage", sep: info_sep(root.path()) };
        let mut session = scripted(vec!["{tag} NO [NOPERM] no read right"]).await;
        let mut stats = BackupStats::default();
        let saved = backup_folder(&mut session, "Partage", Some("/"), &place, &mut stats, &mut |_| {}).await.unwrap();
        assert_eq!((saved, stats.failed), (0, 0), "no message refused: not retried every hour");
        assert_eq!(stats.unreadable_folders, ["Partage"], "named, the next folders still saved");
    }

    #[tokio::test]
    async fn a_folder_refused_for_a_while_is_a_failed_backup() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("INBOX");
        std::fs::create_dir_all(dir.join("cur")).unwrap();
        std::fs::write(dir.join("cur").join("1.U4V1.facteur!2,S"), b"Subject: x\r\n\r\ny").unwrap();
        let place = Place { root: root.path(), dir: &dir, rel: "INBOX", sep: info_sep(root.path()) };
        let mut session = scripted(vec!["{tag} NO [INUSE] Mailbox is locked"]).await;
        let mut stats = BackupStats::default();
        let saved = backup_folder(&mut session, "INBOX", Some("/"), &place, &mut stats, &mut |_| {}).await.unwrap();
        assert_eq!(saved, 1, "what the backup already holds still counts");
        assert!(stats.failed >= 1, "tried again soon, not in a day");
        assert!(stats.unreadable_folders.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_message_saved_in_another_folder_is_copied_not_downloaded() {
        let root = tempfile::tempdir().unwrap();
        let sep = info_sep(root.path());
        let (all, label) = (root.path().join("All"), root.path().join("Mairie"));
        for dir in [&all, &label] {
            for sub in ["cur", "new", "tmp"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
        }
        let raw = b"Message-ID: <a@x>\r\nSubject: Permis\r\n\r\nbody";
        let saved = format!("100.U7V1.facteur{sep}2,S");
        std::fs::write(all.join("cur").join(&saved), raw).unwrap();
        Index::open(root.path()).unwrap().insert("All", &saved, 100, &mime::parse_headers(raw, 100), raw.len() as u64).unwrap();

        // UID 3 is that message (labelled), UID 4 one of the same Message-ID but another size.
        let fetch: &'static str = format!(
            "* 1 FETCH (UID 3 FLAGS (\\Flagged) RFC822.SIZE {} INTERNALDATE \"01-Jan-1970 00:01:40 +0000\" ENVELOPE (NIL NIL NIL NIL NIL NIL NIL NIL NIL \"<a@x>\"))\r\n\
             * 2 FETCH (UID 4 FLAGS () RFC822.SIZE 9 INTERNALDATE \"01-Jan-1970 00:01:40 +0000\" ENVELOPE (NIL NIL NIL NIL NIL NIL NIL NIL NIL \"<a@x>\"))\r\n{{tag}} OK done",
            raw.len()
        )
        .leak();
        let mut session = scripted(vec![fetch]).await;
        let place = Place { root: root.path(), dir: &label, rel: "Mairie", sep };
        let mut missing = vec![3, 4];
        assert_eq!(copy_saved_elsewhere(&mut session, &place, 1, &mut missing).await.unwrap(), 1);
        assert_eq!(missing, [4], "the other one is still downloaded");
        let file = format!("100.U3V1.facteur{sep}2,F");
        assert_eq!(std::fs::read(label.join("cur").join(&file)).unwrap(), raw);
        let idx = Index::open(root.path()).unwrap();
        let copies = idx.copies("Mairie", "a@x").unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!((copies[0].1.as_str(), copies[0].2), (file.as_str(), raw.len() as i64));
    }

    #[test]
    fn copies_saved_twice_become_one_file_counted_once() {
        let root = tempfile::tempdir().unwrap();
        let sep = info_sep(root.path());
        let idx = Index::open(root.path()).unwrap();
        let save = |folder: &str, uid: u32, raw: &[u8]| {
            let cur = root.path().join(folder).join("cur");
            std::fs::create_dir_all(&cur).unwrap();
            let file = format!("100.U{uid}V1.facteur{sep}2,");
            std::fs::write(cur.join(&file), raw).unwrap();
            idx.insert(folder, &file, 100, &mime::parse_headers(raw, 100), raw.len() as u64).unwrap();
            cur.join(file)
        };
        let raw = b"Message-ID: <a@x>\r\n\r\nbody one";
        let all = save("All", 1, raw);
        let label = save("Mairie", 2, raw);
        // Same Message-ID and size, other bytes: left alone.
        let other = save("Maison", 3, b"Message-ID: <a@x>\r\n\r\nbody two");
        let before = dir_size(root.path());
        assert_eq!(link_copies(root.path()).unwrap(), 1);
        assert_eq!(same_file::is_same_file(&all, &label).unwrap(), true);
        assert_eq!(same_file::is_same_file(&all, &other).unwrap(), false);
        assert_eq!(std::fs::read(&label).unwrap(), raw);
        assert_eq!(dir_size(root.path()), before - raw.len() as u64, "the linked file counts once");
        assert_eq!(link_copies(root.path()).unwrap(), 0, "nothing left to link");
    }

    #[test]
    fn a_manifest_names_its_directory() {
        let dest = tempfile::tempdir().unwrap();
        let dir = account_dir(dest.path(), "y@example.org");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = serde_json::json!({ "email": "y@example.org", "last_backup": 1, "messages": 0, "bytes": 0, "folders": [] });
        std::fs::write(dir.join(MANIFEST_FILE), manifest.to_string()).unwrap();
        assert_eq!(read_manifest(&dir).unwrap().dir, "y@example.org");
    }

    #[test]
    fn message_files_use_a_separator_the_disk_accepts() {
        let root = tempfile::tempdir().unwrap();
        let sep = info_sep(root.path());
        assert_eq!(sep, if cfg!(windows) { '!' } else { ':' });
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0, "the probe leaves nothing behind");
    }

    #[tokio::test]
    async fn a_message_moved_since_the_backup_does_not_come_back_twice() {
        let root = tempfile::tempdir().unwrap();
        let cur = root.path().join("INBOX").join("cur");
        std::fs::create_dir_all(&cur).unwrap();
        std::fs::write(cur.join("100.U1V1.facteur:2,"), b"Message-ID: <m@x>\r\n\r\nx").unwrap();
        let item = Item { folder: "INBOX".into(), file: "100.U1V1.facteur:2,".into(), received: 100, flags: String::new(), message_id: Some("m@x".into()), size: 22, subject: String::new() };
        let folders = HashMap::from([("INBOX".to_string(), FolderInfo { name: "INBOX".into(), delimiter: Some("/".into()) })]);
        let mut session = scripted(vec![
            "* LIST () \"/\" INBOX\r\n* LIST () \"/\" Archive\r\n{tag} OK done",
            "{tag} OK",
            // Archive, where it was moved meanwhile.
            "* 1 EXISTS\r\n* OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-ONLY] done",
            "* SEARCH 9\r\n{tag} OK done",
            "{tag} OK",
            "* 1 FETCH (UID 9 RFC822.SIZE 22 INTERNALDATE \"01-Jan-1970 00:01:40 +0000\" ENVELOPE (NIL NIL NIL NIL NIL NIL NIL NIL NIL \"<m@x>\"))\r\n{tag} OK done",
            "{tag} OK closed",
            // INBOX, empty.
            "* 0 EXISTS\r\n* OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-ONLY] done",
            "{tag} OK closed",
        ])
        .await;
        let stats = restore(&mut session, root.path(), Selection { items: vec![item], folders }, RestoreTarget::Original, "", |_| {}).await.unwrap();
        assert_eq!((stats.messages, stats.skipped), (0, 1), "on the server already, in Archive");
    }

    #[tokio::test]
    async fn a_restore_skips_what_is_there_and_goes_past_messages_refused_upfront() {
        let root = tempfile::tempdir().unwrap();
        let cur = root.path().join("INBOX").join("cur");
        std::fs::create_dir_all(&cur).unwrap();
        let item = |file: &str, received: i64, message_id: Option<&str>, raw: &[u8]| {
            std::fs::write(cur.join(file), raw).unwrap();
            Item {
                folder: "INBOX".into(),
                file: file.into(),
                received,
                flags: String::new(),
                message_id: message_id.map(str::to_string),
                size: raw.len() as i64,
                subject: String::new(),
            }
        };
        let items = vec![
            // No Message-ID, already on the server (same arrival date and size).
            item("100.U1V1.facteur:2,", 100, None, b"aaaaa"),
            // Both refused before the continuation (over APPENDLIMIT).
            item("200.U2V1.facteur:2,", 200, Some("b@x"), b"bbbbbb"),
            item("300.U3V1.facteur:2,", 300, None, b"ccccccc"),
        ];
        let folders = HashMap::from([("INBOX".to_string(), FolderInfo { name: "INBOX".into(), delimiter: Some("/".into()) })]);
        let fetch = "* 1 FETCH (UID 9 RFC822.SIZE 5 INTERNALDATE \"01-Jan-1970 00:01:40 +0000\" ENVELOPE (NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL))\r\n{tag} OK done";
        let mut session = scripted(vec![
            "* LIST () \"/\" INBOX\r\n{tag} OK done",
            "{tag} OK",
            "* 1 EXISTS\r\n* OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-ONLY] done",
            "* SEARCH 9\r\n{tag} OK done",
            "{tag} OK",
            fetch,
            "{tag} OK closed",
            "{tag} NO [TOOBIG] too big",
            "{tag} OK",
            "{tag} NO [TOOBIG] too big",
            "{tag} OK",
        ])
        .await;
        let selection = Selection { items, folders };
        let stats = restore(&mut session, root.path(), selection, RestoreTarget::Original, "", |_| {}).await.unwrap();
        assert_eq!((stats.messages, stats.skipped, stats.failed), (0, 1, 2));
        let undated = Item { folder: "INBOX".into(), file: "0.U4V1.facteur:2,".into(), received: 0, flags: String::new(), message_id: None, size: 5, subject: String::new() };
        assert_eq!(anon_key(&undated), None, "an unknown date never matches");
    }
}
