//! Full backup cycle against a real IMAP server (GreenMail, for instance).
//!
//! FACTEUR_TEST_IMAP=127.0.0.1:3143 FACTEUR_TEST_USER=yann FACTEUR_TEST_PASS=secret \
//!     cargo test -p facteur-core --test backup_imap -- --ignored

use std::path::Path;

use facteur_core::backup::{RestoreFilter, RestoreTarget, backup_account, catalog, restore, select};
use facteur_core::imap::{self, Session};
use facteur_core::{Credential, Security, ServerConfig};
use futures::TryStreamExt;

async fn connect() -> Session {
    let addr = std::env::var("FACTEUR_TEST_IMAP").expect("FACTEUR_TEST_IMAP=host:port");
    let (host, port) = addr.rsplit_once(':').unwrap();
    let server = ServerConfig { host: host.into(), port: port.parse().unwrap(), security: Security::None };
    let user = std::env::var("FACTEUR_TEST_USER").unwrap();
    let pass = std::env::var("FACTEUR_TEST_PASS").unwrap();
    imap::connect(&server, &user, &Credential::Password(pass)).await.unwrap()
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.join("cur"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
#[ignore = "needs a test IMAP server"]
async fn backup_is_incremental_keeps_deleted_and_restores() {
    let dest = tempfile::tempdir().unwrap();
    let root = dest.path().join("yann@localhost");
    let mut s = connect().await;

    let first = backup_account(&mut s, "yann@localhost", &root, |_| {}).await.unwrap();
    assert!(first.downloaded >= 3, "{first:?}");
    let inbox = root.join("INBOX");
    let before = files(&inbox);
    assert_eq!(before.len(), 3);
    assert!(root.join("Éléments supprimés").is_dir(), "UTF-7 folder name decoded");

    // Nothing new: nothing is downloaded again.
    let second = backup_account(&mut s, "yann@localhost", &root, |_| {}).await.unwrap();
    assert_eq!(second.downloaded, 0);
    assert_eq!(second.messages, first.messages);

    // A flag changes on the server: the file is renamed, not duplicated.
    s.select("INBOX").await.unwrap();
    s.uid_store("3", "+FLAGS.SILENT (\\Flagged)").await.unwrap().try_collect::<Vec<_>>().await.unwrap();
    backup_account(&mut s, "yann@localhost", &root, |_| {}).await.unwrap();
    let after = files(&inbox);
    assert_eq!(after.len(), 3);
    assert!(after.iter().any(|f| f.contains("U3V") && f.ends_with(",F")), "{after:?}");

    // A message vanishes from the server: it stays in the backup.
    s.select("INBOX").await.unwrap();
    s.uid_store("1", "+FLAGS.SILENT (\\Deleted)").await.unwrap().try_collect::<Vec<_>>().await.unwrap();
    s.expunge().await.unwrap().try_collect::<Vec<_>>().await.unwrap();
    assert_eq!(s.examine("INBOX").await.unwrap().exists, 2, "deleted from the server");
    let fourth = backup_account(&mut s, "yann@localhost", &root, |_| {}).await.unwrap();
    assert_eq!(files(&inbox).len(), 3);
    assert_eq!(fourth.downloaded, 0);

    let all = catalog(&root, &RestoreFilter::default(), 50, 0).unwrap();
    assert_eq!(all.matching, all.folders.iter().map(|f| f.total).sum::<i64>());
    let inbox_only = RestoreFilter { folders: vec!["INBOX".into()], ..Default::default() };
    assert_eq!(catalog(&root, &inbox_only, 50, 0).unwrap().matching, 3);
    let future = RestoreFilter { since: Some(all.newest.unwrap() + 1), ..Default::default() };
    assert_eq!(catalog(&root, &future, 50, 0).unwrap().matching, 0, "nothing after the newest");
    let text = RestoreFilter { query: Some("facture".into()), ..inbox_only.clone() };
    let found = catalog(&root, &text, 50, 0).unwrap();
    assert_eq!(found.matching, 1);
    assert_eq!(found.entries[0].subject, "Facture octobre");

    // Restore into a separate folder, flags kept.
    let restored = restore(&mut s, &root, select(&root, &inbox_only, None).unwrap(), RestoreTarget::NewFolder, "Restauration test", |_| {})
        .await
        .unwrap();
    assert_eq!(restored.messages, 3);
    let delim = imap::list_mailboxes(&mut s).await.unwrap().iter().find_map(|l| l.delimiter.clone()).unwrap();
    let target = format!("{}{delim}INBOX", restored.target);
    let mb = s.examine(&target).await.unwrap();
    assert_eq!(mb.exists, 3);
    let fetched: Vec<_> = s.uid_fetch("1:*", "(UID FLAGS)").await.unwrap().try_collect().await.unwrap();
    let flagged = fetched
        .iter()
        .filter(|f| f.flags().any(|fl| matches!(fl, async_imap::types::Flag::Flagged)))
        .count();
    assert_eq!(flagged, 1, "the Flagged flag is restored");

    // Back into the original folder: only the deleted message returns, no duplicate.
    let back = restore(&mut s, &root, select(&root, &inbox_only, None).unwrap(), RestoreTarget::Original, "", |_| {})
        .await
        .unwrap();
    assert_eq!((back.messages, back.skipped), (1, 2));
    assert_eq!(s.examine("INBOX").await.unwrap().exists, 3);

    let one = select(&root, &RestoreFilter::default(), Some(&[found.entries[0].id])).unwrap();
    assert_eq!(one.len(), 1);
    s.logout().await.unwrap();
}
