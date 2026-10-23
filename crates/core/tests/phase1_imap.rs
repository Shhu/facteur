//! Drafts, history and server search against a real IMAP server (GreenMail).
//!
//! FACTEUR_TEST_IMAP=127.0.0.1:3143 FACTEUR_TEST_USER=yann FACTEUR_TEST_PASS=secret \
//!     cargo test -p facteur-core --test phase1_imap -- --ignored

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use facteur_core::imap::{self, Session};
use facteur_core::store::Store;
use facteur_core::{Credential, Draft, NewAccount, Role, Security, ServerConfig, ops, smtp, sync};
use futures::TryStreamExt;

fn server() -> ServerConfig {
    let addr = std::env::var("FACTEUR_TEST_IMAP").expect("FACTEUR_TEST_IMAP=host:port");
    let (host, port) = addr.rsplit_once(':').unwrap();
    ServerConfig { host: host.into(), port: port.parse().unwrap(), security: Security::None }
}

async fn connect() -> Session {
    let user = std::env::var("FACTEUR_TEST_USER").unwrap();
    let pass = std::env::var("FACTEUR_TEST_PASS").unwrap();
    imap::connect(&server(), &user, &Credential::Password(pass)).await.unwrap()
}

fn setup_store() -> (Store, i64) {
    let store = Store::open_in_memory().unwrap();
    let account = store
        .insert_account(&NewAccount {
            name: "Yann".into(),
            email: "yann@localhost".into(),
            username: "yann".into(),
            password: String::new(),
            imap: server(),
            smtp: server(),
            auth: Default::default(),
        })
        .unwrap();
    (store, account)
}

async fn drafts_with(s: &mut Session, mailbox: &str) -> Vec<String> {
    s.examine(mailbox).await.unwrap();
    let fetched: Vec<_> = s.uid_fetch("1:*", "(UID ENVELOPE)").await.unwrap().try_collect().await.unwrap();
    fetched
        .iter()
        .filter_map(|f| f.envelope()?.subject.as_ref().map(|b| String::from_utf8_lossy(b).into_owned()))
        .collect()
}

#[tokio::test]
#[ignore = "needs a test IMAP server"]
async fn drafts_history_and_server_search() {
    let (store, acc) = setup_store();
    let account = store.account(acc).unwrap();
    let mut s = connect().await;
    s.create("Brouillons-test").await.ok();
    let caps = sync::capabilities(&mut s).await.unwrap();

    // Drafts: two saves, a single version on the server.
    let save = |subject: &str| {
        let draft = Draft {
            account_id: acc,
            to: vec!["bob@localhost".into()],
            subject: subject.into(),
            body: "Bonjour".into(),
            draft_id: Some("brouillon-1@localhost".into()),
            ..Default::default()
        };
        let raw = smtp::build(&account, &draft, None, &[], true).unwrap().formatted();
        let op = ops::Op::SaveDraft { mailbox: "Brouillons-test".into(), message_id: "brouillon-1@localhost".into(), raw: B64.encode(raw), source: None };
        serde_json::to_string(&op).unwrap()
    };
    store.push_op(acc, &save("Version 1"), "", None).unwrap();
    ops::flush(&store, acc, &mut s, &caps).await.unwrap();
    store.push_op(acc, &save("Version 2"), "", None).unwrap();
    ops::flush(&store, acc, &mut s, &caps).await.unwrap();
    assert_eq!(drafts_with(&mut s, "Brouillons-test").await, ["Version 2"]);

    let delete = ops::Op::DeleteDraft { mailbox: "Brouillons-test".into(), message_id: "brouillon-1@localhost".into() };
    store.push_op(acc, &serde_json::to_string(&delete).unwrap(), "", None).unwrap();
    ops::flush(&store, acc, &mut s, &caps).await.unwrap();
    assert!(drafts_with(&mut s, "Brouillons-test").await.is_empty());

    // History: window reduced to the last message, then walking back.
    { let caps = sync::capabilities(&mut s).await.unwrap(); let flushed = facteur_core::ops::flush(&store, account.id, &mut s, &caps).await.unwrap(); sync::sync_account(&store, &account, &mut s, &caps, flushed, sync::Scope::All, &mut |_| {}, &mut || {}).await.unwrap() };
    let mut inbox = store.mailbox_by_role(acc, Role::Inbox).unwrap().unwrap();
    store.clear_mailbox(inbox.id).unwrap();
    inbox.sync_from_uid = Some(3);
    store.save_mailbox_state(&inbox).unwrap();
    sync::sync_mailbox(&store, acc, &mut s, &caps, store.mailbox_state(inbox.id).unwrap(), &mut |_| {}).await.unwrap();
    assert_eq!(store.mailbox_uids(inbox.id).unwrap().len(), 1, "only UID 3 is in the window");
    let added = sync::load_older(&store, &mut s, store.mailbox_state(inbox.id).unwrap(), 10).await.unwrap();
    assert_eq!(added, 2);
    assert_eq!(store.mailbox_state(inbox.id).unwrap().sync_from_uid, Some(1));
    assert_eq!(sync::load_older(&store, &mut s, store.mailbox_state(inbox.id).unwrap(), 10).await.unwrap(), 0);

    // Server search: finds outside the cache, in the body.
    store.clear_mailbox(inbox.id).unwrap();
    let found = sync::search_server(&store, &mut s, &[store.mailbox_state(inbox.id).unwrap()], "parfait", 50).await.unwrap();
    assert_eq!(found.len(), 1, "the word only appears in the body of a reply");
    assert_eq!(store.summary(found[0]).unwrap().subject, "Re: Réunion lundi");
    s.logout().await.unwrap();
}

#[tokio::test]
#[ignore = "needs a test IMAP server"]
async fn grouped_moves_and_deletes() {
    let (store, acc) = setup_store();
    let mut s = connect().await;
    s.create("Lot").await.ok();
    let caps = sync::capabilities(&mut s).await.unwrap();
    let uids: Vec<u32> = {
        s.select("INBOX").await.unwrap();
        let mut u: Vec<u32> = s.uid_search("ALL").await.unwrap().into_iter().collect();
        u.sort_unstable();
        u
    };
    assert!(uids.len() >= 2, "test mailbox too empty: {uids:?}");

    // A single move for two messages.
    let moved = ops::Op::MoveSet { mailbox: "INBOX".into(), uids: uids[..2].to_vec(), dest: "Lot".into(), uidvalidity: None };
    store.push_op(acc, &serde_json::to_string(&moved).unwrap(), "", None).unwrap();
    assert_eq!(ops::flush(&store, acc, &mut s, &caps).await.unwrap(), 1);
    assert_eq!(s.examine("Lot").await.unwrap().exists, 2);
    assert_eq!(s.examine("INBOX").await.unwrap().exists as usize, uids.len() - 2);

    // Then a single permanent deletion for both.
    let lot: Vec<u32> = { s.select("Lot").await.unwrap(); s.uid_search("ALL").await.unwrap().into_iter().collect() };
    let deleted = ops::Op::DeleteSet { mailbox: "Lot".into(), uids: lot, uidvalidity: None };
    store.push_op(acc, &serde_json::to_string(&deleted).unwrap(), "", None).unwrap();
    ops::flush(&store, acc, &mut s, &caps).await.unwrap();
    assert_eq!(s.examine("Lot").await.unwrap().exists, 0);
    s.logout().await.unwrap();
}
