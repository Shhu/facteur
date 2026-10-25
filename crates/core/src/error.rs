use serde::Serialize;

#[derive(Debug)]
pub enum Error {
    Db(rusqlite::Error),
    Imap(async_imap::error::Error),
    Io(std::io::Error),
    Tls(String),
    Smtp(String),
    Mime(String),
    Secret(String),
    NotFound(String),
    Other(String),
    Auth(String),
    /// Sending was cut off midway: the message may have gone out.
    Uncertain(String),
    /// The SMTP server refused the message for good (5xx reply, or a message it cannot take):
    /// sending it again unchanged would fail the same way, so it is not retried on its own.
    Rejected(String),
    /// The folder was recreated on the server: its UIDs no longer name the same messages.
    Stale(String),
    /// The server closed the session or refused a command on it while the account itself is fine
    /// (too many connections, a session cut on its side): a new connection is worth one more try.
    Dropped(String),
    /// The server refused the sign-in for a passing reason (busy, mailbox briefly out of reach):
    /// the credentials themselves are not in question, at least for a while.
    Refused(String),
    /// The server could not be reached at all (no network, name not found, nothing answering):
    /// nothing was sent, and it is only a matter of waiting.
    Unreachable(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A silent server or a half-dead connection (sleep, Wi-Fi change) must not block forever.
pub(crate) async fn bounded<T>(secs: u64, what: &str, job: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(secs), job)
        .await
        .map_err(|_| Error::Other(crate::tr!("{what} : le serveur ne répond plus", "{what}: the server stopped responding")))?
}

// Hand-written rather than thiserror: the prefixes follow the chosen language.
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Category prefix as (French, English); `None` for a message that is already complete. A
        // text written in both languages (`i18n::both`) shows the language of the moment.
        let text = |e: &String| crate::i18n::pick(e).to_string();
        let (prefix, detail): (Option<(&str, &str)>, String) = match self {
            Error::Db(e) if let Some(sentence) = db_sentence(e) => return f.write_str(&sentence),
            Error::Db(e) => (Some(("base de données", "database")), e.to_string()),
            Error::Imap(e) => (Some(("IMAP", "IMAP")), e.to_string()),
            Error::Io(e) if e.kind() == std::io::ErrorKind::StorageFull => {
                return f.write_str(&crate::tr!("disque plein : libérez de la place puis réessayez", "disk full: free some space, then try again"));
            }
            Error::Io(e) if is_local(e) => (Some(("fichier", "file")), e.to_string()),
            Error::Io(e) => (Some(("réseau", "network")), e.to_string()),
            Error::Tls(e) => (Some(("TLS", "TLS")), text(e)),
            Error::Smtp(e) => (Some(("SMTP", "SMTP")), text(e)),
            Error::Rejected(e) => (Some(("refusé par le serveur d'envoi", "refused by the sending server")), text(e)),
            Error::Mime(e) => (Some(("message invalide", "invalid message")), text(e)),
            Error::Secret(e) => (Some(("trousseau", "keychain")), text(e)),
            Error::NotFound(e) => (Some(("introuvable", "not found")), text(e)),
            Error::Other(e) | Error::Auth(e) | Error::Refused(e) | Error::Unreachable(e) => (None, text(e)),
            Error::Stale(e) => (Some(("opération périmée", "stale operation")), text(e)),
            Error::Dropped(e) => (Some(("connexion coupée par le serveur", "connection dropped by the server")), text(e)),
            // What to check before sending again depends on the provider: added by the outbox
            // (see `smtp::maybe_sent_advice`).
            Error::Uncertain(e) => (Some(("envoi incertain", "uncertain send")), text(e)),
        };
        match prefix {
            None => write!(f, "{detail}"),
            Some((_, en)) if crate::i18n::english() => write!(f, "{en}: {detail}"),
            Some((fr, _)) => write!(f, "{fr} : {detail}"),
        }
    }
}

impl Error {
    pub fn is_damaged_db(&self) -> bool {
        use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
        matches!(self, Error::Db(rusqlite::Error::SqliteFailure(e, _)) if matches!(e.code, DatabaseCorrupt | NotADatabase))
    }

    pub fn is_connection_drop(&self) -> bool {
        use async_imap::error::Error as Imap;
        match self {
            Error::Dropped(_) | Error::Imap(Imap::ConnectionLost) => true,
            Error::Imap(Imap::Io(e)) => !is_parse_failure(e),
            Error::Io(e) => !is_local(e) && !is_parse_failure(e),
            _ => false,
        }
    }
}

/// async-imap 0.12 reports a response it cannot parse as this I/O error, whether it reaches us
/// wrapped (`Imap(Io)`) or straight from `read_response` (`Io`).
pub(crate) fn is_parse_failure(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::Other && e.to_string().contains("during parsing")
}

fn is_local(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        NotFound | PermissionDenied | AlreadyExists | ReadOnlyFilesystem | IsADirectory | NotADirectory | DirectoryNotEmpty
            | FileTooLarge | QuotaExceeded | InvalidFilename | CrossesDevices | ResourceBusy | ExecutableFileBusy
    )
}

fn db_sentence(e: &rusqlite::Error) -> Option<String> {
    use rusqlite::ErrorCode::*;
    let rusqlite::Error::SqliteFailure(failure, _) = e else { return None };
    Some(match failure.code {
        DiskFull => crate::tr!(
            "disque plein : facteur ne peut plus écrire dans sa base, libérez de la place",
            "disk full: facteur can no longer write to its database, free some space"
        ),
        DatabaseBusy | DatabaseLocked => crate::tr!(
            "base de données occupée : réessayez dans un instant",
            "database busy: try again in a moment"
        ),
        DatabaseCorrupt | NotADatabase => crate::tr!(
            "base de données endommagée : redémarrez facteur, et consultez les journaux si cela persiste",
            "database damaged: restart facteur, and look at the logs if it persists"
        ),
        ReadOnly | PermissionDenied | CannotOpen => crate::tr!(
            "base de données inaccessible en écriture : vérifiez les droits du dossier de facteur",
            "database cannot be written: check the permissions of facteur's folder"
        ),
        SystemIoFailure => crate::tr!(
            "erreur du disque en lisant ou écrivant la base de données",
            "disk error while reading or writing the database"
        ),
        _ => return None,
    })
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Db(e) => Some(e),
            Error::Imap(e) => Some(e),
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

thread_local! {
    /// Another database is being used on this thread (a backup's index): its damage is not the
    /// main database's.
    static OTHER_DB: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// While it lives, SQLite errors on this thread are another database's (see [`OTHER_DB`]).
pub struct OtherDb(());

impl OtherDb {
    pub fn enter() -> Self {
        OTHER_DB.with(|n| n.set(n.get() + 1));
        OtherDb(())
    }
}

impl Drop for OtherDb {
    fn drop(&mut self) {
        OTHER_DB.with(|n| n.set(n.get().saturating_sub(1)));
    }
}

/// A damaged main database, which the next start must check.
fn marks_damage(e: &Error) -> bool {
    e.is_damaged_db() && OTHER_DB.with(|n| n.get() == 0)
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        let e = Error::Db(e);
        if marks_damage(&e)
            && let Some(marker) = DAMAGE_MARKER.get()
        {
            // The next start checks the whole file and offers to set it aside (see `Store::open`):
            // a page damaged while running is not seen by merely opening the database.
            if let Err(write) = std::fs::write(marker, b"") {
                tracing::error!("damaged database, its marker not written: {write}");
            }
        }
        e
    }
}

/// The file whose presence makes the next start check the database (`Store::open`).
static DAMAGE_MARKER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub(crate) fn set_damage_marker(path: std::path::PathBuf) {
    let _ = DAMAGE_MARKER.set(path);
}

impl From<async_imap::error::Error> for Error {
    fn from(e: async_imap::error::Error) -> Self {
        Error::Imap(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Other(e.to_string())
    }
}

impl Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_databases_damage_leaves_the_main_one_alone() {
        let corrupt = Error::Db(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT), None));
        assert!(marks_damage(&corrupt));
        {
            let _other = OtherDb::enter();
            assert!(!marks_damage(&corrupt), "a backup index's damage is not the main database's");
        }
        assert!(marks_damage(&corrupt));
    }

    #[test]
    fn local_disk_errors_are_not_called_network_errors() {
        let file = Error::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "accès refusé"));
        assert_eq!(file.to_string(), "fichier : accès refusé");
        let net = Error::Io(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "coupé"));
        assert_eq!(net.to_string(), "réseau : coupé");
        let full = Error::Io(std::io::Error::new(std::io::ErrorKind::StorageFull, "ENOSPC"));
        assert!(full.to_string().starts_with("disque plein"));
    }

    #[test]
    fn common_sqlite_failures_read_as_sentences() {
        let failure = |code| Error::Db(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error { code, extended_code: 0 }, None));
        assert!(failure(rusqlite::ErrorCode::DiskFull).to_string().starts_with("disque plein"));
        assert!(failure(rusqlite::ErrorCode::DatabaseBusy).to_string().starts_with("base de données occupée"));
        assert!(failure(rusqlite::ErrorCode::DatabaseCorrupt).is_damaged_db());
        assert!(!failure(rusqlite::ErrorCode::DiskFull).is_damaged_db());
    }
}
