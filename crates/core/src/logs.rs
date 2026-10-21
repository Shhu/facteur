//! Reads back the end of the newest app log (tracing's default format) for the diagnostics panel.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::Result;

pub const MAX_LINES: usize = 5000;
const MAX_SCAN: u64 = 8 << 20;
const CHUNK: u64 = 64 << 10;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogLine {
    /// Unix ms.
    pub ts: i64,
    pub level: String,
    pub target: String,
    /// Continuation lines (a message with newlines) joined with `\n`.
    pub message: String,
    pub account: Option<i64>,
}

/// Log file names sort by date.
pub fn newest_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("facteur.") && name.ends_with(".log")
        })
        .map(|e| e.path())
        .max()
}

/// `levels`: comma-separated, none means all; `query` matches the target or the message,
/// ignoring case. Oldest first.
pub fn read_tail(dir: &Path, lines: usize, levels: Option<&str>, query: Option<&str>) -> Result<Vec<LogLine>> {
    let Some(path) = newest_file(dir) else { return Ok(Vec::new()) };
    let mut file = std::fs::File::open(path)?;
    let levels: Vec<String> = levels.unwrap_or("").split(',').map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty()).collect();
    let query = query.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
    let wanted = |line: &LogLine| {
        (levels.is_empty() || levels.contains(&line.level))
            && query.as_ref().is_none_or(|q| line.message.to_lowercase().contains(q) || line.target.to_lowercase().contains(q))
    };
    let limit = lines.clamp(1, MAX_LINES);
    let mut tail = Tail::default();
    let mut pos = file.seek(SeekFrom::End(0))?;
    // The start of the earliest line read so far, maybe partial (completed by the next chunk).
    let mut carry: Vec<u8> = Vec::new();
    let mut scanned = 0;
    while pos > 0 && tail.found.len() < limit && scanned < MAX_SCAN {
        let size = CHUNK.min(pos);
        pos -= size;
        scanned += size;
        file.seek(SeekFrom::Start(pos))?;
        let mut chunk = vec![0; size as usize];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&carry);
        let complete = if pos == 0 {
            carry.clear();
            &chunk[..]
        } else {
            match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    carry = chunk[..i].to_vec();
                    &chunk[i + 1..]
                }
                None => {
                    carry = chunk;
                    continue;
                }
            }
        };
        for line in complete.rsplit(|&b| b == b'\n') {
            tail.line(line, &wanted);
            if tail.found.len() >= limit {
                break;
            }
        }
    }
    let mut found = tail.found;
    found.truncate(limit);
    found.reverse();
    Ok(found)
}

/// Lines taken from the end: continuation lines wait for their entry's first line, above them.
#[derive(Default)]
struct Tail {
    /// Newest first.
    found: Vec<LogLine>,
    /// Continuation lines below the next entry up, bottom first.
    pending: Vec<String>,
}

impl Tail {
    fn line(&mut self, raw: &[u8], wanted: &impl Fn(&LogLine) -> bool) {
        let text = String::from_utf8_lossy(raw);
        let text = text.trim_end_matches('\r');
        if text.trim().is_empty() {
            return;
        }
        match parse_line(text) {
            Some(mut entry) => {
                if !self.pending.is_empty() {
                    for more in self.pending.drain(..).rev() {
                        entry.message.push('\n');
                        entry.message.push_str(&more);
                    }
                    entry.account = entry.account.or_else(|| account_of(&entry.message));
                }
                if wanted(&entry) {
                    self.found.push(entry);
                }
            }
            None => self.pending.push(text.to_string()),
        }
    }
}

/// `2026-10-17T19:18:34.123456Z  INFO facteur_core::engine: message key=value` (tracing's
/// default format, without colours). `None`: not the first line of an entry.
fn parse_line(line: &str) -> Option<LogLine> {
    let (ts, rest) = line.split_once(' ')?;
    let ts = chrono::DateTime::parse_from_rfc3339(ts).ok()?.timestamp_millis();
    let rest = rest.trim_start();
    let (level, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    if !matches!(level, "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE") {
        return None;
    }
    // Spans (`name{fields}:`) would come before the target; the target has no spaces.
    let (target, message) = match rest.split_once(": ") {
        Some((target, message)) if !target.contains(' ') => (target, message),
        _ => ("", rest),
    };
    Some(LogLine { ts, level: level.to_ascii_lowercase(), target: target.to_string(), message: message.to_string(), account: account_of(message) })
}

fn account_of(message: &str) -> Option<i64> {
    for marker in ["account_id=", "account=", "account ", "compte ", "account_id: "] {
        let mut from = 0;
        while let Some(i) = message[from..].find(marker) {
            let at = from + i;
            from = at + marker.len();
            // A whole word: `subaccount 3` is not one.
            if message[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let digits: String = message[from..].chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() {
                return digits.parse().ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_parsed_from_tracing_s_format() {
        let line = parse_line("2026-10-17T19:18:34.123456Z  WARN facteur_core::engine: synchro du compte 3 : timeout").unwrap();
        assert_eq!(line.level, "warn");
        assert_eq!(line.target, "facteur_core::engine");
        assert_eq!(line.message, "synchro du compte 3 : timeout");
        assert_eq!(line.account, Some(3));
        assert_eq!(line.ts, 1_792_264_714_123);
        let fields = parse_line("2026-10-17T19:18:34.1Z  INFO facteur_app_lib: engine events lagged: asking the UI to resync missed=4").unwrap();
        assert_eq!(fields.target, "facteur_app_lib");
        assert_eq!(fields.message, "engine events lagged: asking the UI to resync missed=4");
        assert!(parse_line("  at src/engine.rs:12").is_none());
        assert!(parse_line("2026-10-17T19:18:34Z NOTALEVEL x: y").is_none());
    }

    #[test]
    fn account_ids_are_found_as_whole_words() {
        assert_eq!(account_of("sync after reconnecting (account 12): timeout"), Some(12));
        assert_eq!(account_of("lagged account_id=7 missed=2"), Some(7));
        assert_eq!(account_of("subaccount 4 and account x"), None);
        assert_eq!(account_of("no account here"), None);
    }

    fn write_log(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn the_tail_of_the_newest_file_is_read_and_filtered() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), "facteur.2026-10-16.log", "2026-10-16T10:00:00Z  INFO a: old\n");
        let mut text = String::new();
        for i in 0..2000 {
            let level = if i % 10 == 0 { "ERROR" } else { " INFO" };
            text.push_str(&format!("2026-10-17T10:00:00.{i:06}Z {level} facteur_core::sync: line {i}\n"));
        }
        text.push_str("2026-10-17T11:00:00Z ERROR facteur_app_lib: panic: boom\n  at engine.rs:1\n  at sync.rs:2\n");
        write_log(dir.path(), "facteur.2026-10-17.log", &text);

        let last = read_tail(dir.path(), 3, None, None).unwrap();
        assert_eq!(last.len(), 3);
        assert_eq!(last[0].message, "line 1998");
        assert_eq!(last[2].message, "panic: boom\n  at engine.rs:1\n  at sync.rs:2");

        let errors = read_tail(dir.path(), 5000, Some("error"), None).unwrap();
        assert_eq!(errors.len(), 201);
        assert!(errors.iter().all(|l| l.level == "error"));
        assert_eq!(errors[0].message, "line 0");

        let found = read_tail(dir.path(), 10, Some("info,error"), Some("LINE 1999")).unwrap();
        assert_eq!(found.iter().map(|l| l.message.as_str()).collect::<Vec<_>>(), ["line 1999"]);
        assert!(read_tail(&dir.path().join("none"), 10, None, None).unwrap().is_empty());
    }
}
