//! SQLite index of an account backup, to filter and preview tens of thousands of messages
//! without reading the files. Rebuilt on the fly for what it lacks (older backups, files copied
//! by hand).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};

use super::{FolderInfo, collect_folders, split_info};
use crate::error::{Error, Result};
use crate::mime::{self, HeaderInfo};
use crate::model::Address;

const INDEX_FILE: &str = "facteur-index.sqlite";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (
    path      TEXT PRIMARY KEY,
    name      TEXT NOT NULL,
    delimiter TEXT
);
CREATE TABLE IF NOT EXISTS entries (
    id         INTEGER PRIMARY KEY,
    folder     TEXT NOT NULL,
    base       TEXT NOT NULL,
    file       TEXT NOT NULL,
    received   INTEGER NOT NULL,
    date       INTEGER NOT NULL,
    from_name  TEXT,
    from_addr  TEXT,
    subject    TEXT NOT NULL DEFAULT '',
    message_id TEXT,
    size       INTEGER NOT NULL DEFAULT 0,
    flags      TEXT NOT NULL DEFAULT '',
    UNIQUE (folder, base)
);
CREATE INDEX IF NOT EXISTS entries_received ON entries (received);
CREATE INDEX IF NOT EXISTS entries_msgid ON entries (folder, message_id);
CREATE INDEX IF NOT EXISTS entries_msgid_any ON entries (message_id, size);
";

/// One entry per message and folder: the same message saved twice in a folder (downloaded
/// again after a UIDVALIDITY change, by an older version) is listed and restored once. Same
/// Message-ID and same size: two versions of a draft sharing an identifier both stay.
const UNIQUE: &str = "(e.message_id IS NULL OR e.id = (SELECT MIN(x.id) FROM entries x
                      WHERE x.folder = e.folder AND x.message_id = e.message_id AND x.size = e.size))";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestoreFilter {
    /// Folder paths in the backup; empty means all.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Inclusive, Unix seconds.
    #[serde(default)]
    pub since: Option<i64>,
    /// Exclusive, Unix seconds.
    #[serde(default)]
    pub until: Option<i64>,
    /// Matched against the subject and the sender.
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub flagged_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogFolder {
    pub path: String,
    /// Original IMAP name, decoded.
    pub name: String,
    pub total: i64,
    /// Messages matching the date, text and flag criteria (no folder filter).
    pub matching: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub id: i64,
    pub folder: String,
    pub received: i64,
    pub date: i64,
    pub from: Option<Address>,
    pub subject: String,
    pub size: i64,
    pub flags: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub folders: Vec<CatalogFolder>,
    /// Messages matching every criterion, folders included.
    pub matching: i64,
    pub matching_bytes: i64,
    pub entries: Vec<CatalogEntry>,
    /// Arrival date bounds of the whole backup.
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

#[derive(Debug, Clone)]
pub(super) struct Item {
    pub folder: String,
    pub file: String,
    pub received: i64,
    pub flags: String,
    pub message_id: Option<String>,
    pub size: i64,
    pub subject: String,
}

pub(super) struct Index {
    conn: Connection,
}

impl Index {
    pub fn open(root: &Path) -> Result<Self> {
        let _other = crate::error::OtherDb::enter();
        std::fs::create_dir_all(root)?;
        let conn = Connection::open(root.join(INDEX_FILE))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        // A backup and a catalog read of the same backup may write at once: wait for the other
        // writer rather than fail with "database is locked".
        conn.busy_timeout(std::time::Duration::from_secs(30))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Index { conn })
    }

    /// One transaction per batch: one disk write instead of one per message.
    pub fn begin(&self) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    pub fn upsert_folder(&self, path: &str, info: &FolderInfo) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        self.conn.execute(
            "INSERT INTO folders (path, name, delimiter) VALUES (?1, ?2, ?3)
             ON CONFLICT (path) DO UPDATE SET name = excluded.name, delimiter = excluded.delimiter",
            params![path, info.name, info.delimiter],
        )?;
        Ok(())
    }

    pub fn insert(&self, folder: &str, file: &str, received: i64, h: &HeaderInfo, size: u64) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        let (base, flags) = split_info(file);
        let (from_name, from_addr) = match &h.from {
            Some(a) => (a.name.clone(), Some(a.email.clone())),
            None => (None, None),
        };
        // Updated in place, not replaced: an entry indexed by a catalog read during a backup keeps
        // the id the restore page already shows when the backup records it in turn.
        self.conn.execute(
            "INSERT INTO entries
                (folder, base, file, received, date, from_name, from_addr, subject, message_id, size, flags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (folder, base) DO UPDATE SET
                file = excluded.file, received = excluded.received, date = excluded.date,
                from_name = excluded.from_name, from_addr = excluded.from_addr, subject = excluded.subject,
                message_id = excluded.message_id, size = excluded.size, flags = excluded.flags",
            params![folder, base, file, received, h.date, from_name, from_addr, h.subject, h.message_id, size as i64, flags],
        )?;
        Ok(())
    }

    /// The file was renamed (flags changed).
    pub fn rename(&self, folder: &str, file: &str) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        let (base, flags) = split_info(file);
        self.conn.execute(
            "UPDATE entries SET file = ?3, flags = ?4 WHERE folder = ?1 AND base = ?2",
            params![folder, base, file, flags],
        )?;
        Ok(())
    }

    /// Entries of a folder with this Message-ID: (base, file, size, received).
    pub fn copies(&self, folder: &str, message_id: &str) -> Result<Vec<(String, String, i64, i64)>> {
        let _other = crate::error::OtherDb::enter();
        let mut st = self.conn.prepare_cached("SELECT base, file, size, received FROM entries WHERE folder = ?1 AND message_id = ?2")?;
        let rows = st.query_map(params![folder, message_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// A file of another folder holding this message (same Message-ID, same size): (folder, file).
    /// Same Message-ID, size and arrival date (kept by a server's COPY and MOVE): two different
    /// messages are not taken for one on a reused Message-ID.
    pub fn elsewhere(&self, folder: &str, message_id: &str, size: i64, received: i64) -> Result<Option<(String, String)>> {
        let _other = crate::error::OtherDb::enter();
        let mut st = self.conn.prepare_cached(
            "SELECT folder, file FROM entries WHERE message_id = ?2 AND size = ?3 AND received = ?4 AND folder <> ?1 ORDER BY id LIMIT 1",
        )?;
        Ok(st.query_row(params![folder, message_id, size, received], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    }

    /// `file` of `folder` is a copy of `source`: its headers are not read again.
    pub fn insert_copy(&self, source_folder: &str, source: &str, folder: &str, file: &str, received: i64) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        let (base, flags) = split_info(file);
        self.conn.execute(
            "INSERT INTO entries
                (folder, base, file, received, date, from_name, from_addr, subject, message_id, size, flags)
             SELECT ?3, ?4, ?5, ?6, date, from_name, from_addr, subject, message_id, size, ?7
             FROM entries WHERE folder = ?1 AND file = ?2
             ON CONFLICT (folder, base) DO UPDATE SET
                file = excluded.file, received = excluded.received, date = excluded.date,
                from_name = excluded.from_name, from_addr = excluded.from_addr, subject = excluded.subject,
                message_id = excluded.message_id, size = excluded.size, flags = excluded.flags",
            params![source_folder, source, folder, base, file, received, flags],
        )?;
        Ok(())
    }

    /// Messages saved more than once (same Message-ID, same size: a Gmail label, an older backup
    /// that downloaded each copy): their files, (folder, file), by message.
    pub fn saved_twice(&self) -> Result<Vec<Vec<(String, String)>>> {
        let _other = crate::error::OtherDb::enter();
        let mut st = self.conn.prepare(
            "SELECT message_id, size, folder, file FROM entries
             WHERE message_id IS NOT NULL AND (message_id, size) IN
                (SELECT message_id, size FROM entries WHERE message_id IS NOT NULL GROUP BY message_id, size HAVING count(*) > 1)
             ORDER BY message_id, size, id",
        )?;
        let mut groups: Vec<Vec<(String, String)>> = Vec::new();
        let mut last: Option<(String, i64)> = None;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
        for row in rows {
            let (id, size, folder, file) = row?;
            let key = Some((id, size));
            if key != last || groups.is_empty() {
                groups.push(Vec::new());
                last = key;
            }
            groups.last_mut().expect("pushed above").push((folder, file));
        }
        Ok(groups)
    }

    /// The file of an entry was renamed to a new base (taken over by a new UID).
    pub fn rebase(&self, folder: &str, old_base: &str, file: &str) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        let (base, flags) = split_info(file);
        self.conn.execute(
            "UPDATE entries SET base = ?3, file = ?4, flags = ?5 WHERE folder = ?1 AND base = ?2",
            params![folder, old_base, base, file, flags],
        )?;
        Ok(())
    }

    pub fn refresh(&self, root: &Path) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        let mut folders = Vec::new();
        collect_folders(root, &mut folders)?;
        let kept: HashSet<String> = folders.iter().map(|(dir, _)| rel_path(root, dir)).collect();
        for (dir, info) in folders {
            self.refresh_folder(root, &dir, &info)?;
        }
        // Folder directories removed from the disk: forgotten too, or the catalog would list
        // them and a restore would stop on their first missing file.
        let known: Vec<String> = {
            let mut st = self.conn.prepare("SELECT path FROM folders UNION SELECT DISTINCT folder FROM entries")?;
            st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        let gone: Vec<&String> = known.iter().filter(|p| !kept.contains(*p)).collect();
        if !gone.is_empty() {
            let tx = self.conn.unchecked_transaction()?;
            for path in gone {
                self.conn.execute("DELETE FROM entries WHERE folder = ?1", [path])?;
                self.conn.execute("DELETE FROM folders WHERE path = ?1", [path])?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    pub fn refresh_folder(&self, root: &Path, dir: &Path, info: &FolderInfo) -> Result<()> {
        let _other = crate::error::OtherDb::enter();
        let rel = rel_path(root, dir);
        let tx = self.conn.unchecked_transaction()?;
        self.upsert_folder(&rel, info)?;
        let known: HashMap<String, String> = {
            let mut st = self.conn.prepare("SELECT base, file FROM entries WHERE folder = ?1")?;
            st.query_map([&rel], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        let mut seen = Vec::new();
        for sub in ["cur", "new"] {
            let Ok(entries) = std::fs::read_dir(dir.join(sub)) else { continue };
            for entry in entries {
                let entry = entry?;
                let file = entry.file_name().to_string_lossy().into_owned();
                if !is_message_file(&entry, &file) {
                    continue;
                }
                let (base, _) = split_info(&file);
                seen.push(base.to_string());
                match known.get(base) {
                    Some(f) if f == &file => {}
                    Some(_) => self.rename(&rel, &file)?,
                    None => {
                        let raw = std::fs::read(entry.path())?;
                        let received = file.split('.').next().and_then(|t| t.parse().ok()).unwrap_or(0);
                        let h = mime::parse_headers(&raw, received);
                        self.insert(&rel, &file, if received > 0 { received } else { h.date }, &h, raw.len() as u64)?;
                    }
                }
            }
        }
        // Files gone from the disk (deleted by hand): forget them.
        if seen.len() < known.len() {
            let seen: HashSet<&str> = seen.iter().map(String::as_str).collect();
            for base in known.keys().filter(|b| !seen.contains(b.as_str())) {
                self.conn.execute("DELETE FROM entries WHERE folder = ?1 AND base = ?2", params![rel, base])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn catalog(&self, filter: &RestoreFilter, limit: usize, offset: usize) -> Result<Catalog> {
        let _other = crate::error::OtherDb::enter();
        // Per-folder counts: date/text/flag criteria without the folder filter.
        let (cond, args) = conditions(filter, false);
        let mut st = self.conn.prepare(&format!(
            "SELECT f.path, f.name,
                    (SELECT COUNT(*) FROM entries e WHERE e.folder = f.path AND {UNIQUE}),
                    (SELECT COUNT(*) FROM entries e WHERE e.folder = f.path AND {cond})
             FROM folders f ORDER BY f.path"
        ))?;
        let folders = st
            .query_map(params_from_iter(args.iter()), |r| {
                Ok(CatalogFolder {
                    path: r.get(0)?,
                    name: crate::utf7::decode(&r.get::<_, String>(1)?),
                    total: r.get(2)?,
                    matching: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;

        let (cond, args) = conditions(filter, true);
        let (matching, matching_bytes): (i64, i64) = self.conn.query_row(
            &format!("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM entries e WHERE {cond}"),
            params_from_iter(args.iter()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut page_args = args.clone();
        page_args.push(Value::Integer(limit as i64));
        page_args.push(Value::Integer(offset as i64));
        let mut st = self.conn.prepare(&format!(
            "SELECT id, folder, received, date, from_name, from_addr, subject, size, flags
             FROM entries e WHERE {cond} ORDER BY received DESC, id DESC LIMIT ? OFFSET ?"
        ))?;
        let entries = st
            .query_map(params_from_iter(page_args.iter()), |r| {
                let addr: Option<String> = r.get(5)?;
                Ok(CatalogEntry {
                    id: r.get(0)?,
                    folder: r.get(1)?,
                    received: r.get(2)?,
                    date: r.get(3)?,
                    from: addr.map(|email| Address { name: r.get(4).ok().flatten(), email }),
                    subject: r.get(6)?,
                    size: r.get(7)?,
                    flags: r.get(8)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let (oldest, newest) = self.conn.query_row("SELECT MIN(received), MAX(received) FROM entries", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(Catalog { folders, matching, matching_bytes, entries, oldest, newest })
    }

    /// The explicit selection when given, otherwise everything the filter matches.
    pub fn items(&self, filter: &RestoreFilter, ids: Option<&[i64]>) -> Result<Vec<Item>> {
        let _other = crate::error::OtherDb::enter();
        let map = |r: &rusqlite::Row| {
            Ok(Item {
                folder: r.get(0)?,
                file: r.get(1)?,
                received: r.get(2)?,
                flags: r.get(3)?,
                message_id: r.get(4)?,
                size: r.get(5)?,
                subject: r.get(6)?,
            })
        };
        let cols = "folder, file, received, flags, message_id, size, subject";
        let rows = match ids {
            Some(ids) => {
                let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
                let mut st = self.conn.prepare(&format!("SELECT {cols} FROM entries WHERE id IN ({list}) ORDER BY folder, received"))?;
                st.query_map([], map)?.collect::<Result<_, _>>()?
            }
            None => {
                let (cond, args) = conditions(filter, true);
                let mut st = self.conn.prepare(&format!("SELECT {cols} FROM entries e WHERE {cond} ORDER BY folder, received"))?;
                st.query_map(params_from_iter(args.iter()), map)?.collect::<Result<_, _>>()?
            }
        };
        Ok(rows)
    }

    pub fn folder(&self, path: &str) -> Result<FolderInfo> {
        let _other = crate::error::OtherDb::enter();
        self.conn
            .query_row("SELECT name, delimiter FROM folders WHERE path = ?1", [path], |r| {
                Ok(FolderInfo { name: r.get(0)?, delimiter: r.get(1)? })
            })
            .optional()?
            .ok_or_else(|| Error::NotFound(crate::tr!("dossier « {path} » absent de la sauvegarde", "folder “{path}” missing from the backup")))
    }

    pub fn location(&self, id: i64) -> Result<(String, String)> {
        let _other = crate::error::OtherDb::enter();
        self.conn
            .query_row("SELECT folder, file FROM entries WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or_else(|| Error::NotFound(crate::tr!("message {id} absent de la sauvegarde", "message {id} missing from the backup")))
    }
}

fn conditions(filter: &RestoreFilter, with_folders: bool) -> (String, Vec<Value>) {
    let mut cond = vec![UNIQUE.to_string()];
    let mut args = Vec::new();
    if with_folders && !filter.folders.is_empty() {
        cond.push(format!("e.folder IN ({})", vec!["?"; filter.folders.len()].join(",")));
        args.extend(filter.folders.iter().map(|f| Value::Text(f.clone())));
    }
    if let Some(since) = filter.since {
        cond.push("e.received >= ?".into());
        args.push(Value::Integer(since));
    }
    if let Some(until) = filter.until {
        cond.push("e.received < ?".into());
        args.push(Value::Integer(until));
    }
    if let Some(q) = filter.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        cond.push("(e.subject LIKE ? ESCAPE '\\' OR e.from_name LIKE ? ESCAPE '\\' OR e.from_addr LIKE ? ESCAPE '\\')".into());
        args.extend(std::iter::repeat_n(Value::Text(like), 3));
    }
    if filter.flagged_only {
        cond.push("instr(e.flags, 'F') > 0".into());
    }
    (cond.join(" AND "), args)
}

/// Always with `/`.
pub(super) fn rel_path(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// An entry of cur/ or new/ that can be a message: a regular file, neither hidden nor one of the
/// files a file manager leaves behind (.DS_Store, Thumbs.db, desktop.ini). Older backups of a
/// folder named New/Cur/Tmp on a case-insensitive disk also hold that folder's own directories
/// and .facteur-dossier.json in the parent's new/.
pub(super) fn is_message_file(entry: &std::fs::DirEntry, name: &str) -> bool {
    entry.file_type().is_ok_and(|t| t.is_file() || (t.is_symlink() && entry.path().is_file()))
        && !name.starts_with('.')
        && !["Thumbs.db", "desktop.ini"].iter().any(|junk| name.eq_ignore_ascii_case(junk))
}

/// Finds a file even if its flags (so its name) changed since it was indexed.
pub(super) fn find_file(dir: &Path, file: &str) -> Option<std::path::PathBuf> {
    // The indexed name is nearly always right: no need to list the directory.
    if let Some(exact) = ["cur", "new"].iter().map(|sub| dir.join(sub).join(file)).find(|p| p.is_file()) {
        return Some(exact);
    }
    // Flags changed since (another client, an older version): same base, other suffix.
    let (base, _) = split_info(file);
    ["cur", "new"].iter().find_map(|sub| {
        std::fs::read_dir(dir.join(sub)).ok()?.flatten().find_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (split_info(&name).0 == base).then(|| e.path())
        })
    })
}
