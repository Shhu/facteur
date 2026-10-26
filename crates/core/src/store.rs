//! Local SQLite cache: accounts, folders, headers, raw bodies, full-text index, and the
//! queue of operations to replay on the server.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::error::{Error, Result};
use crate::mime::HeaderInfo;
use crate::model::*;
use crate::utf7;

const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE accounts (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,
    email         TEXT NOT NULL,
    username      TEXT NOT NULL,
    imap_host     TEXT NOT NULL,
    imap_port     INTEGER NOT NULL,
    imap_security TEXT NOT NULL,
    smtp_host     TEXT NOT NULL,
    smtp_port     INTEGER NOT NULL,
    smtp_security TEXT NOT NULL,
    last_sync     INTEGER
);

CREATE TABLE mailboxes (
    id            INTEGER PRIMARY KEY,
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    delimiter     TEXT,
    role          TEXT,
    selectable    INTEGER NOT NULL DEFAULT 1,
    uidvalidity   INTEGER,
    uidnext       INTEGER,
    highestmodseq INTEGER,
    sync_from_uid INTEGER,
    UNIQUE (account_id, name)
);

CREATE TABLE messages (
    id              INTEGER PRIMARY KEY,
    account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    mailbox_id      INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    -- NULL until the server confirms a local move.
    uid             INTEGER,
    message_id      TEXT,
    in_reply_to     TEXT,
    refs            TEXT NOT NULL DEFAULT '',
    thread_id       INTEGER NOT NULL DEFAULT 0,
    subject         TEXT NOT NULL DEFAULT '',
    from_name       TEXT,
    from_addr       TEXT,
    to_list         TEXT NOT NULL DEFAULT '[]',
    cc_list         TEXT NOT NULL DEFAULT '[]',
    date            INTEGER NOT NULL,
    size            INTEGER NOT NULL DEFAULT 0,
    seen            INTEGER NOT NULL DEFAULT 0,
    flagged         INTEGER NOT NULL DEFAULT 0,
    answered        INTEGER NOT NULL DEFAULT 0,
    draft           INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    preview         TEXT NOT NULL DEFAULT '',
    UNIQUE (mailbox_id, uid)
);
CREATE INDEX messages_mailbox_date ON messages (mailbox_id, date DESC);
CREATE INDEX messages_thread       ON messages (account_id, thread_id);
CREATE INDEX messages_msgid        ON messages (account_id, message_id);
CREATE INDEX messages_irt          ON messages (account_id, in_reply_to);

CREATE TABLE bodies (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    raw        BLOB NOT NULL
);

CREATE VIRTUAL TABLE messages_fts USING fts5 (
    subject, sender, recipients, body,
    tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
END;

CREATE TABLE pending_ops (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    op         TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);
"#,
r#"
ALTER TABLE accounts ADD COLUMN auth TEXT NOT NULL DEFAULT 'password';
ALTER TABLE accounts ADD COLUMN oauth_provider TEXT;
"#,
r#"
ALTER TABLE messages ADD COLUMN received_at INTEGER;
CREATE INDEX messages_received ON messages (received_at);
"#,
r#"
ALTER TABLE pending_ops ADD COLUMN label TEXT;
ALTER TABLE pending_ops ADD COLUMN failed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN signature TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN avatar TEXT;
"#,
r#"
ALTER TABLE messages ADD COLUMN bulk INTEGER NOT NULL DEFAULT 0;
CREATE TABLE blocked_senders (
    email      TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Logo per sender domain; data NULL: looked up, nothing found.
CREATE TABLE sender_logos (
    domain     TEXT PRIMARY KEY,
    data       TEXT,
    fetched_at INTEGER NOT NULL
);
"#,
r#"
-- Trust-score signals (JSON), read from the headers at sync.
ALTER TABLE messages ADD COLUMN signals TEXT;
-- Logos stored before facteur re-rendered them as PNG: fetch them again.
DELETE FROM sender_logos;
"#,
r#"
ALTER TABLE accounts ADD COLUMN label TEXT;
"#,
r#"
-- Senders whose remote images load by default (a domain, or an address).
CREATE TABLE trusted_images (
    key        TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);
"#,
r#"
-- Link domains, space-separated (NULL: content not analysed yet).
ALTER TABLE messages ADD COLUMN link_domains TEXT;
ALTER TABLE messages ADD COLUMN list_id TEXT;
CREATE INDEX messages_list_id ON messages (list_id);
"#,
r#"
ALTER TABLE mailboxes ADD COLUMN server_count INTEGER;
"#,
r#"
-- Messages waiting to be sent: undo delay or scheduled send.
CREATE TABLE outbox (
    id           INTEGER PRIMARY KEY,
    account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    draft        TEXT NOT NULL,
    send_at      INTEGER NOT NULL,
    scheduled    INTEGER NOT NULL DEFAULT 0,
    -- pending | sending | failed
    state        TEXT NOT NULL DEFAULT 'pending',
    attempts     INTEGER NOT NULL DEFAULT 0,
    error        TEXT,
    remind_after INTEGER,
    created_at   INTEGER NOT NULL
);
-- Snoozed threads wait in "Later" until `until`; `woke_at`: back in the inbox (they rise to
-- the top of the list).
CREATE TABLE snoozes (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thread_id   INTEGER NOT NULL,
    -- Message-IDs of the snoozed messages (JSON): the thread may be renumbered meanwhile.
    message_ids TEXT NOT NULL DEFAULT '[]',
    until       INTEGER NOT NULL,
    woke_at     INTEGER,
    PRIMARY KEY (account_id, thread_id)
);
-- Follow-up reminders: alert when a sent message gets no reply.
CREATE TABLE reminders (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    subject    TEXT NOT NULL,
    recipients TEXT NOT NULL DEFAULT '[]',
    sent_at    INTEGER NOT NULL,
    due        INTEGER NOT NULL,
    -- waiting | due
    state      TEXT NOT NULL DEFAULT 'waiting'
);
"#,
r#"
-- Held until this date: the action can still be undone.
ALTER TABLE pending_ops ADD COLUMN not_before INTEGER;
-- Extra sending addresses (aliases), JSON.
ALTER TABLE accounts ADD COLUMN identities TEXT NOT NULL DEFAULT '[]';
-- Rules applied to new inbox messages (conditions and actions as JSON).
CREATE TABLE rules (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    position   INTEGER NOT NULL DEFAULT 0,
    enabled    INTEGER NOT NULL DEFAULT 1,
    data       TEXT NOT NULL
);
"#,
r#"
-- One row per References entry: threading a message no longer scans the whole account.
CREATE TABLE message_refs (
    account_id INTEGER NOT NULL,
    ref        TEXT NOT NULL,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE
);
INSERT INTO message_refs (account_id, ref, message_id)
WITH RECURSIVE split(account_id, id, ref, rest) AS (
    SELECT account_id, id, '', refs || ' ' FROM messages WHERE refs != ''
    UNION ALL
    SELECT account_id, id, substr(rest, 1, instr(rest, ' ') - 1), substr(rest, instr(rest, ' ') + 1)
    FROM split WHERE rest != ''
)
SELECT account_id, ref, id FROM split WHERE ref != '';
CREATE INDEX message_refs_ref ON message_refs (account_id, ref);
CREATE INDEX message_refs_message ON message_refs (message_id);
-- Covering index: a folder's thread aggregates without reading the rows.
CREATE INDEX messages_box_thread ON messages (mailbox_id, thread_id, date, id, seen, flagged, has_attachments);
CREATE INDEX messages_from_lc ON messages (lower(from_addr), account_id, date);
CREATE INDEX messages_flagged ON messages (date) WHERE flagged = 1;
-- Addresses written to (Sent folder): "you have written to them" without rereading all sent mail.
CREATE TABLE correspondents (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email      TEXT NOT NULL,
    PRIMARY KEY (account_id, email)
) WITHOUT ROWID;
INSERT OR IGNORE INTO correspondents (account_id, email)
SELECT m.account_id, lower(json_extract(j.value, '$.email'))
FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id, json_each(m.to_list) j
WHERE b.role = 'sent' AND json_extract(j.value, '$.email') IS NOT NULL
UNION
SELECT m.account_id, lower(json_extract(j.value, '$.email'))
FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id, json_each(m.cc_list) j
WHERE b.role = 'sent' AND json_extract(j.value, '$.email') IS NOT NULL;
CREATE TABLE message_links (
    domain     TEXT NOT NULL,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    PRIMARY KEY (domain, message_id)
) WITHOUT ROWID;
INSERT OR IGNORE INTO message_links (domain, message_id)
WITH RECURSIVE split(id, domain, rest) AS (
    SELECT id, '', link_domains || ' ' FROM messages WHERE COALESCE(link_domains, '') != ''
    UNION ALL
    SELECT id, substr(rest, 1, instr(rest, ' ') - 1), substr(rest, instr(rest, ' ') + 1) FROM split WHERE rest != ''
)
SELECT domain, id FROM split WHERE domain != '';
CREATE INDEX message_links_message ON message_links (message_id);
-- Address book kept up to date with each message, so autocomplete does not aggregate all mail
-- on every keystroke. `written`: messages sent to them.
CREATE TABLE contacts (
    email   TEXT PRIMARY KEY,
    name    TEXT,
    written INTEGER NOT NULL DEFAULT 0,
    seen    INTEGER NOT NULL DEFAULT 0,
    last    INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
INSERT INTO contacts (email, name, written, seen, last)
SELECT email, MAX(name), SUM(sent), COUNT(*), MAX(date) FROM (
    SELECT lower(from_addr) AS email, NULLIF(from_name, '') AS name, date, 0 AS sent
    FROM messages WHERE from_addr IS NOT NULL
    UNION ALL
    SELECT lower(json_extract(j.value, '$.email')), NULLIF(json_extract(j.value, '$.name'), ''), m.date, 1
    FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id, json_each(m.to_list) j
    WHERE b.role = 'sent'
    UNION ALL
    SELECT lower(json_extract(j.value, '$.email')), NULLIF(json_extract(j.value, '$.name'), ''), m.date, 1
    FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id, json_each(m.cc_list) j
    WHERE b.role = 'sent'
)
WHERE email IS NOT NULL GROUP BY email;
"#,
r#"
-- Smart inbox: people, notification or newsletter (classified on arrival, overridable per
-- sender). Messages already cached: from what is known about them.
ALTER TABLE messages ADD COLUMN category TEXT NOT NULL DEFAULT 'people';
UPDATE messages SET category = CASE
    WHEN lower(COALESCE(from_addr, '')) LIKE '%newsletter%' OR lower(COALESCE(from_addr, '')) LIKE 'news%@%' THEN 'newsletter'
    WHEN bulk = 1 AND list_id IS NULL THEN 'notification'
    WHEN bulk = 1 AND (lower(COALESCE(from_addr, '')) LIKE '%noreply%' OR lower(COALESCE(from_addr, '')) LIKE '%no-reply%'
                       OR lower(COALESCE(from_addr, '')) LIKE '%notification%') THEN 'notification'
    WHEN bulk = 1 THEN 'newsletter'
    ELSE 'people' END;
-- The user's choice for a sender ("file under...").
CREATE TABLE sender_categories (
    email    TEXT PRIMARY KEY,
    category TEXT NOT NULL
) WITHOUT ROWID;
DROP INDEX messages_box_thread;
CREATE INDEX messages_box_thread ON messages (mailbox_id, thread_id, date, id, seen, flagged, has_attachments, category);
-- People the user has written to are always people.
UPDATE messages SET category = 'people'
WHERE category != 'people' AND EXISTS (
    SELECT 1 FROM correspondents c WHERE c.account_id = messages.account_id AND c.email = lower(messages.from_addr));
"#,
    // Account colour picked in facteur (empty: assigned by rank).
    "ALTER TABLE accounts ADD COLUMN color TEXT;",
    // Op kind, folder and destination as columns: sync checks them for each folder without
    // decoding the payload (a whole message for an append).
    r#"
ALTER TABLE pending_ops ADD COLUMN kind TEXT;
ALTER TABLE pending_ops ADD COLUMN mailbox TEXT;
ALTER TABLE pending_ops ADD COLUMN dest TEXT;
UPDATE pending_ops SET kind = json_extract(op, '$.kind'),
                       mailbox = COALESCE(json_extract(op, '$.mailbox'), json_extract(op, '$.from')),
                       dest = COALESCE(json_extract(op, '$.dest'), json_extract(op, '$.to'))
WHERE json_valid(op);
CREATE INDEX pending_ops_mailbox ON pending_ops (account_id, mailbox);
"#,
    // has_body also counts another folder's copy of the message (Gmail labels), so bodies need
    // not be read. pending_flags: flags changed during a move, sent once the UID is known.
    // thread_seq: thread numbers are never reused (message ids are, after a delete).
    r#"
ALTER TABLE messages ADD COLUMN has_body INTEGER NOT NULL DEFAULT 0;
UPDATE messages SET has_body = 1 WHERE id IN (SELECT message_id FROM bodies);
CREATE INDEX messages_nobody ON messages (mailbox_id, date DESC) WHERE has_body = 0 AND uid IS NOT NULL;
ALTER TABLE messages ADD COLUMN pending_flags TEXT;
CREATE TABLE thread_seq (id INTEGER PRIMARY KEY AUTOINCREMENT);
INSERT INTO thread_seq (id) SELECT MAX(COALESCE(MAX(id), 0), COALESCE(MAX(thread_id), 0)) FROM messages;
DELETE FROM thread_seq;
CREATE INDEX messages_unread ON messages (mailbox_id) WHERE seen = 0;
CREATE INDEX messages_account_date ON messages (account_id, date DESC);
DROP INDEX messages_from_lc;
CREATE INDEX messages_from_lc ON messages (lower(from_addr), account_id, date, mailbox_id);
"#,
r#"
-- Local rows of the folder (plus those moved out, not yet gone on the server) when its server
-- count was read: what changed locally since moves the shown total right away.
ALTER TABLE mailboxes ADD COLUMN count_base INTEGER;
"#,
r#"
-- The account's avatar was picked by the user: never replaced automatically.
ALTER TABLE accounts ADD COLUMN avatar_custom INTEGER NOT NULL DEFAULT 0;
"#,
r#"
-- Screener: on since this date (NULL: off), and the user's decisions about senders: one address,
-- or a whole domain written '@example.org'.
ALTER TABLE accounts ADD COLUMN screener_since INTEGER;
CREATE TABLE screening (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    pattern    TEXT NOT NULL,
    decision   TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (account_id, pattern)
) WITHOUT ROWID;
"#,
r#"
-- Order of the accounts chosen by the user (NULL: after the others, in the order added).
ALTER TABLE accounts ADD COLUMN position INTEGER;
"#,
r#"
-- Searches for unread or pinned messages of an account (is:unread, is:pinned), newest first,
-- without going through all of its messages.
CREATE INDEX messages_account_unread ON messages (account_id, date DESC) WHERE seen = 0;
CREATE INDEX messages_account_flagged ON messages (account_id, date DESC) WHERE flagged = 1;
"#,
r#"
-- Calendars of the accounts (Google Calendar, CalDAV), read only for now. `remote_id`: the Google
-- calendar id, or the CalDAV collection's address; `ctag`: the CalDAV getctag or sync-token of
-- the events stored, `synced_at`: when they were fetched.
CREATE TABLE calendars (
    id         INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id  TEXT NOT NULL,
    name       TEXT NOT NULL,
    color      TEXT,
    source     TEXT NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1,
    read_only  INTEGER NOT NULL DEFAULT 0,
    ctag       TEXT,
    synced_at  INTEGER,
    UNIQUE (account_id, remote_id)
);
-- Occurrences of the events (recurrences expanded) within the synced window. `instance`: tells
-- apart the occurrences of one series (empty for a single event).
CREATE TABLE calendar_events (
    id          INTEGER PRIMARY KEY,
    calendar_id INTEGER NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    uid         TEXT NOT NULL,
    instance    TEXT NOT NULL,
    start_at    INTEGER NOT NULL,
    end_at      INTEGER NOT NULL,
    all_day     INTEGER NOT NULL,
    summary     TEXT NOT NULL,
    location    TEXT,
    description TEXT,
    status      TEXT,
    organizer   TEXT,
    url         TEXT,
    UNIQUE (calendar_id, uid, instance)
);
CREATE INDEX calendar_events_start ON calendar_events (calendar_id, start_at);
-- Per account: the CalDAV address given by the user, the calendar home found by discovery, and
-- how the last calendar sync went.
CREATE TABLE calendar_accounts (
    account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    caldav_url TEXT,
    home_url   TEXT,
    state      TEXT,
    message    TEXT,
    last_sync  INTEGER
);
"#,
r#"
-- Screener memory. `screened`: Message-IDs this device held or refused, with their server
-- arrival date; the same message (same arrival date: not a resend) found again in the inbox was
-- moved back by another device or client, which accepts its sender. `sent_ids`: Message-IDs
-- sent from facteur; a reply to one is never held, even before Sent is synced.
CREATE TABLE screened (
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id  TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    at          INTEGER NOT NULL,
    PRIMARY KEY (account_id, message_id)
) WITHOUT ROWID;
CREATE TABLE sent_ids (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    at         INTEGER NOT NULL,
    PRIMARY KEY (account_id, message_id)
) WITHOUT ROWID;
"#,
r#"
-- Senders held by the screener, grouped per folder and sender in one pass; the latest message
-- of an address across accounts (sender logos).
CREATE INDEX messages_box_from ON messages (mailbox_id, lower(from_addr), date);
CREATE INDEX messages_from_date ON messages (lower(from_addr), date);
-- Size of a queued operation, stored when it is queued: the queue's details never read the
-- payloads (whole messages for an append or a draft).
ALTER TABLE pending_ops ADD COLUMN op_size INTEGER;
UPDATE pending_ops SET op_size = length(op);
"#,
r#"
-- Calendars: whether an occurrence takes the time (free/busy), why a calendar's last sync
-- failed, and the site trusted with the password for a remembered CalDAV home. `generation`
-- grows with each CalDAV address saved: a sync started before drops what it found. Whole-day
-- events are now stored as UTC midnights: every calendar is fetched again.
ALTER TABLE calendar_events ADD COLUMN busy INTEGER NOT NULL DEFAULT 1;
ALTER TABLE calendars ADD COLUMN error TEXT;
ALTER TABLE calendar_accounts ADD COLUMN home_site TEXT;
ALTER TABLE calendar_accounts ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
UPDATE calendar_accounts SET home_url = NULL;
DELETE FROM calendar_events;
UPDATE calendars SET ctag = NULL, synced_at = NULL;
"#,
r#"
-- An account whose calendar the user does not use: never synced, kept out of the calendar.
ALTER TABLE calendar_accounts ADD COLUMN ignored INTEGER NOT NULL DEFAULT 0;
"#,
r#"
-- An account's messages by server arrival: the usual authentication server reads the 500
-- latest without sorting the whole account (the expression must match the query's).
CREATE INDEX messages_account_arrival ON messages (account_id, COALESCE(received_at, date));
"#,
r#"
-- When each folder's periodic full checks (whole UID list, all flags) last ran to the end:
-- kept across launches, so that each start does not check every folder again.
ALTER TABLE mailboxes ADD COLUMN checked_list_at INTEGER;
ALTER TABLE mailboxes ADD COLUMN checked_flags_at INTEGER;
"#,
r#"
-- Labels (Gmail): a message is addressed through one folder, its own (`mailbox_id`, `uid`),
-- and may carry labels, folders it is listed in without a UID of its own there. A folder
-- `is_label` lists its messages through this table only.
CREATE TABLE labels (
    mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    PRIMARY KEY (mailbox_id, message_id)
) WITHOUT ROWID;
CREATE INDEX labels_message ON labels (message_id, mailbox_id);
ALTER TABLE mailboxes ADD COLUMN is_label INTEGER NOT NULL DEFAULT 0;
"#,
r#"
-- The account's provider as its server announces it ('imap', 'gmail'): NULL until a connection
-- has told.
ALTER TABLE accounts ADD COLUMN provider TEXT;
"#,
r#"
-- Gmail accounts converted to labels: only All Mail, the bin and spam are synced, the other
-- folders are labels of their messages.
ALTER TABLE accounts ADD COLUMN labels_model INTEGER NOT NULL DEFAULT 0;
"#,
r#"
-- Gmail's system labels (inbox, important, sent) were not recognised as the server writes them
-- (quoted, the backslash escaped): their labels are read again in full at the next sync.
UPDATE mailboxes SET checked_flags_at = NULL
 WHERE role = 'all' AND account_id IN (SELECT id FROM accounts WHERE labels_model = 1);
"#,
r#"
-- Mailbox space as the server reports it (IMAP QUOTA), in bytes, and when it was read.
ALTER TABLE accounts ADD COLUMN quota_used INTEGER;
ALTER TABLE accounts ADD COLUMN quota_limit INTEGER;
ALTER TABLE accounts ADD COLUMN quota_at INTEGER;
"#,
r#"
-- Previews that began with a mailing tool's unfilled placeholder (Airbnb's `%opentrack%`): the
-- placeholder goes, as it now does for new messages.
UPDATE messages SET preview = ltrim(substr(preview, 12)) WHERE preview LIKE '\%opentrack\%%' ESCAPE '\';
"#,
r#"
-- New mail the screener has yet to look at (its account has it on): stored with the flag in the
-- same transaction, and kept out of every list until the screener has decided. Never more than
-- the latest sync's arrivals.
ALTER TABLE messages ADD COLUMN unscreened INTEGER NOT NULL DEFAULT 0;
CREATE INDEX messages_unscreened ON messages (account_id) WHERE unscreened = 1;
"#,
r#"
-- Muted conversations: their new messages skip the inbox (archived, read, no notification).
CREATE TABLE muted_threads (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thread_id  INTEGER NOT NULL,
    PRIMARY KEY (account_id, thread_id)
) WITHOUT ROWID;
"#,
r#"
-- Gmail's own threads (X-GM-THRID) and the local thread number each one has: a conversation is
-- what Gmail says it is. Kept apart from the messages, so that a thread keeps its number (its
-- snooze, its mute) once all its rows have left the cache.
CREATE TABLE gmail_threads (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thrid      INTEGER NOT NULL,
    thread_id  INTEGER NOT NULL,
    PRIMARY KEY (account_id, thrid)
) WITHOUT ROWID;
CREATE INDEX gmail_threads_by_thread ON gmail_threads (account_id, thread_id);
-- The message's Gmail thread, once read (NULL: still to read).
ALTER TABLE messages ADD COLUMN gm_thrid INTEGER;
CREATE INDEX messages_without_thrid ON messages (mailbox_id) WHERE gm_thrid IS NULL;
"#,
r#"
-- A folder's unread count on the server when last synced (STATUS UNSEEN): without CONDSTORE, an
-- unchanged folder is left unopened only while this count holds.
ALTER TABLE mailboxes ADD COLUMN server_unseen INTEGER;
"#,
r#"
-- The marker for an Authentication-Results header without authserv-id, now in English.
UPDATE messages SET signals = json_set(signals, '$.authserv', '(no identifier)')
WHERE signals IS NOT NULL AND json_extract(signals, '$.authserv') = '(sans identifiant)';
"#,
r#"
-- Events facteur may change: the CalDAV resource holding one, whether it is an occurrence of a
-- series, and whether the account organizes it. Every calendar is fetched again to fill them.
ALTER TABLE calendar_events ADD COLUMN href TEXT;
ALTER TABLE calendar_events ADD COLUMN recurring INTEGER NOT NULL DEFAULT 0;
ALTER TABLE calendar_events ADD COLUMN own INTEGER NOT NULL DEFAULT 0;
UPDATE calendars SET ctag = NULL, synced_at = NULL;
"#,
r#"
-- The accounts' address books (Google, Outlook, CardDAV): one row per address of a contact, for
-- the composer's suggestions. `address_book_accounts`: how the last sync of each went.
CREATE TABLE address_book (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id  TEXT NOT NULL,
    email      TEXT NOT NULL,
    name       TEXT,
    PRIMARY KEY (account_id, remote_id, email)
) WITHOUT ROWID;
CREATE INDEX address_book_email ON address_book(email);
CREATE TABLE address_book_accounts (
    account_id INTEGER PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
    state      TEXT NOT NULL,
    message    TEXT,
    synced_at  INTEGER,
    checked_at INTEGER NOT NULL
);
"#,
r#"
-- Events: the provider's id of the series an occurrence belongs to, and the account's answer
-- to someone else's invitation. Every calendar is fetched again to fill them.
ALTER TABLE calendar_events ADD COLUMN series TEXT;
ALTER TABLE calendar_events ADD COLUMN response TEXT;
UPDATE calendars SET ctag = NULL, synced_at = NULL;
"#,
r#"
-- Events: their guests and video meeting (JSON list, link) and their reminders (JSON list of
-- minutes before the start). Every calendar is fetched again to fill them.
ALTER TABLE calendar_events ADD COLUMN attendees TEXT;
ALTER TABLE calendar_events ADD COLUMN meeting TEXT;
ALTER TABLE calendar_events ADD COLUMN reminders TEXT;
UPDATE calendars SET ctag = NULL, synced_at = NULL;
"#,
r#"
-- A queued operation's server refusals, counted apart from its network cuts: each has its limit.
ALTER TABLE pending_ops ADD COLUMN refusals INTEGER NOT NULL DEFAULT 0;
"#,
r#"
-- When a thread last left the inbox, however it left (archived here or elsewhere, a label
-- removed): the "Done" view sorts by it. Not written for an account being removed.
CREATE TABLE handled (
    account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    thread_id  INTEGER NOT NULL,
    at         INTEGER NOT NULL,
    PRIMARY KEY (account_id, thread_id)
) WITHOUT ROWID;
CREATE TRIGGER handled_on_delete AFTER DELETE ON messages
WHEN EXISTS (SELECT 1 FROM mailboxes WHERE id = old.mailbox_id AND role = 'inbox')
 AND EXISTS (SELECT 1 FROM accounts WHERE id = old.account_id)
BEGIN
    INSERT OR REPLACE INTO handled VALUES (old.account_id, old.thread_id, CAST(strftime('%s', 'now') AS INTEGER));
END;
CREATE TRIGGER handled_on_move AFTER UPDATE OF mailbox_id ON messages
WHEN old.mailbox_id <> new.mailbox_id
 AND EXISTS (SELECT 1 FROM mailboxes WHERE id = old.mailbox_id AND role = 'inbox')
BEGIN
    INSERT OR REPLACE INTO handled VALUES (old.account_id, old.thread_id, CAST(strftime('%s', 'now') AS INTEGER));
END;
CREATE TRIGGER handled_on_unlabel AFTER DELETE ON labels
WHEN EXISTS (SELECT 1 FROM mailboxes WHERE id = old.mailbox_id AND role = 'inbox')
BEGIN
    INSERT OR REPLACE INTO handled
    SELECT m.account_id, m.thread_id, CAST(strftime('%s', 'now') AS INTEGER)
    FROM messages m JOIN accounts a ON a.id = m.account_id WHERE m.id = old.message_id;
END;
"#,
r#"
-- Events: the IANA zone a timed event was set in. Every calendar is fetched again to fill it.
ALTER TABLE calendar_events ADD COLUMN zone TEXT;
UPDATE calendars SET ctag = NULL, synced_at = NULL;
"#,
r#"
-- The `handled` triggers stay silent while this table has a row: a folder emptied for a reset
-- (UIDVALIDITY changed) or an undone move is no thread leaving the inbox.
CREATE TABLE handled_quiet (on_ INTEGER NOT NULL);
DROP TRIGGER handled_on_delete;
DROP TRIGGER handled_on_move;
DROP TRIGGER handled_on_unlabel;
CREATE TRIGGER handled_on_delete AFTER DELETE ON messages
WHEN NOT EXISTS (SELECT 1 FROM handled_quiet)
 AND EXISTS (SELECT 1 FROM mailboxes WHERE id = old.mailbox_id AND role = 'inbox')
 AND EXISTS (SELECT 1 FROM accounts WHERE id = old.account_id)
BEGIN
    INSERT OR REPLACE INTO handled VALUES (old.account_id, old.thread_id, CAST(strftime('%s', 'now') AS INTEGER));
END;
CREATE TRIGGER handled_on_move AFTER UPDATE OF mailbox_id ON messages
WHEN old.mailbox_id <> new.mailbox_id
 AND NOT EXISTS (SELECT 1 FROM handled_quiet)
 AND EXISTS (SELECT 1 FROM mailboxes WHERE id = old.mailbox_id AND role = 'inbox')
BEGIN
    INSERT OR REPLACE INTO handled VALUES (old.account_id, old.thread_id, CAST(strftime('%s', 'now') AS INTEGER));
END;
CREATE TRIGGER handled_on_unlabel AFTER DELETE ON labels
WHEN NOT EXISTS (SELECT 1 FROM handled_quiet)
 AND EXISTS (SELECT 1 FROM mailboxes WHERE id = old.mailbox_id AND role = 'inbox')
BEGIN
    INSERT OR REPLACE INTO handled
    SELECT m.account_id, m.thread_id, CAST(strftime('%s', 'now') AS INTEGER)
    FROM messages m JOIN accounts a ON a.id = m.account_id WHERE m.id = old.message_id;
END;
"#,
r#"
-- Sends that failed before reaching the server (offline, an account waiting to be signed in
-- again), counted apart from real attempts: a day offline does not use up a message's tries.
ALTER TABLE outbox ADD COLUMN offline INTEGER NOT NULL DEFAULT 0;
"#,
r#"
-- Each CalDAV object's ETag when last read: a changed calendar is read again only for the objects
-- whose ETag changed, not whole.
CREATE TABLE caldav_etags (
    calendar_id INTEGER NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    href        TEXT NOT NULL,
    etag        TEXT NOT NULL,
    PRIMARY KEY (calendar_id, href)
) WITHOUT ROWID;
"#,
r#"
-- A send that may have reached the server once (tried, or cut by a quit), kept through "Retry":
-- edited, it leaves under a new Message-ID, or receivers would drop it as the same message.
ALTER TABLE outbox ADD COLUMN tried INTEGER NOT NULL DEFAULT 0;
UPDATE outbox SET tried = 1 WHERE attempts > 0 OR error = 'interrupted';
"#,
r#"
-- A folder's latest arrivals in order (the screener's usual verdicts, read from the inbox):
-- 500 rows of the index rather than a sort of the whole folder.
CREATE INDEX messages_box_arrival ON messages (mailbox_id, COALESCE(received_at, date));
"#,
r#"
-- Gmail's labels: what was sent sits in All Mail with the Sent label, and its recipients were
-- never written to (only a Sent folder's were).
INSERT OR IGNORE INTO correspondents (account_id, email)
SELECT m.account_id, lower(json_extract(j.value, '$.email'))
FROM labels l JOIN mailboxes b ON b.id = l.mailbox_id AND b.role = 'sent' JOIN messages m ON m.id = l.message_id, json_each(m.to_list) j
WHERE json_extract(j.value, '$.email') IS NOT NULL
UNION
SELECT m.account_id, lower(json_extract(j.value, '$.email'))
FROM labels l JOIN mailboxes b ON b.id = l.mailbox_id AND b.role = 'sent' JOIN messages m ON m.id = l.message_id, json_each(m.cc_list) j
WHERE json_extract(j.value, '$.email') IS NOT NULL;
INSERT INTO contacts (email, name, written, seen, last)
SELECT email, MAX(name), COUNT(*), COUNT(*), MAX(date) FROM (
    SELECT lower(json_extract(j.value, '$.email')) AS email, NULLIF(json_extract(j.value, '$.name'), '') AS name, m.date AS date
    FROM labels l JOIN mailboxes b ON b.id = l.mailbox_id AND b.role = 'sent' JOIN messages m ON m.id = l.message_id, json_each(m.to_list) j
    UNION ALL
    SELECT lower(json_extract(j.value, '$.email')), NULLIF(json_extract(j.value, '$.name'), ''), m.date
    FROM labels l JOIN mailboxes b ON b.id = l.mailbox_id AND b.role = 'sent' JOIN messages m ON m.id = l.message_id, json_each(m.cc_list) j
)
WHERE email IS NOT NULL GROUP BY email
ON CONFLICT (email) DO UPDATE SET
    name = COALESCE(contacts.name, excluded.name),
    written = MAX(contacts.written, excluded.written),
    seen = MAX(contacts.seen, excluded.seen),
    last = MAX(contacts.last, excluded.last);
"#,
];

/// A move into folder ?1 still on its way. With Gmail's labels, a move out of the bin or spam
/// into a label (the inbox, "Mairie") lands in All Mail, where its placeholder waits.
const MOVE_PENDING_INTO: &str = "EXISTS (SELECT 1 FROM pending_ops p JOIN mailboxes b ON b.account_id = p.account_id AND b.name = p.dest
                                JOIN mailboxes here ON here.id = ?1 AND here.account_id = p.account_id
                                WHERE p.failed = 0 AND (b.id = here.id OR (here.role = 'all' AND b.is_label = 1)))";

/// A folder's UIDs whose flags changed here: the tick of the change, and when.
type FlagMarks = HashMap<u32, (u64, std::time::Instant)>;

/// Longer than any flag read of a folder: a mark older than that concerns no merge under way.
const FLAG_MARKS_KEPT: std::time::Duration = std::time::Duration::from_secs(3600);

/// A searched word as an FTS5 phrase, a prefix unless its last piece is one letter: the index
/// splits `paul@g` into `paul g`, and the star applies to `g` alone.
fn fts_word(w: &str) -> String {
    let last = w.rsplit(|c: char| !c.is_alphanumeric()).find(|s| !s.is_empty()).map_or(0, |s| s.chars().count());
    let star = if last < 2 { "" } else { "*" };
    format!("\"{}\"{star}", w.replace('"', "\"\""))
}

const UID_COUNT: &str = "SELECT COUNT(uid) FROM messages WHERE mailbox_id = ?1";
/// The unread rows alone, through their partial index: SQLite would otherwise pick the (mailbox_id,
/// uid) one and read every row for `seen`.
const UID_UNSEEN: &str = "SELECT COUNT(*) FROM messages INDEXED BY messages_unread WHERE mailbox_id = ?1 AND seen = 0 AND uid IS NOT NULL";

#[derive(Debug, Clone, PartialEq)]
pub struct MailboxState {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub role: Option<Role>,
    pub selectable: bool,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub highestmodseq: Option<u64>,
    pub sync_from_uid: Option<u32>,
    pub server_count: Option<i64>,
    /// A label (Gmail): its messages are listed in `labels`; the folder itself is not synced.
    pub is_label: bool,
}

/// A folder as stored: (id, delimiter, role, selectable).
pub type StoredMailbox = (i64, Option<String>, Option<Role>, bool);

#[derive(Debug, Clone, Copy)]
pub struct ServerFlags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
}

#[derive(Debug, Clone)]
pub struct Location {
    pub account_id: i64,
    pub mailbox_id: i64,
    pub mailbox: String,
    pub uid: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct PendingOp {
    pub id: i64,
    /// Operation kind (`move_set`, `append`...), without its payload: see `op_payload`.
    pub kind: String,
    pub attempts: i64,
    /// Held until this date (undo delay).
    pub not_before: Option<i64>,
    /// Sent to the server before, at least in part (see `mark_op_started`): even once the user
    /// retries it (attempts back to 0), what must not be done twice is checked first.
    pub tried: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Coverage {
    pub account_id: i64,
    pub local: i64,
    /// `None` until every folder has been synced.
    pub server: Option<i64>,
    pub bodies: i64,
}

pub struct TrustInputs {
    pub account_id: i64,
    pub from: Option<String>,
    pub role: Option<Role>,
    /// Signals stored at sync (absent for messages stored by older versions).
    pub signals: Option<crate::trust::Signals>,
}

/// An operation the server refused for good, kept for the user to decide.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FailedOp {
    pub id: i64,
    pub account_id: i64,
    pub label: String,
    pub error: String,
    pub created_at: i64,
}

/// One queued operation as stored, for the queue details window.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpRow {
    pub id: i64,
    pub account_id: i64,
    pub kind: String,
    pub label: String,
    pub mailbox: Option<String>,
    pub dest: Option<String>,
    pub created_at: i64,
    pub attempts: i64,
    pub error: Option<String>,
    pub not_before: Option<i64>,
    pub failed: bool,
    /// Size of the stored operation (a whole message for an append or a draft).
    pub size: i64,
}

pub struct BodyInfo {
    pub preview: String,
    pub text: String,
    pub has_attachments: bool,
    pub link_domains: Vec<String>,
    pub list_id: Option<String>,
}

pub struct Store {
    /// The only connection that writes (and reads what must follow a write in a transaction).
    conn: Mutex<Connection>,
    /// Read-only connections: under WAL they read while a sync writes, without waiting for the
    /// writer's lock. Empty for an in-memory database (tests).
    readers: Vec<Mutex<Connection>>,
    next_reader: std::sync::atomic::AtomicUsize,
    /// Last `PRAGMA shrink_memory` (seconds).
    last_shrink: std::sync::atomic::AtomicI64,
    /// Flags changed here, per folder and UID, with the tick of the change: a sync merging the
    /// server's flags read before then leaves them alone (see [`Store::flags_tick`]).
    flag_marks: Mutex<HashMap<i64, FlagMarks>>,
    flag_clock: std::sync::atomic::AtomicU64,
}

const READERS: usize = 3;

/// A store connection, timed while the developer tools are on (`crate::perf`).
type Conn<'a> = crate::perf::Held<'a>;

/// Caches given back to the system at most this often (seconds).
const SHRINK_EVERY: i64 = 3600;

/// The file is rewritten (VACUUM) when at least this share of it is free pages, and this much.
const COMPACT_FREE_SHARE: i64 = 4;
const COMPACT_MIN_FREE: i64 = 64 * 1024 * 1024;

/// Two rows (`m` and `o`) are copies of one message (Gmail labels, a moved message seen again)
/// only when they agree on more than the Message-ID: distinct messages can share one (a draft
/// saved twice, mailing-list copies, a forged identifier). Indexed by (account_id, message_id).
const SAME_COPY: &str = "o.account_id = m.account_id AND o.message_id = m.message_id AND o.id != m.id \
                         AND o.size = m.size AND o.date = m.date AND o.from_addr IS m.from_addr";

/// Message `m` is in one of the user's own folders, or carries one of their labels (Gmail). The
/// inbox, system folders (roles, Gmail's `[Gmail]/…`) don't count.
const LABELLED: &str = "SELECT 1 FROM mailboxes ob
    WHERE (ob.id = m.mailbox_id OR ob.id IN (SELECT mailbox_id FROM labels WHERE message_id = m.id))
      AND ob.role IS NULL AND ob.name NOT LIKE '[Gmail]/%' AND ob.name NOT LIKE '[Google Mail]/%'";

/// Message `m` (in mailbox `b`) counts as evidence that a link's domain is familiar: not junk or
/// in the bin, and not held by the screener or refused (Gmail: by its label), as the screener's
/// own test ignores those.
const VOUCHES: &str = "COALESCE(b.role, '') NOT IN ('junk', 'trash', 'screener', 'refused')
    AND NOT EXISTS (SELECT 1 FROM labels vl JOIN mailboxes vb ON vb.id = vl.mailbox_id
                    WHERE vl.message_id = m.id AND vb.role IN ('screener', 'refused'))";

/// Verdicts of an account's 500 latest arrivals, by server and with their number of sender
/// domains. Walks messages_account_arrival, whose expression it repeats.
const USUAL_AUTHSERV: &str = "SELECT a, COUNT(*), COUNT(DISTINCT d) FROM (
        SELECT json_extract(m.signals, '$.authserv') AS a, lower(substr(m.from_addr, instr(m.from_addr, '@') + 1)) AS d
        FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
        WHERE m.account_id = ?1 AND COALESCE(b.role, '') NOT IN ('sent', 'drafts', 'junk', 'trash', 'screener', 'refused') AND m.signals IS NOT NULL
        ORDER BY COALESCE(m.received_at, m.date) DESC LIMIT 500)
     WHERE a IS NOT NULL GROUP BY a ORDER BY 2 DESC";

/// [`USUAL_AUTHSERV`] among the inbox's messages alone, read from the inbox's rows: walking the
/// whole account's arrivals for them would read every archived message when the inbox is small.
const USUAL_AUTHSERV_INBOX: &str = "SELECT a, COUNT(*), COUNT(DISTINCT d) FROM (
        SELECT json_extract(m.signals, '$.authserv') AS a, lower(substr(m.from_addr, instr(m.from_addr, '@') + 1)) AS d
        FROM messages m
        WHERE m.mailbox_id = (SELECT id FROM mailboxes WHERE account_id = ?1 AND role = 'inbox' LIMIT 1) AND m.signals IS NOT NULL
        ORDER BY COALESCE(m.received_at, m.date) DESC LIMIT 500)
     WHERE a IS NOT NULL GROUP BY a ORDER BY 2 DESC";

/// Folders, split by how their messages are found: by the messages' own folder, or, for a label
/// (`is_label`), through the `labels` table.
#[derive(Debug, Default, Clone)]
struct Boxes {
    own: Vec<i64>,
    labels: Vec<i64>,
}

impl Boxes {
    fn of(db: &Connection, ids: &[i64]) -> Result<Boxes> {
        let mut boxes = Boxes::default();
        if ids.is_empty() {
            return Ok(boxes);
        }
        let mut st = db.prepare(&format!("SELECT id, is_label FROM mailboxes WHERE id IN ({})", id_list(ids)))?;
        for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?)))? {
            let (id, label) = row?;
            if label { boxes.labels.push(id) } else { boxes.own.push(id) }
        }
        Ok(boxes)
    }

    fn holds(&self, alias: &str) -> String {
        match (self.own.is_empty(), self.labels.is_empty()) {
            (true, true) => "0".to_string(),
            (false, true) => format!("{alias}.mailbox_id IN ({})", id_list(&self.own)),
            // One list of ids, for the reason given at `in_folder_where`.
            _ => format!("{alias}.id IN (SELECT id FROM ({}))", self.pool("id")),
        }
    }

    /// The folders' messages as rows of `cols` (columns of `messages`, unqualified) plus `box`,
    /// the folder each is listed in: its own, or the label. One row per (message, folder).
    fn pool(&self, cols: &str) -> String {
        let mut parts = Vec::new();
        if !self.own.is_empty() {
            parts.push(format!("SELECT {cols}, mailbox_id AS box FROM messages WHERE mailbox_id IN ({})", id_list(&self.own)));
        }
        if !self.labels.is_empty() {
            let qualified = cols.split(',').map(|c| format!("m.{}", c.trim())).collect::<Vec<_>>().join(", ");
            parts.push(format!(
                "SELECT {qualified}, l.mailbox_id AS box FROM labels l JOIN messages m ON m.id = l.message_id WHERE l.mailbox_id IN ({})",
                id_list(&self.labels)
            ));
        }
        if parts.is_empty() {
            parts.push(format!("SELECT {cols}, NULL AS box FROM messages WHERE 0"));
        }
        parts.join(" UNION ALL ")
    }
}

/// Message `alias` is in a folder matching `cond` (on `rb`, a `mailboxes` row): its own folder,
/// or one of its labels. Written as one list of ids: an OR of the two would have SQLite walk the
/// whole account instead of the folders' index.
fn in_folder_where(alias: &str, cond: &str) -> String {
    format!(
        "{alias}.id IN (SELECT x.id FROM messages x WHERE x.mailbox_id IN (SELECT rb.id FROM mailboxes rb WHERE {cond}) \
          UNION ALL SELECT rl.message_id FROM labels rl JOIN mailboxes rb ON rb.id = rl.mailbox_id WHERE {cond})"
    )
}

fn replace_labels(tx: &Transaction, labels: &[(i64, Vec<i64>)]) -> Result<()> {
    let mut clear = tx.prepare_cached("DELETE FROM labels WHERE message_id = ?1")?;
    let mut add = tx.prepare_cached("INSERT OR IGNORE INTO labels (mailbox_id, message_id) VALUES (?1, ?2)")?;
    for (id, boxes) in labels {
        clear.execute([id])?;
        for b in boxes {
            add.execute(params![b, id])?;
        }
    }
    Ok(())
}

/// Every account's messages waiting for the screener (a sync's arrivals at most).
fn unscreened_ids(db: &Connection) -> Result<Vec<i64>> {
    let mut st = db.prepare_cached("SELECT id FROM messages WHERE unscreened = 1")?;
    let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    Ok(rows)
}

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// Outcome of [`Store::undo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Undone {
    Done,
    /// An operation has left the queue, or a moved message is gone: nothing done.
    TooLate,
    /// An operation is no longer held (see `unlocked`): nothing done, to try again under the
    /// replay lock.
    Released,
}

/// The database file and its connections, for the diagnostics panel.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StoreDiagnostics {
    pub path: Option<String>,
    pub file_bytes: u64,
    pub wal_bytes: u64,
    pub page_size: Option<i64>,
    pub page_count: Option<i64>,
    pub freelist_count: Option<i64>,
    /// Configured page cache per connection: pages, or KiB when negative (SQLite's convention).
    pub cache_size: Option<i64>,
    pub connections: Vec<ConnectionCache>,
}

/// One store connection: in use right now, and its page cache since it was opened.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectionCache {
    /// `writer` or `reader`.
    pub role: &'static str,
    pub busy: bool,
    pub cache_used_bytes: Option<i64>,
    pub cache_hits: Option<i64>,
    pub cache_misses: Option<i64>,
}

fn db_status(conn: &Connection, op: std::ffi::c_int) -> Option<i64> {
    let (mut current, mut high) = (0, 0);
    // SAFETY: the handle is the open connection's, used on this thread only while its lock is
    // held; the call only reads counters into the two integers given.
    let rc = unsafe { rusqlite::ffi::sqlite3_db_status(conn.handle(), op, &mut current, &mut high, 0) };
    (rc == rusqlite::ffi::SQLITE_OK).then_some(current as i64)
}

/// Bytes of the file each reader maps.
const READER_MMAP: i64 = 256 << 20;

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Text as the search operators (`de:`, `objet:`…) and the contact suggestions compare it:
/// lowercase and without diacritics, as the full-text index does. SQLite's own lower() and
/// LIKE fold ASCII only: "Émilie" would match neither `de:Émilie` nor `de:emilie`.
pub(crate) fn fold(s: &str) -> String {
    let folded: String = if s.is_ascii() {
        s.to_ascii_lowercase()
    } else {
        icu_normalizer::DecomposingNormalizerBorrowed::new_nfd()
            .normalize_iter(s.chars().flat_map(char::to_lowercase))
            .filter(|c| !matches!(*c as u32, 0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F))
            .collect()
    };
    // Runs of whitespace count as one space (typed twice, or a tab).
    if !folded.contains(|c: char| c.is_whitespace() && c != ' ') && !folded.contains("  ") {
        return folded;
    }
    let mut out = String::with_capacity(folded.len());
    let mut space = false;
    for c in folded.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

/// SQL functions of facteur's own, on every connection (the writer and each reader): `fold`.
fn register_functions(conn: &Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function("fold", 1, FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        Ok(ctx.get_raw(0).as_str_or_null()?.map(fold))
    })
}

/// Left next to the database when it was found damaged while running.
fn damage_marker(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push("-damaged");
    std::path::PathBuf::from(name)
}

/// The whole file read: a damaged page reported as a corrupt database, which the start offers to
/// set aside.
/// The tables and their indexes, before the window shows: not the full-text index, whose own
/// check reads every message's text again (minutes on a large base). That one is checked later,
/// in the background ([`Store::check_search_index`]).
fn check_integrity(path: &Path) -> Result<()> {
    let conn = Connection::open(path)?;
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND sql NOT LIKE 'CREATE VIRTUAL TABLE%'")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for table in tables {
        let report: String = conn.query_row(&format!("PRAGMA quick_check(\"{}\")", table.replace('"', "\"\"")), [], |r| r.get(0))?;
        if !report.eq_ignore_ascii_case("ok") {
            tracing::error!("database check failed ({table}): {report}");
            return Err(Error::Db(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT), Some(report))));
        }
    }
    // The commonest damage, an index out of step with its table, is not seen by quick_check:
    // every index is built again from its table. While the unique index on (folder, UID) was
    // wrong, the sync could add a second row for a message: the later copies go first (read
    // from the table itself, not through that index), or REINDEX would refuse. Whatever still
    // fails is damage: the set-aside offer, never a start that fails for good.
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // A passing failure (a full disk, a busy file) is said as such, the marker left to check again
    // at the next start; anything else is damage, offered to be set aside.
    let rebuilt = drop_duplicate_copies(&conn).and_then(|()| conn.execute_batch("REINDEX;")).map_err(|e| {
        tracing::error!("database check failed (indexes not rebuilt): {e}");
        use rusqlite::ErrorCode::*;
        let passing = e.sqlite_error_code().is_some_and(|c| {
            matches!(c, DiskFull | DatabaseBusy | DatabaseLocked | ReadOnly | PermissionDenied | CannotOpen | SystemIoFailure | OutOfMemory | OperationInterrupted)
        });
        if passing {
            Error::Db(e)
        } else {
            Error::Db(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT), Some(e.to_string())))
        }
    });
    rebuilt?;
    tracing::info!("database checked after a damage report: sound, indexes rebuilt");
    Ok(())
}

/// The later copies of a message the sync added twice (while the unique index on folder and UID
/// was wrong), read from the table itself. With what hangs on them (bodies, labels, references):
/// a bare connection has foreign keys off, and a new message reusing a freed id would show a
/// deleted copy's body.
fn drop_duplicate_copies(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         BEGIN;
         INSERT INTO handled_quiet VALUES (1);
         DELETE FROM messages WHERE uid IS NOT NULL AND id NOT IN (SELECT MIN(id) FROM messages NOT INDEXED WHERE uid IS NOT NULL GROUP BY mailbox_id, uid);
         DELETE FROM handled_quiet;
         COMMIT;",
    )
    .inspect_err(|_| {
        let _ = conn.execute_batch("ROLLBACK;");
    })
}

/// Rows deleted per transaction when many go (a folder, an account): the writer is free between.
const STEP_ROWS: usize = 2000;
/// Raw bytes a step of the body index rebuild reads at most (one message beyond it is still read).
const BODY_STEP_BYTES: usize = 32 << 20;

/// The next stored bodies after `after`, as many as fit in `budget` bytes (one at least).
fn body_step(db: &Connection, after: i64, budget: usize) -> Result<Vec<i64>> {
    let mut st = db.prepare_cached("SELECT message_id, length(raw) FROM bodies WHERE message_id > ?1 ORDER BY message_id LIMIT 200")?;
    let mut total = 0usize;
    let mut ids = Vec::new();
    for row in st.query_map([after], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, len) = row?;
        total += len.max(0) as usize;
        if total > budget && !ids.is_empty() {
            break;
        }
        ids.push(id);
    }
    Ok(ids)
}

/// Set after a damage report: the full-text index is checked once facteur has started.
const SEARCH_INDEX_CHECK: &str = "search_index_check";
const SET_SETTING: &str = "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value";

/// Where a rebuild of the search index stands (see `Store::rebuild_search_index`).
const SEARCH_INDEX_REBUILD: &str = "search_index_rebuild";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let marker = damage_marker(path);
        crate::error::set_damage_marker(marker.clone());
        let damaged = marker.exists();
        if damaged {
            check_integrity(path)?;
        }
        // Compacting (VACUUM) is left to the engine, after the start (`Engine::compact_later`): it
        // can take a while on a large base, and the window would wait for it.
        let mut store = Self::init(Connection::open(path)?)?;
        // After the migrations, so the readers see the up-to-date schema.
        for _ in 0..READERS {
            let conn = Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX | rusqlite::OpenFlags::SQLITE_OPEN_URI,
            )?;
            // Read-only (the file cannot be modified), temporary tables still allowed. Modest
            // cache: there are three of them, open for days.
            conn.execute_batch(&format!("PRAGMA cache_size = -8192; PRAGMA temp_store = MEMORY; PRAGMA mmap_size = {READER_MMAP};"))?;
            register_functions(&conn)?;
            let mut conn = conn;
            crate::perf::profile(&mut conn);
            store.readers.push(Mutex::new(conn));
        }
        if damaged {
            store.set_setting(SEARCH_INDEX_CHECK, "1")?;
            let _ = std::fs::remove_file(&marker);
        }
        Ok(store)
    }

    /// After a damage report, the full-text index checked, and built again from the messages when
    /// damaged. Whether it was.
    pub fn check_search_index(&self) -> Result<bool> {
        // A rebuild cut short (a quit, an error) goes on: the part done passes the check below,
        // the rest would never be indexed.
        if self.setting(SEARCH_INDEX_REBUILD)?.is_some() {
            self.rebuild_search_index()?;
            self.db().execute("DELETE FROM settings WHERE key = ?1", [SEARCH_INDEX_CHECK])?;
            return Ok(true);
        }
        if self.setting(SEARCH_INDEX_CHECK)?.is_none() {
            return Ok(false);
        }
        // On a reader: the check reads every message's text again, the writer stays free.
        let report = self.read().query_row("PRAGMA integrity_check(messages_fts)", [], |r| r.get::<_, String>(0));
        let rebuilt = match report {
            Ok(r) if r.eq_ignore_ascii_case("ok") => false,
            found => {
                tracing::warn!("search index damaged ({found:?}): built again");
                self.set_setting(SEARCH_INDEX_REBUILD, "headers:0")?;
                self.rebuild_search_index()?;
                true
            }
        };
        self.db().execute("DELETE FROM settings WHERE key = ?1", [SEARCH_INDEX_CHECK])?;
        Ok(rebuilt)
    }

    /// The search index built again from the messages and their bodies, its own content not
    /// trusted (FTS5's 'rebuild' reads it back, keeping what it lost). In steps, the writer let
    /// go between them; where it stands is kept in [`SEARCH_INDEX_REBUILD`] (`headers:<id>` or
    /// `bodies:<id>`), so a quit resumes it at the next start. Messages synced meanwhile index
    /// themselves: a step only adds rows still missing, of messages still there.
    fn rebuild_search_index(&self) -> Result<()> {
        let (phase, mut after) = match self.setting(SEARCH_INDEX_REBUILD)?.as_deref().and_then(|v| v.split_once(':')) {
            Some((phase, at)) => (phase.to_string(), at.parse::<i64>().unwrap_or(0)),
            None => ("headers".to_string(), 0),
        };
        if phase == "headers" && after == 0 {
            // Made anew, in one transaction (its content may be what is damaged, which a DELETE
            // would need to read). The same table as the first migration's.
            let mut db = self.db();
            let tx = db.transaction()?;
            tx.execute_batch(
                "DROP TABLE IF EXISTS messages_fts;
                 CREATE VIRTUAL TABLE messages_fts USING fts5 (subject, sender, recipients, body, tokenize = 'unicode61 remove_diacritics 2');",
            )?;
            tx.commit()?;
        }
        if phase == "headers" {
            loop {
                // id, subject, sender's name and address, To and Cc.
                type Row = (i64, String, Option<String>, Option<String>, String, String);
                let rows: Vec<Row> = {
                    let db = self.read();
                    let mut st = db.prepare_cached(
                        "SELECT id, subject, from_name, from_addr, to_list, cc_list FROM messages WHERE id > ?1 ORDER BY id LIMIT ?2",
                    )?;
                    st.query_map(params![after, STEP_ROWS as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                        .collect::<std::result::Result<_, _>>()?
                };
                let Some(last) = rows.last().map(|r| r.0) else { break };
                let mut db = self.db();
                let tx = db.transaction()?;
                {
                    let mut st = tx.prepare_cached(
                        "INSERT INTO messages_fts (rowid, subject, sender, recipients, body) SELECT ?1, ?2, ?3, ?4, ''
                         WHERE EXISTS (SELECT 1 FROM messages WHERE id = ?1) AND NOT EXISTS (SELECT 1 FROM messages_fts WHERE rowid = ?1)",
                    )?;
                    for (id, subject, from_name, from_addr, to, cc) in &rows {
                        let sender = from_addr.as_ref().map(|email| format_address(&Address { name: from_name.clone(), email: email.clone() })).unwrap_or_default();
                        let list = |json: &str| serde_json::from_str::<Vec<Address>>(json).unwrap_or_default();
                        let recipients = list(to).iter().chain(&list(cc)).map(format_address).collect::<Vec<_>>().join(" ");
                        st.execute(params![id, subject, sender, recipients])?;
                    }
                }
                tx.execute(SET_SETTING, params![SEARCH_INDEX_REBUILD, format!("headers:{last}")])?;
                tx.commit()?;
                after = last;
            }
            after = 0;
        }
        // Then each stored body's text, as when it was downloaded.
        loop {
            let (last, texts) = {
                let db = self.read();
                let step = body_step(&db, after, BODY_STEP_BYTES)?;
                let Some(&last) = step.last() else { break };
                // One raw message in memory at a time: only its text is kept.
                let mut st = db.prepare_cached("SELECT raw FROM bodies WHERE message_id = ?1")?;
                let mut texts: Vec<(i64, String)> = Vec::with_capacity(step.len());
                for id in step {
                    if let Some(raw) = st.query_row([id], |r| r.get::<_, Vec<u8>>(0)).optional()? {
                        texts.push((id, crate::mime::body_info(&raw).text));
                    }
                }
                (last, texts)
            };
            let mut db = self.db();
            let tx = db.transaction()?;
            {
                let mut st = tx.prepare_cached("UPDATE messages_fts SET body = ?2 WHERE rowid = ?1")?;
                for (id, text) in &texts {
                    st.execute(params![id, text])?;
                }
            }
            tx.execute(SET_SETTING, params![SEARCH_INDEX_REBUILD, format!("bodies:{last}")])?;
            tx.commit()?;
            after = last;
        }
        self.db().execute("DELETE FROM settings WHERE key = ?1", [SEARCH_INDEX_REBUILD])?;
        Ok(())
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        crate::perf::profile(&mut conn);
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -65536; PRAGMA temp_store = MEMORY; PRAGMA mmap_size = 268435456;
             PRAGMA journal_size_limit = 67108864;",
        )?;
        register_functions(&conn)?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            conn.execute_batch(&format!("BEGIN; {sql}; PRAGMA user_version = {}; COMMIT;", i + 1))?;
        }
        // Query planner statistics, without waiting (time-limited).
        conn.execute_batch("PRAGMA optimize = 0x10002;")?;
        Ok(Store {
            conn: Mutex::new(conn),
            readers: Vec::new(),
            next_reader: Default::default(),
            last_shrink: Default::default(),
            flag_marks: Default::default(),
            flag_clock: Default::default(),
        })
    }

    /// Run now and then (after a sync): SQLite refreshes its statistics if the tables changed a lot.
    pub fn optimize(&self) {
        if let Err(e) = self.db().execute_batch("PRAGMA optimize;") {
            tracing::debug!("PRAGMA optimize: {e}");
        }
        // Bodies read during sync fill the caches: give the memory back, at most hourly (emptying
        // them every cycle makes the next one read everything again).
        let now = now();
        let last = self.last_shrink.load(std::sync::atomic::Ordering::Relaxed);
        if now - last < SHRINK_EVERY {
            return;
        }
        self.last_shrink.store(now, std::sync::atomic::Ordering::Relaxed);
        let _ = self.db().execute_batch("PRAGMA shrink_memory;");
        for reader in &self.readers {
            let _ = reader.lock().unwrap_or_else(|e| e.into_inner()).execute_batch("PRAGMA shrink_memory;");
        }
    }

    /// Deleted rows leave their pages in the file, on SQLite's free list, until a VACUUM
    /// rewrites it: done when a quarter of the file at least, and 64 MiB, is wasted (free pages,
    /// or a file longer than the base: Windows does not shorten a file other connections have
    /// mapped). Returns the bytes given back, if it ran. Reads go on meanwhile, without mapping.
    pub fn compact_if_wasteful(&self) -> Result<Option<u64>> {
        self.compact_above(COMPACT_MIN_FREE)
    }

    fn compact_above(&self, min_free: i64) -> Result<Option<u64>> {
        let db = self.db();
        let Some(path) = db.path().filter(|p| !p.is_empty()).map(str::to_string) else { return Ok(None) };
        let file_len = || std::fs::metadata(&path).map_or(0, |m| m.len() as i64);
        let pragma = |name: &str| db.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0));
        let (pages, free, page_size) = (pragma("page_count")?, pragma("freelist_count")?, pragma("page_size")?);
        let before = file_len();
        let wasted = free * page_size + (before - pages * page_size).max(0);
        if wasted * COMPACT_FREE_SHARE < before || wasted < min_free {
            return Ok(None);
        }
        // Windows does not shorten a file other connections have mapped: the readers let go of
        // theirs, then serve reads unmapped during the VACUUM (held, they would stall every list
        // and search for its whole length). Never waited for (a reader may be waiting for the
        // writer held here): busy, the compaction is for another time.
        {
            let mut readers = Vec::new();
            for reader in &self.readers {
                match reader.try_lock() {
                    Ok(r) => readers.push(r),
                    Err(_) => return Ok(None),
                }
            }
            for r in &readers {
                r.execute_batch("PRAGMA mmap_size = 0;")?;
            }
        }
        // The copy VACUUM builds goes to a temporary file, not to memory (the size of the base).
        db.execute_batch("PRAGMA temp_store = FILE;")?;
        let done = db.execute_batch("VACUUM;");
        let _ = db.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA wal_checkpoint(TRUNCATE);");
        drop(db);
        for reader in &self.readers {
            let _ = reader.lock().unwrap_or_else(|e| e.into_inner()).execute_batch(&format!("PRAGMA mmap_size = {READER_MMAP};"));
        }
        done?;
        let freed = before.saturating_sub(file_len()).max(0) as u64;
        // A file left as long would be rewritten whole at every launch: told as a failure, which
        // waits a week.
        if freed == 0 {
            return Err(Error::Other("the database file was not shortened".into()));
        }
        Ok(Some(freed))
    }

    #[track_caller]
    fn db(&self) -> Conn<'_> {
        let (started, at) = (std::time::Instant::now(), std::panic::Location::caller());
        // A panic mid-query leaves the connection consistent (SQLite rolls the transaction back),
        // so the poisoned lock can be recovered.
        let guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        crate::perf::Held::new(guard, started.elapsed(), at, "writer")
    }

    /// A free reader, else the next one in turn. Each query sees everything committed before it.
    #[track_caller]
    fn read(&self) -> Conn<'_> {
        if self.readers.is_empty() {
            return self.db();
        }
        let (started, at) = (std::time::Instant::now(), std::panic::Location::caller());
        let guard = self.readers.iter().find_map(|r| r.try_lock().ok()).unwrap_or_else(|| {
            let i = self.next_reader.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.readers.len();
            self.readers[i].lock().unwrap_or_else(|e| e.into_inner())
        });
        crate::perf::Held::new(guard, started.elapsed(), at, "reader")
    }

    /// The database's size and each connection's page cache, for the diagnostics. Never waits:
    /// a connection in use is reported busy, and the sizes come from a free one.
    pub fn diagnostics(&self) -> StoreDiagnostics {
        let mut d = StoreDiagnostics::default();
        let all = std::iter::once(("writer", &self.conn)).chain(self.readers.iter().map(|r| ("reader", r)));
        for (role, conn) in all {
            let Ok(conn) = conn.try_lock() else {
                d.connections.push(ConnectionCache { role, busy: true, cache_used_bytes: None, cache_hits: None, cache_misses: None });
                continue;
            };
            if d.page_count.is_none() {
                let pragma = |name: &str| conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get::<_, i64>(0)).ok();
                d.page_count = pragma("page_count");
                d.page_size = pragma("page_size");
                d.freelist_count = pragma("freelist_count");
                d.cache_size = pragma("cache_size");
                if let Some(path) = conn.path().filter(|p| !p.is_empty()) {
                    let size = |p: &str| std::fs::metadata(p).map_or(0, |m| m.len());
                    d.file_bytes = size(path);
                    d.wal_bytes = size(&format!("{path}-wal"));
                    d.path = Some(path.to_string());
                }
            }
            let status = |op| db_status(&conn, op);
            d.connections.push(ConnectionCache {
                role,
                busy: false,
                cache_used_bytes: status(rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED),
                cache_hits: status(rusqlite::ffi::SQLITE_DBSTATUS_CACHE_HIT),
                cache_misses: status(rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS),
            });
        }
        d
    }

    // ── Accounts ───────────────────────────────────────────────────────────

    pub fn insert_account(&self, a: &NewAccount) -> Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO accounts (name, email, username, imap_host, imap_port, imap_security,
                                   smtp_host, smtp_port, smtp_security, auth, oauth_provider)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                a.name, a.email, a.username,
                a.imap.host, a.imap.port, a.imap.security.as_str(),
                a.smtp.host, a.smtp.port, a.smtp.security.as_str(),
                match a.auth { NewAuth::Password => "password", NewAuth::OAuth2 { .. } => "oauth2" },
                match &a.auth { NewAuth::Password => None, NewAuth::OAuth2 { provider, .. } => Some(provider.as_str()) },
            ],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT {ACCOUNT_COLS} FROM accounts ORDER BY position IS NULL, position, id"))?;
        let rows = st.query_map([], account_from_row)?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn account(&self, id: i64) -> Result<Account> {
        self.read()
            .query_row(&format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE id = ?1"), [id], account_from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(crate::tr!("compte {id}", "account {id}")))
    }

    /// Its rows go with it (foreign keys); its settings too, which an account given the same id
    /// later would otherwise take over. The people known only through it leave the composer's
    /// suggestions: `contacts` is counted again from the mail left.
    /// Its messages go in short steps first: each one deleted unindexes its text, and a large
    /// account in one transaction would hold the writer (every other account's writes) for a minute.
    pub fn delete_account(&self, id: i64) -> Result<()> {
        let theirs = self.removal_correspondents(id)?;
        loop {
            let mut db = self.db();
            let tx = db.transaction()?;
            quiet_handled(&tx, true)?;
            let n = tx.execute("DELETE FROM messages WHERE id IN (SELECT id FROM messages WHERE account_id = ?1 LIMIT ?2)", params![id, STEP_ROWS as i64])?;
            quiet_handled(&tx, false)?;
            tx.commit()?;
            if n == 0 {
                break;
            }
        }
        // Their counts again, from the mail left (as `contacts` counts: a message from them, or
        // one sent to them, is seen; the latter written), or gone with nothing left. In the same
        // transaction as the account's own row: cut before, the removal is tried again whole.
        let written_to = self.written_to()?;
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        tx.execute("DELETE FROM settings WHERE key IN ('sync_every:' || ?1, 'screener_release:' || ?1, 'single_connection:' || ?1, 'single_connection_offer:' || ?1)", [id])?;
        tx.execute("DELETE FROM settings WHERE substr(key, 1, length('restore_folder:' || ?1 || ':')) = 'restore_folder:' || ?1 || ':'", [id])?;
        {
            let mut from = tx.prepare_cached("SELECT COUNT(*), MAX(date) FROM messages WHERE lower(from_addr) = ?1")?;
            let mut forget = tx.prepare_cached("DELETE FROM contacts WHERE email = ?1")?;
            let mut recount = tx.prepare_cached("UPDATE contacts SET written = ?2, seen = ?3, last = MAX(?4, 0) WHERE email = ?1")?;
            for email in &theirs {
                let (received, last): (i64, Option<i64>) = from.query_row([email], |r| Ok((r.get(0)?, r.get(1)?)))?;
                let (written, last_written) = written_to.get(email).copied().unwrap_or((0, 0));
                if received + written == 0 {
                    forget.execute([email])?;
                } else {
                    recount.execute(params![email, written, received + written, last.unwrap_or(0).max(last_written)])?;
                }
            }
        }
        tx.execute("DELETE FROM settings WHERE key = 'removing_contacts:' || ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// The account's correspondents, to leave the suggestions unless another account knows them
    /// too: checked one by one afterwards rather than every account's mail counted again. Kept
    /// until the removal ends, so one cut halfway still forgets those whose mail went first.
    fn removal_correspondents(&self, id: i64) -> Result<HashSet<String>> {
        let key = format!("removing_contacts:{id}");
        let mut theirs = self.correspondents(id)?;
        if let Some(saved) = self.setting(&key)? {
            theirs.extend(serde_json::from_str::<Vec<String>>(&saved).unwrap_or_default());
        }
        self.set_setting(&key, &serde_json::to_string(&theirs)?)?;
        Ok(theirs)
    }

    /// Those every account wrote to: how many times, and the latest.
    fn written_to(&self) -> Result<HashMap<String, (i64, i64)>> {
        let db = self.read();
        // A Sent folder's, or (Gmail) labelled Sent.
        let sent = in_folder_where("m", "rb.role = 'sent'");
        let mut st = db.prepare(&format!(
            "SELECT email, COUNT(*), MAX(date) FROM (
                 SELECT lower(json_extract(j.value, '$.email')) AS email, m.date AS date FROM messages m, json_each(m.to_list) j WHERE {sent}
                 UNION ALL
                 SELECT lower(json_extract(j.value, '$.email')), m.date FROM messages m, json_each(m.cc_list) j WHERE {sent}
             ) WHERE email IS NOT NULL GROUP BY email"
        ))?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?))))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Addresses of an account's mail: its senders, and those it wrote to.
    fn correspondents(&self, account_id: i64) -> Result<HashSet<String>> {
        let db = self.read();
        let sent = in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'sent'");
        let mut st = db.prepare(&format!(
            "SELECT lower(from_addr) FROM messages WHERE account_id = ?1 AND from_addr IS NOT NULL
             UNION SELECT lower(json_extract(j.value, '$.email')) FROM messages m, json_each(m.to_list) j WHERE m.account_id = ?1 AND {sent}
             UNION SELECT lower(json_extract(j.value, '$.email')) FROM messages m, json_each(m.cc_list) j WHERE m.account_id = ?1 AND {sent}
             UNION SELECT email FROM correspondents WHERE account_id = ?1"
        ))?;
        let rows = st.query_map([account_id], |r| r.get::<_, Option<String>>(0))?.filter_map(|r| r.transpose()).collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn set_identities(&self, id: i64, identities: &[Identity]) -> Result<()> {
        self.db().execute("UPDATE accounts SET identities = ?2 WHERE id = ?1", params![id, serde_json::to_string(identities)?])?;
        Ok(())
    }

    pub fn set_signature(&self, id: i64, signature: &str) -> Result<()> {
        self.db().execute("UPDATE accounts SET signature = ?2 WHERE id = ?1", params![id, signature])?;
        Ok(())
    }

    pub fn coverage(&self, account_id: i64) -> Result<Coverage> {
        let db = self.read();
        let (local, bodies): (i64, i64) = db.query_row(
            "SELECT COUNT(*), COALESCE(SUM(has_body), 0) FROM messages WHERE account_id = ?1 AND uid IS NOT NULL",
            [account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // Unknown while any folder has never been synced.
        let (server, unknown): (i64, i64) = db.query_row(
            "SELECT COALESCE(SUM(server_count), 0), SUM(server_count IS NULL) FROM mailboxes
             WHERE account_id = ?1 AND selectable = 1 AND is_label = 0",
            [account_id],
            |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )?;
        Ok(Coverage { account_id, local, server: (unknown == 0).then_some(server), bodies })
    }

    /// The mailbox space the server reported (bytes), read now.
    pub fn set_quota(&self, id: i64, used: i64, limit: i64) -> Result<()> {
        self.db().execute(
            "UPDATE accounts SET quota_used = ?2, quota_limit = ?3, quota_at = ?4 WHERE id = ?1",
            params![id, used, limit, now()],
        )?;
        Ok(())
    }

    pub fn set_color(&self, id: i64, color: Option<&str>) -> Result<()> {
        self.db().execute("UPDATE accounts SET color = ?2 WHERE id = ?1", params![id, color])?;
        Ok(())
    }

    pub fn set_screener_since(&self, id: i64, since: Option<i64>) -> Result<()> {
        self.db().execute("UPDATE accounts SET screener_since = ?2 WHERE id = ?1", params![id, since])?;
        Ok(())
    }

    pub fn set_label(&self, id: i64, label: Option<&str>) -> Result<()> {
        self.db().execute("UPDATE accounts SET label = ?2 WHERE id = ?1", params![id, label])?;
        Ok(())
    }

    /// Marks every message of the account read, or only the inbox's. Returns the folders that had
    /// unread ones (IMAP names).
    pub fn mark_account_seen(&self, account_id: i64, inbox_only: bool) -> Result<Vec<String>> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let boxes: Vec<String> = {
            let mut st = tx.prepare(&format!(
                "SELECT DISTINCT b.name FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
                 WHERE m.account_id = ?1 AND m.seen = 0 AND (?2 = 0 OR {})",
                in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'inbox'")
            ))?;
            st.query_map(params![account_id, inbox_only], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        // Messages on their way to another folder (no UID yet) arrive after the folder's cutoff:
        // the change waits on them and leaves once they are there, as for `set_flags_local`.
        let inbox = in_folder_where("messages", "rb.account_id = ?1 AND rb.role = 'inbox'");
        tx.execute(
            &format!(
                "UPDATE messages SET pending_flags = json_set(COALESCE(pending_flags, '{{}}'), '$.seen', json('true'))
                 WHERE account_id = ?1 AND seen = 0 AND uid IS NULL AND (?2 = 0 OR {inbox})"
            ),
            params![account_id, inbox_only],
        )?;
        // Noted before the write, as for `set_flags_local`: a flag read under way does not make
        // them unread again.
        let places: Vec<(i64, u32)> = {
            let mut st = tx.prepare(&format!(
                "SELECT mailbox_id, uid FROM messages WHERE account_id = ?1 AND seen = 0 AND uid IS NOT NULL AND (?2 = 0 OR {inbox})"
            ))?;
            st.query_map(params![account_id, inbox_only], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        self.note_flag_changes(&places);
        tx.execute(
            &format!("UPDATE messages SET seen = 1 WHERE account_id = ?1 AND seen = 0 AND (?2 = 0 OR {inbox})"),
            params![account_id, inbox_only],
        )?;
        tx.commit()?;
        Ok(boxes)
    }

    /// UIDs in All Mail (`all`) of the unread messages labelled inbox (Gmail's labels).
    pub fn unread_inbox_uids(&self, account_id: i64, all: i64) -> Result<Vec<u32>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT uid FROM messages WHERE mailbox_id = ?2 AND seen = 0 AND uid IS NOT NULL AND {} ORDER BY uid",
            in_folder_where("messages", "rb.account_id = ?1 AND rb.role = 'inbox'")
        ))?;
        Ok(st.query_map(params![account_id, all], |r| r.get(0))?.collect::<Result<_, _>>()?)
    }

    pub fn account_stats(&self, account_id: i64, email: &str, now: i64) -> Result<AccountStats> {
        let db = self.read();
        // One message per Message-ID (Gmail files one message in several folders), read once into
        // a narrow temporary table, then counted several ways. Drafts are left out before
        // deduplicating: otherwise a message whose lowest copy is a draft would not be counted.
        db.execute_batch("DROP TABLE IF EXISTS temp.stats_u")?;
        // Dropped on every path, early errors included: the reader connection lives for days.
        struct TempTable<'a>(&'a Connection);
        impl Drop for TempTable<'_> {
            fn drop(&mut self) {
                let _ = self.0.execute_batch("DROP TABLE IF EXISTS temp.stats_u");
            }
        }
        let _temp = TempTable(&db);
        db.execute(
            "CREATE TEMP TABLE stats_u AS
             SELECT COALESCE(m.received_at, m.date) AS at, lower(COALESCE(m.from_addr, '')) = lower(?2) AS mine,
                    m.seen, m.flagged, m.has_attachments, m.bulk, m.size, m.from_addr, m.from_name, m.to_list
             FROM messages m
             WHERE m.id IN (
                     SELECT MIN(x.id) FROM messages x JOIN mailboxes xb ON xb.id = x.mailbox_id
                     WHERE x.account_id = ?1 AND x.message_id IS NOT NULL AND COALESCE(xb.role, '') != 'drafts'
                       AND NOT EXISTS (SELECT 1 FROM labels xl JOIN mailboxes xlb ON xlb.id = xl.mailbox_id WHERE xl.message_id = x.id AND xlb.role = 'drafts')
                     GROUP BY x.message_id
                     UNION ALL
                     SELECT x.id FROM messages x JOIN mailboxes xb ON xb.id = x.mailbox_id
                     WHERE x.account_id = ?1 AND x.message_id IS NULL AND COALESCE(xb.role, '') != 'drafts'
                       AND NOT EXISTS (SELECT 1 FROM labels xl JOIN mailboxes xlb ON xlb.id = xl.mailbox_id WHERE xl.message_id = x.id AND xlb.role = 'drafts'))",
            params![account_id, email],
        )?;
        // The following queries keep their parameters (account, address): `?1 = ?1` consumes them.
        let base = "WITH u AS (SELECT * FROM temp.stats_u WHERE ?1 = ?1 AND ?2 = ?2)";
        let month_ago = now - 30 * 86_400;
        let (total, received, sent, unread, flagged, with_attachments, bulk, bytes, received_30d, sent_30d, oldest, newest) =
            db.query_row(
                &format!(
                    "{base} SELECT COUNT(*), SUM(NOT mine), SUM(mine), SUM(seen = 0), SUM(flagged), SUM(has_attachments),
                            SUM(bulk), SUM(size), SUM(NOT mine AND at >= ?3), SUM(mine AND at >= ?3), MIN(at), MAX(at)
                     FROM u"
                ),
                params![account_id, email, month_ago],
                |r| {
                    let n = |i: usize| r.get::<_, Option<i64>>(i).map(|v| v.unwrap_or(0));
                    Ok((n(0)?, n(1)?, n(2)?, n(3)?, n(4)?, n(5)?, n(6)?, n(7)?, n(8)?, n(9)?, r.get(10)?, r.get(11)?))
                },
            )?;

        let mut per_month: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut st = db.prepare(&format!(
                "{base} SELECT strftime('%Y-%m', at, 'unixepoch', 'localtime') AS mo, SUM(NOT mine), SUM(mine)
                 FROM u WHERE at >= ?3 GROUP BY mo"
            ))?;
            let since = now - 400 * 86_400;
            for row in st.query_map(params![account_id, email, since], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))? {
                let (mo, rec, snt) = row?;
                per_month.insert(mo, (rec, snt));
            }
        }
        let months = last_months(now, 12)
            .into_iter()
            .map(|month| {
                let (received, sent) = per_month.get(&month).copied().unwrap_or((0, 0));
                MonthStat { month, received, sent }
            })
            .collect();

        let mut weekdays = vec![0i64; 7];
        let mut hours = vec![0i64; 24];
        {
            let mut st = db.prepare(&format!(
                "{base} SELECT CAST(strftime('%w', at, 'unixepoch', 'localtime') AS INTEGER),
                               CAST(strftime('%H', at, 'unixepoch', 'localtime') AS INTEGER), COUNT(*)
                 FROM u WHERE NOT mine GROUP BY 1, 2"
            ))?;
            for row in st.query_map(params![account_id, email], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
                let (wd, hour, n) = row?;
                // SQLite: 0 = Sunday. The week starts on Monday.
                weekdays[((wd + 6) % 7) as usize] += n;
                hours[hour.clamp(0, 23) as usize] += n;
            }
        }

        let counted = |sql: &str| -> Result<Vec<CountedAddress>> {
            let mut st = db.prepare(sql)?;
            let rows = st
                .query_map(params![account_id, email], |r| Ok(CountedAddress { email: r.get(0)?, name: r.get(1)?, count: r.get(2)? }))?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        };
        let top_senders = counted(&format!(
            "{base} SELECT lower(from_addr), MAX(from_name), COUNT(*) AS n FROM u
             WHERE NOT mine AND from_addr IS NOT NULL GROUP BY lower(from_addr) ORDER BY n DESC LIMIT 10"
        ))?;
        let top_recipients = counted(&format!(
            "{base} SELECT lower(json_extract(j.value, '$.email')) AS e, MAX(json_extract(j.value, '$.name')), COUNT(*) AS n
             FROM u, json_each(u.to_list) j WHERE mine GROUP BY e ORDER BY n DESC LIMIT 10"
        ))?;
        drop(_temp);
        drop(db);

        let folders = self
            .list_mailboxes(account_id)?
            .into_iter()
            .filter(|m| m.selectable)
            .map(|m| FolderStat { id: m.id, name: m.display_name, role: m.role, total: m.total, unread: m.unread })
            .collect();

        Ok(AccountStats {
            total, received, sent, unread, flagged, with_attachments, bulk, bytes, received_30d, sent_30d, oldest, newest,
            months, weekdays, hours, top_senders, top_recipients, folders,
        })
    }

    /// Avatar found by facteur (provider's photo, domain's logo): left alone when the user
    /// picked one.
    pub fn set_avatar(&self, id: i64, avatar: &str) -> Result<()> {
        self.db().execute("UPDATE accounts SET avatar = ?2 WHERE id = ?1 AND avatar_custom = 0", params![id, avatar])?;
        Ok(())
    }

    /// Avatar set on the user's request. `custom`: picked by the user (uploaded, or none for
    /// initials); otherwise found again by facteur, which may replace it later.
    pub fn replace_avatar(&self, id: i64, avatar: Option<&str>, custom: bool) -> Result<()> {
        self.db().execute("UPDATE accounts SET avatar = ?2, avatar_custom = ?3 WHERE id = ?1", params![id, avatar, custom])?;
        Ok(())
    }

    pub fn set_last_sync(&self, id: i64) -> Result<()> {
        self.db().execute("UPDATE accounts SET last_sync = ?2 WHERE id = ?1", params![id, now()])?;
        Ok(())
    }

    // ── Folders ────────────────────────────────────────────────────────────

    pub fn upsert_mailbox(
        &self,
        account_id: i64,
        name: &str,
        delimiter: Option<&str>,
        role: Option<Role>,
        selectable: bool,
    ) -> Result<i64> {
        let was_selectable: Option<bool> = self
            .read()
            .query_row("SELECT selectable FROM mailboxes WHERE account_id = ?1 AND name = ?2", params![account_id, name], |r| r.get(0))
            .optional()?;
        let db = self.db();
        db.execute(
            "INSERT INTO mailboxes (account_id, name, delimiter, role, selectable)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (account_id, name) DO UPDATE
                SET delimiter = excluded.delimiter, role = excluded.role, selectable = excluded.selectable
                WHERE (delimiter, role, selectable) IS NOT (excluded.delimiter, excluded.role, excluded.selectable)",
            params![account_id, name, delimiter, role.map(Role::as_str), selectable],
        )?;
        let id = db.query_row("SELECT id FROM mailboxes WHERE account_id = ?1 AND name = ?2", params![account_id, name], |r| r.get(0))?;
        drop(db);
        // Deleted on the server while it held subfolders: only its name is left (\Noselect), its
        // messages are gone. Selectable again some day, it starts over.
        if was_selectable == Some(true) && !selectable {
            self.clear_mailbox(id)?;
            self.db().execute("UPDATE mailboxes SET uidvalidity = NULL, uidnext = NULL WHERE id = ?1", [id])?;
        }
        Ok(id)
    }

    /// Folders as stored, by IMAP name: (id, delimiter, role, selectable). Lets the sync write
    /// only what the server listing changed.
    pub fn mailbox_listing(&self, account_id: i64) -> Result<HashMap<String, StoredMailbox>> {
        let db = self.read();
        let mut st = db.prepare("SELECT name, id, delimiter, role, selectable FROM mailboxes WHERE account_id = ?1")?;
        let rows = st
            .query_map([account_id], |r| {
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get::<_, Option<String>>(3)?.as_deref().and_then(Role::parse), r.get(4)?)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Caution: every message of a removed folder goes with it. An empty `keep` is refused
    /// (an empty or truncated LIST must never wipe the cache).
    pub fn retain_mailboxes(&self, account_id: i64, keep: &[i64]) -> Result<usize> {
        if keep.is_empty() {
            return Ok(0);
        }
        let ids = keep.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let going: Vec<i64> = {
            let db = self.read();
            let mut st = db.prepare(&format!("SELECT id FROM mailboxes WHERE account_id = ?1 AND id NOT IN ({ids})"))?;
            st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        for id in going {
            self.shed_mailbox_rows(id)?;
        }
        let mut db = self.db();
        let tx = db.transaction()?;
        let gone: Vec<i64> = {
            let mut st = tx.prepare(&format!("SELECT id FROM mailboxes WHERE account_id = ?1 AND id NOT IN ({ids})"))?;
            st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        // Their messages first, the way that hands a body they hold to the copies that stay
        // (Gmail labels): the cascade would take it from them.
        for id in &gone {
            delete_mailbox_rows(&tx, *id)?;
        }
        let n = tx.execute(&format!("DELETE FROM mailboxes WHERE account_id = ?1 AND id NOT IN ({ids})"), [account_id])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn list_mailboxes(&self, account_id: i64) -> Result<Vec<Mailbox>> {
        let db = self.read();
        // Unread counts read from the partial index of unread messages only; totals from the
        // narrowest index on mailbox_id. No pass over the rows of the whole account. A label's
        // messages are counted through the labels table. Arrivals the screener has not seen yet
        // are not counted as unread: shown, then filed away, they would make the count jump.
        let mut st = db.prepare(
            "SELECT b.id, b.account_id, b.name, b.delimiter, b.role, b.selectable,
                    (SELECT COUNT(*) FROM messages m WHERE m.mailbox_id = b.id AND m.seen = 0)
                      - (SELECT COUNT(*) FROM messages m WHERE m.unscreened = 1 AND m.mailbox_id = b.id AND m.seen = 0)
                      + (SELECT COUNT(*) FROM labels l JOIN messages m ON m.id = l.message_id WHERE l.mailbox_id = b.id AND m.seen = 0 AND m.unscreened = 0),
                    (SELECT COUNT(*) FROM messages m WHERE m.mailbox_id = b.id)
                      + (SELECT COUNT(*) FROM labels l WHERE l.mailbox_id = b.id),
                    b.server_count, b.count_base,
                    (SELECT COUNT(*) FROM (SELECT thread_id FROM messages WHERE mailbox_id = b.id
                                           UNION SELECT m.thread_id FROM labels l JOIN messages m ON m.id = l.message_id
                                                 WHERE l.mailbox_id = b.id)),
                    (SELECT COUNT(*) FROM (SELECT thread_id FROM messages WHERE mailbox_id = b.id AND seen = 0 AND unscreened = 0
                                           UNION SELECT m.thread_id FROM labels l JOIN messages m ON m.id = l.message_id
                                                 WHERE l.mailbox_id = b.id AND m.seen = 0 AND m.unscreened = 0)),
                    b.is_label
             FROM mailboxes b
             WHERE b.account_id = ?1
             ORDER BY CASE b.role
                 WHEN 'inbox' THEN 0 WHEN 'screener' THEN 1 WHEN 'flagged' THEN 2 WHEN 'snoozed' THEN 3
                 WHEN 'drafts' THEN 4 WHEN 'sent' THEN 5 WHEN 'archive' THEN 6 WHEN 'all' THEN 7
                 WHEN 'refused' THEN 8 WHEN 'junk' THEN 9 WHEN 'trash' THEN 10 ELSE 11 END, b.name",
        )?;
        let rows = st
            .query_map([account_id], |r| {
                let name: String = r.get(2)?;
                let delimiter: Option<String> = r.get(3)?;
                let leaf = match &delimiter {
                    Some(d) if !d.is_empty() => name.rsplit(d.as_str()).next().unwrap_or(&name).to_string(),
                    _ => name.clone(),
                };
                Ok(Mailbox {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    display_name: utf7::decode(&leaf),
                    name,
                    delimiter,
                    role: r.get::<_, Option<String>>(4)?.as_deref().and_then(Role::parse),
                    selectable: r.get(5)?,
                    unread: r.get(6)?,
                    total: r.get(7)?,
                    // The server's count, moved by the local changes made since it was read
                    // (moves, deletes not yet synced).
                    server_total: r.get::<_, Option<i64>>(8)?.map(|count| match r.get::<_, Option<i64>>(9) {
                        Ok(Some(base)) => (count + r.get::<_, i64>(7).unwrap_or(base) - base).max(0),
                        _ => count,
                    }),
                    threads: r.get(10)?,
                    unread_threads: r.get(11)?,
                    user_label: false,
                })
                .and_then(|mut m: Mailbox| {
                    let folder = crate::provider::LabelFolder { id: m.id, name: m.name.clone(), role: m.role };
                    m.user_label = r.get::<_, bool>(12)? && crate::provider::user_label(&folder);
                    Ok(m)
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn mailbox_states(&self, account_id: i64) -> Result<Vec<MailboxState>> {
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT {STATE_COLS} FROM mailboxes WHERE account_id = ?1"))?;
        let rows = st.query_map([account_id], state_from_row)?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn mailbox_state(&self, id: i64) -> Result<MailboxState> {
        self.read()
            .query_row(&format!("SELECT {STATE_COLS} FROM mailboxes WHERE id = ?1"), [id], state_from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(crate::tr!("dossier {id}", "folder {id}")))
    }

    pub fn mailbox_by_role(&self, account_id: i64, role: Role) -> Result<Option<MailboxState>> {
        Ok(self
            .read()
            .query_row(
                &format!("SELECT {STATE_COLS} FROM mailboxes WHERE account_id = ?1 AND role = ?2"),
                params![account_id, role.as_str()],
                state_from_row,
            )
            .optional()?)
    }

    /// Cutoff for "everything that is in the folder now" (empty the trash, mark all as read):
    /// UIDs below it. The cache can hold UIDs at or above the last UIDNEXT seen (headers added
    /// by a later search or partial sync), so it is never lower than the highest local UID + 1.
    pub fn uid_cutoff(&self, mailbox_id: i64) -> Result<Option<u32>> {
        let (next, max): (Option<u32>, Option<u32>) = self.read().query_row(
            "SELECT uidnext, (SELECT MAX(uid) FROM messages WHERE mailbox_id = ?1) FROM mailboxes WHERE id = ?1",
            [mailbox_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(match (next, max.map(|m| m.saturating_add(1))) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        })
    }

    pub fn save_mailbox_state(&self, s: &MailboxState) -> Result<()> {
        self.db().execute(
            // The window only widens into the past: a sync started before a "load older" must
            // not shrink it with the state it read earlier.
            "UPDATE mailboxes SET uidvalidity = ?2, uidnext = ?3, highestmodseq = ?4,
                    sync_from_uid = CASE WHEN sync_from_uid IS NULL OR ?5 IS NULL THEN ?5 ELSE MIN(sync_from_uid, ?5) END,
                    server_count = ?6
             WHERE id = ?1",
            params![s.id, s.uidvalidity, s.uidnext, s.highestmodseq.map(|v| v as i64), s.sync_from_uid, s.server_count],
        )?;
        Ok(())
    }

    /// The account's provider: as its server announced it, else Gmail when the account has an All
    /// Mail folder (before the first connection).
    pub fn provider(&self, account_id: i64) -> Result<crate::provider::Provider> {
        use crate::provider::Provider;
        let stored: Option<String> = self
            .read()
            .query_row("SELECT provider FROM accounts WHERE id = ?1", [account_id], |r| r.get(0))
            .optional()?
            .flatten();
        match stored.as_deref().and_then(Provider::parse) {
            Some(p) => Ok(p),
            None => Ok(if self.has_all_mail(account_id)? { Provider::Gmail } else { Provider::Imap }),
        }
    }

    pub fn set_provider(&self, account_id: i64, provider: crate::provider::Provider) -> Result<()> {
        self.db().execute(
            "UPDATE accounts SET provider = ?2 WHERE id = ?1 AND provider IS NOT ?2",
            params![account_id, provider.as_str()],
        )?;
        Ok(())
    }

    /// The account keeps Gmail's labels as labels (converted): only All Mail, the bin and spam
    /// are synced.
    pub fn labels_model(&self, account_id: i64) -> Result<bool> {
        Ok(self
            .read()
            .query_row("SELECT labels_model FROM accounts WHERE id = ?1", [account_id], |r| r.get(0))
            .optional()?
            .unwrap_or(false))
    }

    /// Gmail: every selectable folder but All Mail, the bin and spam is a label. Run at each
    /// sync of a converted account, for folders created since.
    pub fn mark_label_folders(&self, account_id: i64) -> Result<()> {
        self.db().execute(
            "UPDATE mailboxes SET is_label = 1
             WHERE account_id = ?1 AND is_label = 0 AND selectable = 1 AND COALESCE(role, '') NOT IN ('all', 'trash', 'junk')",
            [account_id],
        )?;
        Ok(())
    }

    /// Converts a Gmail account to labels: each copy of a message in a label folder becomes a
    /// label of its All Mail row, then goes (its body, if it held it, passing to that row). The
    /// copies with no All Mail row (older than the synced window) go too: the sync fetches the
    /// inbox and the folders facteur keeps whole again, the rest comes back when asked for.
    /// Only with nothing on its way for the account (the queue names copies by folder and UID):
    /// `false` when it has to wait. A failed op does not hold it back: it waits for the user,
    /// and the folder and UID it names stay valid on Gmail's server.
    /// In steps, the writer let go between them (the copies' deletion unindexes their text): a
    /// conversion cut by a quit goes on at the next sync, its copies hidden meanwhile (a label
    /// lists its messages through `labels` only).
    pub fn convert_to_labels(&self, account_id: i64) -> Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let busy: bool =
            tx.query_row("SELECT EXISTS (SELECT 1 FROM pending_ops WHERE account_id = ?1 AND failed = 0)", [account_id], |r| r.get(0))?;
        let all: Option<i64> = tx
            .query_row("SELECT id FROM mailboxes WHERE account_id = ?1 AND role = 'all'", [account_id], |r| r.get(0))
            .optional()?;
        let Some(all) = all else { return Ok(false) };
        // Begun already: only hidden copies are left to delete, which no queued op names.
        let begun: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM mailboxes WHERE account_id = ?1 AND is_label = 1)", [account_id], |r| r.get(0))?;
        if busy && !begun {
            return Ok(false);
        }
        // Their counts as folders no longer hold: the folder listing gives them again.
        tx.execute(
            "UPDATE mailboxes SET is_label = 1, server_count = NULL, count_base = NULL
             WHERE account_id = ?1 AND selectable = 1 AND COALESCE(role, '') NOT IN ('all', 'trash', 'junk')",
            [account_id],
        )?;
        tx.execute(
            &format!(
                "INSERT OR IGNORE INTO labels (mailbox_id, message_id)
                 SELECT m.mailbox_id, o.id FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
                 JOIN messages o ON {SAME_COPY} AND o.mailbox_id = ?2 AND o.uid IS NOT NULL
                 WHERE b.account_id = ?1 AND b.is_label = 1"
            ),
            params![account_id, all],
        )?;
        // What was learned on the inbox copy (the local DKIM verdict, its category) stays.
        tx.execute(
            &format!(
                "UPDATE messages AS o SET signals = COALESCE(m.signals, o.signals), category = m.category
                 FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
                 WHERE b.account_id = ?1 AND b.role = 'inbox' AND o.mailbox_id = ?2 AND {SAME_COPY}"
            ),
            params![account_id, all],
        )?;
        tx.commit()?;
        drop(db);
        let mut n = 0;
        loop {
            let mut db = self.db();
            let tx = db.transaction()?;
            let doomed: Vec<i64> = {
                let mut st = tx.prepare(
                    "SELECT m.id FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id WHERE b.account_id = ?1 AND b.is_label = 1 LIMIT 500",
                )?;
                st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?
            };
            if doomed.is_empty() {
                tx.execute("UPDATE accounts SET labels_model = 1 WHERE id = ?1", [account_id])?;
                tx.commit()?;
                break;
            }
            n += delete_ids(&tx, &doomed)?;
            tx.commit()?;
        }
        tracing::info!("account {account_id} converted to Gmail labels: {n} copies became labels or left");
        Ok(true)
    }

    /// Gmail account whose All Mail is no longer listed (hidden from IMAP in Gmail's settings): its
    /// labels go back to being folders, synced from the start (their copies were dropped as labels).
    /// Converted again once All Mail is back.
    pub fn revert_labels_model(&self, account_id: i64) -> Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let gone: bool = tx.query_row(
            "SELECT labels_model = 1 AND NOT EXISTS (SELECT 1 FROM mailboxes WHERE account_id = ?1 AND role = 'all') FROM accounts WHERE id = ?1",
            [account_id],
            |r| r.get(0),
        )?;
        if !gone {
            return Ok(false);
        }
        tx.execute("DELETE FROM labels WHERE mailbox_id IN (SELECT id FROM mailboxes WHERE account_id = ?1)", [account_id])?;
        tx.execute(
            "UPDATE mailboxes SET is_label = 0, server_count = NULL, count_base = NULL, server_unseen = NULL, uidvalidity = NULL,
             uidnext = NULL, highestmodseq = NULL, sync_from_uid = NULL, checked_list_at = NULL, checked_flags_at = NULL
             WHERE account_id = ?1 AND is_label = 1",
            [account_id],
        )?;
        tx.execute("UPDATE accounts SET labels_model = 0 WHERE id = ?1", [account_id])?;
        tx.commit()?;
        Ok(true)
    }

    /// The folder's unread count on the server when it was last synced (STATUS UNSEEN).
    pub fn server_unseen(&self, mailbox_id: i64) -> Result<Option<u32>> {
        Ok(self
            .read()
            .query_row("SELECT server_unseen FROM mailboxes WHERE id = ?1", [mailbox_id], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn set_server_unseen(&self, mailbox_id: i64, unseen: Option<u32>) -> Result<()> {
        self.db().execute("UPDATE mailboxes SET server_unseen = ?2 WHERE id = ?1 AND server_unseen IS NOT ?2", params![mailbox_id, unseen])?;
        Ok(())
    }

    /// Gmail: messages of the folder whose Gmail thread is still to read, the newest first.
    pub fn uids_without_gmail_thread(&self, mailbox_id: i64, limit: usize) -> Result<Vec<u32>> {
        let db = self.read();
        let mut st = db.prepare_cached(
            "SELECT uid FROM messages WHERE mailbox_id = ?1 AND gm_thrid IS NULL AND uid IS NOT NULL ORDER BY uid DESC LIMIT ?2",
        )?;
        Ok(st.query_map(params![mailbox_id, limit as i64], |r| r.get(0))?.collect::<Result<_, _>>()?)
    }

    /// Gmail: the threads Gmail gives these messages of `mailbox_id` (UID, X-GM-THRID). Each Gmail
    /// thread keeps one local number: the one it was given before, else that of its oldest
    /// message here, unless another Gmail thread already has that one (the two were joined by
    /// their references): then a new one. Whether a message changed thread.
    pub fn set_gmail_threads(&self, account_id: i64, mailbox_id: i64, given: &[(u32, u64)]) -> Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        // Gmail thread → (message, its thread now, date).
        let mut groups: HashMap<i64, Vec<(i64, i64, i64)>> = HashMap::new();
        {
            let mut find = tx.prepare_cached("SELECT id, thread_id, date FROM messages WHERE mailbox_id = ?1 AND uid = ?2")?;
            for (uid, thrid) in given {
                // Stored as SQLite integers: the same 64 bits, read back the same way.
                let thrid = *thrid as i64;
                if let Some(row) = find.query_row(params![mailbox_id, uid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()? {
                    groups.entry(thrid).or_default().push(row);
                }
            }
        }
        // The oldest conversations first: the one a message heads keeps its number.
        let mut groups: Vec<(i64, Vec<(i64, i64, i64)>)> = groups.into_iter().collect();
        groups.sort_by_key(|(thrid, rows)| (rows.iter().map(|(_, _, date)| *date).min(), *thrid));
        let mut changed = false;
        for (thrid, rows) in groups {
            let known: Option<i64> = tx
                .query_row("SELECT thread_id FROM gmail_threads WHERE account_id = ?1 AND thrid = ?2", params![account_id, thrid], |r| r.get(0))
                .optional()?;
            let target = match known {
                Some(t) => t,
                None => {
                    let oldest = rows.iter().min_by_key(|(_, _, date)| *date).map_or(0, |(_, t, _)| *t);
                    let taken: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM gmail_threads WHERE account_id = ?1 AND thread_id = ?2)",
                        params![account_id, oldest],
                        |r| r.get(0),
                    )?;
                    let t = if taken || oldest == 0 { new_thread_id(&tx)? } else { oldest };
                    tx.execute("INSERT INTO gmail_threads (account_id, thrid, thread_id) VALUES (?1, ?2, ?3)", params![account_id, thrid, t])?;
                    t
                }
            };
            let mut set = tx.prepare_cached("UPDATE messages SET thread_id = ?2, gm_thrid = ?3 WHERE id = ?1")?;
            for (id, thread_id, _) in &rows {
                set.execute(params![id, target, thrid])?;
                changed |= *thread_id != target;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// A label's message count on the server (from the folder listing), against the messages
    /// carrying it here now: the shown total then follows local changes until the next listing.
    /// Whether that changed what is shown.
    pub fn set_label_count(&self, mailbox_id: i64, count: u32) -> Result<bool> {
        let n = self.db().execute(
            "UPDATE mailboxes SET server_count = ?2, count_base = (SELECT COUNT(*) FROM labels WHERE mailbox_id = ?1)
             WHERE id = ?1 AND is_label = 1
               AND (server_count IS NOT ?2 OR count_base IS NOT (SELECT COUNT(*) FROM labels WHERE mailbox_id = ?1))",
            params![mailbox_id, count],
        )?;
        Ok(n > 0)
    }

    /// Gmail's label names (as X-GM-LABELS gives them) of the account's label folders.
    pub fn label_names(&self, account_id: i64) -> Result<HashMap<String, i64>> {
        let db = self.read();
        let mut st = db.prepare("SELECT id, name, role FROM mailboxes WHERE account_id = ?1 AND is_label = 1")?;
        let mut out = HashMap::new();
        for row in st.query_map([account_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))? {
            let (id, name, role) = row?;
            let label = crate::provider::gmail_label(&name, role.as_deref().and_then(Role::parse));
            out.insert(crate::provider::unquote_label(&label), id);
        }
        Ok(out)
    }

    /// The labels the server gives these messages of `mailbox_id` (by UID, as label folder ids),
    /// except those whose labels are still being changed here (`keep`). Whether any changed.
    pub fn set_server_labels(&self, mailbox_id: i64, labels: &[(u32, Vec<i64>)], keep: &HashSet<u32>, since: u64) -> Result<bool> {
        let mut db = self.db();
        let keep: HashSet<u32> = keep.union(&self.flags_touched(mailbox_id, since)).copied().collect();
        let keep = &keep;
        let tx = db.transaction()?;
        let mut changed = false;
        // Labelled Sent from now on (sent from Gmail or another client): its recipients are written
        // to, as a Sent folder's.
        let sent: Option<i64> = tx
            .query_row(
                "SELECT id FROM mailboxes WHERE role = 'sent' AND account_id = (SELECT account_id FROM mailboxes WHERE id = ?1)",
                [mailbox_id],
                |r| r.get(0),
            )
            .optional()?;
        {
            let mut find = tx.prepare_cached("SELECT id FROM messages WHERE mailbox_id = ?1 AND uid = ?2")?;
            let mut current = tx.prepare_cached("SELECT mailbox_id FROM labels WHERE message_id = ?1 ORDER BY mailbox_id")?;
            let mut clear = tx.prepare_cached("DELETE FROM labels WHERE message_id = ?1")?;
            let mut add = tx.prepare_cached("INSERT OR IGNORE INTO labels (mailbox_id, message_id) VALUES (?1, ?2)")?;
            for (uid, boxes) in labels {
                if keep.contains(uid) {
                    continue;
                }
                let Some(id) = find.query_row(params![mailbox_id, uid], |r| r.get::<_, i64>(0)).optional()? else { continue };
                let mut wanted = boxes.clone();
                wanted.sort_unstable();
                wanted.dedup();
                let have: Vec<i64> = current.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
                if have == wanted {
                    continue;
                }
                changed = true;
                clear.execute([id])?;
                for b in &wanted {
                    add.execute(params![b, id])?;
                }
                if let Some(sent) = sent
                    && wanted.contains(&sent)
                    && !have.contains(&sent)
                {
                    let (account_id, to, cc, date): (i64, String, String, i64) = tx
                        .prepare_cached("SELECT account_id, to_list, cc_list, date FROM messages WHERE id = ?1")?
                        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                    let list = |json: &str| serde_json::from_str::<Vec<Address>>(json).unwrap_or_default();
                    note_written(&tx, account_id, list(&to).iter().chain(&list(&cc)), date)?;
                }
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn label_count(&self, label: i64) -> Result<i64> {
        Ok(self.read().query_row("SELECT COUNT(*) FROM labels WHERE mailbox_id = ?1", [label], |r| r.get(0))?)
    }

    /// UIDs, in `mailbox_id` (All Mail), of the messages carrying the label `label`.
    pub fn label_uids(&self, mailbox_id: i64, label: i64) -> Result<HashSet<u32>> {
        let db = self.read();
        let mut st = db.prepare_cached(
            "SELECT m.uid FROM labels l JOIN messages m ON m.id = l.message_id
             WHERE l.mailbox_id = ?2 AND m.mailbox_id = ?1 AND m.uid IS NOT NULL",
        )?;
        let rows = st.query_map(params![mailbox_id, label], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn unscreened(&self, account_id: i64) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare_cached("SELECT id FROM messages WHERE account_id = ?1 AND unscreened = 1")?;
        let rows = st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn set_thread_muted(&self, account_id: i64, thread_id: i64, muted: bool) -> Result<()> {
        let sql = if muted {
            "INSERT OR IGNORE INTO muted_threads (account_id, thread_id) VALUES (?1, ?2)"
        } else {
            "DELETE FROM muted_threads WHERE account_id = ?1 AND thread_id = ?2"
        };
        self.db().execute(sql, params![account_id, thread_id])?;
        Ok(())
    }

    pub fn thread_muted(&self, account_id: i64, thread_id: i64) -> Result<bool> {
        Ok(self.read().query_row(
            "SELECT EXISTS (SELECT 1 FROM muted_threads WHERE account_id = ?1 AND thread_id = ?2)",
            params![account_id, thread_id],
            |r| r.get(0),
        )?)
    }

    pub fn muted_among(&self, ids: &[i64]) -> Result<HashSet<i64>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT m.id FROM messages m JOIN muted_threads t ON t.account_id = m.account_id AND t.thread_id = m.thread_id
             WHERE m.id IN ({})",
            id_list(ids)
        ))?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The screener has decided about these messages: the lists may show them.
    pub fn clear_unscreened(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.db().execute(&format!("UPDATE messages SET unscreened = 0 WHERE unscreened = 1 AND id IN ({})", id_list(ids)), [])?;
        Ok(())
    }

    pub fn label_folders(&self, account_id: i64) -> Result<Vec<crate::provider::LabelFolder>> {
        let db = self.read();
        let mut st = db.prepare("SELECT id, name, role FROM mailboxes WHERE account_id = ?1 AND is_label = 1")?;
        let rows = st
            .query_map([account_id], |r| {
                Ok(crate::provider::LabelFolder { id: r.get(0)?, name: r.get(1)?, role: r.get::<_, Option<String>>(2)?.as_deref().and_then(Role::parse) })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Gives each message its labels, all in one transaction.
    pub fn relabel(&self, labels: &[(i64, Vec<i64>)]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        // Noted, as flags are: a label read under way does not put the old ones back.
        let places: Vec<(i64, u32)> = {
            let mut st = tx.prepare_cached("SELECT mailbox_id, uid FROM messages WHERE id = ?1 AND uid IS NOT NULL")?;
            let mut out = Vec::new();
            for (id, _) in labels {
                out.extend(st.query_row([id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?);
            }
            out
        };
        self.note_flag_changes(&places);
        replace_labels(&tx, labels)?;
        tx.commit()?;
        Ok(())
    }

    pub fn with_label_role(&self, ids: &[i64], roles: &[Role]) -> Result<HashSet<i64>> {
        if ids.is_empty() || roles.is_empty() {
            return Ok(HashSet::new());
        }
        let roles = roles.iter().map(|r| format!("'{}'", r.as_str())).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT DISTINCT l.message_id FROM labels l JOIN mailboxes b ON b.id = l.mailbox_id
             WHERE l.message_id IN ({}) AND b.role IN ({roles})",
            id_list(ids)
        ))?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// UIDs of `mailbox` whose labels a queued operation is still changing.
    pub fn pending_label_uids(&self, account_id: i64, mailbox: &str) -> Result<HashSet<u32>> {
        self.op_uids(account_id, mailbox, "('labels')")
    }

    /// Whether the folder is a label: its messages are those listed in `labels`, addressed
    /// through their own folder.
    pub fn set_label_folder(&self, mailbox_id: i64, label: bool) -> Result<()> {
        self.db().execute("UPDATE mailboxes SET is_label = ?2 WHERE id = ?1", params![mailbox_id, label])?;
        Ok(())
    }

    pub fn set_labels(&self, message_id: i64, mailboxes: &[i64]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM labels WHERE message_id = ?1", [message_id])?;
        {
            let mut st = tx.prepare("INSERT OR IGNORE INTO labels (mailbox_id, message_id) VALUES (?1, ?2)")?;
            for mailbox_id in mailboxes {
                st.execute(params![mailbox_id, message_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn message_labels(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<i64>>> {
        let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT message_id, mailbox_id FROM labels WHERE message_id IN ({}) ORDER BY mailbox_id", id_list(ids)))?;
        for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (id, mailbox_id) = row?;
            out.entry(id).or_default().push(mailbox_id);
        }
        Ok(out)
    }

    /// When the folder's periodic full check (`list`: the whole UID list; otherwise all flags)
    /// last ran to the end.
    pub fn full_check_at(&self, mailbox_id: i64, list: bool) -> Result<Option<i64>> {
        let col = if list { "checked_list_at" } else { "checked_flags_at" };
        Ok(self
            .read()
            .query_row(&format!("SELECT {col} FROM mailboxes WHERE id = ?1"), [mailbox_id], |r| r.get(0))
            .optional()?
            .flatten())
    }

    pub fn set_full_check_at(&self, mailbox_id: i64, list: bool, at: i64) -> Result<()> {
        let col = if list { "checked_list_at" } else { "checked_flags_at" };
        self.db().execute(&format!("UPDATE mailboxes SET {col} = ?2 WHERE id = ?1"), params![mailbox_id, at])?;
        Ok(())
    }

    /// Nothing in the folder waits on a read of it: no row awaiting its UID (a move, a Gmail
    /// label copy) and no recent body left to fetch (`bodies`: the folder's bodies are fetched
    /// ahead). A folder the server reports unchanged can then be left unopened.
    pub fn folder_settled(&self, mailbox_id: i64, bodies: bool, max_size: u32, since: i64) -> Result<bool> {
        Ok(self.read().query_row(
            "SELECT NOT EXISTS (SELECT 1 FROM messages WHERE mailbox_id = ?1 AND uid IS NULL)
                AND NOT (?2 AND EXISTS (SELECT 1 FROM messages WHERE mailbox_id = ?1 AND has_body = 0 AND uid IS NOT NULL
                                                                     AND size <= ?3 AND date >= ?4))",
            params![mailbox_id, bodies, max_size, since],
            |r| r.get(0),
        )?)
    }

    /// Widens the synced window into the past, leaving the rest of the folder's state alone.
    pub fn extend_window(&self, mailbox_id: i64, from_uid: u32) -> Result<()> {
        self.db().execute(
            "UPDATE mailboxes SET sync_from_uid = MIN(COALESCE(sync_from_uid, ?2), ?2) WHERE id = ?1",
            params![mailbox_id, from_uid],
        )?;
        Ok(())
    }

    /// The folder was recreated on the server (UIDVALIDITY changed): its cache and window start over.
    pub fn clear_mailbox(&self, mailbox_id: i64) -> Result<()> {
        self.shed_mailbox_rows(mailbox_id)?;
        let mut db = self.db();
        let tx = db.transaction()?;
        delete_mailbox_rows(&tx, mailbox_id)?;
        tx.execute("UPDATE mailboxes SET sync_from_uid = NULL, highestmodseq = NULL WHERE id = ?1", [mailbox_id])?;
        tx.commit()?;
        Ok(())
    }

    /// A purge is pending for this folder: every UID below it is about to be deleted.
    pub fn purge_pending_below(&self, account_id: i64, mailbox: &str) -> Result<Option<u32>> {
        self.pending_below(account_id, mailbox, "purge")
    }

    /// "Mark all as read" waiting to leave for this folder: every UID below it is read locally
    /// already, whatever the server still says.
    pub fn mark_all_seen_below(&self, account_id: i64, mailbox: &str) -> Result<Option<u32>> {
        self.pending_below(account_id, mailbox, "mark_all_seen")
    }

    /// Highest `below` of the pending (not failed) operations of this kind on this folder; an
    /// operation without one covers everything.
    fn pending_below(&self, account_id: i64, mailbox: &str, kind: &str) -> Result<Option<u32>> {
        let db = self.read();
        let mut st = db.prepare_cached(
            "SELECT json_extract(op, '$.below') FROM pending_ops
             WHERE account_id = ?1 AND mailbox = ?2 AND kind = ?3 AND failed = 0",
        )?;
        let mut below: Option<u32> = None;
        for b in st.query_map(params![account_id, mailbox, kind], |r| r.get::<_, Option<i64>>(0))? {
            let b = b?.map_or(u32::MAX, |b| b.clamp(0, u32::MAX as i64) as u32);
            below = Some(below.map_or(b, |x| x.max(b)));
        }
        Ok(below)
    }

    /// Folders an operation works on (its source, and its destination for a move), to read
    /// again in full once it is abandoned (failed or discarded): what it had changed locally
    /// must come back from the server. To call before the operation is removed.
    pub fn op_folders(&self, op_id: i64) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT b.id FROM pending_ops p JOIN mailboxes b ON b.account_id = p.account_id AND b.name IN (p.mailbox, p.dest)
             WHERE p.id = ?1",
        )?;
        let rows = st.query_map([op_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Message-IDs of messages moved to this folder whose UID is not known yet.
    pub fn placeholder_message_ids(&self, mailbox_id: i64) -> Result<Vec<String>> {
        let db = self.read();
        let mut st = db.prepare("SELECT DISTINCT message_id FROM messages WHERE mailbox_id = ?1 AND uid IS NULL AND message_id IS NOT NULL")?;
        let rows = st.query_map([mailbox_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn purge_local(&self, mailbox_id: i64) -> Result<usize> {
        let shed = self.shed_mailbox_rows(mailbox_id)?;
        let mut db = self.db();
        let tx = db.transaction()?;
        let n = delete_mailbox_rows(&tx, mailbox_id)?;
        tx.commit()?;
        Ok(shed + n)
    }

    /// UIDs of the folder targeted by queued operations (move, delete): the server still has them,
    /// but locally they are gone or going, so the sync must not bring them back. A failed
    /// operation no longer counts: it will not run unless the user retries it, and the messages
    /// must come back meanwhile.
    pub fn in_flight_uids(&self, account_id: i64, mailbox: &str) -> Result<HashSet<u32>> {
        self.op_uids(account_id, mailbox, "('move', 'move_set', 'delete', 'delete_set', 'save_draft')")
    }

    /// Message-IDs of the drafts a queued `delete_draft` (not failed) takes out of this folder:
    /// sent or discarded, already gone from the cache, but still on the server until it runs.
    /// The sync must not fetch them back (a sent draft reopened and sent twice). A save of the
    /// same draft queued after it (a cancelled send) puts the draft back: not counted then.
    pub fn pending_draft_deletes(&self, account_id: i64, mailbox: &str) -> Result<HashSet<String>> {
        // With Gmail's labels, drafts are read through All Mail but deleted from the Drafts label.
        let drafts = match self.labels_model(account_id)? && self.mailbox_by_role(account_id, Role::All)?.is_some_and(|a| a.name == mailbox) {
            true => self.mailbox_by_role(account_id, Role::Drafts)?.map(|d| d.name),
            false => None,
        };
        let db = self.read();
        let mut st = db.prepare(
            "SELECT kind, op FROM pending_ops WHERE account_id = ?1 AND mailbox IN (?2, ?3) AND failed = 0
               AND kind IN ('delete_draft', 'save_draft') ORDER BY id",
        )?;
        let mut out = HashSet::new();
        for row in st.query_map(params![account_id, mailbox, drafts], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (kind, op) = row?;
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&op) else { continue };
            let Some(mid) = v["message_id"].as_str() else { continue };
            if kind == "delete_draft" {
                out.insert(mid.to_string());
            } else {
                out.remove(mid);
            }
        }
        Ok(out)
    }

    // ── Messages: sync ─────────────────────────────────────────────────────

    pub fn mailbox_uids(&self, mailbox_id: i64) -> Result<HashMap<u32, i64>> {
        let db = self.read();
        let mut st = db.prepare("SELECT uid, id FROM messages WHERE mailbox_id = ?1 AND uid IS NOT NULL")?;
        let rows = st.query_map([mailbox_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// [`Store::mailbox_uids`] of these UIDs only (those cached), from the (mailbox_id, uid) index.
    pub fn mailbox_uids_among(&self, mailbox_id: i64, uids: &[u32]) -> Result<HashMap<u32, i64>> {
        if uids.is_empty() {
            return Ok(HashMap::new());
        }
        let list = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT uid, id FROM messages WHERE mailbox_id = ?1 AND uid IN ({list})"))?;
        let rows = st.query_map([mailbox_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// UIDs of the folder's cached messages, highest first; `limit`: only the most recent ones.
    /// Read from the (mailbox_id, uid) index alone.
    pub fn mailbox_uid_list(&self, mailbox_id: i64, limit: Option<usize>) -> Result<Vec<u32>> {
        let db = self.read();
        let mut st = db.prepare_cached(
            "SELECT uid FROM messages WHERE mailbox_id = ?1 AND uid IS NOT NULL ORDER BY uid DESC LIMIT ?2",
        )?;
        let limit = limit.map_or(-1, |l| l as i64);
        let rows = st.query_map(params![mailbox_id, limit], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Cached UIDs of the folder below `below` (what lies before the synced window: usually
    /// little), from the (mailbox_id, uid) index.
    pub fn uids_below(&self, mailbox_id: i64, below: u32) -> Result<HashSet<u32>> {
        let db = self.read();
        let mut st = db.prepare_cached("SELECT uid FROM messages WHERE mailbox_id = ?1 AND uid < ?2")?;
        let rows = st.query_map(params![mailbox_id, below], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Messages of the folder known by their UID, and the unread among them: each counted on an
    /// index, never by reading the folder's rows (every sync of a server without CONDSTORE).
    pub fn uid_unseen(&self, mailbox_id: i64) -> Result<(i64, i64)> {
        let db = self.read();
        let known = db.prepare_cached(UID_COUNT)?.query_row([mailbox_id], |r| r.get(0))?;
        let unseen = db.prepare_cached(UID_UNSEEN)?.query_row([mailbox_id], |r| r.get(0))?;
        Ok((known, unseen))
    }

    /// How many messages of the folder are cached with a UID, and the highest UID: counted on
    /// the (mailbox_id, uid) index, without loading the UID map.
    pub fn uid_stats(&self, mailbox_id: i64) -> Result<(usize, Option<u32>)> {
        let (count, max): (i64, Option<u32>) = self.read().query_row(
            "SELECT COUNT(uid), MAX(uid) FROM messages WHERE mailbox_id = ?1",
            [mailbox_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((count as usize, max))
    }

    /// In steps (see [`STEP_ROWS`]): thousands gone at once must not hold the writer.
    pub fn delete_uids(&self, mailbox_id: i64, uids: &[u32]) -> Result<()> {
        for step in uids.chunks(STEP_ROWS) {
            let mut db = self.db();
            let tx = db.transaction()?;
            let ids: Vec<i64> = {
                let mut st = tx.prepare_cached("SELECT id FROM messages WHERE mailbox_id = ?1 AND uid = ?2")?;
                let mut ids = Vec::with_capacity(step.len());
                for uid in step {
                    ids.extend(st.query_row(params![mailbox_id, uid], |r| r.get::<_, i64>(0)).optional()?);
                }
                ids
            };
            delete_ids(&tx, &ids)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// A folder's rows deleted in steps of their own, before the transaction that ends the folder
    /// or its reset: one transaction for 100,000 rows (each unindexed from the full-text table)
    /// would hold the writer, every other write waiting, for a minute, and swell the WAL. Cut
    /// midway, what is left goes with that transaction, or at the next try.
    fn shed_mailbox_rows(&self, mailbox_id: i64) -> Result<usize> {
        let mut total = 0;
        loop {
            let mut db = self.db();
            let tx = db.transaction()?;
            quiet_handled(&tx, true)?;
            let ids: Vec<i64> = {
                let mut st = tx.prepare_cached("SELECT id FROM messages WHERE mailbox_id = ?1 LIMIT ?2")?;
                st.query_map(params![mailbox_id, STEP_ROWS as i64], |r| r.get(0))?.collect::<Result<_, _>>()?
            };
            if ids.is_empty() {
                return Ok(total);
            }
            let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            let holders: Vec<i64> = {
                let mut st = tx.prepare(&format!("SELECT message_id FROM bodies WHERE message_id IN ({list})"))?;
                st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?
            };
            let groups = rehome_bodies(&tx, &holders, &|_, mailbox| mailbox == mailbox_id)?;
            total += tx.execute(&format!("DELETE FROM messages WHERE id IN ({list})"), [])?;
            fix_bodiless(&tx, &groups)?;
            quiet_handled(&tx, false)?;
            tx.commit()?;
        }
    }

    /// Inserts a header and threads it. A locally moved message (uid NULL) waiting in this folder
    /// gets the UID instead of a duplicate being created, so it keeps its downloaded body.
    pub fn insert_header(
        &self,
        account_id: i64,
        mailbox_id: i64,
        uid: u32,
        h: &HeaderInfo,
        flags: ServerFlags,
        size: u32,
    ) -> Result<i64> {
        Ok(self.insert_headers(account_id, mailbox_id, &[(uid, h.clone(), flags, size)])?[0].0)
    }

    /// Several headers of one folder in one transaction (a sync batch). For each: (id, new). Not
    /// new: already cached (two syncs crossed), or a message the user moved arriving at its
    /// destination.
    pub fn insert_headers(&self, account_id: i64, mailbox_id: i64, batch: &[(u32, HeaderInfo, ServerFlags, u32)]) -> Result<Vec<(i64, bool)>> {
        self.insert_headers_held(account_id, mailbox_id, batch, false)
    }

    /// [`insert_headers`], the new messages kept out of the lists until the screener has seen
    /// them (`hold`), in the same transaction: no list ever shows one before.
    pub fn insert_headers_held(&self, account_id: i64, mailbox_id: i64, batch: &[(u32, HeaderInfo, ServerFlags, u32)], hold: bool) -> Result<Vec<(i64, bool)>> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let role: Option<String> = tx.query_row("SELECT role FROM mailboxes WHERE id = ?1", [mailbox_id], |r| r.get(0)).optional()?.flatten();
        let sent = role.as_deref() == Some("sent");
        // Spam and refused senders never become suggestions: a phisher sending often would come
        // first when typing a name in To.
        let unwanted = matches!(role.as_deref(), Some("junk" | "refused"));
        let mut ids = Vec::with_capacity(batch.len());
        for (uid, h, flags, size) in batch {
            let (id, new) = insert_header_tx(&tx, account_id, mailbox_id, *uid, h, *flags, *size)?;
            ids.push((id, new));
            if !new {
                continue;
            }
            if hold {
                tx.prepare_cached("UPDATE messages SET unscreened = 1 WHERE id = ?1")?.execute([id])?;
            }
            // Address book: the sender, and the recipients of what the user sends.
            if let Some(from) = h.from.as_ref().filter(|_| !unwanted) {
                tx.prepare_cached(CONTACT_SEEN)?.execute(params![from.email, from.name, 0, h.date])?;
            }
            if sent {
                note_written(&tx, account_id, h.to.iter().chain(&h.cc), h.date)?;
            }
        }
        tx.commit()?;
        Ok(ids)
    }
}

/// A contact seen once more: (address, name, written, date).
const CONTACT_SEEN: &str = "INSERT INTO contacts (email, name, written, seen, last) VALUES (lower(?1), NULLIF(?2, ''), ?3, 1, ?4)
     ON CONFLICT (email) DO UPDATE SET
        name = COALESCE(excluded.name, contacts.name),
        written = contacts.written + excluded.written,
        seen = contacts.seen + 1,
        last = MAX(contacts.last, excluded.last)";

/// The recipients of a message the account sent: correspondents, and written to in the address book.
fn note_written<'a>(tx: &Connection, account_id: i64, recipients: impl Iterator<Item = &'a Address>, date: i64) -> Result<()> {
    let mut correspondent = tx.prepare_cached("INSERT OR IGNORE INTO correspondents (account_id, email) VALUES (?1, lower(?2))")?;
    let mut contact = tx.prepare_cached(CONTACT_SEEN)?;
    for a in recipients {
        correspondent.execute(params![account_id, a.email])?;
        contact.execute(params![a.email, a.name, 1, date])?;
    }
    Ok(())
}

/// A new message's category: the user's choice for this sender, else "people" if the account
/// wrote to them, else what its headers say.
fn category_of(tx: &Transaction, account_id: i64, h: &HeaderInfo) -> Result<Category> {
    let Some(from) = &h.from else { return Ok(h.category) };
    let chosen: Option<String> = tx
        .prepare_cached("SELECT category FROM sender_categories WHERE email = lower(?1)")?
        .query_row([&from.email], |r| r.get(0))
        .optional()?;
    if let Some(c) = chosen.as_deref().and_then(Category::parse) {
        return Ok(c);
    }
    let known: bool = tx
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM correspondents WHERE account_id = ?1 AND email = lower(?2))")?
        .query_row(params![account_id, from.email], |r| r.get(0))?;
    Ok(if known { Category::People } else { h.category })
}

fn write_links(tx: &Transaction, id: i64, domains: &[String]) -> Result<()> {
    tx.execute("DELETE FROM message_links WHERE message_id = ?1", [id])?;
    let mut st = tx.prepare_cached("INSERT OR IGNORE INTO message_links (domain, message_id) VALUES (?1, ?2)")?;
    for d in domains.iter().filter(|d| !d.is_empty()) {
        st.execute(params![d.to_lowercase(), id])?;
    }
    Ok(())
}

/// The labels of these messages, as Gmail shows them: the folders holding a copy, the inbox and
/// the user's own (Gmail's system folders, the bin and facteur's folders left out), in name
/// order. Messages with none are left out.
fn labels_of(db: &Connection, ids: &[i64]) -> Result<HashMap<i64, Vec<i64>>> {
    let mut out: HashMap<i64, Vec<i64>> = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let mut st = db.prepare(&format!(
        "SELECT DISTINCT x.id, x.box FROM (
            SELECT id, mailbox_id AS box FROM messages WHERE id IN ({list})
            UNION ALL SELECT message_id, mailbox_id FROM labels WHERE message_id IN ({list})) x
         JOIN mailboxes b ON b.id = x.box
         WHERE (b.role IS NULL OR b.role = 'inbox')
           AND b.name NOT LIKE '[Gmail]/%' AND b.name NOT LIKE '[Google Mail]/%'
         ORDER BY b.role IS NULL, b.name COLLATE NOCASE"
    ))?;
    for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, mailbox_id) = row?;
        out.entry(id).or_default().push(mailbox_id);
    }
    Ok(out)
}

/// Deletes these messages. A row about to go may hold the one stored body that the other
/// copies of the message (Gmail labels: INBOX and All Mail) read too: see `rehome_bodies`.
fn delete_ids(tx: &Transaction, ids: &[i64]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let doomed: HashSet<i64> = ids.iter().copied().collect();
    let mut holders = Vec::new();
    for chunk in ids.chunks(500) {
        let list = chunk.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let mut st = tx.prepare(&format!("SELECT message_id FROM bodies WHERE message_id IN ({list})"))?;
        holders.extend(st.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?);
    }
    let groups = rehome_bodies(tx, &holders, &|id, _| doomed.contains(&id))?;
    let mut n = 0;
    {
        let mut st = tx.prepare_cached("DELETE FROM messages WHERE id = ?1")?;
        for id in ids {
            n += st.execute([id])?;
        }
    }
    fix_bodiless(tx, &groups)?;
    Ok(n)
}

/// Deletes every message of a folder (emptied, or recreated on the server), bodies kept for the
/// copies that stay elsewhere.
/// The `handled` triggers silent (`true`) or not for what follows in `tx`.
fn quiet_handled(tx: &Transaction, quiet: bool) -> Result<()> {
    tx.execute("DELETE FROM handled_quiet", [])?;
    if quiet {
        tx.execute("INSERT INTO handled_quiet VALUES (1)", [])?;
    }
    Ok(())
}

/// A folder's rows going for a reset or its removal: no thread is handled by it.
fn delete_mailbox_rows(tx: &Transaction, mailbox_id: i64) -> Result<usize> {
    quiet_handled(tx, true)?;
    let n = delete_mailbox_rows_now(tx, mailbox_id);
    quiet_handled(tx, false)?;
    n
}

fn delete_mailbox_rows_now(tx: &Transaction, mailbox_id: i64) -> Result<usize> {
    let holders: Vec<i64> = {
        let mut st = tx.prepare("SELECT m.id FROM messages m JOIN bodies b ON b.message_id = m.id WHERE m.mailbox_id = ?1")?;
        st.query_map([mailbox_id], |r| r.get(0))?.collect::<Result<_, _>>()?
    };
    let groups = rehome_bodies(tx, &holders, &|_, mailbox| mailbox == mailbox_id)?;
    let n = tx.execute("DELETE FROM messages WHERE mailbox_id = ?1", [mailbox_id])?;
    fix_bodiless(tx, &groups)?;
    Ok(n)
}

/// Before deleting rows that hold a body (`holders`): unless a surviving copy of the message
/// already has its own, the body, its indexed text and its links move to a surviving copy
/// (`doomed(id, mailbox_id)`: the row is deleted too). Returns the messages (account,
/// Message-ID) to check with `fix_bodiless` once the rows are gone.
fn rehome_bodies(tx: &Transaction, holders: &[i64], doomed: &dyn Fn(i64, i64) -> bool) -> Result<Vec<(i64, String)>> {
    let mut groups = Vec::new();
    for &holder in holders {
        // Surviving copies first in this order: under a UID (settled) before a moved one.
        let copies: Vec<(i64, i64, bool)> = {
            let mut st = tx.prepare_cached(&format!(
                "SELECT o.id, o.mailbox_id, EXISTS (SELECT 1 FROM bodies b WHERE b.message_id = o.id)
                 FROM messages m JOIN messages o ON {SAME_COPY}
                 WHERE m.id = ?1 AND m.message_id IS NOT NULL
                 ORDER BY o.uid IS NULL, o.id"
            ))?;
            st.query_map([holder], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?
        };
        let group: Option<(i64, String)> = tx
            .prepare_cached("SELECT account_id, message_id FROM messages WHERE id = ?1 AND message_id IS NOT NULL")?
            .query_row([holder], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        groups.extend(group);
        let survivors: Vec<&(i64, i64, bool)> = copies.iter().filter(|(id, mailbox, _)| !doomed(*id, *mailbox)).collect();
        if survivors.iter().any(|(_, _, has)| *has) {
            continue;
        }
        let Some(&&(heir, _, _)) = survivors.first() else { continue };
        tx.execute("UPDATE bodies SET message_id = ?2 WHERE message_id = ?1", params![holder, heir])?;
        tx.execute(
            "UPDATE messages_fts SET body = (SELECT body FROM messages_fts WHERE rowid = ?1) WHERE rowid = ?2",
            params![holder, heir],
        )?;
        tx.execute(
            "UPDATE messages SET has_body = 1,
                    link_domains = (SELECT link_domains FROM messages WHERE id = ?1),
                    list_id = COALESCE(list_id, (SELECT list_id FROM messages WHERE id = ?1))
             WHERE id = ?2",
            params![holder, heir],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO message_links (domain, message_id) SELECT domain, ?2 FROM message_links WHERE message_id = ?1",
            params![holder, heir],
        )?;
    }
    Ok(groups)
}

/// After deletions: copies of these messages that claim a body no longer stored anywhere (its
/// holder is gone, or an older version shared it too loosely) are marked to download again.
fn fix_bodiless(tx: &Transaction, groups: &[(i64, String)]) -> Result<()> {
    let mut st = tx.prepare_cached(&format!(
        "UPDATE messages AS m SET has_body = 0
         WHERE m.account_id = ?1 AND m.message_id = ?2 AND m.has_body = 1
           AND NOT EXISTS (SELECT 1 FROM bodies b WHERE b.message_id = m.id)
           AND NOT EXISTS (SELECT 1 FROM messages o JOIN bodies b ON b.message_id = o.id WHERE {SAME_COPY})"
    ))?;
    for (account_id, message_id) in groups {
        st.execute(params![account_id, message_id])?;
    }
    Ok(())
}

fn insert_header_tx(
    tx: &Transaction,
    account_id: i64,
    mailbox_id: i64,
    uid: u32,
    h: &HeaderInfo,
    flags: ServerFlags,
    size: u32,
) -> Result<(i64, bool)> {
        // Already there: refresh the flags rather than fail on uniqueness.
        if let Some(id) = tx
            .query_row("SELECT id FROM messages WHERE mailbox_id = ?1 AND uid = ?2", params![mailbox_id, uid], |r| r.get::<_, i64>(0))
            .optional()?
        {
            tx.execute(
                "UPDATE messages SET seen = ?2, flagged = ?3, answered = ?4, draft = ?5 WHERE id = ?1",
                params![id, flags.seen, flags.flagged, flags.answered, flags.draft],
            )?;
            return Ok((id, false));
        }
        let from_addr = h.from.as_ref().map(|a| a.email.as_str());
        if let Some(mid) = &h.message_id {
            // Indexed by (account_id, message_id). Only the same message is adopted: same date
            // and sender at least (distinct messages can share a Message-ID), the one of the same
            // size too first. A placeholder left without its message is dropped later
            // (`drop_orphan_placeholders`); adopting another message's UID would show one under
            // the other's header and body.
            let placeholder: Option<i64> = tx
                .prepare_cached(
                    "SELECT id FROM messages WHERE account_id = ?3 AND message_id = ?2 AND mailbox_id = ?1 AND uid IS NULL
                       AND date = ?5 AND from_addr IS ?6
                     ORDER BY size = ?4 DESC, id LIMIT 1",
                )?
                .query_row(params![mailbox_id, mid, account_id, size, h.date, from_addr], |r| r.get(0))
                .optional()?;
            if let Some(id) = placeholder {
                tx.execute(
                    "UPDATE messages SET uid = ?2, seen = ?3, flagged = ?4, answered = ?5, draft = ?6 WHERE id = ?1",
                    params![id, uid, flags.seen, flags.flagged, flags.answered, flags.draft],
                )?;
                // Read or pinned during the move: the server does not know yet.
                queue_pending_flags(tx, id, Some(flags))?;
                return Ok((id, false));
            }
        }

        let (from_name, from_addr) = match &h.from {
            Some(a) => (a.name.clone(), Some(a.email.clone())),
            None => (None, None),
        };
        tx.execute(
            "INSERT INTO messages (account_id, mailbox_id, uid, message_id, in_reply_to, refs, subject,
                                   from_name, from_addr, to_list, cc_list, date, size,
                                   seen, flagged, answered, draft, received_at, bulk, signals, list_id, category)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                account_id, mailbox_id, uid, h.message_id, h.in_reply_to, h.references.join(" "),
                h.subject, from_name, from_addr,
                serde_json::to_string(&h.to)?, serde_json::to_string(&h.cc)?,
                h.date, size, flags.seen, flags.flagged, flags.answered, flags.draft, h.received_at, h.bulk,
                serde_json::to_string(&h.signals)?, h.list_id, category_of(tx, account_id, h)?.as_str(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut st = tx.prepare_cached("INSERT INTO message_refs (account_id, ref, message_id) VALUES (?1, ?2, ?3)")?;
            for r in thread_refs(h) {
                st.execute(params![account_id, r, id])?;
            }
        }
        assign_thread(tx, account_id, id, h)?;

        let sender = h.from.as_ref().map(format_address).unwrap_or_default();
        let recipients = h.to.iter().chain(&h.cc).map(format_address).collect::<Vec<_>>().join(" ");
        tx.execute(
            "INSERT INTO messages_fts (rowid, subject, sender, recipients, body) VALUES (?1, ?2, ?3, ?4, '')",
            params![id, h.subject, sender, recipients],
        )?;
        // Body already stored under another copy of this very message (Gmail labels): shared
        // now, with one indexed lookup, rather than by rescanning the folder at every sync.
        if h.message_id.is_some() {
            tx.prepare_cached(&format!(
                "UPDATE messages AS m SET has_body = 1, preview = o.preview, has_attachments = o.has_attachments
                 FROM messages o
                 WHERE m.id = ?1 AND {SAME_COPY} AND EXISTS (SELECT 1 FROM bodies b WHERE b.message_id = o.id)"
            ))?
            .execute([id])?;
        }
        Ok((id, true))
}

impl Store {
    /// Flags reported by the server. `keep`: UIDs with a local change waiting to leave, not to be
    /// overwritten with the server's still-old state.
    /// Flags changed here on these (folder, UID): a merge of the server's flags read before now
    /// leaves them alone. Called under the writer, before the change is written.
    fn note_flag_changes(&self, places: &[(i64, u32)]) {
        let tick = self.flag_clock.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let mut marks = self.flag_marks.lock().unwrap();
        // Old marks dropped once per folder and call: a whole account marked read names thousands.
        let mut pruned = HashSet::new();
        let now = std::time::Instant::now();
        for (mailbox_id, uid) in places {
            let folder = marks.entry(*mailbox_id).or_default();
            if folder.len() > 1000 && pruned.insert(*mailbox_id) {
                folder.retain(|_, (_, at)| at.elapsed() < FLAG_MARKS_KEPT);
            }
            folder.insert(*uid, (tick, now));
        }
    }

    /// The folder's UIDs whose flags or labels changed here after `since`. Read under the writer.
    fn flags_touched(&self, mailbox_id: i64, since: u64) -> HashSet<u32> {
        self.flag_marks
            .lock()
            .unwrap()
            .get(&mailbox_id)
            .map(|marks| marks.iter().filter(|(_, (tick, _))| *tick > since).map(|(uid, _)| *uid).collect())
            .unwrap_or_default()
    }

    /// Now, for [`Store::update_flags`]: taken before the server's flags are asked for.
    pub fn flags_tick(&self) -> u64 {
        self.flag_clock.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `since`: the tick taken before the server's flags were read; a flag changed here after it
    /// (marked read while the FETCH ran, or just before this write) is not put back to the older
    /// value. Checked while holding the writer, which every local change takes too.
    pub fn update_flags(&self, mailbox_id: i64, updates: &[(u32, ServerFlags)], keep: &HashSet<u32>, since: u64) -> Result<usize> {
        let mut db = self.db();
        let keep: HashSet<u32> = keep.union(&self.flags_touched(mailbox_id, since)).copied().collect();
        let keep = &keep;
        let tx = db.transaction()?;
        let mut changed = 0;
        {
            let mut st = tx.prepare(
                "UPDATE messages SET seen = ?3, flagged = ?4, answered = ?5, draft = ?6
                 WHERE mailbox_id = ?1 AND uid = ?2
                   AND (seen, flagged, answered, draft) IS NOT (?3, ?4, ?5, ?6)",
            )?;
            for (uid, f) in updates.iter().filter(|(uid, _)| !keep.contains(uid)) {
                changed += st.execute(params![mailbox_id, uid, f.seen, f.flagged, f.answered, f.draft])?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    // ── Bodies ─────────────────────────────────────────────────────────────

    /// The message's body, else that of another copy of the same message (Gmail files one message
    /// in several folders): only one is stored. A copy must match on size, date and sender too
    /// (`SAME_COPY`), not on the Message-ID alone.
    pub fn body(&self, id: i64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .read()
            .query_row(
                &format!(
                    "SELECT raw FROM bodies WHERE message_id = ?1
                     UNION ALL
                     SELECT b.raw FROM messages m
                     JOIN messages o ON {SAME_COPY}
                     JOIN bodies b ON b.message_id = o.id
                     WHERE m.id = ?1 AND m.message_id IS NOT NULL
                     LIMIT 1"
                ),
                [id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn store_body(&self, id: i64, raw: &[u8], info: &BodyInfo) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("INSERT OR REPLACE INTO bodies (message_id, raw) VALUES (?1, ?2)", params![id, raw])?;
        tx.execute(
            "UPDATE messages SET preview = ?2, has_attachments = ?3, link_domains = ?4, list_id = COALESCE(list_id, ?5), has_body = 1
             WHERE id = ?1",
            params![id, info.preview, info.has_attachments, info.link_domains.join(" "), info.list_id],
        )?;
        write_links(&tx, id, &info.link_domains)?;
        tx.execute("UPDATE messages_fts SET body = ?2 WHERE rowid = ?1", params![id, info.text])?;
        // The message's other copies benefit too: preview, and a body readable without downloading.
        tx.execute(
            &format!(
                "UPDATE messages AS o SET preview = ?2, has_attachments = ?3, has_body = 1
                 FROM messages m
                 WHERE m.id = ?1 AND m.message_id IS NOT NULL AND o.has_body = 0 AND {SAME_COPY}"
            ),
            params![id, info.preview, info.has_attachments],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Messages of the folder whose body is not cached yet, newest first, dated from `since`:
    /// (id, UID, size).
    pub fn missing_bodies(&self, mailbox_id: i64, limit: usize, max_size: u32, since: i64) -> Result<Vec<(i64, u32, u32)>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT id, uid, size FROM messages
             WHERE mailbox_id = ?1 AND has_body = 0 AND uid IS NOT NULL AND size <= ?2 AND date >= ?4
             ORDER BY date DESC LIMIT ?3",
        )?;
        let rows = st
            .query_map(params![mailbox_id, max_size, limit as i64, since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    // ── Messages: reading ──────────────────────────────────────────────────

    /// Threads of the given folders: one folder, or the inboxes of the unified view.
    /// `pinned_first`: pinned threads come first (inboxes). `except_in`: threads left out if they
    /// have a message in one of these folders ("Done" view: what is archived, except what came
    /// back to the inbox).
    #[allow(clippy::too_many_arguments)]
    pub fn list_threads(
        &self,
        mailbox_ids: &[i64],
        except_in: &[i64],
        filter: ThreadFilter,
        pinned_first: bool,
        category: Option<Category>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ThreadSummary>> {
        let db = self.read();
        // One snapshot for every statement: a sync committing between the page and its messages
        // would leave blank rows, or show held mail.
        let _snapshot = db.unchecked_transaction()?;
        let boxes = Boxes::of(&db, mailbox_ids)?;
        let mut conds: Vec<String> = Vec::new();
        match filter {
            ThreadFilter::All => {}
            ThreadFilter::Unread => conds.push("unread > 0".into()),
            ThreadFilter::Flagged => conds.push("any_flagged > 0".into()),
        }
        // Smart inbox: the thread's category (a person wins). Pinned threads stay visible in every
        // category.
        if let Some(c) = category {
            conds.push(format!("(cat = {} OR any_flagged > 0)", c.rank()));
        }
        let condition = if conds.is_empty() { String::new() } else { format!("WHERE {}", conds.join(" AND ")) };
        let pinned = if pinned_first { "any_flagged DESC," } else { "" };
        // The "Done" view sorts a thread by when it left the inbox, if that came after its
        // last message.
        let handled = if except_in.is_empty() {
            ""
        } else {
            "LEFT JOIN handled h ON h.account_id = gb.account_id AND h.thread_id = g.thread_id"
        };
        let handled_at = if except_in.is_empty() { "0" } else { "COALESCE(h.at, 0)" };
        let mut except = if except_in.is_empty() {
            String::new()
        } else {
            format!("AND thread_id NOT IN (SELECT thread_id FROM ({}))", Boxes::of(&db, except_in)?.pool("thread_id"))
        };
        // New mail the screener has not decided about yet: in no list before it has.
        let hidden = unscreened_ids(&db)?;
        if !hidden.is_empty() {
            except.push_str(&format!(" AND id NOT IN ({})", hidden.iter().map(i64::to_string).collect::<Vec<_>>().join(",")));
        }
        // 1. Per-thread aggregates, read from the (mailbox_id, thread_id, date, id, flags) index
        //    without reading the rows; sorting and paging on these narrow rows. A thread back from
        //    a snooze sorts at the date it came back. Thread numbers come from a dedicated counter,
        //    so each belongs to one account, but numbers from before that counter may collide
        //    across accounts: snoozes are matched on the account too (taken from the folder, still
        //    read from the index alone). A label's messages (Gmail) come through the labels table,
        //    the folder they are listed in standing for their own.
        let pool = boxes.pool("id, thread_id, date, seen, flagged, has_attachments, category");
        let mut st = db.prepare(&format!(
            "WITH g AS (
                SELECT thread_id, MAX(date) AS last, MAX(id) AS last_id, COUNT(*) AS cnt, SUM(seen = 0) AS unread,
                       MAX(flagged) AS any_flagged, MAX(has_attachments) AS any_attachment,
                       MIN({CATEGORY_RANK}) AS cat, MIN(box) AS box
                FROM ({pool}) WHERE 1 {except} GROUP BY thread_id
             )
             SELECT g.thread_id, cnt, unread, any_flagged, any_attachment,
                    MAX(last, COALESCE(s.woke_at, 0), {handled_at}) AS activity,
                    CASE WHEN s.woke_at IS NULL THEN s.until END, s.woke_at IS NOT NULL, cat
             FROM g JOIN mailboxes gb ON gb.id = g.box
             LEFT JOIN snoozes s ON s.account_id = gb.account_id AND s.thread_id = g.thread_id
             {handled}
             {condition}
             ORDER BY {pinned} activity DESC, last_id DESC LIMIT ?1 OFFSET ?2"
        ))?;
        struct Row {
            thread_id: i64,
            count: i64,
            unread: i64,
            flagged: bool,
            has_attachments: bool,
            activity: i64,
            snoozed_until: Option<i64>,
            woken: bool,
            category: Category,
        }
        let page: Vec<Row> = st
            .query_map(params![limit as i64, offset as i64], |r| {
                Ok(Row {
                    thread_id: r.get(0)?,
                    count: r.get(1)?,
                    unread: r.get(2)?,
                    flagged: r.get(3)?,
                    has_attachments: r.get(4)?,
                    activity: r.get(5)?,
                    snoozed_until: r.get(6)?,
                    woken: r.get(7)?,
                    category: Category::from_rank(r.get(8)?),
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(st);
        if page.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Messages of the page's threads only, in chronological order: participants, ids, and
        //    the latest for the row.
        let ids = page.iter().map(|t| t.thread_id.to_string()).collect::<Vec<_>>().join(",");
        let pool = boxes.pool("id, thread_id, subject, date, preview, from_name, from_addr, to_list, account_id");
        let mut st = db.prepare(&format!(
            "SELECT thread_id, id, subject, date, preview, from_name, from_addr, to_list, account_id, box
             FROM ({pool}) WHERE thread_id IN ({ids}) {except} ORDER BY date, id"
        ))?;
        let mut threads: Vec<ThreadSummary> = page
            .iter()
            .map(|t| ThreadSummary {
                account_id: 0,
                mailbox_id: 0,
                thread_id: t.thread_id,
                latest_id: 0,
                subject: String::new(),
                from: None,
                to: Vec::new(),
                participants: Vec::new(),
                message_ids: Vec::new(),
                date: 0,
                preview: String::new(),
                count: t.count,
                unread: t.unread,
                flagged: t.flagged,
                has_attachments: t.has_attachments,
                activity: t.activity,
                snoozed_until: t.snoozed_until,
                woken: t.woken,
                category: t.category,
                labels: Vec::new(),
            })
            .collect();
        let index: HashMap<i64, usize> = threads.iter().enumerate().map(|(i, t)| (t.thread_id, i)).collect();
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
            ))
        })?;
        for row in rows {
            let (thread_id, id, subject, date, preview, name, addr, to, account_id, mailbox_id) = row?;
            let Some(t) = index.get(&thread_id).map(|&i| &mut threads[i]) else { continue };
            t.message_ids.push(id);
            if let Some(email) = &addr {
                match t.participants.iter_mut().find(|p| p.email.eq_ignore_ascii_case(email)) {
                    // Keep the fullest name seen for this address.
                    Some(p) if p.name.is_none() => p.name = name.clone(),
                    Some(_) => {}
                    None => t.participants.push(Address { name: name.clone(), email: email.clone() }),
                }
            }
            // Chronological order: the last row seen is the latest.
            t.latest_id = id;
            t.subject = subject;
            t.date = date;
            t.preview = preview;
            t.from = addr.map(|email| Address { name, email });
            t.to = serde_json::from_str(&to).unwrap_or_default();
            t.account_id = account_id;
            t.mailbox_id = mailbox_id;
        }
        drop(st);
        threads.retain(|t| t.latest_id != 0);
        // Gmail's All Mail: the labels its messages carry, as Gmail shows them (the inbox and the
        // user's folders; Gmail's own system folders, the bin and facteur's folders left out).
        let all_mail: bool = db.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM mailboxes WHERE id IN ({}) AND role = 'all')", id_list(mailbox_ids)),
            [],
            |r| r.get(0),
        )?;
        if all_mail {
            let shown: Vec<i64> = threads.iter().flat_map(|t| t.message_ids.iter().copied()).collect();
            let labels = labels_of(&db, &shown)?;
            for t in &mut threads {
                for id in &t.message_ids {
                    for mailbox_id in labels.get(id).into_iter().flatten() {
                        if !t.labels.contains(mailbox_id) {
                            t.labels.push(*mailbox_id);
                        }
                    }
                }
            }
        }
        Ok(threads)
    }

    /// Known contacts whose address or name contains `query`. People the user wrote to come before
    /// those who only wrote to them, then by frequency and recency.
    pub fn search_contacts(&self, query: &str, exclude: &[String], limit: usize) -> Result<Vec<Address>> {
        let q = fold(query.trim());
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let db = self.read();
        // The address books name people; those written to still come first, then the people the
        // address books know, then the others.
        let mut st = db.prepare_cached(
            "WITH book AS (SELECT email, MAX(name) AS name FROM address_book GROUP BY email),
             everyone AS (
                SELECT c.email, COALESCE(b.name, c.name) AS name, c.written, c.seen, c.last, b.email IS NOT NULL AS listed
                FROM contacts c LEFT JOIN book b ON b.email = c.email
                UNION ALL
                SELECT b.email, b.name, 0, 0, 0, 1 FROM book b WHERE NOT EXISTS (SELECT 1 FROM contacts c WHERE c.email = b.email)
             )
             SELECT email, name FROM everyone
             WHERE fold(email) LIKE ?1 ESCAPE '\\' OR fold(name) LIKE ?1 ESCAPE '\\'
             ORDER BY written > 0 DESC, listed DESC, written DESC, seen DESC, last DESC
             LIMIT ?2",
        )?;
        let exclude: Vec<String> = exclude.iter().map(|e| e.to_lowercase()).collect();
        let rows: Vec<Address> = st
            .query_map(params![like, (limit + exclude.len()) as i64], |r| {
                Ok(Address { email: r.get(0)?, name: r.get::<_, Option<String>>(1)?.filter(|n| !n.is_empty()) })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows.into_iter().filter(|a| !exclude.contains(&a.email)).take(limit).collect())
    }

    /// The people written to most recently (To and Cc of the latest sent messages, every
    /// account), newest first: the composer's suggestions before anything is typed. The name is
    /// the address book's when it knows one.
    pub fn recent_recipients(&self, exclude: &[String], limit: usize) -> Result<Vec<Address>> {
        let db = self.read();
        let mut st = db.prepare_cached(&format!(
            "WITH recent AS (
                SELECT m.to_list, m.cc_list, m.date FROM messages m
                WHERE {} ORDER BY m.date DESC LIMIT 200
             ), people AS (
                SELECT lower(json_extract(j.value, '$.email')) AS email, NULLIF(json_extract(j.value, '$.name'), '') AS name, r.date
                FROM recent r, json_each(r.to_list) j
                UNION ALL
                SELECT lower(json_extract(j.value, '$.email')), NULLIF(json_extract(j.value, '$.name'), ''), r.date
                FROM recent r, json_each(r.cc_list) j
             )
             SELECT p.email, COALESCE(NULLIF(c.name, ''), MAX(p.name)) FROM people p LEFT JOIN contacts c ON c.email = p.email
             WHERE p.email IS NOT NULL AND p.email != ''
             GROUP BY p.email ORDER BY MAX(p.date) DESC LIMIT ?1",
            in_folder_where("m", "rb.role = 'sent'")
        ))?;
        let exclude: Vec<String> = exclude.iter().map(|e| e.to_lowercase()).collect();
        let rows: Vec<Address> = st
            .query_map([(limit + exclude.len()) as i64], |r| Ok(Address { email: r.get(0)?, name: r.get(1)? }))?
            .collect::<Result<_, _>>()?;
        Ok(rows.into_iter().filter(|a| !exclude.contains(&a.email)).take(limit).collect())
    }

    // ── Address books ──────────────────────────────────────────────────────

    /// The account's address book in place of the previous one. `email`: the account it was read
    /// for; nothing is written when the id now names another account (removed, then one added).
    pub fn replace_address_book(&self, account_id: i64, email: &str, contacts: &[crate::contacts::Contact]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let same: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM accounts WHERE id = ?1 AND email = ?2)", params![account_id, email], |r| r.get(0))?;
        if !same {
            return Ok(());
        }
        tx.execute("DELETE FROM address_book WHERE account_id = ?1", [account_id])?;
        {
            let mut insert = tx.prepare("INSERT OR IGNORE INTO address_book (account_id, remote_id, email, name) VALUES (?1, ?2, ?3, ?4)")?;
            for c in contacts {
                for email in &c.emails {
                    insert.execute(params![account_id, c.remote_id, email, c.name])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The contacts kept of an address book whose ids start with `prefix` (a CardDAV book's
    /// path): what to keep of a book that failed to answer this time.
    pub fn address_book_under(&self, account_id: i64, prefix: &str) -> Result<Vec<crate::contacts::Contact>> {
        let db = self.read();
        let mut st = db.prepare("SELECT remote_id, name, email FROM address_book WHERE account_id = ?1 AND substr(remote_id, 1, length(?2)) = ?2 ORDER BY remote_id")?;
        let mut out: Vec<crate::contacts::Contact> = Vec::new();
        for row in st.query_map(params![account_id, prefix], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?)))? {
            let (remote_id, name, email) = row?;
            match out.last_mut().filter(|c| c.remote_id == remote_id) {
                Some(c) => c.emails.push(email),
                None => out.push(crate::contacts::Contact { remote_id, name, emails: vec![email] }),
            }
        }
        Ok(out)
    }

    /// `synced`: the address book was just replaced.
    pub fn set_address_book_state(&self, account_id: i64, state: CalendarState, message: Option<&str>, synced: bool) -> Result<()> {
        let now = now();
        self.db().execute(
            "INSERT INTO address_book_accounts (account_id, state, message, synced_at, checked_at) VALUES (?1, ?2, ?3, CASE WHEN ?4 THEN ?5 END, ?5)
             ON CONFLICT (account_id) DO UPDATE SET state = excluded.state, message = excluded.message,
                 synced_at = COALESCE(excluded.synced_at, address_book_accounts.synced_at), checked_at = excluded.checked_at",
            params![account_id, state.as_str(), message, synced, now],
        )?;
        Ok(())
    }

    /// When each account's address book was last checked (synced or not), and how that went.
    pub fn address_book_checks(&self) -> Result<HashMap<i64, (i64, CalendarState)>> {
        let db = self.read();
        let mut st = db.prepare("SELECT account_id, checked_at, state FROM address_book_accounts")?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, (r.get(1)?, CalendarState::parse(&r.get::<_, String>(2)?).unwrap_or(CalendarState::Error)))))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn address_book_status(&self) -> Result<Vec<AddressBookStatus>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT s.account_id, s.state, s.message, s.synced_at, (SELECT COUNT(DISTINCT remote_id) FROM address_book b WHERE b.account_id = s.account_id)
             FROM address_book_accounts s",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(AddressBookStatus {
                    account_id: r.get(0)?,
                    state: CalendarState::parse(&r.get::<_, String>(1)?).unwrap_or(CalendarState::Error),
                    message: r.get(2)?,
                    last_sync: r.get(3)?,
                    contacts: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    // ── Blocked senders ────────────────────────────────────────────────────

    pub fn block_sender(&self, email: &str) -> Result<()> {
        self.db().execute(
            "INSERT OR IGNORE INTO blocked_senders (email, created_at) VALUES (lower(?1), ?2)",
            params![email.trim(), now()],
        )?;
        Ok(())
    }

    pub fn unblock_sender(&self, email: &str) -> Result<()> {
        self.db().execute("DELETE FROM blocked_senders WHERE email = lower(?1)", [email.trim()])?;
        Ok(())
    }

    pub fn blocked_senders(&self) -> Result<Vec<String>> {
        let db = self.read();
        let mut st = db.prepare("SELECT email FROM blocked_senders ORDER BY email")?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Inbox messages from blocked senders (already on the server).
    pub fn inbox_from_blocked(&self, account_id: i64) -> Result<Vec<i64>> {
        let db = self.read();
        // From the (few) blocked addresses into the messages_from_lc index (CROSS JOIN keeps
        // that order): never a scan of the whole inbox, run after every sync.
        let mut st = db.prepare_cached(&format!(
            "SELECT m.id FROM blocked_senders s
             CROSS JOIN messages m ON lower(m.from_addr) = lower(s.email) AND m.account_id = ?1
             WHERE {} AND m.uid IS NOT NULL",
            in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'inbox'")
        ))?;
        let rows = st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    // ── Screener ────────────────────────────────────────────────────────────

    /// The decision recorded for exactly this pattern (an address, or `@domain`).
    pub fn screening_decision(&self, account_id: i64, pattern: &str) -> Result<Option<String>> {
        Ok(self
            .read()
            .query_row("SELECT decision FROM screening WHERE account_id = ?1 AND pattern = ?2", params![account_id, pattern], |r| r.get(0))
            .optional()?)
    }

    /// Records a decision (`accept` or `block`), or forgets it (`None`).
    pub fn set_screening(&self, account_id: i64, pattern: &str, decision: Option<&str>) -> Result<()> {
        let db = self.db();
        match decision {
            Some(decision) => db.execute(
                "INSERT INTO screening (account_id, pattern, decision, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account_id, pattern) DO UPDATE SET decision = excluded.decision, created_at = excluded.created_at",
                params![account_id, pattern, decision, now()],
            )?,
            None => db.execute("DELETE FROM screening WHERE account_id = ?1 AND pattern = ?2", params![account_id, pattern])?,
        };
        Ok(())
    }

    pub fn screening_list(&self, account_id: i64) -> Result<Vec<Screening>> {
        let db = self.read();
        let mut st = db.prepare("SELECT pattern, decision, created_at FROM screening WHERE account_id = ?1 ORDER BY created_at DESC")?;
        let rows = st
            .query_map([account_id], |r| Ok(Screening { pattern: r.get(0)?, decision: r.get(1)?, created_at: r.get(2)? }))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// What the screener does with these new inbox messages, received since `since` (the
    /// server's arrival date, not the sender's Date header): hold (first messages of an unknown
    /// sender), refuse, or accept; the others are left out.
    ///
    /// - A message found again in the inbox after being in the screener's folders (this device
    ///   held it, or its row there is still cached) was moved back by another device or client:
    ///   its sender is accepted.
    /// - Mail from one of `own` (every address of every account, `+tag` ignored) passes when its
    ///   authentication passed or a copy lies in Sent; otherwise only an explicit decision about
    ///   the address lets it through, since anyone can write one's own address in From.
    /// - A decision about the address wins over one about the domain. A domain block spares the
    ///   people the account wrote to or is talking with.
    /// - A sender is known when the account wrote to them, when the message replies to one the
    ///   account sent, in a conversation the account took part in, or when one of their messages
    ///   lies anywhere but the screener's folders, Junk and Trash. This last test reads the
    ///   folders, shared by every device: a sender accepted elsewhere is known here too.
    ///   Messages left in "Screened out" refuse their sender the same way.
    pub fn screen_verdicts(&self, account_id: i64, ids: &[i64], since: i64, own: &HashSet<String>) -> Result<Vec<(i64, Verdict)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let own: HashSet<String> = own.iter().map(|e| base_address(e)).collect();
        // Read before taking a connection: without spare readers, both would wait on the writer.
        let usual = self.usual_authserv(account_id)?;
        let batch = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let candidates: Vec<(i64, String, i64, Option<String>, Option<String>, i64)> = {
            let mut st = db.prepare(&format!(
                "SELECT m.id, COALESCE(lower(m.from_addr), ''), m.thread_id, m.message_id, m.signals, COALESCE(m.received_at, m.date)
                 FROM messages m
                 WHERE m.id IN ({batch}) AND {} AND m.uid IS NOT NULL
                   AND COALESCE(m.received_at, m.date) >= ?1",
                in_folder_where("m", "rb.account_id = ?2 AND rb.role = 'inbox'")
            ))?;
            st.query_map(params![since, account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?
        };
        let mut decision = db.prepare_cached("SELECT decision FROM screening WHERE account_id = ?1 AND pattern = ?2")?;
        // Held or refused here, or still cached in those folders with a UID (confirmed there by
        // the server). Same server arrival date: the same message moved, not the sender sending
        // it again under the same Message-ID (a move keeps INTERNALDATE, a new delivery does
        // not). A copy on its way there (no UID) is this device's own move, not a return.
        let mut moved_back = db.prepare_cached(
            "SELECT (EXISTS (SELECT 1 FROM screened WHERE account_id = ?1 AND message_id = ?2 AND received_at = ?3)
                     OR EXISTS (SELECT 1 FROM messages o JOIN mailboxes b ON b.id = o.mailbox_id
                                WHERE o.account_id = ?1 AND o.message_id = ?2 AND b.role IN ('screener', 'refused') AND o.uid IS NOT NULL
                                  AND COALESCE(o.received_at, o.date) = ?3))
                    AND NOT EXISTS (SELECT 1 FROM messages o JOIN mailboxes b ON b.id = o.mailbox_id
                                    WHERE o.account_id = ?1 AND o.message_id = ?2 AND b.role IN ('screener', 'refused') AND o.uid IS NULL)",
        )?;
        let in_sent = in_folder_where("o", "rb.account_id = ?1 AND rb.role = 'sent'");
        // The message is one the account sent (to itself): its Message-ID was sent from here, or
        // it is the copy kept in Sent. Only that copy: the same row (Gmail's labels), or one
        // matching it on more than the Message-ID, which whoever received it knows.
        let mut sent_copy = db.prepare_cached(&format!(
            "SELECT EXISTS (SELECT 1 FROM sent_ids WHERE account_id = ?1 AND message_id = ?2)
                 OR EXISTS (SELECT 1 FROM messages o, messages c
                            WHERE c.id = ?3 AND o.account_id = ?1 AND o.message_id = ?2 AND {in_sent}
                              AND (o.id = c.id OR (o.size = c.size AND o.date = c.date AND o.from_addr IS c.from_addr)))"
        ))?;
        let mut refused = db.prepare_cached(&format!(
            "SELECT EXISTS (SELECT 1 FROM messages o WHERE lower(o.from_addr) = ?2 AND o.account_id = ?1 AND {})",
            in_folder_where("o", "rb.account_id = ?1 AND rb.role = 'refused'")
        ))?;
        let mut wrote_to = db.prepare_cached("SELECT EXISTS (SELECT 1 FROM correspondents WHERE account_id = ?1 AND email = ?2)")?;
        // In-Reply-To or References naming a message the account sent (from facteur, followed
        // up, or in Sent).
        let mut replies = db.prepare_cached(&format!(
            "WITH r(ref) AS (SELECT in_reply_to FROM messages WHERE id = ?2 AND in_reply_to IS NOT NULL
                             UNION SELECT ref FROM message_refs WHERE message_id = ?2)
             SELECT EXISTS (SELECT 1 FROM r WHERE
                    EXISTS (SELECT 1 FROM sent_ids s WHERE s.account_id = ?1 AND s.message_id = r.ref)
                 OR EXISTS (SELECT 1 FROM reminders s WHERE s.account_id = ?1 AND s.message_id = r.ref)
                 OR EXISTS (SELECT 1 FROM messages o WHERE o.account_id = ?1 AND o.message_id = r.ref AND {in_sent}))"
        ))?;
        let mut thread = db.prepare_cached("SELECT DISTINCT lower(from_addr) FROM messages WHERE account_id = ?1 AND thread_id = ?2 AND from_addr IS NOT NULL")?;
        // Copies of the batch's own messages (Gmail: All Mail) and messages also labelled as
        // held or refused do not count.
        let mut elsewhere = db.prepare(&format!(
            "SELECT EXISTS (SELECT 1 FROM messages o JOIN mailboxes b ON b.id = o.mailbox_id
                 WHERE lower(o.from_addr) = ?2 AND o.account_id = ?1
                   AND (b.role IS NULL OR b.role NOT IN ('screener', 'refused', 'junk', 'trash'))
                   AND o.id NOT IN ({batch})
                   AND (o.message_id IS NULL OR o.message_id NOT IN
                        (SELECT message_id FROM messages WHERE id IN ({batch}) AND message_id IS NOT NULL))
                   AND NOT EXISTS (SELECT 1 FROM messages c JOIN mailboxes cb ON cb.id = c.mailbox_id
                                   WHERE c.account_id = o.account_id AND c.message_id = o.message_id
                                     AND cb.role IN ('screener', 'refused'))
                   AND NOT EXISTS (SELECT 1 FROM labels ol JOIN mailboxes lb ON lb.id = ol.mailbox_id
                                   WHERE ol.message_id = o.id AND lb.role IN ('screener', 'refused')))"
        ))?;
        let mut out = Vec::new();
        for (id, sender, thread_id, message_id, signals, arrived) in candidates {
            // No sender to know: a stranger.
            if sender.is_empty() {
                out.push((id, Verdict::Hold));
                continue;
            }
            if let Some(mid) = &message_id
                && moved_back.query_row(params![account_id, mid, arrived], |r| r.get::<_, bool>(0))?
            {
                out.push((id, Verdict::Accept(sender)));
                continue;
            }
            // Authentication failed, or the shown name carries another address: the address may
            // be borrowed, so nothing known about it lets the message through (a block still
            // refuses it). It waits for the user, who can accept it.
            let suspect = suspect_sender(signals.as_deref());
            let by_address: Option<String> = decision.query_row(params![account_id, sender], |r| r.get(0)).optional()?;
            if own.contains(&base_address(&sender)) {
                let proven = authenticated(signals.as_deref(), usual.as_deref())
                    || !suspect
                        && message_id
                            .as_ref()
                            .is_some_and(|mid| sent_copy.query_row(params![account_id, mid, id], |r| r.get::<_, bool>(0)).unwrap_or(false));
                match by_address.as_deref() {
                    _ if proven => {}
                    Some("accept") => {}
                    Some("block") => out.push((id, Verdict::Refuse)),
                    _ => out.push((id, Verdict::Hold)),
                }
                continue;
            }
            match by_address.as_deref() {
                Some("block") => {
                    out.push((id, Verdict::Refuse));
                    continue;
                }
                _ if suspect => {
                    out.push((id, Verdict::Hold));
                    continue;
                }
                Some("accept") => continue,
                _ => {}
            }
            let correspondent = wrote_to.query_row(params![account_id, sender], |r| r.get::<_, bool>(0))?
                || replies.query_row(params![account_id, id], |r| r.get::<_, bool>(0))?
                || thread
                    .query_map(params![account_id, thread_id], |r| r.get::<_, String>(0))?
                    .filter_map(|r| r.ok())
                    .any(|from| own.contains(&base_address(&from)));
            let domain = sender.rsplit_once('@').map(|(_, d)| format!("@{d}")).unwrap_or_default();
            let by_domain: Option<String> = decision.query_row(params![account_id, domain], |r| r.get(0)).optional()?;
            match by_domain.as_deref() {
                Some("accept") => continue,
                Some("block") if !correspondent => {
                    out.push((id, Verdict::Refuse));
                    continue;
                }
                _ => {}
            }
            if refused.query_row(params![account_id, sender], |r| r.get::<_, bool>(0))? {
                out.push((id, Verdict::Refuse));
                continue;
            }
            let known = correspondent || elsewhere.query_row(params![account_id, sender], |r| r.get::<_, bool>(0))?;
            if !known {
                out.push((id, Verdict::Hold));
            }
        }
        Ok(out)
    }

    /// Remembers that these messages were held or refused here (by Message-ID): found again in
    /// the inbox, they were moved back elsewhere. Entries older than half a year are dropped.
    pub fn remember_screened(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.db();
        let now = now();
        db.execute(
            &format!(
                "INSERT OR REPLACE INTO screened (account_id, message_id, received_at, at)
                 SELECT account_id, message_id, COALESCE(received_at, date), ?1 FROM messages WHERE id IN ({list}) AND message_id IS NOT NULL"
            ),
            [now],
        )?;
        db.execute("DELETE FROM screened WHERE at < ?1", [now - 183 * 86_400])?;
        Ok(())
    }

    /// Forgets that these messages were screened (their sender was accepted since).
    pub fn forget_screened(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        self.db().execute(
            &format!(
                "DELETE FROM screened WHERE (account_id, message_id) IN
                 (SELECT account_id, message_id FROM messages WHERE id IN ({list}) AND message_id IS NOT NULL)"
            ),
            [],
        )?;
        Ok(())
    }

    /// A message just sent from facteur: its recipients become correspondents and its
    /// Message-ID is kept, so that their replies are never held, even before Sent is synced.
    pub fn record_sent(&self, account_id: i64, message_id: &str, recipients: &[String]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let now = now();
        tx.execute(
            "INSERT OR REPLACE INTO sent_ids (account_id, message_id, at) VALUES (?1, ?2, ?3)",
            params![account_id, message_id, now],
        )?;
        tx.execute("DELETE FROM sent_ids WHERE account_id = ?1 AND at < ?2", params![account_id, now - 365 * 86_400])?;
        {
            let mut st = tx.prepare_cached("INSERT OR IGNORE INTO correspondents (account_id, email) VALUES (?1, lower(?2))")?;
            for email in recipients.iter().map(|e| e.trim()).filter(|e| e.contains('@')) {
                st.execute(params![account_id, email])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether a domain decision leaves this address alone: an address with a decision of its
    /// own, or (`correspondents`, for a block) someone the account wrote to.
    pub fn spared_by_domain(&self, account_id: i64, email: &str, correspondents: bool) -> Result<bool> {
        Ok(self.read().query_row(
            "SELECT (?3 AND EXISTS (SELECT 1 FROM correspondents WHERE account_id = ?1 AND email = lower(?2)))
                 OR EXISTS (SELECT 1 FROM screening WHERE account_id = ?1 AND pattern = lower(?2))",
            params![account_id, email, correspondents],
            |r| r.get(0),
        )?)
    }

    /// The sender's messages still cached in "New senders" with a UID, except copies of a message
    /// that also lies in another folder (All Mail aside): moved out elsewhere, the cached copy
    /// is only stale until that folder is synced.
    pub fn held_leftovers(&self, account_id: i64, email: &str) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare_cached(&format!(
            "SELECT m.id FROM messages m
             WHERE m.account_id = ?1 AND {} AND lower(m.from_addr) = lower(?2) AND m.uid IS NOT NULL
               AND (m.message_id IS NULL OR NOT EXISTS (
                    SELECT 1 FROM messages i JOIN mailboxes ib ON ib.id = i.mailbox_id
                    WHERE i.account_id = m.account_id AND i.message_id = m.message_id
                      AND (ib.role IS NULL OR ib.role NOT IN ('screener', 'all'))))",
            in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'screener'")
        ))?;
        let rows = st.query_map(params![account_id, email], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The account's messages in folders of `role` from this sender (`pattern`: an address,
    /// or `@domain` for all of its addresses).
    pub fn messages_from_pattern(&self, account_id: i64, pattern: &str, role: Role) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT m.id FROM messages m
             WHERE m.account_id = ?1 AND {} AND m.from_addr IS NOT NULL
               AND (lower(m.from_addr) = ?2 OR (substr(?2, 1, 1) = '@' AND substr(lower(m.from_addr), -length(?2)) = ?2))
             ORDER BY m.date DESC",
            in_folder_where("m", "rb.account_id = ?1 AND rb.role = ?3")
        ))?;
        let rows = st.query_map(params![account_id, pattern, role.as_str()], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn mailbox_message_ids(&self, mailbox_id: i64) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT id FROM ({})", Boxes::of(&db, &[mailbox_id])?.pool("id")))?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn senders_of(&self, ids: &[i64]) -> Result<Vec<String>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT DISTINCT lower(from_addr) FROM messages WHERE id IN ({list}) AND from_addr IS NOT NULL"))?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Senders waiting in the "New senders" folder (of one account, or all), the most recent
    /// first: (account, address, name, number of messages, latest message).
    pub fn held_senders(&self, account_id: Option<i64>) -> Result<Vec<(i64, String, Option<String>, i64, i64)>> {
        let db = self.read();
        // One pass over the screener folders only, along the (mailbox_id, lower(from_addr), date)
        // index: no lookup of the latest message per sender.
        let screeners: Vec<i64> = db
            .prepare_cached("SELECT id FROM mailboxes WHERE role = 'screener' AND (?1 IS NULL OR account_id = ?1)")?
            .query_map([account_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let pool = Boxes::of(&db, &screeners)?.pool("id, account_id, date, from_addr, from_name");
        let mut st = db.prepare(&format!(
            "SELECT account_id, e, name, n, id FROM (
                SELECT m.id, m.account_id, m.date, lower(m.from_addr) AS e,
                       MAX(NULLIF(m.from_name, '')) OVER w AS name, COUNT(*) OVER w AS n,
                       ROW_NUMBER() OVER (PARTITION BY m.box, lower(m.from_addr) ORDER BY m.date DESC, m.id DESC) AS rn
                FROM ({pool}) m
                WHERE m.from_addr IS NOT NULL
                WINDOW w AS (PARTITION BY m.box, lower(m.from_addr)))
             WHERE rn = 1 ORDER BY date DESC"
        ))?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    // ── Similar messages ───────────────────────────────────────────────────

    /// Messages whose body is cached but not analysed yet (links); the bodies are then read one by
    /// one, not all in memory at once.
    pub fn ids_without_links(&self, limit: usize) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT m.id FROM messages m JOIN bodies b ON b.message_id = m.id
             WHERE m.link_domains IS NULL LIMIT ?1",
        )?;
        let rows = st.query_map([limit as i64], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn set_links(&self, id: i64, domains: &[String], list_id: Option<&str>) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE messages SET link_domains = ?2, list_id = COALESCE(list_id, ?3) WHERE id = ?1",
            params![id, domains.join(" "), list_id],
        )?;
        write_links(&tx, id, domains)?;
        tx.commit()?;
        Ok(())
    }

    pub fn has_body(&self, id: i64) -> Result<bool> {
        Ok(self.read().query_row("SELECT EXISTS (SELECT 1 FROM bodies WHERE message_id = ?1)", [id], |r| r.get(0))?)
    }

    /// Messages matching every given criterion, outside the bin.
    pub fn similar(&self, c: &SimilarCriteria, preview: usize) -> Result<SimilarResult> {
        use rusqlite::types::Value;
        let like = |s: &str| format!("%{}%", s.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let mut cond = vec!["COALESCE(b.role, '') != 'trash'".to_string()];
        let mut args: Vec<Value> = Vec::new();
        if let Some(a) = c.account_id {
            cond.push("m.account_id = ?".into());
            args.push(Value::Integer(a));
        }
        if let Some(addr) = &c.from_address {
            cond.push("lower(m.from_addr) = lower(?)".into());
            args.push(Value::Text(addr.trim().into()));
        }
        if let Some(d) = &c.from_domain {
            let d = d.trim().to_lowercase();
            cond.push("(lower(m.from_addr) LIKE ? OR lower(m.from_addr) LIKE ?)".into());
            args.push(Value::Text(format!("%@{d}")));
            args.push(Value::Text(format!("%.{d}")));
        }
        if let Some(to) = &c.to_address {
            cond.push(
                "(EXISTS (SELECT 1 FROM json_each(m.to_list) j WHERE lower(json_extract(j.value, '$.email')) = lower(?))
                  OR EXISTS (SELECT 1 FROM json_each(m.cc_list) j WHERE lower(json_extract(j.value, '$.email')) = lower(?)))"
                    .into(),
            );
            args.push(Value::Text(to.trim().into()));
            args.push(Value::Text(to.trim().into()));
        }
        if let Some(s) = c.subject.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            // Folded both sides: case, accents and doubled spaces don't keep a message out.
            cond.push("fold(m.subject) LIKE ? ESCAPE '\\'".into());
            args.push(Value::Text(like(&fold(s))));
        }
        if let Some(text) = c.body.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            // Every word must appear in the body (prefix match, like the search box). Gmail keeps
            // one body for several copies of a message: the copies match too.
            // A token without a letter or digit (« : », « - », « € ») is an empty phrase to FTS5,
            // which an explicit AND turns into no match at all.
            let words: Vec<String> = text
                .split_whitespace()
                .filter(|w| w.chars().any(char::is_alphanumeric))
                .map(|w| format!("body:\"{}\"*", w.replace('"', "\"\"")))
                .collect();
            if words.is_empty() {
                cond.push("0".into());
            }
            let query = if words.is_empty() { "\"\"".to_string() } else { words.join(" AND ") };
            cond.push(
                "(m.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)
                  OR (m.message_id IS NOT NULL AND (m.account_id, m.message_id) IN (
                      SELECT o.account_id, o.message_id FROM messages o
                      WHERE o.id IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?))))"
                    .into(),
            );
            args.push(Value::Text(query.clone()));
            args.push(Value::Text(query));
        }
        if !c.link_domains.is_empty() {
            let any = vec!["?"; c.link_domains.len()].join(",");
            cond.push(format!("m.id IN (SELECT message_id FROM message_links WHERE domain IN ({any}))"));
            args.extend(c.link_domains.iter().map(|d| Value::Text(d.trim().to_lowercase())));
        }
        if let Some(l) = &c.list_id {
            cond.push("m.list_id = lower(?)".into());
            args.push(Value::Text(l.trim().into()));
        }
        if c.bulk_only {
            cond.push("m.bulk = 1".into());
        }
        if let Some(from) = c.date_from {
            cond.push("m.date >= ?".into());
            args.push(Value::Integer(from));
        }
        if let Some(to) = c.date_to {
            cond.push("m.date <= ?".into());
            args.push(Value::Integer(to));
        }
        let mut where_ = cond.join(" AND ");
        // Whole conversations: every message of a thread one of them matched (out of the bin),
        // the matching ones read in the subquery with the same arguments.
        if c.whole_threads {
            where_ = format!(
                "COALESCE(b.role, '') != 'trash' AND (m.account_id, m.thread_id) IN (
                    SELECT m.account_id, m.thread_id FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id WHERE {where_})"
            );
        }
        let from = "FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id";

        let db = self.read();
        let (count, bytes): (i64, i64) = db.query_row(
            &format!(
                "SELECT COUNT(DISTINCT m.account_id || ':' || COALESCE(m.message_id, 'id:' || m.id)), COALESCE(SUM(m.size), 0)
                 {from} WHERE {where_}"
            ),
            rusqlite::params_from_iter(args.iter()),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // Every matching row, with the message it belongs to (account and Message-ID): a server
        // of folders may hold a message in several (a copy filed by hand, mail sent to oneself).
        let rows: Vec<(i64, String)> = {
            let mut st = db.prepare(&format!(
                "SELECT m.id, m.account_id || ':' || COALESCE(m.message_id, 'id:' || m.id) {from} WHERE {where_}"
            ))?;
            st.query_map(rusqlite::params_from_iter(args.iter()), |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
        let key_of: HashMap<i64, &str> = rows.iter().map(|(id, key)| (*id, key.as_str())).collect();
        let mut by_key: HashMap<&str, Vec<i64>> = HashMap::new();
        for (id, key) in &rows {
            by_key.entry(key.as_str()).or_default().push(*id);
        }
        let messages: Vec<MessageSummary> = {
            let mut page = args.clone();
            page.push(Value::Integer(preview as i64));
            let mut st = db.prepare(&format!(
                "SELECT {SUMMARY_COLS} {from} WHERE {where_}
                 GROUP BY m.account_id, COALESCE(m.message_id, 'id:' || m.id) ORDER BY m.date DESC LIMIT ?"
            ))?;
            st.query_map(rusqlite::params_from_iter(page.iter()), summary_from_row)?.collect::<Result<_, _>>()?
        };
        // A message shown through a copy left out above (GROUP BY picks any) still names its
        // copies by its key.
        let key_by_message = |m: &MessageSummary| -> Option<String> {
            db.query_row(
                "SELECT account_id || ':' || COALESCE(message_id, 'id:' || id) FROM messages WHERE id = ?1",
                [m.id],
                |r| r.get(0),
            )
            .ok()
        };
        let copies = messages
            .iter()
            .map(|m| {
                key_of
                    .get(&m.id)
                    .and_then(|k| by_key.get(k))
                    .cloned()
                    .or_else(|| key_by_message(m).and_then(|k| by_key.get(k.as_str()).cloned()))
                    .unwrap_or_else(|| vec![m.id])
            })
            .collect();
        Ok(SimilarResult { count, bytes, ids, messages, copies })
    }

    pub fn trusted_images(&self) -> Result<Vec<String>> {
        let db = self.read();
        let mut st = db.prepare("SELECT key FROM trusted_images ORDER BY key")?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn add_trusted_images(&self, key: &str) -> Result<()> {
        self.db().execute(
            "INSERT OR IGNORE INTO trusted_images (key, created_at) VALUES (lower(?1), ?2)",
            params![key.trim(), now()],
        )?;
        Ok(())
    }

    pub fn remove_trusted_images(&self, key: &str) -> Result<()> {
        self.db().execute("DELETE FROM trusted_images WHERE key = lower(?1)", [key.trim()])?;
        Ok(())
    }

    pub fn images_trusted(&self, key: &str) -> Result<bool> {
        Ok(self.read().query_row("SELECT EXISTS (SELECT 1 FROM trusted_images WHERE key = ?1)", [key], |r| r.get(0))?)
    }

    // ── Settings ───────────────────────────────────────────────────────────

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.read().query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db().execute(SET_SETTING, params![key, value])?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.db().execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Known logos: domain -> (logo if any, lookup date).
    pub fn sender_logos(&self, domains: &[String]) -> Result<HashMap<String, (Option<String>, i64)>> {
        if domains.is_empty() {
            return Ok(HashMap::new());
        }
        let db = self.read();
        let placeholders = vec!["?"; domains.len()].join(",");
        let mut st = db.prepare(&format!("SELECT domain, data, fetched_at FROM sender_logos WHERE domain IN ({placeholders})"))?;
        let rows = st
            .query_map(rusqlite::params_from_iter(domains.iter()), |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn save_sender_logo(&self, domain: &str, data: Option<&str>) -> Result<()> {
        self.db().execute(
            "INSERT OR REPLACE INTO sender_logos (domain, data, fetched_at) VALUES (?1, ?2, ?3)",
            params![domain, data, now()],
        )?;
        Ok(())
    }

    // ── Trust ──────────────────────────────────────────────────────────────

    /// The server that usually signs the authentication verdict of this account's mail: Gmail's
    /// for a Gmail account (Gmail puts its own verdict on top of every message), else the most
    /// frequent among the recently received messages, whatever their folder (an inbox kept empty
    /// teaches nothing), when it is clear (at least 5 messages and 60 % of the verdicts).
    /// `None`: not enough evidence yet.
    pub fn usual_authserv(&self, account_id: i64) -> Result<Option<String>> {
        let db = self.read();
        let host: Option<String> = db.query_row("SELECT imap_host FROM accounts WHERE id = ?1", [account_id], |r| r.get(0)).optional()?;
        if host.is_some_and(|h| matches!(h.to_ascii_lowercase().trim_end_matches('.'), "imap.gmail.com" | "imap.googlemail.com")) {
            return Ok(Some("mx.google.com".into()));
        }
        // Sent and drafts were written here: their headers say nothing of the incoming mail.
        // Nor junk and bin, where a spammer's own forged verdict may sit on top on a provider
        // that writes none, nor the screener's folders, which hold only what unknown or refused
        // senders sent. The most recently received, not by row (a folder synced or
        // backfilled last holds the highest ids) nor by the Date the sender wrote. The inbox
        // first: mail imported from another provider (its verdicts written by that one) rarely
        // sits there; the other folders only when it says nothing.
        // The winner must come from several sender domains: one sender alone, sending enough
        // messages with a planted verdict, does not teach it.
        for sql in [USUAL_AUTHSERV_INBOX, USUAL_AUTHSERV] {
            let mut st = db.prepare_cached(sql)?;
            let counts: Vec<(String, i64, i64)> = st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            let total: i64 = counts.iter().map(|(_, n, _)| n).sum();
            if let Some((a, ..)) = counts.into_iter().next().filter(|(_, n, domains)| *n >= 5 && n * 100 >= total * 60 && *domains >= 3) {
                return Ok(Some(a));
            }
        }
        Ok(None)
    }

    pub fn trust_inputs(&self, id: i64) -> Result<TrustInputs> {
        Ok(self.read().query_row(
            "SELECT m.account_id, m.from_addr, b.role, m.signals
             FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id WHERE m.id = ?1",
            [id],
            |r| {
                let role: Option<String> = r.get(2)?;
                let signals: Option<String> = r.get(3)?;
                Ok(TrustInputs {
                    account_id: r.get(0)?,
                    from: r.get(1)?,
                    role: role.as_deref().and_then(Role::parse),
                    signals: signals.and_then(|s| serde_json::from_str(&s).ok()),
                })
            },
        )?)
    }

    pub fn set_signals(&self, id: i64, signals: &crate::trust::Signals) -> Result<()> {
        self.db().execute("UPDATE messages SET signals = ?2 WHERE id = ?1", params![id, serde_json::to_string(signals)?])?;
        Ok(())
    }

    /// (the account wrote to them, number of their other messages outside junk).
    pub fn sender_context(&self, account_id: i64, email: &str, exclude: i64) -> Result<(bool, i64)> {
        let db = self.read();
        let wrote: bool = db.query_row(
            "SELECT EXISTS (SELECT 1 FROM correspondents WHERE account_id = ?1 AND email = lower(?2))",
            params![account_id, email],
            |r| r.get(0),
        )?;
        // Counted up to 10 only: trust only looks at "seen twice already", and a notification
        // sender may have thousands of messages.
        let seen: i64 = db.query_row(
            "SELECT COUNT(*) FROM (
                SELECT 1 FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
                WHERE lower(m.from_addr) = lower(?2) AND m.account_id = ?1 AND m.id != ?3 AND COALESCE(b.role, '') != 'junk'
                LIMIT 10)",
            params![account_id, email, exclude],
            |r| r.get(0),
        )?;
        Ok((wrote, seen))
    }

    pub fn is_blocked(&self, email: &str) -> Result<bool> {
        Ok(self.read().query_row(
            "SELECT EXISTS (SELECT 1 FROM blocked_senders WHERE email = lower(?1))",
            [email],
            |r| r.get(0),
        )?)
    }

    /// Latest message received from this address, across all accounts.
    pub fn latest_from(&self, email: &str) -> Result<Option<i64>> {
        Ok(self
            .read()
            .query_row(
                "SELECT id FROM messages WHERE lower(from_addr) = lower(?1) ORDER BY date DESC LIMIT 1",
                [email],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// [`Store::latest_from`] for many addresses in one statement (the `messages_from_date`
    /// index), keyed by the address as given. Addresses never seen are left out.
    pub fn latest_from_many(&self, emails: &[String]) -> Result<HashMap<String, i64>> {
        if emails.is_empty() {
            return Ok(HashMap::new());
        }
        let db = self.read();
        let mut st = db.prepare_cached(
            "SELECT j.value, (SELECT m.id FROM messages m WHERE lower(m.from_addr) = lower(j.value) ORDER BY m.date DESC LIMIT 1)
             FROM json_each(?1) j",
        )?;
        let rows = st
            .query_map([serde_json::to_string(emails)?], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)))?
            .filter_map(|r| r.map(|(e, id)| id.map(|id| (e, id))).transpose())
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// [`Store::trust_inputs`], with [`Store::sender_context`] and [`Store::is_blocked`] for its
    /// sender (`None`: no sender address), for many messages in one statement. Missing messages
    /// are left out.
    pub fn trust_inputs_many(&self, ids: &[i64]) -> Result<HashMap<i64, (TrustInputs, Option<(bool, i64, bool)>)>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT m.id, m.account_id, m.from_addr, b.role, m.signals,
                    EXISTS (SELECT 1 FROM correspondents c WHERE c.account_id = m.account_id AND c.email = lower(m.from_addr)),
                    (SELECT COUNT(*) FROM (
                        SELECT 1 FROM messages x JOIN mailboxes xb ON xb.id = x.mailbox_id
                        WHERE lower(x.from_addr) = lower(m.from_addr) AND x.account_id = m.account_id AND x.id != m.id
                          AND (m.message_id IS NULL OR x.message_id IS NOT m.message_id)
                          AND COALESCE(xb.role, '') != 'junk'
                        LIMIT 10)),
                    EXISTS (SELECT 1 FROM blocked_senders s WHERE s.email = lower(m.from_addr))
             FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id WHERE m.id IN ({list})"
        ))?;
        let rows = st
            .query_map([], |r| {
                let from: Option<String> = r.get(2)?;
                let role: Option<String> = r.get(3)?;
                let signals: Option<String> = r.get(4)?;
                let sender = if from.is_some() { Some((r.get(5)?, r.get(6)?, r.get(7)?)) } else { None };
                let inputs = TrustInputs {
                    account_id: r.get(1)?,
                    from,
                    role: role.as_deref().and_then(Role::parse),
                    signals: signals.and_then(|s| serde_json::from_str(&s).ok()),
                };
                Ok((r.get(0)?, (inputs, sender)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn message_category(&self, id: i64) -> Result<Category> {
        let c: String = self.read().query_row("SELECT category FROM messages WHERE id = ?1", [id], |r| r.get(0))?;
        Ok(Category::parse(&c).unwrap_or_default())
    }

    pub fn inbox_ids(&self) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare("SELECT id FROM mailboxes WHERE role = 'inbox' ORDER BY account_id")?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Messages arrived since `since` (INTERNALDATE), still in the inbox or already handled; sent,
    /// drafts, bin and junk aside. A message in two folders (INBOX and Gmail's All Mail) counts
    /// once.
    pub fn received_since(&self, since: i64) -> Result<i64> {
        Ok(self.read().query_row(
            "SELECT COUNT(DISTINCT m.account_id || ':' || COALESCE(m.message_id, 'id:' || m.id))
             FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
             WHERE (m.received_at >= ?1 OR (m.received_at IS NULL AND m.date >= ?1))
               AND COALESCE(b.role, '') NOT IN ('sent', 'drafts', 'trash', 'junk')
               AND NOT EXISTS (SELECT 1 FROM labels l JOIN mailboxes lb ON lb.id = l.mailbox_id
                               WHERE l.message_id = m.id AND lb.role IN ('sent', 'drafts'))",
            [since],
            |r| r.get(0),
        )?)
    }

    /// Pinned messages of every account, bin and junk aside, newest first, with their total. A
    /// message in several folders (INBOX, Starred and All Mail on Gmail) shows once, preferably
    /// from the inbox.
    pub fn flagged_messages(&self, limit: usize) -> Result<(i64, Vec<MessageSummary>)> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "WITH f AS (
                SELECT m.*, ROW_NUMBER() OVER (
                    PARTITION BY m.account_id, COALESCE(m.message_id, 'id:' || m.id)
                    ORDER BY CASE b.role WHEN 'inbox' THEN 0 WHEN 'all' THEN 2 WHEN 'flagged' THEN 2 ELSE 1 END, m.id
                ) AS rn
                FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
                WHERE m.flagged = 1 AND COALESCE(b.role, '') NOT IN ('trash', 'junk')
             )
             SELECT {SUMMARY_COLS}, COUNT(*) OVER () FROM f m WHERE rn = 1 ORDER BY m.date DESC LIMIT ?1"
        ))?;
        let mut total = 0;
        let rows = st
            .query_map([limit as i64], |r| Ok((summary_from_row(r)?, r.get::<_, i64>(15)?)))?
            .map(|row| {
                row.map(|(m, count)| {
                    total = count;
                    m
                })
            })
            .collect::<Result<_, _>>()?;
        Ok((total, rows))
    }

    /// Every message of a thread, across folders, without duplicates (one Message-ID in INBOX and
    /// All Mail, for instance).
    /// `prefer`: the folder the thread is opened from. Of a message's copies (Gmail labels),
    /// the one in that folder is shown, so the actions on "this folder" find it.
    pub fn thread_messages(&self, account_id: i64, thread_id: i64, prefer: Option<i64>) -> Result<Vec<MessageSummary>> {
        let db = self.read();
        // A message carrying `prefer` as a label is shown from there, as if it were its folder.
        let labelled = "EXISTS (SELECT 1 FROM labels l WHERE l.message_id = m.id AND l.mailbox_id = ?3)";
        let cols = SUMMARY_COLS.replacen("m.mailbox_id", &format!("CASE WHEN {labelled} THEN ?3 ELSE m.mailbox_id END"), 1);
        let mut st = db.prepare(&format!(
            "SELECT {cols}, m.message_id FROM messages m
             LEFT JOIN mailboxes b ON b.id = m.mailbox_id
             WHERE m.account_id = ?1 AND m.thread_id = ?2
             ORDER BY m.date, m.mailbox_id IS NOT ?3 AND NOT {labelled},
                      CASE b.role WHEN 'all' THEN 2 WHEN 'trash' THEN 3 ELSE 1 END"
        ))?;
        let rows = st.query_map(params![account_id, thread_id, prefer], |r| {
            Ok((summary_from_row(r)?, r.get::<_, Option<String>>(15)?))
        })?;
        let mut seen_ids = std::collections::HashSet::new();
        let mut out = Vec::new();
        for row in rows {
            let (m, key) = row?;
            if key.is_none_or(|k| seen_ids.insert(k)) {
                out.push(m);
            }
        }
        Ok(out)
    }

    /// Summaries of several messages, newest first.
    pub fn summaries(&self, ids: &[i64], limit: usize) -> Result<Vec<MessageSummary>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT {SUMMARY_COLS} FROM messages m WHERE m.id IN ({list}) ORDER BY m.date DESC LIMIT {limit}"
        ))?;
        let rows = st.query_map([], summary_from_row)?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Those of `ids` the server received at `since` or later (INTERNALDATE, which a move keeps;
    /// the Date the sender wrote when it is unknown).
    pub fn arrived_since(&self, ids: &[i64], since: i64) -> Result<HashSet<i64>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT id FROM messages WHERE id IN ({list}) AND COALESCE(received_at, date) >= ?1"))?;
        let rows = st.query_map([since], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Those of `ids` whose content is not downloaded yet (nor shared from a stored copy).
    pub fn without_body(&self, ids: &[i64]) -> Result<HashSet<i64>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT id FROM messages WHERE id IN ({list}) AND has_body = 0"))?;
        let rows = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn summary(&self, id: i64) -> Result<MessageSummary> {
        self.read()
            .query_row(&format!("SELECT {SUMMARY_COLS} FROM messages m WHERE m.id = ?1"), [id], summary_from_row)
            .optional()?
            .ok_or_else(|| Error::NotFound(crate::tr!("message {id}", "message {id}")))
    }

    pub fn location(&self, id: i64) -> Result<Location> {
        self.read()
            .query_row(
                "SELECT m.account_id, m.mailbox_id, b.name, m.uid
                 FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id WHERE m.id = ?1",
                [id],
                |r| Ok(Location { account_id: r.get(0)?, mailbox_id: r.get(1)?, mailbox: r.get(2)?, uid: r.get(3)? }),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(crate::tr!("message {id}", "message {id}")))
    }

    /// Size of the message announced by the server (RFC822.SIZE), in bytes; 0 if unknown.
    pub fn message_size(&self, id: i64) -> Result<u64> {
        let size: Option<i64> = self.read().query_row("SELECT size FROM messages WHERE id = ?1", [id], |r| r.get(0)).optional()?.flatten();
        Ok(size.unwrap_or(0).max(0) as u64)
    }

    /// Message-ID of a message and its References. Without References (some clients only write
    /// In-Reply-To), its In-Reply-To stands for them, so that a reply still joins the thread.
    pub fn message_ids(&self, id: i64) -> Result<(Option<String>, Vec<String>)> {
        Ok(self.read().query_row("SELECT message_id, refs, in_reply_to FROM messages WHERE id = ?1", [id], |r| {
            let refs: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
            let mut refs: Vec<String> = refs.split_whitespace().map(str::to_string).collect();
            if refs.is_empty()
                && let Some(parent) = r.get::<_, Option<String>>(2)?.filter(|p| !p.trim().is_empty())
            {
                refs.push(parent);
            }
            Ok((r.get(0)?, refs))
        })?)
    }

    /// Searches the cache, operators included (see `search::parse`). Free words go through the
    /// full-text index; operators become SQL conditions.
    ///
    /// `bins`: search the bin and junk too. Without it they are left out unless asked for
    /// explicitly (`dans:corbeille`).
    pub fn search(&self, account_id: i64, query: &str, limit: usize, bins: bool) -> Result<Vec<MessageSummary>> {
        use rusqlite::types::Value;
        let q = crate::search::parse(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let like = |s: &str| Value::Text(format!("%{}%", fold(s).replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
        // New mail the screener has not decided about yet stays out of the results too.
        let mut cond = vec!["m.account_id = ?".to_string(), "m.unscreened = 0".to_string()];
        let mut args: Vec<Value> = vec![Value::Integer(account_id)];
        for v in &q.from {
            cond.push("(fold(COALESCE(m.from_addr, '')) LIKE ? ESCAPE '\\' OR fold(COALESCE(m.from_name, '')) LIKE ? ESCAPE '\\')".into());
            args.extend([like(v), like(v)]);
        }
        // The recipients' addresses and names, not their JSON: its keys and `null` would match
        // `à:mai` or `avec:nam` on every message. Four placeholders, the same value in each.
        let recipient = |list: &str| {
            format!(
                "EXISTS (SELECT 1 FROM json_each(m.{list}) j
                         WHERE fold(COALESCE(json_extract(j.value, '$.email'), '')) LIKE ? ESCAPE '\\'
                            OR fold(COALESCE(json_extract(j.value, '$.name'), '')) LIKE ? ESCAPE '\\')"
            )
        };
        let recipients = format!("{} OR {}", recipient("to_list"), recipient("cc_list"));
        for v in &q.to {
            cond.push(format!("({recipients})"));
            args.extend([like(v), like(v), like(v), like(v)]);
        }
        for v in &q.with {
            cond.push(format!(
                "(fold(COALESCE(m.from_addr, '')) LIKE ? ESCAPE '\\' OR fold(COALESCE(m.from_name, '')) LIKE ? ESCAPE '\\'
                  OR {recipients})"
            ));
            args.extend([like(v), like(v), like(v), like(v), like(v), like(v)]);
        }
        for v in &q.subject {
            cond.push("fold(m.subject) LIKE ? ESCAPE '\\'".into());
            args.push(like(v));
        }
        if q.has_attachment {
            cond.push("m.has_attachments = 1".into());
        }
        if let Some(labelled) = q.labelled {
            cond.push(format!("{}EXISTS ({LABELLED})", if labelled { "" } else { "NOT " }));
        }
        if let Some(unread) = q.unread {
            cond.push(format!("m.seen = {}", if unread { 0 } else { 1 }));
        }
        if q.flagged {
            cond.push("m.flagged = 1".into());
        }
        if let Some(after) = q.after {
            cond.push("m.date >= ?".into());
            args.push(Value::Integer(after));
        }
        if let Some(before) = q.before {
            cond.push("m.date < ?".into());
            args.push(Value::Integer(before));
        }
        if let Some(folder) = &q.folder {
            let ids = self.folders_matching(account_id, folder)?;
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            cond.push(Boxes::of(&self.read(), &ids)?.holds("m"));
        } else if !bins {
            // List computed once (account known), not a subquery per message.
            cond.push(format!("m.mailbox_id NOT IN (SELECT id FROM mailboxes WHERE account_id = {account_id} AND role IN ('trash', 'junk'))"));
        }
        // Free words: quoted prefixes, so no FTS5 syntax injection.
        // A word of one letter is matched whole: as a prefix it would walk most of the index's
        // terms (seconds on a large mailbox, at each key typed).
        let fts: String = q
            .words
            .iter()
            .map(|w| fts_word(w))
            .collect::<Vec<_>>()
            .join(" ");
        let from = if fts.is_empty() {
            "messages m".to_string()
        } else {
            // Only one copy of a message holds its body (Gmail labels): a copy in the folder
            // searched matches when the copy holding the body does, wherever that one is (with
            // the bin and junk searched too, the inbox's copy still stands for it). Driven
            // by the index's hits and their copies, never a scan of the messages.
            // Its two placeholders come first in the statement.
            args.splice(0..0, [Value::Text(fts.clone()), Value::Text(fts)]);
            format!(
                "(SELECT rowid AS id FROM messages_fts WHERE messages_fts MATCH ?
                  UNION
                  SELECT o.id FROM messages_fts f JOIN messages m ON m.id = f.rowid JOIN messages o ON {SAME_COPY}
                  WHERE messages_fts MATCH ? AND m.message_id IS NOT NULL) k
                 CROSS JOIN messages m ON m.id = k.id"
            )
        };
        args.push(Value::Integer(limit.saturating_mul(3) as i64));
        let db = self.read();
        // Copies of one message (Gmail: the inbox's and All Mail's) are read with room for them
        // (the limit above, times 3), then shown once.
        let mut st = db.prepare(&format!(
            "SELECT {SUMMARY_COLS}, m.message_id, m.size, (SELECT role FROM mailboxes WHERE id = m.mailbox_id) IS 'all'
             FROM {from} WHERE {} ORDER BY m.date DESC LIMIT ?",
            cond.join(" AND ")
        ))?;
        let rows = st.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            let message_id: Option<String> = r.get(15)?;
            let size: i64 = r.get(16)?;
            Ok((summary_from_row(r)?, message_id, size, r.get::<_, bool>(17)?))
        })?;
        let mut found = one_per_message(rows.collect::<Result<_, _>>()?, limit);
        drop(st);
        // Gmail: each result with its labels, as in All Mail's list.
        let gmail: bool = db.query_row("SELECT EXISTS (SELECT 1 FROM mailboxes WHERE account_id = ?1 AND role = 'all')", [account_id], |r| r.get(0))?;
        if gmail {
            let mut labels = labels_of(&db, &found.iter().map(|m| m.id).collect::<Vec<_>>())?;
            for m in &mut found {
                m.labels = labels.remove(&m.id).unwrap_or_default();
            }
        }
        Ok(found)
    }

    /// The account's folders named by `dans:...`: a role (`envoyés`), else the name.
    pub fn folders_matching(&self, account_id: i64, value: &str) -> Result<Vec<i64>> {
        let boxes = self.list_mailboxes(account_id)?;
        let v = value.to_lowercase();
        // A role word: the folders of that role, and those only named so (a plain "Spam" next
        // to the server's junk folder), as the suggestions show them under that word.
        if let Some(role) = crate::search::folder_role(value) {
            return Ok(boxes.iter().filter(|m| m.role == Some(role) || m.display_name.to_lowercase() == v).map(|m| m.id).collect());
        }
        Ok(boxes.iter().filter(|m| m.display_name.to_lowercase().contains(&v)).map(|m| m.id).collect())
    }

    // ── Local (optimistic) changes ─────────────────────────────────────────

    pub fn set_flag(&self, id: i64, flag: FlagKind, value: bool) -> Result<()> {
        self.set_flags_local(&[id], flag, value)
    }

    /// Changes a flag locally, for a whole batch in one transaction; the message's other copies
    /// follow at once (Gmail, where they are labels of one message). A message being moved, with
    /// no UID, keeps the change pending: it goes to the server once the message has arrived
    /// (otherwise the sync would overwrite it). Gmail's labels: pinning is the Starred label too
    /// (Gmail ties `\Flagged` to it), changed here at once so that the Pinned folder follows
    /// before the next sync.
    pub fn set_flags_local(&self, ids: &[i64], flag: FlagKind, value: bool) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let col = flag.column();
            let mut set = tx.prepare(&format!("UPDATE messages SET {col} = ?2 WHERE id = ?1"))?;
            let mut keep = tx.prepare(&format!(
                "UPDATE messages SET pending_flags = json_set(COALESCE(pending_flags, '{{}}'), '$.{col}', json(?2))
                 WHERE id = ?1 AND uid IS NULL"
            ))?;
            let starred = match (flag, value) {
                (FlagKind::Flagged, true) => Some(
                    "INSERT OR IGNORE INTO labels (mailbox_id, message_id)
                     SELECT b.id, m.id FROM messages m JOIN mailboxes b ON b.account_id = m.account_id AND b.role = 'flagged' AND b.is_label = 1
                     WHERE m.id = ?1",
                ),
                (FlagKind::Flagged, false) => Some(
                    "DELETE FROM labels WHERE message_id = ?1 AND mailbox_id IN (SELECT id FROM mailboxes WHERE role = 'flagged' AND is_label = 1)",
                ),
                _ => None,
            };
            let mut starred = starred.map(|sql| tx.prepare(sql)).transpose()?;
            let mut place = tx.prepare("SELECT mailbox_id, uid FROM messages WHERE id = ?1 AND uid IS NOT NULL")?;
            let mut places = Vec::new();
            for id in ids {
                places.extend(place.query_row([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?))).optional()?);
            }
            // Noted before the write, under the writer: a sync's merge after it sees it.
            self.note_flag_changes(&places);
            for id in ids {
                set.execute(params![id, value])?;
                keep.execute(params![id, if value { "true" } else { "false" }])?;
                if let Some(st) = starred.as_mut() {
                    st.execute([id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes the rows of the account's Pinned folder (role 'flagged') that are no longer
    /// pinned: the server takes them out of that folder as soon as the flag goes, and the list
    /// and its count follow now rather than at the next sync. Moved rows (no UID) stay.
    pub fn drop_unpinned(&self, account_id: i64) -> Result<usize> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let ids: Vec<i64> = {
            let mut st = tx.prepare(
                "SELECT m.id FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
                 WHERE b.account_id = ?1 AND b.role = 'flagged' AND m.flagged = 0 AND m.uid IS NOT NULL",
            )?;
            st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        let n = delete_ids(&tx, &ids)?;
        tx.commit()?;
        Ok(n)
    }

    /// Those of `ids` that still exist, in the order given, each once.
    pub fn current_ids(&self, ids: &[i64]) -> Vec<i64> {
        let existing: HashSet<i64> = if ids.is_empty() {
            HashSet::new()
        } else {
            let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            let db = self.read();
            db.prepare(&format!("SELECT id FROM messages WHERE id IN ({list})"))
                .and_then(|mut st| st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>())
                .unwrap_or_default()
        };
        let mut seen = HashSet::new();
        ids.iter().copied().filter(|id| existing.contains(id) && seen.insert(*id)).collect()
    }

    /// Where these messages are, in the order given. Messages gone are left out.
    pub fn locations(&self, ids: &[i64]) -> Result<Vec<(i64, Location)>> {
        let ids = &self.current_ids(ids);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT m.id, m.account_id, m.mailbox_id, b.name, m.uid
             FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id WHERE m.id IN ({list})"
        ))?;
        let rows: HashMap<i64, Location> = st
            .query_map([], |r| Ok((r.get(0)?, Location { account_id: r.get(1)?, mailbox_id: r.get(2)?, mailbox: r.get(3)?, uid: r.get(4)? })))?
            .collect::<Result<_, _>>()?;
        Ok(ids.iter().filter_map(|id| rows.get(id).map(|l| (*id, l.clone()))).collect())
    }

    /// The account files each message once under several labels (Gmail: it has an All Mail folder).
    pub fn has_all_mail(&self, account_id: i64) -> Result<bool> {
        Ok(self.read().query_row(
            "SELECT EXISTS (SELECT 1 FROM mailboxes WHERE account_id = ?1 AND role = 'all')",
            [account_id],
            |r| r.get(0),
        )?)
    }

    pub fn set_answered(&self, id: i64) -> Result<()> {
        self.db().execute("UPDATE messages SET answered = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Moves locally; the UID is unknown until the destination folder's next sync.
    pub fn move_local(&self, id: i64, dest_mailbox_id: i64) -> Result<()> {
        self.db().execute("UPDATE messages SET mailbox_id = ?2, uid = NULL WHERE id = ?1", params![id, dest_mailbox_id])?;
        Ok(())
    }

    /// Those of `ids` filed in one of the user's own folders (see `LABELLED`).
    pub fn labelled(&self, ids: &[i64]) -> Result<HashSet<i64>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!("SELECT m.id FROM messages m WHERE m.id IN ({list}) AND EXISTS ({LABELLED})"))?;
        Ok(st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?)
    }

    pub fn move_many_local(&self, ids: &[i64], dest_mailbox_id: i64) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        {
            let mut st = tx.prepare("UPDATE messages SET mailbox_id = ?2, uid = NULL WHERE id = ?1")?;
            for id in ids {
                st.execute(params![id, dest_mailbox_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_message(&self, id: i64) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        delete_ids(&tx, &[id])?;
        tx.commit()?;
        Ok(())
    }

    /// Notes the folder's local rows as matching the server count just read. `leaving`: rows
    /// moved or deleted locally that the server still lists. `if_unset`: only when none is
    /// noted yet (folder unchanged on the server, the one noted still holds).
    pub fn set_count_base(&self, mailbox_id: i64, leaving: usize, if_unset: bool) -> Result<()> {
        self.db().execute(
            "UPDATE mailboxes SET count_base = ?2 + (SELECT COUNT(*) FROM messages WHERE mailbox_id = ?1 AND uid IS NOT NULL)
             WHERE id = ?1 AND (NOT ?3 OR count_base IS NULL)",
            params![mailbox_id, leaving as i64, if_unset],
        )?;
        Ok(())
    }

    /// `n` rows were added to the folder outside a sync (older history, a server search), that
    /// the server's count as last read already included: the displayed total stays as it was.
    pub fn raise_count_base(&self, mailbox_id: i64, n: usize) -> Result<()> {
        if n > 0 {
            self.db().execute(
                "UPDATE mailboxes SET count_base = count_base + ?2 WHERE id = ?1 AND count_base IS NOT NULL",
                params![mailbox_id, n as i64],
            )?;
        }
        Ok(())
    }

    /// The moves waiting out their undo window that carry these messages, given by the folder
    /// and UID they left (`moved`), go now. Other moves, into the same folders or not, keep
    /// their window: the user may still undo them. Returns how many were released.
    ///
    /// The queue keeps its order: an action still undoable holds back every move queued after
    /// it. Those queued before a released move, or before a move into `into` that has no undo
    /// window (screener, rules), lose theirs too, or the messages would still not go.
    pub fn release_moves_of(&self, account_id: i64, moved: &HashSet<(String, u32)>, into: &[String]) -> Result<usize> {
        if moved.is_empty() && into.is_empty() {
            return Ok(0);
        }
        // id, mailbox, dest, not_before, op
        type QueuedMove = (i64, String, Option<String>, Option<i64>, String);
        let db = self.db();
        let rows: Vec<QueuedMove> = {
            let mut st = db.prepare(
                "SELECT id, mailbox, dest, not_before, op FROM pending_ops
                 WHERE account_id = ?1 AND failed = 0 AND kind IN ('move', 'move_set') AND mailbox IS NOT NULL",
            )?;
            st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?
        };
        let mut release = db.prepare("UPDATE pending_ops SET not_before = NULL WHERE id = ?1")?;
        let mut n = 0;
        // The last move that must go now.
        let mut last = None;
        for (id, mailbox, dest, not_before, op) in rows {
            if not_before.is_none() {
                if dest.is_some_and(|d| into.contains(&d)) {
                    last = last.max(Some(id));
                }
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&op) else { continue };
            let uids = v["uid"].as_u64().into_iter().chain(v["uids"].as_array().into_iter().flatten().filter_map(|u| u.as_u64()));
            if uids.map(|u| (mailbox.clone(), u as u32)).any(|key| moved.contains(&key)) {
                n += release.execute([id])?;
                last = last.max(Some(id));
            }
        }
        if let Some(last) = last {
            n += db.execute(
                "UPDATE pending_ops SET not_before = NULL WHERE account_id = ?1 AND failed = 0 AND not_before IS NOT NULL AND id < ?2",
                params![account_id, last],
            )?;
        }
        Ok(n)
    }

    /// A move into this folder is still waiting to leave (or to finish).
    pub fn moves_pending_into(&self, mailbox_id: i64) -> Result<bool> {
        Ok(self.read().query_row(
            &format!("SELECT {MOVE_PENDING_INTO}"),
            [mailbox_id],
            |r| r.get(0),
        )?)
    }

    /// Removes moved messages whose arrival the server never confirmed. `pending_before`: a move
    /// into this folder was on its way when the sync read it; it may have arrived since unseen,
    /// so wait for the next sync.
    pub fn drop_orphan_placeholders(&self, mailbox_id: i64, pending_before: bool) -> Result<()> {
        // Nothing moved here: no write at all (this runs after every folder sync).
        let any: bool = self.read().query_row(
            "SELECT EXISTS (SELECT 1 FROM messages WHERE mailbox_id = ?1 AND uid IS NULL)",
            [mailbox_id],
            |r| r.get(0),
        )?;
        if !any || pending_before {
            return Ok(());
        }
        let mut db = self.db();
        let tx = db.transaction()?;
        let orphans: Vec<i64> = {
            let mut st = tx.prepare(
                &format!("SELECT id FROM messages WHERE mailbox_id = ?1 AND uid IS NULL AND NOT {MOVE_PENDING_INTO}"),
            )?;
            st.query_map([mailbox_id], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        // Moves that never arrived: no thread left the folder (the inbox) by them.
        quiet_handled(&tx, true)?;
        delete_ids(&tx, &orphans)?;
        quiet_handled(&tx, false)?;
        tx.commit()?;
        Ok(())
    }

    // ── Rules ──────────────────────────────────────────────────────────────

    pub fn rules(&self, account_id: Option<i64>) -> Result<Vec<crate::rules::Rule>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT id, account_id, enabled, data FROM rules WHERE ?1 IS NULL OR account_id = ?1 ORDER BY position, id",
        )?;
        let rows = st.query_map([account_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, bool>(2)?, r.get::<_, String>(3)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, account_id, enabled, data) = row?;
            match serde_json::from_str::<crate::rules::Rule>(&data) {
                Ok(rule) => out.push(crate::rules::Rule { id, account_id, enabled, ..rule }),
                Err(e) => tracing::warn!("rule {id} unreadable: {e}"),
            }
        }
        Ok(out)
    }

    /// Creates the rule (`id` = 0) or replaces it. Returns its id.
    pub fn save_rule(&self, rule: &crate::rules::Rule) -> Result<i64> {
        let db = self.db();
        let data = serde_json::to_string(rule)?;
        if rule.id == 0 {
            db.execute(
                "INSERT INTO rules (account_id, enabled, data, position)
                 VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(position), 0) + 1 FROM rules))",
                params![rule.account_id, rule.enabled, data],
            )?;
            Ok(db.last_insert_rowid())
        } else {
            db.execute(
                "UPDATE rules SET account_id = ?2, enabled = ?3, data = ?4 WHERE id = ?1",
                params![rule.id, rule.account_id, rule.enabled, data],
            )?;
            Ok(rule.id)
        }
    }

    pub fn delete_rule(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM rules WHERE id = ?1", [id])?;
        Ok(())
    }

    /// The accounts in this order (the others follow, in the order they were added).
    pub fn reorder_accounts(&self, ids: &[i64]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute("UPDATE accounts SET position = ?2 WHERE id = ?1", params![id, position as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn reorder_rules(&self, ids: &[i64]) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute("UPDATE rules SET position = ?2 WHERE id = ?1", params![id, position as i64])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn rule_candidates(&self, ids: &[i64]) -> Result<Vec<crate::rules::Candidate>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT id, from_name, from_addr, to_list, cc_list, subject, list_id, has_attachments, category != 'people', seen, date
             FROM messages WHERE id IN ({list})"
        ))?;
        let rows = st
            .query_map([], |r| {
                let to: String = r.get(3)?;
                let cc: String = r.get(4)?;
                let mut recipients: Vec<Address> = serde_json::from_str(&to).unwrap_or_default();
                recipients.extend(serde_json::from_str::<Vec<Address>>(&cc).unwrap_or_default());
                Ok(crate::rules::Candidate {
                    id: r.get(0)?,
                    from: r.get::<_, Option<String>>(2)?.map(|email| Address { name: r.get(1).ok().flatten(), email }),
                    recipients,
                    subject: r.get(5)?,
                    list_id: r.get(6)?,
                    has_attachments: r.get(7)?,
                    bulk: r.get(8)?,
                    seen: r.get(9)?,
                    date: r.get(10)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Files a sender under a category (the user's choice): their cached and future messages.
    pub fn set_sender_category(&self, email: &str, category: Category) -> Result<usize> {
        let db = self.db();
        db.execute(
            "INSERT INTO sender_categories (email, category) VALUES (lower(?1), ?2)
             ON CONFLICT (email) DO UPDATE SET category = excluded.category",
            params![email, category.as_str()],
        )?;
        Ok(db.execute("UPDATE messages SET category = ?2 WHERE lower(from_addr) = lower(?1)", params![email, category.as_str()])?)
    }

    pub fn sender_category(&self, email: &str) -> Result<Option<Category>> {
        let c: Option<String> = self
            .read()
            .query_row("SELECT category FROM sender_categories WHERE email = lower(?1)", [email], |r| r.get(0))
            .optional()?;
        Ok(c.as_deref().and_then(Category::parse))
    }

    /// The sender's chosen category and the categories of their cached messages, to put back
    /// with [`Store::restore_sender_category`] (undoing a decision that sorted them).
    pub fn sender_category_snapshot(&self, email: &str) -> Result<SenderCategorySnapshot> {
        let chosen = self.sender_category(email)?;
        let db = self.read();
        let mut st = db.prepare("SELECT id, category FROM messages WHERE lower(from_addr) = lower(?1)")?;
        let messages = st
            .query_map([email], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter_map(|(id, c)| Category::parse(&c).map(|c| (id, c)))
            .collect();
        Ok(SenderCategorySnapshot { email: email.to_lowercase(), chosen, messages })
    }

    /// Puts a sender's categories back as they were ([`Store::sender_category_snapshot`]): the
    /// choice is removed when there was none, and each message gets its category back.
    pub fn restore_sender_category(&self, snapshot: &SenderCategorySnapshot) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        match snapshot.chosen {
            Some(c) => tx.execute(
                "INSERT INTO sender_categories (email, category) VALUES (lower(?1), ?2)
                 ON CONFLICT (email) DO UPDATE SET category = excluded.category",
                params![snapshot.email, c.as_str()],
            )?,
            None => tx.execute("DELETE FROM sender_categories WHERE email = lower(?1)", [&snapshot.email])?,
        };
        {
            let mut st = tx.prepare_cached("UPDATE messages SET category = ?2 WHERE id = ?1")?;
            for (id, c) in &snapshot.messages {
                st.execute(params![id, c.as_str()])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Unread threads per category in these folders (every category present, zero when none).
    pub fn category_unread(&self, mailbox_ids: &[i64]) -> Result<HashMap<Category, i64>> {
        let mut out: HashMap<Category, i64> = Category::ALL.iter().map(|c| (*c, 0)).collect();
        if mailbox_ids.is_empty() {
            return Ok(out);
        }
        let db = self.read();
        let pool = Boxes::of(&db, mailbox_ids)?.pool("id, thread_id, seen, category");
        let mut st = db.prepare(&format!(
            "SELECT cat, COUNT(*) FROM (
                SELECT MIN({CATEGORY_RANK}) AS cat, SUM(seen = 0) AS unread
                FROM ({pool}) WHERE id NOT IN (SELECT id FROM messages WHERE unscreened = 1) GROUP BY thread_id)
             WHERE unread > 0 GROUP BY cat"
        ))?;
        for row in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (cat, n) = row?;
            out.insert(Category::from_rank(cat), n);
        }
        Ok(out)
    }

    pub fn inbox_messages(&self, account_id: i64) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT m.id FROM messages m WHERE m.account_id = ?1 AND {} AND m.uid IS NOT NULL",
            in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'inbox'")
        ))?;
        let rows = st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    // ── Deleting a folder ──────────────────────────────────────────────────

    /// Figures of the cached messages of a set of folders. `recent`: how many of the latest
    /// messages to list.
    pub fn folder_summary(&self, mailbox: Mailbox, children: Vec<Mailbox>, gmail: bool, recent: usize) -> Result<FolderSummary> {
        let ids: Vec<i64> = std::iter::once(mailbox.id).chain(children.iter().map(|c| c.id)).collect();
        let db = self.read();
        let server_count: Option<i64> = db.query_row(
            &format!("SELECT CASE WHEN COUNT(*) = COUNT(server_count) THEN SUM(server_count) END FROM mailboxes WHERE id IN ({})", id_list(&ids)),
            [],
            |r| r.get(0),
        )?;
        let boxes = Boxes::of(&db, &ids)?;
        let pool = boxes.pool("seen, flagged, has_attachments, size, date, from_addr, from_name");
        let (local, unread, flagged, with_attachments, bytes, oldest, newest) = db.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(seen = 0), 0), COALESCE(SUM(flagged), 0), COALESCE(SUM(has_attachments), 0),
                        COALESCE(SUM(size), 0), MIN(date), MAX(date)
                 FROM ({pool})"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )?;
        let years = {
            let mut st = db.prepare(&format!(
                "SELECT CAST(strftime('%Y', date, 'unixepoch', 'localtime') AS INTEGER), COUNT(*)
                 FROM ({pool}) GROUP BY 1 ORDER BY 1"
            ))?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        let top_senders = {
            let mut st = db.prepare(&format!(
                "SELECT lower(from_addr), MAX(from_name), COUNT(*) AS n FROM ({pool})
                 WHERE from_addr IS NOT NULL
                 GROUP BY lower(from_addr) ORDER BY n DESC LIMIT 6"
            ))?;
            st.query_map([], |r| Ok(CountedAddress { email: r.get(0)?, name: r.get(1)?, count: r.get(2)? }))?
                .collect::<Result<_, _>>()?
        };
        let recent = {
            let (cols, select) = POOL_SUMMARY;
            let mut st = db.prepare(&format!("SELECT {select} FROM ({}) ORDER BY date DESC LIMIT {recent}", boxes.pool(cols)))?;
            st.query_map([], summary_from_row)?.collect::<Result<_, _>>()?
        };
        Ok(FolderSummary {
            mailbox, children, server_count, local, unread, flagged, with_attachments, bytes, oldest, newest, years,
            top_senders, recent, gmail,
        })
    }

    /// Most recent cached messages of these folders sent from `from` (an address, any case).
    pub fn folder_messages_from(&self, mailboxes: &[i64], from: &str, limit: usize) -> Result<Vec<MessageSummary>> {
        let db = self.read();
        let (cols, select) = POOL_SUMMARY;
        let mut st = db.prepare(&format!(
            "SELECT {select} FROM ({}) WHERE lower(from_addr) = lower(?1) ORDER BY date DESC LIMIT ?2",
            Boxes::of(&db, mailboxes)?.pool(cols)
        ))?;
        let rows = st.query_map(params![from, limit as i64], summary_from_row)?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Renames a folder and its subfolders ("Clients/2024" follows "Clients").
    pub fn rename_mailbox_tree(&self, account_id: i64, from: &str, to: &str, delimiter: &str) -> Result<()> {
        self.db().execute(
            "UPDATE mailboxes SET name = ?3 || substr(name, length(?2) + 1)
             WHERE account_id = ?1 AND (name = ?2 OR substr(name, 1, length(?2) + length(?4)) = ?2 || ?4)",
            params![account_id, from, to, delimiter],
        )?;
        Ok(())
    }

    /// Forgets a deleted folder (its cached messages go with it).
    pub fn delete_mailbox(&self, id: i64) -> Result<()> {
        self.shed_mailbox_rows(id)?;
        let mut db = self.db();
        let tx = db.transaction()?;
        // Bodies the folder's messages hold go to their copies elsewhere first.
        delete_mailbox_rows(&tx, id)?;
        tx.execute("DELETE FROM mailboxes WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// Moves every cached message of a folder into another, locally (UID unknown until the
    /// destination folder syncs).
    pub fn move_all_local(&self, from: i64, to: i64) -> Result<()> {
        self.db().execute("UPDATE messages SET mailbox_id = ?2, uid = NULL WHERE mailbox_id = ?1", params![from, to])?;
        Ok(())
    }

    // ── Links ──────────────────────────────────────────────────────────────

    /// What is known about a link domain: other messages (junk and bin aside) linking to it and
    /// their senders, and whether the user wrote to an address of that domain. Mail held by the
    /// screener or refused (any of its copies) does not vouch for a domain either.
    pub fn link_domain_context(&self, domain: &str, exclude: i64) -> Result<(i64, i64, bool)> {
        let db = self.read();
        let (messages, senders) = db.query_row(
            &format!(
                "SELECT COUNT(DISTINCT COALESCE(m.message_id, m.id)), COUNT(DISTINCT lower(m.from_addr))
                 FROM message_links l JOIN messages m ON m.id = l.message_id JOIN mailboxes b ON b.id = m.mailbox_id
                 WHERE l.domain = ?1 AND m.id != ?2 AND {VOUCHES}"
            ),
            params![domain, exclude],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let wrote_to = db.query_row(
            "SELECT EXISTS (SELECT 1 FROM correspondents WHERE email LIKE ?1 OR email LIKE ?2)",
            params![format!("%@{domain}"), format!("%.{domain}")],
            |r| r.get(0),
        )?;
        Ok((messages, senders, wrote_to))
    }

    /// Other messages (junk and bin aside, mail held by the screener or refused, copies of
    /// `exclude` too) that link to `domain` or come from an address of it or of one of its
    /// subdomains.
    pub fn domain_seen(&self, domain: &str, exclude: i64) -> Result<i64> {
        let db = self.read();
        // Candidates through indexes only: the links of the domain, and the senders of it found in
        // the (small) address book, then their messages by the messages_from_lc index. A LIKE on
        // the messages themselves would scan the whole cache.
        let escaped = domain.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let n = db.query_row(
            &format!(
                "SELECT COUNT(DISTINCT COALESCE(m.message_id, m.id)) FROM (
                     SELECT message_id AS id FROM message_links WHERE domain = ?1
                     UNION
                     SELECT s.id FROM contacts c CROSS JOIN messages s ON lower(s.from_addr) = +c.email
                     WHERE c.email LIKE ?3 ESCAPE '\\' OR c.email LIKE ?4 ESCAPE '\\'
                 ) k
                 CROSS JOIN messages m ON m.id = k.id JOIN mailboxes b ON b.id = m.mailbox_id
                 WHERE m.id != ?2 AND {VOUCHES}
                   AND (m.message_id IS NULL OR m.message_id IS NOT (SELECT message_id FROM messages WHERE id = ?2))"
            ),
            params![domain, exclude, format!("%@{escaped}"), format!("%.{escaped}")],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    // ── Outbox ─────────────────────────────────────────────────────────────

    pub fn queue_outgoing(&self, draft: &Draft, send_at: i64, scheduled: bool, remind_after: Option<i64>) -> Result<i64> {
        let db = self.db();
        db.execute(
            "INSERT INTO outbox (account_id, draft, send_at, scheduled, remind_after, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![draft.account_id, serde_json::to_string(draft)?, send_at, scheduled, remind_after, now()],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn outbox(&self) -> Result<Vec<Outgoing>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT o.id, o.account_id, o.draft, o.send_at, o.scheduled, o.state, o.error, o.remind_after, a.smtp_host
             FROM outbox o LEFT JOIN accounts a ON a.id = o.account_id ORDER BY o.send_at",
        )?;
        let rows = st
            .query_map([], |r| {
                let draft: Draft = serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default();
                let preview: String = draft.body.chars().take(600).collect();
                Ok(Outgoing {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    in_reply_to: draft.in_reply_to,
                    preview,
                    subject: draft.subject,
                    to: draft.to.into_iter().chain(draft.cc).chain(draft.bcc).collect(),
                    send_at: r.get(3)?,
                    scheduled: r.get(4)?,
                    state: r.get(5)?,
                    error: r.get::<_, Option<String>>(6)?.map(|e| outgoing_error(e, r.get::<_, Option<String>>(8).ok().flatten().as_deref().unwrap_or(""))),
                    remind_after: r.get(7)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Next planned send (failures aside), to set the scheduler's wake-up.
    pub fn next_outgoing(&self) -> Result<Option<i64>> {
        Ok(self.read().query_row("SELECT MIN(send_at) FROM outbox WHERE state = 'pending'", [], |r| r.get(0))?)
    }

    /// Claims a message that is due: it becomes in-flight and can no longer be cancelled. `None`
    /// if there is none.
    pub fn claim_outgoing(&self, now: i64) -> Result<Option<(i64, Draft, Option<i64>)>> {
        self.claim_outgoing_except(now, &[])
    }

    /// The next due message of an account not in `busy` (its sending lanes taken).
    pub fn claim_outgoing_except(&self, now: i64, busy: &[i64]) -> Result<Option<(i64, Draft, Option<i64>)>> {
        let db = self.db();
        let skip = busy.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let row: Option<(i64, String, Option<i64>)> = db
            .query_row(
                &format!(
                    "UPDATE outbox SET state = 'sending'
                     WHERE id = (SELECT id FROM outbox WHERE state = 'pending' AND send_at <= ?1 AND account_id NOT IN ({skip}) ORDER BY send_at LIMIT 1)
                     RETURNING id, draft, remind_after"
                ),
                [now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((id, draft, remind)) = row else { return Ok(None) };
        match serde_json::from_str(&draft) {
            Ok(draft) => Ok(Some((id, draft, remind))),
            // Unreadable: failed, visible in the outbox, rather than stuck "sending" (neither
            // cancellable nor resendable) until the next launch.
            Err(e) => {
                db.execute("UPDATE outbox SET state = 'failed', error = ?2 WHERE id = ?1", params![id, e.to_string()])?;
                Err(e.into())
            }
        }
    }

    /// A claimed message back to pending, before anything was tried (no sending lane free).
    pub fn unclaim_outgoing(&self, id: i64) -> Result<()> {
        self.db().execute("UPDATE outbox SET state = 'pending' WHERE id = ?1 AND state = 'sending'", [id])?;
        Ok(())
    }

    pub fn delete_outgoing(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM outbox WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Send failure: retry at `retry_at`, or give up (`None`) until the user acts.
    /// `offline`: it never reached the server (counted apart, see [`Store::outgoing_tries`]).
    pub fn outgoing_failed(&self, id: i64, error: &str, retry_at: Option<i64>, offline: bool) -> Result<()> {
        self.db().execute(
            "UPDATE outbox SET attempts = attempts + (?4 = 0), offline = offline + (?4 = 1), tried = MAX(tried, ?4 = 0), error = ?2,
                    state = CASE WHEN ?3 IS NULL THEN 'failed' ELSE 'pending' END,
                    send_at = COALESCE(?3, send_at)
             WHERE id = ?1",
            params![id, error, retry_at, offline],
        )?;
        Ok(())
    }

    /// (attempts the server saw, tries that never reached it).
    pub fn outgoing_tries(&self, id: i64) -> Result<(i64, i64)> {
        Ok(self.read().query_row("SELECT attempts, offline FROM outbox WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?)
    }

    /// The account's sends waiting after a failure are due now (the account signed in again, the
    /// network back); how many.
    pub fn retry_outgoing_now(&self, account_id: Option<i64>, now: i64) -> Result<usize> {
        Ok(self.db().execute(
            "UPDATE outbox SET send_at = ?2
             WHERE state = 'pending' AND (attempts > 0 OR offline > 0) AND send_at > ?2 AND (?1 IS NULL OR account_id = ?1)",
            params![account_id, now],
        )?)
    }

    /// Takes a message out of the outbox to edit it; impossible once it is sending.
    /// The message taken back, and whether it may have reached the server already.
    pub fn take_outgoing(&self, id: i64) -> Result<Option<(Draft, bool)>> {
        let row: Option<(String, bool)> = self
            .db()
            .query_row("DELETE FROM outbox WHERE id = ?1 AND state != 'sending' RETURNING draft, tried", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        row.map(|(d, tried)| Ok((serde_json::from_str(&d)?, tried))).transpose()
    }

    /// Brings the send forward ("Send now", "Retry").
    pub fn reschedule_outgoing(&self, id: i64, send_at: i64) -> Result<bool> {
        Ok(self.db().execute(
            "UPDATE outbox SET send_at = ?2, state = 'pending', attempts = 0, offline = 0, error = NULL
             WHERE id = ?1 AND state != 'sending'",
            params![id, send_at],
        )? > 0)
    }

    /// At startup: a send interrupted by the app quitting may or may not have gone. It is not
    /// resent automatically; the user checks Sent. The error is stored as the marker
    /// `interrupted`, put into words when read (see [`outgoing_error`]), in the interface's
    /// language of the moment.
    pub fn interrupted_outgoing(&self) -> Result<usize> {
        Ok(self.db().execute(
            "UPDATE outbox SET state = 'failed', error = 'interrupted', tried = 1 WHERE state = 'sending'",
            [],
        )?)
    }

    // ── Snoozes ────────────────────────────────────────────────────────────

    pub fn snooze(&self, account_id: i64, thread_id: i64, message_ids: &[String], until: i64) -> Result<()> {
        self.db().execute(
            "INSERT INTO snoozes (account_id, thread_id, message_ids, until) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account_id, thread_id) DO UPDATE
                SET message_ids = excluded.message_ids, until = excluded.until, woke_at = NULL",
            params![account_id, thread_id, serde_json::to_string(message_ids)?, until],
        )?;
        Ok(())
    }

    /// Threads still snoozed: (account, thread, Message-IDs, due).
    pub fn snoozed(&self) -> Result<Vec<(i64, i64, Vec<String>, i64)>> {
        let db = self.read();
        let mut st = db.prepare("SELECT account_id, thread_id, message_ids, until FROM snoozes WHERE woke_at IS NULL")?;
        let rows = st
            .query_map([], |r| {
                let ids: String = r.get(2)?;
                Ok((r.get(0)?, r.get(1)?, serde_json::from_str(&ids).unwrap_or_default(), r.get(3)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn next_snooze(&self) -> Result<Option<i64>> {
        Ok(self.read().query_row("SELECT MIN(until) FROM snoozes WHERE woke_at IS NULL", [], |r| r.get(0))?)
    }

    /// Messages of a snoozed thread still in `mailbox_id`: the thread's, and those whose
    /// Message-ID was noted (in case the thread was renumbered).
    pub fn snoozed_messages(&self, mailbox_id: i64, thread_id: i64, message_ids: &[String]) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT m.id FROM messages m WHERE {}
               AND (m.thread_id = ?1 OR m.message_id IN (SELECT value FROM json_each(?2)))
             ORDER BY m.date",
            Boxes::of(&db, &[mailbox_id])?.holds("m")
        ))?;
        let rows = st
            .query_map(params![thread_id, serde_json::to_string(message_ids)?], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The thread came back: it now sorts at this date. Old returns are forgotten.
    pub fn woke(&self, account_id: i64, thread_id: i64, at: i64) -> Result<()> {
        let db = self.db();
        db.execute(
            "UPDATE snoozes SET woke_at = ?3 WHERE account_id = ?1 AND thread_id = ?2",
            params![account_id, thread_id, at],
        )?;
        db.execute("DELETE FROM snoozes WHERE woke_at < ?1", [at - 30 * 86_400])?;
        Ok(())
    }

    pub fn drop_snooze(&self, account_id: i64, thread_id: i64) -> Result<()> {
        self.db().execute("DELETE FROM snoozes WHERE account_id = ?1 AND thread_id = ?2", params![account_id, thread_id])?;
        Ok(())
    }

    /// Snoozed threads with a message back in the inbox (new reply, manual move): they must come
    /// back whole.
    pub fn snoozes_back_in_inbox(&self, account_id: i64) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT s.thread_id FROM snoozes s
             WHERE s.account_id = ?1 AND s.woke_at IS NULL AND EXISTS (
                SELECT 1 FROM messages m
                WHERE m.account_id = s.account_id AND m.thread_id = s.thread_id AND {})",
            in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'inbox'")
        ))?;
        let rows = st.query_map([account_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    // ── Follow-ups ─────────────────────────────────────────────────────────

    pub fn add_reminder(&self, account_id: i64, message_id: &str, subject: &str, recipients: &[String], sent_at: i64, due: i64) -> Result<()> {
        self.db().execute(
            "INSERT INTO reminders (account_id, message_id, subject, recipients, sent_at, due) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![account_id, message_id, subject, serde_json::to_string(recipients)?, sent_at, due],
        )?;
        Ok(())
    }

    pub fn next_reminder(&self) -> Result<Option<i64>> {
        Ok(self.read().query_row("SELECT MIN(due) FROM reminders WHERE state = 'waiting'", [], |r| r.get(0))?)
    }

    /// Someone else wrote in the sent message's thread since it was sent.
    fn replied(db: &Connection, account_id: i64, message_id: &str, sent_at: i64) -> rusqlite::Result<bool> {
        // Three indexed lookups: the sent message's thread, In-Reply-To, References.
        // Not the sent message itself (its Sent copy arrives after `sent_at`), nor anything
        // written from the account or one of its aliases.
        // Nor an automatic message (an out-of-office reply, a bounce): sent back at once, it answers
        // nothing.
        let others = "(m.received_at > ?3 OR (m.received_at IS NULL AND m.date > ?3))
                      AND m.bulk = 0
                      AND COALESCE(m.message_id, '') != ?2
                      AND lower(COALESCE(m.from_addr, '')) != (SELECT lower(email) FROM accounts WHERE id = ?1)
                      AND lower(COALESCE(m.from_addr, '')) NOT IN (
                          SELECT lower(json_extract(i.value, '$.email')) FROM accounts a, json_each(a.identities) i
                          WHERE a.id = ?1 AND json_extract(i.value, '$.email') IS NOT NULL)";
        db.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM messages m WHERE m.account_id = ?1 AND {others}
                     AND m.thread_id = (SELECT thread_id FROM messages WHERE account_id = ?1 AND message_id = ?2 LIMIT 1))
                 OR EXISTS (SELECT 1 FROM messages m WHERE m.account_id = ?1 AND m.in_reply_to = ?2 AND {others})
                 OR EXISTS (SELECT 1 FROM message_refs r JOIN messages m ON m.id = r.message_id
                            WHERE r.account_id = ?1 AND r.ref = ?2 AND {others})"
            ),
            params![account_id, message_id, sent_at],
            |r| r.get(0),
        )
    }

    /// Reviews follow-ups: those that got a reply go, those due without a reply become "to follow
    /// up". Returns the newly due ones.
    pub fn check_reminders(&self, now: i64) -> Result<Vec<FollowUp>> {
        // Sort first, reading (after every sync: without blocking writes); write only if something
        // changes.
        let (mut answered, mut newly_due) = (Vec::new(), Vec::new());
        {
            let db = self.read();
            let mut st = db.prepare("SELECT id, account_id, message_id, sent_at, due, state FROM reminders")?;
            let pending: Vec<(i64, i64, String, i64, i64, String)> = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<Result<_, _>>()?;
            for (id, account_id, message_id, sent_at, due, state) in pending {
                if Self::replied(&db, account_id, &message_id, sent_at)? {
                    answered.push(id);
                } else if state == "waiting" && due <= now {
                    newly_due.push(id);
                }
            }
        }
        if !answered.is_empty() || !newly_due.is_empty() {
            let mut db = self.db();
            let tx = db.transaction()?;
            for id in &answered {
                tx.execute("DELETE FROM reminders WHERE id = ?1", [id])?;
            }
            for id in &newly_due {
                tx.execute("UPDATE reminders SET state = 'due' WHERE id = ?1 AND state = 'waiting'", [id])?;
            }
            tx.commit()?;
        }
        Ok(self.follow_ups()?.into_iter().filter(|f| newly_due.contains(&f.id)).collect())
    }

    /// Sent messages to follow up, oldest first.
    pub fn follow_ups(&self) -> Result<Vec<FollowUp>> {
        let db = self.read();
        // The sent message, if cached: preferably in Sent.
        let mut st = db.prepare(
            "SELECT r.id, r.account_id, r.subject, r.recipients, r.sent_at, r.due,
                    (SELECT m.thread_id FROM messages m WHERE m.account_id = r.account_id AND m.message_id = r.message_id LIMIT 1),
                    COALESCE(
                        (SELECT l.mailbox_id FROM messages m JOIN labels l ON l.message_id = m.id JOIN mailboxes b ON b.id = l.mailbox_id
                         WHERE m.account_id = r.account_id AND m.message_id = r.message_id AND b.role = 'sent' LIMIT 1),
                        (SELECT m.mailbox_id FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id
                         WHERE m.account_id = r.account_id AND m.message_id = r.message_id
                         ORDER BY b.role = 'sent' DESC LIMIT 1))
             FROM reminders r WHERE r.state = 'due' ORDER BY r.due",
        )?;
        let rows = st
            .query_map([], |r| {
                let to: String = r.get(3)?;
                Ok(FollowUp {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    subject: r.get(2)?,
                    to: serde_json::from_str(&to).unwrap_or_default(),
                    sent_at: r.get(4)?,
                    due: r.get(5)?,
                    thread_id: r.get(6)?,
                    mailbox_id: r.get(7)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn dismiss_reminder(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM reminders WHERE id = ?1", [id])?;
        Ok(())
    }

    // ── Operation queue ────────────────────────────────────────────────────

    /// `label`: readable description, shown if the operation fails. `not_before`: the operation
    /// waits until then (undo delay). Returns its id.
    pub fn push_op(&self, account_id: i64, op: &str, label: &str, not_before: Option<i64>) -> Result<i64> {
        /// What the sync reads without decoding the operation (the rest is ignored).
        #[derive(serde::Deserialize, Default)]
        struct Head {
            kind: Option<String>,
            mailbox: Option<String>,
            dest: Option<String>,
            from: Option<String>,
            to: Option<String>,
        }
        let head: Head = serde_json::from_str(op).unwrap_or_default();
        let db = self.db();
        db.execute(
            "INSERT INTO pending_ops (account_id, op, created_at, label, not_before, kind, mailbox, dest, op_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, length(?2))",
            params![account_id, op, now(), label, not_before, head.kind, head.mailbox.or(head.from), head.dest.or(head.to)],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Queued moves into `dest` become permanent deletes at the source, replayed at once (the
    /// destination folder was just emptied).
    pub fn moves_into_become_deletes(&self, account_id: i64, dest: &str) -> Result<usize> {
        use crate::ops::Op;
        let mut db = self.db();
        let tx = db.transaction()?;
        let rows: Vec<(i64, String)> = {
            let mut st = tx.prepare(
                "SELECT id, op FROM pending_ops WHERE account_id = ?1 AND dest = ?2 AND failed = 0 AND kind IN ('move', 'move_set')",
            )?;
            st.query_map(params![account_id, dest], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?
        };
        let mut n = 0;
        for (id, op) in rows {
            let delete = match serde_json::from_str::<Op>(&op) {
                Ok(Op::Move { mailbox, uid, uidvalidity, .. }) => Op::Delete { mailbox, uid, uidvalidity },
                Ok(Op::MoveSet { mailbox, uids, uidvalidity, .. }) => Op::DeleteSet { mailbox, uids, uidvalidity },
                _ => continue,
            };
            let kind = if matches!(delete, Op::Delete { .. }) { "delete" } else { "delete_set" };
            tx.execute(
                "UPDATE pending_ops SET op = ?2, op_size = length(?2), kind = ?3, dest = NULL, not_before = NULL WHERE id = ?1",
                params![id, serde_json::to_string(&delete)?, kind],
            )?;
            n += 1;
        }
        tx.commit()?;
        Ok(n)
    }

    /// An operation's payload, read only when replaying it.
    pub fn op_payload(&self, id: i64) -> Result<Option<String>> {
        Ok(self.read().query_row("SELECT op FROM pending_ops WHERE id = ?1", [id], |r| r.get(0)).optional()?)
    }

    /// The operation to replay now, read again: ids are reused once the last row goes, so the row
    /// listed may since be another operation, perhaps one still held for undo (`None` then).
    pub fn replay_payload(&self, id: i64, now: i64) -> Result<Option<String>> {
        Ok(self
            .read()
            .query_row("SELECT op FROM pending_ops WHERE id = ?1 AND failed = 0 AND (not_before IS NULL OR not_before <= ?2)", params![id, now], |r| r.get(0))
            .optional()?)
    }

    /// A replayed operation done (or dropped): removed only if the row still holds it, not a newer
    /// operation that took its id meanwhile.
    pub fn finish_replayed(&self, id: i64, op: &str) -> Result<()> {
        self.db().execute("DELETE FROM pending_ops WHERE id = ?1 AND op = ?2", params![id, op])?;
        Ok(())
    }

    /// [`fail_op`](Self::fail_op) for a replayed operation, counted only if the row still holds it.
    pub fn fail_replayed(&self, id: i64, op: &str, error: &str, max: i64) -> Result<bool> {
        self.count_failure("attempts", id, Some(op), error, max)
    }

    /// [`refuse_op`](Self::refuse_op) for a replayed operation, counted only if the row still holds it.
    pub fn refuse_replayed(&self, id: i64, op: &str, error: &str, max: i64) -> Result<bool> {
        self.count_failure("refusals", id, Some(op), error, max)
    }

    /// Queued operations on the folders themselves: (kind, folder, destination).
    pub fn pending_folder_ops(&self, account_id: i64) -> Result<Vec<(String, String, Option<String>)>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT kind, mailbox, dest FROM pending_ops
             WHERE account_id = ?1 AND failed = 0 AND kind IN ('create', 'rename', 'delete_mailbox') AND mailbox IS NOT NULL",
        )?;
        let rows = st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// UIDs of a folder with a flag change still waiting to leave: the sync must not overwrite
    /// them with the server's (still old) state.
    pub fn pending_flag_uids(&self, account_id: i64, mailbox: &str) -> Result<HashSet<u32>> {
        self.op_uids(account_id, mailbox, "('store', 'store_set')")
    }

    /// UIDs of the folder that moves or deletes made in facteur and refused for good by the
    /// server (failed, waiting for the user's decision) still cover. Their messages come back
    /// to the folder, but not as new mail, nor to be moved again automatically.
    pub fn failed_op_uids(&self, account_id: i64, mailbox: &str) -> Result<HashSet<u32>> {
        self.op_uids_of(account_id, mailbox, "('move', 'move_set', 'delete', 'delete_set')", true)
    }

    /// Among these messages, those that a failed move or delete still covers (see
    /// `failed_op_uids`): automatic moves (screener, blocklist) leave them alone.
    pub fn failed_op_ids(&self, ids: &[i64]) -> Result<HashSet<i64>> {
        let mut by_folder: HashMap<(i64, String), HashSet<u32>> = HashMap::new();
        let mut out = HashSet::new();
        for (id, loc) in self.locations(ids)? {
            let Some(uid) = loc.uid else { continue };
            let key = (loc.account_id, loc.mailbox);
            if !by_folder.contains_key(&key) {
                let uids = self.failed_op_uids(key.0, &key.1)?;
                by_folder.insert(key.clone(), uids);
            }
            if by_folder[&key].contains(&uid) {
                out.insert(id);
            }
        }
        Ok(out)
    }

    fn op_uids(&self, account_id: i64, mailbox: &str, kinds: &str) -> Result<HashSet<u32>> {
        self.op_uids_of(account_id, mailbox, kinds, false)
    }

    /// UIDs of the folder named by the queued operations of these kinds, failed ones or the others.
    fn op_uids_of(&self, account_id: i64, mailbox: &str, kinds: &str, failed: bool) -> Result<HashSet<u32>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT op FROM pending_ops WHERE account_id = ?1 AND mailbox = ?2 AND failed = ?3 AND kind IN {kinds}"
        ))?;
        let mut out = HashSet::new();
        for op in st.query_map(params![account_id, mailbox, failed], |r| r.get::<_, String>(0))? {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&op?) else { continue };
            out.extend(v["uid"].as_u64().map(|u| u as u32));
            out.extend(v["uids"].as_array().into_iter().flatten().filter_map(|u| u.as_u64()).map(|u| u as u32));
            // The old copy a draft save replaces (one without a Message-ID).
            out.extend(v["source"][0].as_u64().map(|u| u as u32));
        }
        Ok(out)
    }

    /// Undoes an action: removes its operations from the queue, puts the messages back where they
    /// were, forgets the snoozes. All or nothing, in one transaction; [`Undone::TooLate`] if an
    /// operation already left the queue.
    /// `unlocked`: undone without the replay lock, so only while every operation is still held
    /// (no replay can take it), as read inside the transaction; [`Undone::Released`] otherwise
    /// (a settle released one meanwhile), nothing done.
    /// `labels`: the labels the messages had before (Gmail converted to labels).
    #[allow(clippy::too_many_arguments)]
    pub fn undo(
        &self,
        ops: &[(i64, String)],
        moves: &[(i64, i64, Option<u32>)],
        labels: &[(i64, Vec<i64>)],
        snoozes: &[(i64, i64)],
        unlocked: bool,
    ) -> Result<Undone> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let list = ops.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>().join(",");
        // The same content too: SQLite gives a replayed operation's id to the next one queued, which
        // an undo must not remove in its place. Not once a replay of it was tried at all: cut,
        // begun, or refused after a first part went through (a move in chunks), the server may
        // have done some of it.
        for (id, op) in ops {
            let same: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM pending_ops WHERE id = ?1 AND op = ?2 AND attempts = 0 AND refusals = 0 AND last_error IS NULL)",
                params![id, op],
                |r| r.get(0),
            )?;
            if !same {
                return Ok(Undone::TooLate);
            }
        }
        if unlocked {
            // A margin, as the engine's own check: a replay starting now still sees them held.
            let held: i64 = tx.query_row(&format!("SELECT COUNT(*) FROM pending_ops WHERE id IN ({list}) AND not_before > ?1"), [now() + 1], |r| r.get(0))?;
            if held as usize != ops.len() {
                return Ok(Undone::Released);
            }
        }
        tx.execute(&format!("DELETE FROM pending_ops WHERE id IN ({list})"), [])?;
        // A move undone is no thread leaving the inbox: back to the archive, it keeps its date.
        quiet_handled(&tx, true)?;
        for (id, mailbox_id, uid) in moves.iter().rev() {
            // A copy may have been added at that location meanwhile: it makes room.
            if let Some(uid) = uid {
                let other: Option<i64> = tx
                    .query_row("SELECT id FROM messages WHERE mailbox_id = ?1 AND uid = ?2 AND id != ?3", params![mailbox_id, uid, id], |r| r.get(0))
                    .optional()?;
                delete_ids(&tx, &other.into_iter().collect::<Vec<_>>())?;
            }
            // The moved row is gone (dropped meanwhile): nothing would come back, so the undo
            // fails as a whole (the transaction is rolled back, the operations stay queued).
            if tx.execute("UPDATE messages SET mailbox_id = ?2, uid = ?3 WHERE id = ?1", params![id, mailbox_id, uid])? == 0 {
                return Ok(Undone::TooLate);
            }
            // Read or pinned during the undo delay: the server learns it now.
            queue_pending_flags(&tx, *id, None)?;
        }
        replace_labels(&tx, labels)?;
        quiet_handled(&tx, false)?;
        for (account_id, thread_id) in snoozes {
            tx.execute("DELETE FROM snoozes WHERE account_id = ?1 AND thread_id = ?2", params![account_id, thread_id])?;
        }
        tx.commit()?;
        Ok(Undone::Done)
    }

    /// Operations to replay (failed ones wait for the user's decision).
    pub fn pending_ops(&self, account_id: i64) -> Result<Vec<PendingOp>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT id, COALESCE(kind, ''), attempts, not_before, attempts > 0 OR last_error IS NOT NULL
             FROM pending_ops WHERE account_id = ?1 AND failed = 0 ORDER BY id",
        )?;
        let rows = st
            .query_map([account_id], |r| {
                Ok(PendingOp { id: r.get(0)?, kind: r.get(1)?, attempts: r.get(2)?, not_before: r.get(3)?, tried: r.get(4)? })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The operation is about to be sent: from now on it counts as tried, whatever happens
    /// (a cut, a crash, the user's retry, which resets its attempts). The mark is an empty
    /// `last_error`, which the queue details show as no error.
    pub fn mark_op_started(&self, id: i64) -> Result<()> {
        self.db().execute("UPDATE pending_ops SET last_error = COALESCE(last_error, '') WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn finish_op(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM pending_ops WHERE id = ?1", [id])?;
        Ok(())
    }

    /// A server refusal: counted; beyond `max` the operation becomes failed.
    /// `true` when it has just become failed: its folders should then be read again in full
    /// (`op_folders`, `sync::recheck_op_folders`).
    pub fn fail_op(&self, id: i64, error: &str, max: i64) -> Result<bool> {
        self.count_failure("attempts", id, None, error, max)
    }

    /// One more `counter` (attempts or refusals) for the operation `id` (holding `op`, when
    /// given); whether it has just become failed.
    fn count_failure(&self, counter: &str, id: i64, op: Option<&str>, error: &str, max: i64) -> Result<bool> {
        let failed: Option<bool> = self
            .db()
            .query_row(
                &format!(
                    "UPDATE pending_ops SET {counter} = {counter} + 1, last_error = ?2, failed = ({counter} + 1 >= ?3)
                     WHERE id = ?1 AND (?4 IS NULL OR op = ?4) RETURNING failed"
                ),
                params![id, error, max, op],
                |r| r.get(0),
            )
            .optional()?;
        Ok(failed.unwrap_or(false))
    }

    /// [`fail_op`](Self::fail_op) for a refusal by the server: counted apart from the network
    /// cuts, so that a few cuts before do not make one refusal final.
    pub fn refuse_op(&self, id: i64, error: &str, max: i64) -> Result<bool> {
        self.count_failure("refusals", id, None, error, max)
    }

    /// (account, pending, failed) for each account.
    pub fn ops_counts(&self) -> Result<Vec<(i64, i64, i64)>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT account_id, SUM(failed = 0), SUM(failed = 1) FROM pending_ops GROUP BY account_id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Per account: (pending, failed, creation of the oldest pending operation).
    pub fn ops_health(&self) -> Result<HashMap<i64, (i64, i64, Option<i64>)>> {
        let db = self.read();
        let mut st = db.prepare_cached(
            "SELECT account_id, SUM(failed = 0), SUM(failed = 1), MIN(CASE WHEN failed = 0 THEN created_at END)
             FROM pending_ops GROUP BY account_id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Every queued operation, pending or failed, in replay order per account.
    pub fn op_rows(&self) -> Result<Vec<OpRow>> {
        self.op_rows_where("1", [])
    }

    /// One queued operation, without its payload.
    pub fn op_row(&self, id: i64) -> Result<Option<OpRow>> {
        Ok(self.op_rows_where("id = ?1", [id])?.pop())
    }

    /// Payloads of the queued moves (waiting or failed; waiting only with `waiting_only`) out of
    /// `mailbox`, but `except`.
    pub fn move_payloads(&self, account_id: i64, mailbox: &str, except: i64, waiting_only: bool, earlier_only: bool) -> Result<Vec<String>> {
        let db = self.read();
        let mut st = db.prepare_cached(
            "SELECT op FROM pending_ops WHERE account_id = ?1 AND mailbox = ?2 AND id != ?3 AND kind IN ('move', 'move_set')
               AND (?4 = 0 OR failed = 0) AND (?5 = 0 OR id < ?3)",
        )?;
        let rows = st.query_map(params![account_id, mailbox, except, waiting_only, earlier_only], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Queued operations matching `filter` (SQL), without reading their payloads: the size is
    /// stored when they are queued (`length(op)` for those queued before it was).
    fn op_rows_where(&self, filter: &str, params: impl rusqlite::Params) -> Result<Vec<OpRow>> {
        let db = self.read();
        let mut st = db.prepare_cached(&format!(
            "SELECT id, account_id, COALESCE(kind, ''), COALESCE(label, ''), mailbox, dest, created_at, attempts,
                    NULLIF(last_error, ''), not_before, failed, COALESCE(op_size, length(op))
             FROM pending_ops WHERE {filter} ORDER BY account_id, id"
        ))?;
        let rows = st
            .query_map(params, |r| {
                Ok(OpRow {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    kind: r.get(2)?,
                    label: crate::i18n::pick(&r.get::<_, String>(3)?).to_string(),
                    mailbox: r.get(4)?,
                    dest: r.get(5)?,
                    created_at: r.get(6)?,
                    attempts: r.get(7)?,
                    error: r.get::<_, Option<String>>(8)?.map(|e| crate::i18n::pick(&e).to_string()),
                    not_before: r.get(9)?,
                    failed: r.get(10)?,
                    size: r.get(11)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn failed_ops(&self) -> Result<Vec<FailedOp>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT id, account_id, COALESCE(label, ''), COALESCE(last_error, ''), created_at
             FROM pending_ops WHERE failed = 1 ORDER BY id",
        )?;
        let rows = st
            .query_map([], |r| {
                let shown = |i: usize| r.get::<_, String>(i).map(|t| crate::i18n::pick(&t).to_string());
                Ok(FailedOp { id: r.get(0)?, account_id: r.get(1)?, label: shown(2)?, error: shown(3)?, created_at: r.get(4)? })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Drops this draft's saves not replayed yet: only the latest counts.
    pub fn drop_pending_draft(&self, account_id: i64, message_id: &str) -> Result<()> {
        self.db().execute(
            "DELETE FROM pending_ops WHERE account_id = ?1
               AND op LIKE '{\"kind\":\"save_draft\"%'
               AND instr(op, '\"message_id\":\"' || ?2 || '\"') > 0",
            params![account_id, message_id],
        )?;
        Ok(())
    }

    /// Queued deletions of a draft (failed ones too): a save of it queued afterwards replaces them.
    pub fn drop_pending_draft_deletes(&self, account_id: i64, message_id: &str) -> Result<()> {
        self.db().execute(
            "DELETE FROM pending_ops WHERE account_id = ?1 AND kind = 'delete_draft'
               AND instr(op, '\"message_id\":\"' || ?2 || '\"') > 0",
            params![account_id, message_id],
        )?;
        Ok(())
    }

    /// Payloads of the queued saves of a draft (failed ones too), oldest first.
    pub fn pending_draft_saves(&self, account_id: i64, message_id: &str) -> Result<Vec<(String, bool)>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT op, failed FROM pending_ops WHERE account_id = ?1 AND kind = 'save_draft'
               AND instr(op, '\"message_id\":\"' || ?2 || '\"') > 0 ORDER BY id",
        )?;
        let rows = st.query_map(params![account_id, message_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The draft is saved, or on its way: a save still queued (not failed), or a copy in the
    /// account's Drafts folder.
    pub fn draft_present(&self, account_id: i64, message_id: &str) -> Result<bool> {
        Ok(self.read().query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM pending_ops WHERE account_id = ?1 AND failed = 0 AND kind = 'save_draft'
                                  AND instr(op, '\"message_id\":\"' || ?2 || '\"') > 0)
                     OR EXISTS (SELECT 1 FROM messages m WHERE m.account_id = ?1 AND m.message_id = ?2 AND {})",
                in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'drafts'")
            ),
            params![account_id, message_id],
            |r| r.get(0),
        )?)
    }

    /// The cached Drafts copies of a draft, by Message-ID.
    pub fn draft_copies(&self, account_id: i64, message_id: &str) -> Result<Vec<i64>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "SELECT m.id FROM messages m WHERE m.account_id = ?1 AND m.message_id = ?2 AND {}",
            in_folder_where("m", "rb.account_id = ?1 AND rb.role = 'drafts'")
        ))?;
        Ok(st.query_map(params![account_id, message_id], |r| r.get(0))?.collect::<Result<_, _>>()?)
    }

    /// Removes a message's local copies in a folder from the cache (draft sent or discarded).
    pub fn delete_by_message_id(&self, mailbox_id: i64, message_id: &str) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let ids: Vec<i64> = {
            let mut st = tx.prepare(
                "SELECT id FROM messages WHERE account_id = (SELECT account_id FROM mailboxes WHERE id = ?1)
                   AND message_id = ?2
                   AND (mailbox_id = ?1 OR id IN (SELECT message_id FROM labels WHERE mailbox_id = ?1))",
            )?;
            st.query_map(params![mailbox_id, message_id], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        delete_ids(&tx, &ids)?;
        tx.commit()?;
        Ok(())
    }

    pub fn find_by_message_id(&self, account_id: i64, message_id: &str) -> Result<Option<i64>> {
        Ok(self
            .read()
            .query_row(
                "SELECT id FROM messages WHERE account_id = ?1 AND message_id = ?2 ORDER BY id LIMIT 1",
                params![account_id, message_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn retry_op(&self, id: i64) -> Result<i64> {
        let db = self.db();
        db.execute("UPDATE pending_ops SET failed = 0, attempts = 0, refusals = 0 WHERE id = ?1", [id])?;
        Ok(db.query_row("SELECT account_id FROM pending_ops WHERE id = ?1", [id], |r| r.get(0))?)
    }
}

/// The last `n` months ("YYYY-MM", local time), oldest first.
fn last_months(now: i64, n: u32) -> Vec<String> {
    use chrono::{Datelike, Local, TimeZone};
    let today = Local.timestamp_opt(now, 0).single().unwrap_or_else(Local::now);
    let (mut y, mut m) = (today.year(), today.month() as i32);
    let mut out = Vec::new();
    for _ in 0..n {
        out.push(format!("{y:04}-{m:02}"));
        m -= 1;
        if m == 0 {
            m = 12;
            y -= 1;
        }
    }
    out.reverse();
    out
}

/// A message's category rank in SQL, as `Category::rank`: a person wins.
const CATEGORY_RANK: &str = "CASE category WHEN 'people' THEN 0 WHEN 'notification' THEN 1 ELSE 2 END";

/// References used for threading: the latest are enough, and a huge header (a booby-trapped
/// message) would exceed SQLite's parameter limit and block the sync.
const MAX_REFS: usize = 50;

fn thread_refs(h: &HeaderInfo) -> Vec<&str> {
    let mut seen = HashSet::new();
    let mut refs: Vec<&str> = h.references.iter().rev().map(String::as_str).filter(|r| !r.is_empty() && seen.insert(*r)).take(MAX_REFS).collect();
    refs.reverse();
    refs
}

/// Flag changes made during a move (message without a UID): once the UID is known they go to
/// the server. `server`: the flags it reports (nothing to send if already right); `None`:
/// unknown, send.
fn queue_pending_flags(tx: &Transaction, id: i64, server: Option<ServerFlags>) -> Result<()> {
    let row: Option<(String, i64, i64, Option<u32>, String, Option<u32>)> = tx
        .query_row(
            "SELECT m.pending_flags, m.account_id, m.mailbox_id, m.uid, b.name, b.uidvalidity
             FROM messages m JOIN mailboxes b ON b.id = m.mailbox_id WHERE m.id = ?1 AND m.pending_flags IS NOT NULL",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    let Some((pending, account_id, _, Some(uid), mailbox, uidvalidity)) = row else { return Ok(()) };
    let pending: HashMap<String, bool> = serde_json::from_str(&pending).unwrap_or_default();
    for (flag, value) in pending {
        let (kind, current) = match flag.as_str() {
            "seen" => (FlagKind::Seen, server.map(|f| f.seen)),
            "flagged" => (FlagKind::Flagged, server.map(|f| f.flagged)),
            _ => continue,
        };
        tx.execute(&format!("UPDATE messages SET {} = ?2 WHERE id = ?1", kind.column()), params![id, value])?;
        if current == Some(value) {
            continue;
        }
        let op = crate::ops::Op::Store { mailbox: mailbox.clone(), uid, flag: kind.imap().into(), value, uidvalidity };
        tx.execute(
            "INSERT INTO pending_ops (account_id, op, created_at, label, kind, mailbox, op_size) VALUES (?1, ?2, ?3, ?4, 'store', ?5, length(?2))",
            params![account_id, serde_json::to_string(&op)?, now(), crate::i18n::both(|| crate::tr!("Drapeau changé pendant un déplacement", "Flag changed during a move")), mailbox],
        )?;
    }
    tx.execute("UPDATE messages SET pending_flags = NULL WHERE id = ?1", [id])?;
    Ok(())
}

/// A thread number never given before.
fn new_thread_id(tx: &Transaction) -> Result<i64> {
    tx.execute("INSERT INTO thread_seq DEFAULT VALUES", [])?;
    let t = tx.last_insert_rowid();
    tx.execute("DELETE FROM thread_seq WHERE id < ?1", [t])?;
    Ok(t)
}

/// Threads a message: first a known parent (References, In-Reply-To, or the same Message-ID in
/// another folder), then it absorbs the threads of replies that arrived before their parent.
fn assign_thread(tx: &Transaction, account_id: i64, id: i64, h: &HeaderInfo) -> Result<()> {
    let mut keys: Vec<&str> = thread_refs(h);
    keys.extend(h.in_reply_to.as_deref());
    keys.extend(h.message_id.as_deref());

    // New thread: a number never given before. The message's id won't do: SQLite reuses ids
    // after a delete, so a new message would join another's thread, maybe another account's.
    let mut thread_id = None;
    if !keys.is_empty() {
        let placeholders = (3..3 + keys.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
        // A message it names, or one naming the same ancestors: two replies to an original never
        // received (older than the window, or before one was added) are one conversation. A
        // thread Gmail decided is only joined through the messages themselves.
        let not_gmail = "NOT EXISTS (SELECT 1 FROM gmail_threads g WHERE g.account_id = ?1 AND g.thread_id = m.thread_id)";
        let sql = format!(
            "SELECT thread_id FROM (
                 SELECT m.thread_id, m.date FROM messages m WHERE m.account_id = ?1 AND m.id != ?2 AND m.message_id IN ({placeholders})
                 UNION ALL
                 SELECT m.thread_id, m.date FROM message_refs r JOIN messages m ON m.id = r.message_id
                 WHERE r.account_id = ?1 AND r.ref IN ({placeholders}) AND m.id != ?2 AND {not_gmail}
                 UNION ALL
                 SELECT m.thread_id, m.date FROM messages m WHERE m.account_id = ?1 AND m.id != ?2 AND m.in_reply_to IN ({placeholders}) AND {not_gmail}
             ) ORDER BY date LIMIT 1"
        );
        let mut args: Vec<&dyn rusqlite::ToSql> = vec![&account_id, &id];
        args.extend(keys.iter().map(|k| k as &dyn rusqlite::ToSql));
        thread_id = tx.query_row(&sql, args.as_slice(), |r| r.get(0)).optional()?;
    }
    let thread_id: i64 = match thread_id {
        Some(t) => t,
        None => new_thread_id(tx)?,
    };
    tx.execute("UPDATE messages SET thread_id = ?2 WHERE id = ?1", params![id, thread_id])?;

    if let Some(mid) = &h.message_id {
        // Replies that arrived before this message: by In-Reply-To or References (indexed).
        let orphans: Vec<i64> = {
            let mut st = tx.prepare_cached(
                "SELECT thread_id FROM messages WHERE account_id = ?1 AND in_reply_to = ?3 AND thread_id != ?2
                 UNION
                 SELECT m.thread_id FROM message_refs r JOIN messages m ON m.id = r.message_id
                 WHERE r.account_id = ?1 AND r.ref = ?3 AND m.thread_id != ?2",
            )?;
            st.query_map(params![account_id, thread_id, mid], |r| r.get(0))?.collect::<Result<_, _>>()?
        };
        // A thread Gmail decided (X-GM-THRID) stays as Gmail has it.
        let mut gmail = tx.prepare_cached("SELECT EXISTS (SELECT 1 FROM gmail_threads WHERE account_id = ?1 AND thread_id = ?2)")?;
        for t in orphans {
            if gmail.query_row(params![account_id, t], |r| r.get::<_, bool>(0))? {
                continue;
            }
            tx.execute(
                "UPDATE messages SET thread_id = ?3 WHERE account_id = ?1 AND thread_id = ?2",
                params![account_id, t, thread_id],
            )?;
        }
    }
    Ok(())
}

/// A sender's categories before a change, to put back.
#[derive(Debug, Clone)]
pub struct SenderCategorySnapshot {
    pub email: String,
    pub chosen: Option<Category>,
    pub messages: Vec<(i64, Category)>,
}

/// What the screener decides about one new inbox message ([`Store::screen_verdicts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// First messages of an unknown sender: to "New senders".
    Hold,
    /// A refused sender: to "Screened out".
    Refuse,
    /// Moved back to the inbox from the screener's folders by another device or client: the
    /// sender (this lowercased address) is accepted.
    Accept(String),
}

/// An address compared as the same mailbox: lowercased, without its `+tag`
/// (`Yann+news@example.org` is `yann@example.org`).
pub fn base_address(email: &str) -> String {
    let email = email.trim().to_lowercase();
    match email.rsplit_once('@') {
        Some((local, host)) => match local.split_once('+') {
            Some((base, _)) if !base.is_empty() => format!("{base}@{host}"),
            _ => email,
        },
        None => email,
    }
}

/// The sender's address may not be theirs: authentication failed (DMARC, or SPF and DKIM
/// both), or the shown name carries another address ("service@bank.fr <x@elsewhere>").
fn suspect_sender(signals: Option<&str>) -> bool {
    let Some(s) = signals.and_then(|s| serde_json::from_str::<crate::trust::Signals>(s).ok()) else { return false };
    s.dmarc == Some(false) || (s.dmarc.is_none() && s.dkim == Some(false) && s.spf == Some(false)) || s.spoofed_name
}

/// The provider authenticated the sender's domain (stored signals of a message): DMARC passed,
/// or, without a DMARC verdict, both DKIM and SPF did; and the verdict comes from the account's
/// usual server, which must be known (a sender can write their own Authentication-Results).
fn authenticated(signals: Option<&str>, usual: Option<&str>) -> bool {
    let Some(s) = signals.and_then(|s| serde_json::from_str::<crate::trust::Signals>(s).ok()) else { return false };
    let pass = s.dmarc == Some(true) || (s.dmarc.is_none() && s.dkim == Some(true) && s.spf == Some(true));
    // Without a usual server, the topmost Authentication-Results may be the sender's own.
    pass && usual.is_some_and(|u| s.authserv.as_deref() == Some(u))
}

/// "Name <address>", the name quoted if it holds special characters: otherwise a name like
/// "Boss <attacker@x.fr>" would send the reply to another address.
pub fn format_address(a: &Address) -> String {
    match &a.name {
        Some(n) if !n.trim().is_empty() => {
            let n = n.trim();
            if n.chars().any(|c| "()<>[]:;@\\,.\"".contains(c) || c.is_control()) {
                let escaped: String = n.chars().filter(|c| !c.is_control()).flat_map(|c| match c {
                    '"' | '\\' => vec!['\\', c],
                    c => vec![c],
                }).collect();
                format!("\"{escaped}\" <{}>", a.email)
            } else {
                format!("{n} <{}>", a.email)
            }
        }
        _ => a.email.clone(),
    }
}

/// Calendar settings and sync state of an account.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalendarAccount {
    pub caldav_url: Option<String>,
    pub home_url: Option<String>,
    /// The site trusted with the password when the home was found (see `caldav::site_of`).
    pub home_site: Option<String>,
    pub state: Option<CalendarState>,
    pub message: Option<String>,
    pub last_sync: Option<i64>,
    /// Grows with each CalDAV address saved: what a sync started before finds is dropped.
    pub generation: i64,
    /// The user does not use this account's calendar: it is never synced.
    pub ignored: bool,
}

/// A stored calendar, as the sync needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarRow {
    pub id: i64,
    pub remote_id: String,
    pub name: String,
    pub enabled: bool,
    pub ctag: Option<String>,
    pub synced_at: Option<i64>,
}

/// The calendar row `calendar_id` still stands for the calendar a sync fetched: the same account
/// and remote calendar (a deleted row's id is handed out again), the account not ignored.
fn calendar_still_fetched(db: &Connection, calendar_id: i64, account_id: i64, remote_id: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM calendars c WHERE c.id = ?1 AND c.account_id = ?2 AND c.remote_id = ?3
               AND NOT EXISTS (SELECT 1 FROM calendar_accounts ca WHERE ca.account_id = c.account_id AND ca.ignored = 1)",
            params![calendar_id, account_id, remote_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

const EVENT_COLUMNS: &str = "SELECT e.id, e.calendar_id, c.account_id, e.uid, e.instance, e.summary, e.location, e.description,
        e.start_at, e.end_at, e.all_day, e.status, e.organizer, e.url, e.busy,
        e.recurring, e.own AND NOT c.read_only AND NOT (c.source = 'caldav' AND e.href IS NULL),
        c.remote_id, c.source, e.href, e.series, e.response,
        e.response IS NOT NULL AND NOT c.read_only AND NOT (c.source = 'caldav' AND e.href IS NULL),
        e.attendees, e.meeting, e.reminders, e.zone
    FROM calendar_events e JOIN calendars c ON c.id = e.calendar_id";

/// Only what changed is written: an unchanged sync leaves the rows alone.
const UPSERT_EVENT: &str = "INSERT INTO calendar_events
        (calendar_id, uid, instance, start_at, end_at, all_day, summary, location, description, status, organizer, url, busy,
         href, recurring, own, series, response, attendees, meeting, reminders, zone)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)
    ON CONFLICT (calendar_id, uid, instance) DO UPDATE SET
        start_at = excluded.start_at, end_at = excluded.end_at, all_day = excluded.all_day,
        summary = excluded.summary, location = excluded.location, description = excluded.description,
        status = excluded.status, organizer = excluded.organizer, url = excluded.url, busy = excluded.busy,
        href = excluded.href, recurring = excluded.recurring, own = excluded.own,
        series = excluded.series, response = excluded.response,
        attendees = excluded.attendees, meeting = excluded.meeting, reminders = excluded.reminders,
        zone = excluded.zone
    WHERE start_at IS NOT excluded.start_at OR end_at IS NOT excluded.end_at OR all_day IS NOT excluded.all_day
        OR summary IS NOT excluded.summary OR location IS NOT excluded.location
        OR description IS NOT excluded.description OR status IS NOT excluded.status
        OR organizer IS NOT excluded.organizer OR url IS NOT excluded.url OR busy IS NOT excluded.busy
        OR href IS NOT excluded.href OR recurring IS NOT excluded.recurring OR own IS NOT excluded.own
        OR series IS NOT excluded.series OR response IS NOT excluded.response
        OR attendees IS NOT excluded.attendees OR meeting IS NOT excluded.meeting OR reminders IS NOT excluded.reminders
        OR zone IS NOT excluded.zone";

/// An event's guests or reminders as stored: JSON, `NULL` when there are none.
fn json_list<T: serde::Serialize>(list: &[T]) -> Option<String> {
    if list.is_empty() { None } else { serde_json::to_string(list).ok() }
}

fn json_of<T: serde::de::DeserializeOwned>(text: Option<String>) -> Vec<T> {
    text.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Whole-day events are stored as days (UTC midnights) and given in local midnights.
fn event_row(r: &rusqlite::Row) -> rusqlite::Result<CalendarEvent> {
    let all_day: bool = r.get(10)?;
    let day = |t: i64| if all_day { local_day(t) } else { t };
    Ok(CalendarEvent {
        id: r.get(0)?,
        calendar_id: r.get(1)?,
        account_id: r.get(2)?,
        uid: r.get(3)?,
        instance: r.get(4)?,
        summary: r.get(5)?,
        location: r.get(6)?,
        description: r.get(7)?,
        start: day(r.get(8)?),
        end: day(r.get(9)?),
        all_day,
        status: r.get(11)?,
        organizer: r.get(12)?,
        url: r.get(13)?,
        busy: r.get(14)?,
        recurring: r.get(15)?,
        editable: r.get(16)?,
        response: r.get(21)?,
        respondable: r.get(22)?,
        attendees: json_of(r.get(23)?),
        meeting: r.get(24)?,
        reminders: json_of(r.get(25)?),
        zone: r.get(26)?,
    })
}

#[derive(Debug, Clone)]
pub struct EventTarget {
    pub event: CalendarEvent,
    pub calendar_remote_id: String,
    pub source: CalendarSource,
    pub href: Option<String>,
    pub series: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CalendarTarget {
    pub account_id: i64,
    pub remote_id: String,
    pub source: CalendarSource,
    pub read_only: bool,
}

/// Local midnight of the day a stored whole-day time stands for: its UTC midnight (rounded to
/// the nearest one, so that a local midnight stored by an older version reads the same).
fn local_day(ts: i64) -> i64 {
    use chrono::TimeZone;
    let day = (ts + 43_200).div_euclid(86_400) * 86_400;
    let Some(t) = chrono::DateTime::from_timestamp(day, 0).map(|d| d.naive_utc()) else { return ts };
    chrono::Local
        .from_local_datetime(&t)
        .earliest()
        .or_else(|| chrono::Local.from_local_datetime(&(t + chrono::Duration::hours(1))).earliest())
        .map_or(ts, |d| d.timestamp())
}

/// The longest a zone is ahead of or behind UTC (whole-day events read in the local zone).
const MAX_ZONE_OFFSET: i64 = 14 * 3600;

// ── Calendars ──────────────────────────────────────────────────────────────

impl Store {
    pub fn calendars(&self) -> Result<Vec<CalendarInfo>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT c.id, c.account_id, c.name, c.color, c.source, c.enabled, c.read_only, c.error FROM calendars c
             JOIN accounts a ON a.id = c.account_id
             -- A sync that ended after the account was ignored may have put some back.
             WHERE NOT EXISTS (SELECT 1 FROM calendar_accounts ca WHERE ca.account_id = c.account_id AND ca.ignored = 1)
             ORDER BY a.position IS NULL, a.position, a.id, c.name COLLATE NOCASE, c.id",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(CalendarInfo {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    name: r.get(2)?,
                    color: r.get(3)?,
                    source: CalendarSource::parse(&r.get::<_, String>(4)?),
                    enabled: r.get(5)?,
                    read_only: r.get(6)?,
                    error: r.get(7)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Occurrences overlapping [`from`, `to`) in the enabled calendars, by start. Whole-day
    /// events are stored as days (UTC midnights) and given in local midnights.
    pub fn calendar_events(&self, from: i64, to: i64) -> Result<Vec<CalendarEvent>> {
        let db = self.read();
        let mut st = db.prepare(&format!(
            "{EVENT_COLUMNS}
             WHERE c.enabled = 1 AND NOT EXISTS (SELECT 1 FROM calendar_accounts ca WHERE ca.account_id = c.account_id AND ca.ignored = 1) AND (
                 (e.all_day = 0 AND e.start_at < ?2 AND (e.end_at > ?1 OR e.start_at >= ?1))
                 OR (e.all_day = 1 AND e.start_at < ?2 + ?3 AND e.end_at > ?1 - ?3))"
        ))?;
        let mut rows: Vec<CalendarEvent> = st.query_map(params![from, to, MAX_ZONE_OFFSET], event_row)?.collect::<Result<_, _>>()?;
        rows.retain(|e| e.start < to && (e.end > from || e.start >= from));
        rows.sort_by_key(|e| (e.start, e.end, e.id));
        Ok(rows)
    }

    /// An event, with where it lives on the provider's side.
    pub fn event_target(&self, id: i64) -> Result<EventTarget> {
        let db = self.read();
        db.query_row(&format!("{EVENT_COLUMNS} WHERE e.id = ?1"), [id], |r| {
            Ok(EventTarget {
                event: event_row(r)?,
                calendar_remote_id: r.get(17)?,
                source: CalendarSource::parse(&r.get::<_, String>(18)?),
                href: r.get(19)?,
                series: r.get(20)?,
            })
        })
        .optional()?
        .ok_or_else(|| Error::NotFound(crate::tr!("événement {id}", "event {id}")))
    }

    pub fn calendar_target(&self, id: i64) -> Result<CalendarTarget> {
        let db = self.read();
        db.query_row("SELECT account_id, remote_id, source, read_only FROM calendars WHERE id = ?1", [id], |r| {
            Ok(CalendarTarget { account_id: r.get(0)?, remote_id: r.get(1)?, source: CalendarSource::parse(&r.get::<_, String>(2)?), read_only: r.get(3)? })
        })
        .optional()?
        .ok_or_else(|| Error::NotFound(crate::tr!("agenda {id}", "calendar {id}")))
    }

    /// An event facteur just wrote, kept until the next sync brings it; returns its row.
    pub fn put_calendar_event(&self, calendar_id: i64, e: &crate::calendar::Instance) -> Result<i64> {
        let db = self.db();
        db.execute(UPSERT_EVENT, params![
            calendar_id, e.uid, e.instance, e.start, e.end, e.all_day, e.summary,
            e.location, e.description, e.status, e.organizer, e.url, e.busy,
            e.href, e.recurring, e.own, e.series, e.response, json_list(&e.attendees), e.meeting, json_list(&e.reminders), e.zone,
        ])?;
        Ok(db.query_row(
            "SELECT id FROM calendar_events WHERE calendar_id = ?1 AND uid = ?2 AND instance = ?3",
            params![calendar_id, e.uid, e.instance],
            |r| r.get(0),
        )?)
    }

    /// The account's answer, shown before the sync confirms it: on the occurrence, or on every
    /// date of its series.
    pub fn set_event_response(&self, id: i64, answer: &str, series: bool) -> Result<()> {
        let sql = if series {
            "UPDATE calendar_events SET response = ?2 WHERE response IS NOT NULL AND (calendar_id, uid) = (SELECT calendar_id, uid FROM calendar_events WHERE id = ?1)"
        } else {
            "UPDATE calendar_events SET response = ?2 WHERE id = ?1"
        };
        self.db().execute(sql, params![id, answer])?;
        Ok(())
    }

    /// An event written back under another key (a whole day turned into a timed event): its old
    /// row goes in the same transaction. Returns the new row.
    pub fn rekey_calendar_event(&self, old_id: i64, calendar_id: i64, e: &crate::calendar::Instance) -> Result<i64> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM calendar_events WHERE id = ?1", [old_id])?;
        tx.execute(UPSERT_EVENT, params![
            calendar_id, e.uid, e.instance, e.start, e.end, e.all_day, e.summary,
            e.location, e.description, e.status, e.organizer, e.url, e.busy,
            e.href, e.recurring, e.own, e.series, e.response, json_list(&e.attendees), e.meeting, json_list(&e.reminders), e.zone,
        ])?;
        let id = tx.query_row(
            "SELECT id FROM calendar_events WHERE calendar_id = ?1 AND uid = ?2 AND instance = ?3",
            params![calendar_id, e.uid, e.instance],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// The reminders due in (`from`, `to`]: each event with the time of its reminder. Not for a
    /// cancelled or declined event.
    pub fn event_alarms(&self, from: i64, to: i64) -> Result<Vec<(CalendarEvent, i64)>> {
        let db = self.read();
        let mut st = db.prepare_cached(&format!(
            "{EVENT_COLUMNS} WHERE e.reminders IS NOT NULL AND e.start_at > ?1 - 86400 AND e.start_at <= ?2 + 29 * 86400
               AND e.status IS NOT 'cancelled' AND e.response IS NOT 'declined'"
        ))?;
        let events: Vec<CalendarEvent> = st.query_map(params![from, to], event_row)?.collect::<Result<_, _>>()?;
        let mut due: Vec<(CalendarEvent, i64)> = events
            .into_iter()
            .filter_map(|e| {
                // The nearest reminder only: two due at once say the same thing.
                let at = e.reminders.iter().map(|m| e.start - m * 60).filter(|at| *at > from && *at <= to).max()?;
                Some((e, at))
            })
            .collect();
        due.sort_by_key(|(e, at)| (*at, e.start));
        Ok(due)
    }

    pub fn delete_calendar_event(&self, id: i64) -> Result<()> {
        self.db().execute("DELETE FROM calendar_events WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Shows or hides a calendar; returns its account. A hidden calendar keeps no events (they
    /// are fetched again when it is shown).
    pub fn set_calendar_enabled(&self, id: i64, enabled: bool) -> Result<i64> {
        let mut db = self.db();
        let tx = db.transaction()?;
        let account_id: i64 = tx
            .query_row("UPDATE calendars SET enabled = ?2 WHERE id = ?1 RETURNING account_id", params![id, enabled], |r| r.get(0))
            .optional()?
            .ok_or_else(|| Error::NotFound(crate::tr!("agenda {id}", "calendar {id}")))?;
        if !enabled {
            tx.execute("UPDATE calendars SET ctag = NULL, synced_at = NULL, error = NULL WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM calendar_events WHERE calendar_id = ?1", [id])?;
            // Its events gone, nothing of it is known any more: shown again, it is read whole.
            tx.execute("DELETE FROM caldav_etags WHERE calendar_id = ?1", [id])?;
            tx.execute("DELETE FROM settings WHERE key = 'caldav_whole:' || ?1", [id])?;
        }
        tx.commit()?;
        Ok(account_id)
    }

    /// Replaces the calendar list of an account with the provider's: new calendars are added
    /// (shown or not as the provider says), known ones renamed or recolored, and the others
    /// removed with their events. Returns the account's calendars, and whether anything changed.
    pub fn sync_calendar_list(
        &self,
        account_id: i64,
        source: CalendarSource,
        list: &[crate::calendar::RemoteCalendar],
    ) -> Result<(Vec<CalendarRow>, bool)> {
        let mut db = self.db();
        let tx = db.transaction()?;
        // Ignored while a sync was fetching the list: its calendars were just removed and must
        // not come back (their events then have no calendar to go to).
        let ignored: bool = tx
            .query_row("SELECT ignored FROM calendar_accounts WHERE account_id = ?1", [account_id], |r| r.get(0))
            .optional()?
            .unwrap_or(false);
        if ignored {
            return Ok((Vec::new(), false));
        }
        let mut changed = false;
        {
            let mut upsert = tx.prepare(
                "INSERT INTO calendars (account_id, remote_id, name, color, source, enabled, read_only)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (account_id, remote_id) DO UPDATE SET
                     name = excluded.name, color = excluded.color, source = excluded.source, read_only = excluded.read_only
                 WHERE name IS NOT excluded.name OR color IS NOT excluded.color OR source IS NOT excluded.source
                     OR read_only IS NOT excluded.read_only",
            )?;
            for c in list {
                changed |= upsert.execute(params![account_id, c.remote_id, c.name, c.color, source.as_str(), c.enabled, c.read_only])? > 0;
            }
            let keep: HashSet<&str> = list.iter().map(|c| c.remote_id.as_str()).collect();
            let mut st = tx.prepare("SELECT id, remote_id FROM calendars WHERE account_id = ?1")?;
            let gone: Vec<i64> = st
                .query_map([account_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .filter_map(|r| r.ok())
                .filter(|(_, remote)| !keep.contains(remote.as_str()))
                .map(|(id, _)| id)
                .collect();
            for id in gone {
                changed = true;
                tx.execute("DELETE FROM calendars WHERE id = ?1", [id])?;
            }
        }
        let rows = {
            let mut st = tx.prepare("SELECT id, remote_id, name, enabled, ctag, synced_at FROM calendars WHERE account_id = ?1 ORDER BY id")?;
            st.query_map([account_id], |r| {
                Ok(CalendarRow { id: r.get(0)?, remote_id: r.get(1)?, name: r.get(2)?, enabled: r.get(3)?, ctag: r.get(4)?, synced_at: r.get(5)? })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        tx.commit()?;
        Ok((rows, changed))
    }

    /// Stores the occurrences just fetched for a calendar in place of the previous ones: each
    /// occurrence keeps its row (and id) across syncs, by (uid, instance); only those changed
    /// are written and only those gone are removed. Clears the calendar's error. Returns
    /// whether any event changed.
    /// `account_id`, `remote_id`: the calendar the sync fetched. Nothing is written when the row
    /// no longer stands for it (the account ignored meanwhile, its id handed to another calendar).
    pub fn replace_calendar_events(&self, calendar_id: i64, account_id: i64, remote_id: &str, events: &[crate::calendar::Instance], ctag: Option<&str>) -> Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        if !calendar_still_fetched(&tx, calendar_id, account_id, remote_id)? {
            return Ok(false);
        }
        let mut changed = false;
        {
            let mut keep: HashSet<(&str, &str)> = HashSet::new();
            let mut upsert = tx.prepare(UPSERT_EVENT)?;
            for e in events {
                // The same occurrence twice in an answer: the first one counts.
                if !keep.insert((e.uid.as_str(), e.instance.as_str())) {
                    continue;
                }
                changed |= upsert.execute(params![
                    calendar_id, e.uid, e.instance, e.start, e.end, e.all_day, e.summary,
                    e.location, e.description, e.status, e.organizer, e.url, e.busy,
                    e.href, e.recurring, e.own, e.series, e.response, json_list(&e.attendees), e.meeting, json_list(&e.reminders), e.zone,
                ])? > 0;
            }
            let mut st = tx.prepare("SELECT id, uid, instance FROM calendar_events WHERE calendar_id = ?1")?;
            let gone: Vec<i64> = st
                .query_map([calendar_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
                .filter_map(|r| r.ok())
                .filter(|(_, uid, instance)| !keep.contains(&(uid.as_str(), instance.as_str())))
                .map(|(id, _, _)| id)
                .collect();
            let mut delete = tx.prepare("DELETE FROM calendar_events WHERE id = ?1")?;
            for id in gone {
                changed = true;
                delete.execute([id])?;
            }
        }
        tx.execute("UPDATE calendars SET ctag = ?2, synced_at = ?3, error = NULL WHERE id = ?1", params![calendar_id, ctag, now()])?;
        tx.commit()?;
        Ok(changed)
    }

    /// The ETag of each CalDAV object of the calendar, as last read.
    pub fn caldav_etags(&self, calendar_id: i64) -> Result<HashMap<String, String>> {
        let db = self.read();
        let mut st = db.prepare("SELECT href, etag FROM caldav_etags WHERE calendar_id = ?1")?;
        let rows = st.query_map([calendar_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// A CalDAV calendar read again in part: `listed`, the objects it holds in the window with their
    /// ETag; `fetched`, those read again, whose events are `events`. The events of objects no
    /// longer listed go, those of the objects read again are replaced, the others stay.
    pub fn update_calendar_objects(
        &self,
        calendar_id: i64,
        account_id: i64,
        remote_id: &str,
        listed: &[(String, String)],
        fetched: &HashSet<String>,
        events: &[crate::calendar::Instance],
        ctag: Option<&str>,
    ) -> Result<bool> {
        let mut db = self.db();
        let tx = db.transaction()?;
        if !calendar_still_fetched(&tx, calendar_id, account_id, remote_id)? {
            return Ok(false);
        }
        let present: HashSet<&str> = listed.iter().map(|(h, _)| h.as_str()).collect();
        // What came back keeps its row (and id: a form left open on it still saves), as with
        // `replace_calendar_events`.
        let back: HashSet<(&str, &str)> = events.iter().map(|e| (e.uid.as_str(), e.instance.as_str())).collect();
        let mut changed = false;
        {
            // Rows of objects gone or read again, and rows without an object (written locally
            // before the server named them), unless the reading gave them again.
            let mut st = tx.prepare("SELECT id, href, uid, instance FROM calendar_events WHERE calendar_id = ?1")?;
            let stale: Vec<i64> = st
                .query_map([calendar_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?
                .filter_map(|r| r.ok())
                .filter(|(_, href, ..)| href.as_deref().is_none_or(|h| !present.contains(h) || fetched.contains(h)))
                .filter(|(_, _, uid, instance)| !back.contains(&(uid.as_str(), instance.as_str())))
                .map(|(id, ..)| id)
                .collect();
            let mut delete = tx.prepare("DELETE FROM calendar_events WHERE id = ?1")?;
            for id in stale {
                changed = true;
                delete.execute([id])?;
            }
            let mut keep: HashSet<(&str, &str)> = HashSet::new();
            let mut upsert = tx.prepare(UPSERT_EVENT)?;
            for e in events {
                if !keep.insert((e.uid.as_str(), e.instance.as_str())) {
                    continue;
                }
                changed |= upsert.execute(params![
                    calendar_id, e.uid, e.instance, e.start, e.end, e.all_day, e.summary,
                    e.location, e.description, e.status, e.organizer, e.url, e.busy,
                    e.href, e.recurring, e.own, e.series, e.response, json_list(&e.attendees), e.meeting, json_list(&e.reminders), e.zone,
                ])? > 0;
            }
            tx.execute("DELETE FROM caldav_etags WHERE calendar_id = ?1", [calendar_id])?;
            let mut etag = tx.prepare("INSERT OR REPLACE INTO caldav_etags (calendar_id, href, etag) VALUES (?1, ?2, ?3)")?;
            for (href, tag) in listed {
                etag.execute(params![calendar_id, href, tag])?;
            }
        }
        tx.execute("UPDATE calendars SET ctag = ?2, synced_at = ?3, error = NULL WHERE id = ?1", params![calendar_id, ctag, now()])?;
        tx.commit()?;
        Ok(changed)
    }

    /// Why the last sync of a calendar failed (`None`: it did not). Returns whether it changed.
    /// `account_id`, `remote_id`: as for [`Store::replace_calendar_events`].
    pub fn set_calendar_error(&self, calendar_id: i64, account_id: i64, remote_id: &str, error: Option<&str>) -> Result<bool> {
        let db = self.db();
        if !calendar_still_fetched(&db, calendar_id, account_id, remote_id)? {
            return Ok(false);
        }
        Ok(db.execute("UPDATE calendars SET error = ?2 WHERE id = ?1 AND error IS NOT ?2", params![calendar_id, error])? > 0)
    }

    pub fn calendar_account(&self, account_id: i64) -> Result<CalendarAccount> {
        Ok(self.calendar_accounts()?.remove(&account_id).unwrap_or_default())
    }

    pub fn calendar_accounts(&self) -> Result<HashMap<i64, CalendarAccount>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT account_id, caldav_url, home_url, home_site, state, message, last_sync, generation, ignored FROM calendar_accounts",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    CalendarAccount {
                        caldav_url: r.get(1)?,
                        home_url: r.get(2)?,
                        home_site: r.get(3)?,
                        state: r.get::<_, Option<String>>(4)?.as_deref().and_then(CalendarState::parse),
                        message: r.get(5)?,
                        last_sync: r.get(6)?,
                        generation: r.get(7)?,
                        ignored: r.get(8)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Per account, the oldest and latest fetch of its shown calendars that were fetched.
    pub fn calendar_fetches(&self) -> Result<HashMap<i64, (i64, i64)>> {
        let db = self.read();
        let mut st = db.prepare(
            "SELECT account_id, MIN(synced_at), MAX(synced_at) FROM calendars
             WHERE enabled = 1 AND synced_at IS NOT NULL GROUP BY account_id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))))?.collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The CalDAV address given by the user (`None`: discovery). The calendar home found before
    /// and the last state are forgotten, and the settings' generation moves on.
    pub fn set_caldav_url(&self, account_id: i64, url: Option<&str>) -> Result<()> {
        self.db().execute(
            "INSERT INTO calendar_accounts (account_id, caldav_url, generation) VALUES (?1, ?2, 1)
             ON CONFLICT (account_id) DO UPDATE SET caldav_url = excluded.caldav_url, home_url = NULL, home_site = NULL,
                 state = NULL, message = NULL, generation = calendar_accounts.generation + 1",
            params![account_id, url],
        )?;
        Ok(())
    }

    /// Whether the user uses this account's calendar. Ignored, its calendars and their events
    /// go, and so does the last state: used again, everything is found anew. The settings'
    /// generation moves on, so a sync under way drops what it finds.
    pub fn set_calendar_ignored(&self, account_id: i64, ignored: bool) -> Result<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute(
            "INSERT INTO calendar_accounts (account_id, ignored, generation) VALUES (?1, ?2, 1)
             ON CONFLICT (account_id) DO UPDATE SET ignored = excluded.ignored, state = NULL, message = NULL,
                 generation = calendar_accounts.generation + 1",
            params![account_id, ignored],
        )?;
        if ignored {
            tx.execute("DELETE FROM calendars WHERE account_id = ?1", [account_id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The calendar home found, and the site trusted with the password then; written only when
    /// the settings are still those of `generation`. Returns whether it was.
    pub fn set_calendar_home(&self, account_id: i64, home: Option<(&str, &str)>, generation: i64) -> Result<bool> {
        let (url, site) = (home.map(|h| h.0), home.map(|h| h.1));
        Ok(self.db().execute(
            "INSERT INTO calendar_accounts (account_id, home_url, home_site, generation) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account_id) DO UPDATE SET home_url = excluded.home_url, home_site = excluded.home_site
                 WHERE calendar_accounts.generation = excluded.generation",
            params![account_id, url, site, generation],
        )? > 0)
    }

    /// How the calendar sync of an account went, when its settings are still those of
    /// `generation`; `last_sync` moves when some calendar synced (ok or partial). Returns whether
    /// the state or its message changed.
    pub fn set_calendar_state(&self, account_id: i64, state: CalendarState, message: Option<&str>, generation: i64) -> Result<bool> {
        let synced = matches!(state, CalendarState::Ok | CalendarState::Partial).then(now);
        let db = self.db();
        let before: Option<(Option<String>, Option<String>)> = db
            .query_row("SELECT state, message FROM calendar_accounts WHERE account_id = ?1", [account_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let written = db.execute(
            "INSERT INTO calendar_accounts (account_id, state, message, last_sync, generation) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (account_id) DO UPDATE SET state = excluded.state, message = excluded.message,
                 last_sync = COALESCE(excluded.last_sync, calendar_accounts.last_sync)
                 WHERE calendar_accounts.generation = excluded.generation",
            params![account_id, state.as_str(), message, synced, generation],
        )? > 0;
        Ok(written && before != Some((Some(state.as_str().to_string()), message.map(str::to_string))))
    }
}

const ACCOUNT_COLS: &str = "id, name, email, username, imap_host, imap_port, imap_security, \
                            smtp_host, smtp_port, smtp_security, last_sync, auth, oauth_provider, signature, avatar, label, identities, color, avatar_custom, screener_since, \
                            quota_used, quota_limit, quota_at";

fn account_from_row(r: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        name: r.get(1)?,
        email: r.get(2)?,
        username: r.get(3)?,
        imap: ServerConfig { host: r.get(4)?, port: r.get(5)?, security: Security::parse(&r.get::<_, String>(6)?) },
        smtp: ServerConfig { host: r.get(7)?, port: r.get(8)?, security: Security::parse(&r.get::<_, String>(9)?) },
        last_sync: r.get(10)?,
        auth: match (r.get::<_, String>(11)?.as_str(), r.get::<_, Option<String>>(12)?) {
            ("oauth2", Some(p)) => match OAuthProvider::parse(&p) {
                Some(provider) => AuthMethod::OAuth2 { provider },
                None => AuthMethod::Password,
            },
            _ => AuthMethod::Password,
        },
        signature: r.get(13)?,
        avatar: r.get(14)?,
        label: r.get(15)?,
        identities: serde_json::from_str(&r.get::<_, String>(16)?).unwrap_or_default(),
        color: r.get(17)?,
        avatar_custom: r.get(18)?,
        screener_since: r.get(19)?,
        quota: match (r.get::<_, Option<i64>>(20)?, r.get::<_, Option<i64>>(21)?, r.get::<_, Option<i64>>(22)?) {
            (Some(used), Some(limit), Some(at)) if limit > 0 => Some(crate::model::Quota { used, limit, at }),
            _ => None,
        },
    })
}

const STATE_COLS: &str =
    "id, account_id, name, role, selectable, uidvalidity, uidnext, highestmodseq, sync_from_uid, server_count, is_label";

fn state_from_row(r: &rusqlite::Row) -> rusqlite::Result<MailboxState> {
    Ok(MailboxState {
        id: r.get(0)?,
        account_id: r.get(1)?,
        name: r.get(2)?,
        role: r.get::<_, Option<String>>(3)?.as_deref().and_then(Role::parse),
        selectable: r.get(4)?,
        uidvalidity: r.get(5)?,
        uidnext: r.get(6)?,
        highestmodseq: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
        sync_from_uid: r.get(8)?,
        server_count: r.get(9)?,
        is_label: r.get(10)?,
    })
}

const SUMMARY_COLS: &str = "m.id, m.account_id, m.mailbox_id, m.thread_id, m.subject, m.from_name, m.from_addr, \
                            m.to_list, m.date, m.preview, m.seen, m.flagged, m.answered, m.draft, m.has_attachments";

/// [`SUMMARY_COLS`] for a [`Boxes::pool`]: what to pool, then what to select from it, the folder
/// a message is listed in standing for its own.
const POOL_SUMMARY: (&str, &str) = (
    "id, account_id, thread_id, subject, from_name, from_addr, to_list, date, preview, seen, flagged, answered, draft, has_attachments",
    "id, account_id, box, thread_id, subject, from_name, from_addr, to_list, date, preview, seen, flagged, answered, draft, has_attachments",
);

/// Search hits (with their Message-ID, size, and whether they sit in All Mail), newest first, one
/// per message as `SAME_COPY` tells them apart, `limit` at most: the copy outside All Mail, else
/// the first stored. Without a Message-ID, each row is a message of its own.
fn one_per_message(rows: Vec<(MessageSummary, Option<String>, i64, bool)>, limit: usize) -> Vec<MessageSummary> {
    let mut found: Vec<(MessageSummary, bool)> = Vec::with_capacity(rows.len().min(limit));
    let mut seen: HashMap<(String, i64, i64, Option<String>), usize> = HashMap::new();
    for (summary, message_id, size, in_all) in rows {
        let Some(message_id) = message_id else {
            found.push((summary, in_all));
            continue;
        };
        let key = (message_id, size, summary.date, summary.from.as_ref().map(|a| a.email.clone()));
        match seen.get(&key) {
            Some(&i) => {
                let (kept, kept_in_all) = &found[i];
                if (in_all, summary.id) < (*kept_in_all, kept.id) {
                    found[i] = (summary, in_all);
                }
            }
            None => {
                seen.insert(key, found.len());
                found.push((summary, in_all));
            }
        }
    }
    found.truncate(limit);
    found.into_iter().map(|(summary, _)| summary).collect()
}

fn summary_from_row(r: &rusqlite::Row) -> rusqlite::Result<MessageSummary> {
    let from_addr: Option<String> = r.get(6)?;
    let to: String = r.get(7)?;
    Ok(MessageSummary {
        id: r.get(0)?,
        account_id: r.get(1)?,
        mailbox_id: r.get(2)?,
        thread_id: r.get(3)?,
        subject: r.get(4)?,
        from: from_addr.map(|email| Address { name: r.get(5).ok().flatten(), email }),
        to: serde_json::from_str(&to).unwrap_or_default(),
        date: r.get(8)?,
        preview: r.get(9)?,
        flags: Flags { seen: r.get(10)?, flagged: r.get(11)?, answered: r.get(12)?, draft: r.get(13)? },
        has_attachments: r.get(14)?,
        labels: Vec::new(),
    })
}

/// The error of an outbox row, as shown: the `interrupted` marker (and the French sentence
/// stored by earlier versions) in the current language, with the advice that fits the account's
/// sending server (`smtp_host`).
fn outgoing_error(stored: String, smtp_host: &str) -> String {
    if stored == "interrupted" || stored.starts_with("Envoi interrompu par l'arrêt de facteur") {
        crate::tr!(
            "Envoi interrompu par l'arrêt de facteur : {}",
            "Sending interrupted when facteur closed: {}",
            crate::smtp::maybe_sent_advice(smtp_host)
        )
    } else {
        stored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(mid: &str, irt: Option<&str>, subject: &str, date: i64) -> HeaderInfo {
        HeaderInfo {
            message_id: Some(mid.into()),
            in_reply_to: irt.map(Into::into),
            references: irt.map(|r| vec![r.to_string()]).unwrap_or_default(),
            subject: subject.into(),
            from: Some(Address { name: Some("Françoise".into()), email: "f@example.org".into() }),
            date,
            ..Default::default()
        }
    }

    const FLAGS: ServerFlags = ServerFlags { seen: false, flagged: false, answered: false, draft: false };

    #[test]
    fn refusals_and_network_cuts_have_their_own_limits() {
        let (s, acc, _, _) = setup();
        let op = s.push_op(acc, r#"{"kind":"move_set","mailbox":"INBOX","uids":[1],"dest":"Archive"}"#, "", None).unwrap();
        for _ in 0..4 {
            assert!(!s.fail_op(op, "cut", 10).unwrap());
        }
        assert!(!s.refuse_op(op, "NO busy", 5).unwrap(), "four cuts before do not make one refusal final");
        for _ in 0..3 {
            assert!(!s.refuse_op(op, "NO busy", 5).unwrap());
        }
        assert!(s.refuse_op(op, "NO busy", 5).unwrap(), "the fifth refusal is");
    }

    #[test]
    fn a_large_folder_goes_in_steps_its_bodies_kept_for_copies_elsewhere() {
        let (s, acc, inbox, sent) = setup();
        let archive = s.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let n = STEP_ROWS as u32 * 2 + 10;
        for uid in 1..=n {
            s.insert_header(acc, archive, uid, &header(&format!("m{uid}@x"), None, "Vieux", 1), FLAGS, 10).unwrap();
        }
        let kept = s.insert_header(acc, archive, n + 1, &header("k@x", None, "Gardé", 2), FLAGS, 10).unwrap();
        let copy = s.insert_header(acc, sent, 1, &header("k@x", None, "Gardé", 2), FLAGS, 10).unwrap();
        let raw = b"Subject: Garde\r\n\r\nBonjour";
        s.store_body(kept, raw, &crate::mime::body_info(raw)).unwrap();
        assert_eq!(s.purge_local(archive).unwrap(), n as usize + 1);
        assert!(s.mailbox_message_ids(archive).unwrap().is_empty());
        assert_eq!(s.body(copy).unwrap().as_deref(), Some(&raw[..]), "the body passed to the copy that stays");
        assert_eq!(s.mailbox_message_ids(inbox).unwrap().len(), 0);
    }

    #[test]
    fn indexes_that_cannot_be_rebuilt_offer_to_set_the_database_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("facteur.db");
        drop(Store::open(&path).unwrap());
        // An index facteur cannot build again (here: on a function only another connection knew).
        {
            let conn = Connection::open(&path).unwrap();
            conn.create_scalar_function("odd", 1, rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC, |c| c.get::<i64>(0)).unwrap();
            conn.execute_batch("CREATE TABLE extra (x INTEGER); CREATE INDEX extra_odd ON extra (odd(x)); INSERT INTO extra VALUES (1);").unwrap();
        }
        std::fs::write(damage_marker(&path), b"").unwrap();
        let err = Store::open(&path).err().expect("refused");
        assert!(err.is_damaged_db(), "the set-aside dialog, not a start that fails for good: {err:?}");
    }

    #[test]
    fn a_search_index_rebuild_cut_short_resumes_and_tolerates_the_sync() {
        let (s, acc, inbox, _) = setup();
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "Devis", 1), FLAGS, 10).unwrap();
        let b = s.insert_header(acc, inbox, 2, &header("b@x", None, "Facture", 2), FLAGS, 10).unwrap();
        let raw = b"Subject: Facture\r\n\r\nLe chantier commence lundi";
        s.store_body(b, raw, &crate::mime::body_info(raw)).unwrap();
        let indexed = |word: &str| s.read().query_row("SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1", [word], |r| r.get::<_, i64>(0)).unwrap();
        // Cut after its first message's header: the rest is missing, yet the table is sound.
        s.db().execute_batch(&format!("DELETE FROM messages_fts WHERE rowid != {a}")).unwrap();
        s.set_setting(SEARCH_INDEX_REBUILD, &format!("headers:{a}")).unwrap();
        assert_eq!(indexed("facture"), 0);
        // Resumed at the next start, without trusting the integrity check.
        assert!(s.check_search_index().unwrap());
        assert_eq!((indexed("facture"), indexed("chantier"), indexed("devis")), (1, 1, 1));
        assert_eq!(s.setting(SEARCH_INDEX_REBUILD).unwrap(), None);
        // A message the sync indexed meanwhile is no conflict.
        s.set_setting(SEARCH_INDEX_REBUILD, "headers:0").unwrap();
        s.db().execute_batch("DELETE FROM messages_fts").unwrap();
        s.db().execute(&format!("INSERT INTO messages_fts (rowid, subject, sender, recipients, body) VALUES ({b}, 'Facture', '', '', '')"), []).unwrap();
        s.set_setting(SEARCH_INDEX_REBUILD, &format!("headers:{}", a - 1)).unwrap();
        assert!(s.check_search_index().unwrap());
        assert_eq!(indexed("devis"), 1);
    }

    #[test]
    fn a_failed_search_index_rebuild_leaves_the_writer_usable() {
        let (s, ..) = setup();
        // A leftover table under an FTS5 shadow table's name: the CREATE fails after the DROP.
        s.db().execute_batch("DROP TABLE messages_fts; CREATE TABLE messages_fts_config (k PRIMARY KEY, v);").unwrap();
        s.set_setting(SEARCH_INDEX_REBUILD, "headers:0").unwrap();
        assert!(s.check_search_index().is_err());
        assert!(s.db().is_autocommit(), "no transaction left open on the writer");
        s.set_setting("x", "1").unwrap();
    }

    #[test]
    fn body_rebuild_steps_are_bounded_by_size() {
        let (s, acc, inbox, _) = setup();
        let mut ids = Vec::new();
        for uid in 1..=5 {
            let id = s.insert_header(acc, inbox, uid, &header(&format!("{uid}@x"), None, "Devis", 1), FLAGS, 10).unwrap();
            let raw = format!("Subject: Devis\r\n\r\nmot{uid} {}", "x".repeat(1 << 20));
            s.store_body(id, raw.as_bytes(), &crate::mime::body_info(raw.as_bytes())).unwrap();
            ids.push(id);
        }
        let step = body_step(&s.read(), 0, 2 << 20).unwrap();
        assert!(!step.is_empty() && step.len() <= 2, "{step:?}");
        assert_eq!(body_step(&s.read(), 0, 1).unwrap().len(), 1, "always one, however large");
        s.set_setting(SEARCH_INDEX_REBUILD, "headers:0").unwrap();
        assert!(s.check_search_index().unwrap());
        let indexed = s.read().query_row("SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'mot5'", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(indexed, 1);
    }

    #[test]
    fn marking_many_unread_read_is_not_quadratic() {
        let (s, _, inbox, _) = setup();
        let started = std::time::Instant::now();
        s.note_flag_changes(&(1..=60_000u32).map(|u| (inbox, u)).collect::<Vec<_>>());
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "{:?}", started.elapsed());
        assert_eq!(s.flags_touched(inbox, 0).len(), 60_000);
    }

    #[test]
    fn duplicate_cleanup_takes_what_hangs_on_the_copies() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY, mailbox_id INTEGER, uid INTEGER);
             CREATE TABLE bodies (message_id INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE, raw BLOB);
             CREATE TABLE labels (mailbox_id INTEGER, message_id INTEGER REFERENCES messages(id) ON DELETE CASCADE);
             CREATE TABLE handled_quiet (on_ INTEGER NOT NULL);
             INSERT INTO messages VALUES (1, 5, 7), (2, 5, 7);
             INSERT INTO bodies VALUES (1, 'a'), (2, 'b');
             INSERT INTO labels VALUES (9, 1), (9, 2);",
        )
        .unwrap();
        drop_duplicate_copies(&conn).unwrap();
        let orphans = |table: &str| conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE message_id NOT IN (SELECT id FROM messages)"), [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!((orphans("bodies"), orphans("labels")), (0, 0));
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM handled_quiet", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn a_busy_database_at_the_check_is_no_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("facteur.db");
        drop(Store::open(&path).unwrap());
        let other = Connection::open(&path).unwrap();
        other.execute_batch("BEGIN IMMEDIATE;").unwrap();
        std::fs::write(damage_marker(&path), b"").unwrap();
        let err = Store::open(&path).err().expect("busy");
        assert!(!err.is_damaged_db(), "{err}");
        assert!(damage_marker(&path).exists(), "checked again next time");
    }

    #[test]
    fn damage_found_while_running_is_checked_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("facteur.db");
        drop(Store::open(&path).unwrap());
        let marker = damage_marker(&path);

        // A false alarm: the check passes and the marker goes; the search index is checked
        // later, once.
        std::fs::write(&marker, b"").unwrap();
        let s = Store::open(&path).unwrap();
        assert!(!marker.exists());
        assert_eq!(s.setting(SEARCH_INDEX_CHECK).unwrap().as_deref(), Some("1"));
        assert!(!s.check_search_index().unwrap(), "sound");
        assert!(s.setting(SEARCH_INDEX_CHECK).unwrap().is_none());
        // A damaged index is found on a reader, then built again.
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = s
            .insert_account(&NewAccount {
                auth: NewAuth::default(),
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv,
            })
            .unwrap();
        let inbox = s.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let id = s.insert_header(acc, inbox, 1, &header("d@x", None, "Devis", 1), FLAGS, 10).unwrap();
        let raw = b"Subject: Devis\r\n\r\nLe chantier commence lundi";
        s.store_body(id, raw, &crate::mime::body_info(raw)).unwrap();
        s.set_setting(SEARCH_INDEX_CHECK, "1").unwrap();
        s.db().execute_batch("DELETE FROM messages_fts_content;").unwrap();
        assert!(s.check_search_index().unwrap(), "rebuilt");
        assert_eq!(s.read().query_row("PRAGMA integrity_check(messages_fts)", [], |r| r.get::<_, String>(0)).unwrap(), "ok");
        // Rebuilt from the messages: a message whose indexed text was lost is found again.
        let indexed = |word: &str| s.read().query_row("SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1", [word], |r| r.get::<_, i64>(0)).unwrap();
        assert!(indexed("devis") > 0, "found again by its subject");
        assert!(indexed("chantier") > 0, "found again by its body");
        drop(s);

        // A page really damaged: the start reports a damaged database (the set-aside dialog). On
        // a database of its own: the search index, rebuilt above, now lies in the first pages.
        let path = dir.path().join("other.db");
        let fresh = Store::open(&path).unwrap();
        fresh.db().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        drop(fresh);
        let marker = damage_marker(&path);
        let mut bytes = std::fs::read(&path).unwrap();
        let page = 4096;
        for b in &mut bytes[page..page * 3] {
            *b = 0xA5;
        }
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(&marker, b"").unwrap();
        let err = Store::open(&path).err().expect("damaged");
        assert!(err.is_damaged_db(), "{err:?}");
    }

    fn setup() -> (Store, i64, i64, i64) {
        let s = Store::open_in_memory().unwrap();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = s
            .insert_account(&NewAccount {
                auth: NewAuth::default(),
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv,
            })
            .unwrap();
        let inbox = s.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let sent = s.upsert_mailbox(acc, "Sent", Some("/"), Some(Role::Sent), true).unwrap();
        (s, acc, inbox, sent)
    }

    #[test]
    fn a_label_lists_its_messages_in_each_view() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let mairie = s.upsert_mailbox(acc, "Mairie", Some("/"), None, true).unwrap();
        for label in [inbox, mairie] {
            s.set_label_folder(label, true).unwrap();
        }
        let a = s.insert_header(acc, all, 3, &header("a@x", None, "Façade", 5), FLAGS, 10).unwrap();
        let b = s.insert_header(acc, all, 4, &header("b@x", None, "Autre", 6), FLAGS, 10).unwrap();
        s.set_labels(a, &[inbox, mairie]).unwrap();
        let threads = |boxes: &[i64], except: &[i64]| s.list_threads(boxes, except, ThreadFilter::All, false, None, 50, 0).unwrap();

        // Listed in its labels, from there; still in its own folder.
        let listed = threads(&[inbox], &[]);
        assert_eq!(listed.iter().map(|t| (t.latest_id, t.mailbox_id)).collect::<Vec<_>>(), vec![(a, inbox)]);
        assert_eq!(threads(&[mairie], &[]).len(), 1);
        assert_eq!(threads(&[all], &[]).len(), 2);
        // The "Done" view: All Mail less what is in the inbox.
        assert_eq!(threads(&[all], &[inbox]).iter().map(|t| t.latest_id).collect::<Vec<_>>(), vec![b]);

        let boxes = s.list_mailboxes(acc).unwrap();
        let counts = |id| boxes.iter().find(|m| m.id == id).map(|m| (m.unread, m.total)).unwrap();
        assert_eq!((counts(inbox), counts(mairie), counts(all)), ((1, 1), (1, 1), (2, 2)));
        let conversations = |id| boxes.iter().find(|m| m.id == id).map(|m| (m.threads, m.unread_threads)).unwrap();
        assert_eq!((conversations(inbox), conversations(all)), ((1, 1), (2, 2)));
        assert_eq!(s.category_unread(&[mairie]).unwrap().values().sum::<i64>(), 1);
        let folder = boxes.iter().find(|m| m.id == mairie).cloned().unwrap();
        let summary = s.folder_summary(folder, Vec::new(), true, 5).unwrap();
        assert_eq!((summary.local, summary.recent[0].mailbox_id), (1, mairie));
        assert_eq!(s.folder_messages_from(&[mairie], "f@example.org", 5).unwrap().len(), 1);
        assert_eq!(s.inbox_messages(acc).unwrap(), vec![a]);
        assert_eq!(s.mailbox_message_ids(mairie).unwrap(), vec![a]);
        let thread = s.summary(a).unwrap().thread_id;
        assert_eq!(s.thread_messages(acc, thread, Some(mairie)).unwrap()[0].mailbox_id, mairie);
        assert_eq!(s.search(acc, "dans:mairie", 10, false).unwrap().iter().map(|m| m.id).collect::<Vec<_>>(), vec![a]);

        // Marking the inbox read reaches the labelled message only.
        s.mark_account_seen(acc, true).unwrap();
        assert!(s.summary(a).unwrap().flags.seen && !s.summary(b).unwrap().flags.seen);

        // Labels change; a message gone takes its labels along.
        s.set_labels(a, &[mairie]).unwrap();
        assert!(threads(&[inbox], &[]).is_empty());
        assert_eq!(s.message_labels(&[a, b]).unwrap(), HashMap::from([(a, vec![mairie])]));
        s.delete_uids(all, &[3]).unwrap();
        assert!(threads(&[mairie], &[]).is_empty());
        assert!(s.message_labels(&[a]).unwrap().is_empty());
    }

    #[test]
    fn folder_roles_reach_labelled_messages() {
        let (s, acc, _, sent) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let drafts = s.upsert_mailbox(acc, "[Gmail]/Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        let screener = s.upsert_mailbox(acc, "Screener", Some("/"), Some(Role::Screener), true).unwrap();
        let later = s.upsert_mailbox(acc, "Later", Some("/"), Some(Role::Snoozed), true).unwrap();
        for label in [sent, drafts, screener, later] {
            s.set_label_folder(label, true).unwrap();
        }
        let mut mine = header("sent@x", None, "Devis", 5);
        mine.to = vec![Address { name: Some("Cédric".into()), email: "cedric@example.org".into() }];
        let sent_one = s.insert_header(acc, all, 1, &mine, FLAGS, 10).unwrap();
        let draft = s.insert_header(acc, all, 2, &header("draft@x", None, "Brouillon", 6), FLAGS, 10).unwrap();
        let held = s.insert_header(acc, all, 3, &header("held@x", None, "Inconnu", 7), FLAGS, 10).unwrap();
        s.set_labels(sent_one, &[sent]).unwrap();
        s.set_labels(draft, &[drafts]).unwrap();
        s.set_labels(held, &[screener, later]).unwrap();

        assert_eq!(s.recent_recipients(&[], 5).unwrap().iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), vec!["cedric@example.org"]);
        assert!(s.draft_present(acc, "draft@x").unwrap() && !s.draft_present(acc, "held@x").unwrap());
        assert_eq!(s.held_senders(Some(acc)).unwrap().iter().map(|h| h.4).collect::<Vec<_>>(), vec![held]);
        assert_eq!(s.messages_from_pattern(acc, "@example.org", Role::Screener).unwrap(), vec![held]);
        let thread = s.summary(held).unwrap().thread_id;
        assert_eq!(s.snoozed_messages(later, thread, &[]).unwrap(), vec![held]);
    }

    #[test]
    fn a_message_restored_from_the_bin_into_a_label_waits_in_all_mail() {
        let (s, acc, _, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        assert!(s.convert_to_labels(acc).unwrap());
        let id = s.insert_header(acc, all, 9, &header("back@x", None, "Retour", 5), FLAGS, 10).unwrap();
        s.db().execute("UPDATE messages SET uid = NULL WHERE id = ?1", [id]).unwrap();
        s.push_op(acc, r#"{"kind":"move_set","mailbox":"[Gmail]/Trash","uids":[3],"dest":"INBOX"}"#, "", None).unwrap();
        assert!(s.moves_pending_into(all).unwrap(), "a move into the inbox label lands in All Mail");
        s.drop_orphan_placeholders(all, false).unwrap();
        assert!(s.location(id).is_ok(), "kept while the move waits");
    }

    #[test]
    fn gmail_drafts_deleted_from_their_label_are_known_to_the_all_mail_sync() {
        let (s, acc, _, _) = setup();
        s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        s.upsert_mailbox(acc, "[Gmail]/Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        s.push_op(acc, r#"{"kind":"delete_draft","mailbox":"[Gmail]/Drafts","message_id":"d@x"}"#, "", None).unwrap();
        assert!(s.pending_draft_deletes(acc, "[Gmail]/All Mail").unwrap().is_empty(), "folders: Drafts only");
        s.db().execute("UPDATE accounts SET labels_model = 1 WHERE id = ?1", [acc]).unwrap();
        assert_eq!(s.pending_draft_deletes(acc, "[Gmail]/All Mail").unwrap(), HashSet::from(["d@x".to_string()]));
    }

    #[test]
    fn a_gmail_conversion_goes_in_steps_and_resumes() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        for uid in 1..=1200 {
            let h = header(&format!("m{uid}@x"), None, "Sujet", i64::from(uid));
            s.insert_header(acc, all, uid, &h, FLAGS, 10).unwrap();
            s.insert_header(acc, inbox, uid, &h, FLAGS, 10).unwrap();
        }
        // Cut after its first step (facteur closed): folders made labels, copies still there,
        // and an operation queued since.
        s.db().execute("UPDATE mailboxes SET is_label = 1 WHERE id = ?1", [inbox]).unwrap();
        s.push_op(acc, r#"{"kind":"store","mailbox":"[Gmail]/All Mail","uids":[1],"flag":"\\Seen","add":true}"#, "", None).unwrap();
        assert!(s.convert_to_labels(acc).unwrap(), "goes on: only hidden copies are left");
        assert!(s.labels_model(acc).unwrap());
        let count = |sql: &str| s.read().query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count(&format!("SELECT COUNT(*) FROM messages WHERE mailbox_id = {inbox}")), 0);
        assert_eq!(count(&format!("SELECT COUNT(*) FROM labels WHERE mailbox_id = {inbox}")), 1200);
    }

    #[test]
    fn a_gmail_cache_converts_its_copies_to_labels() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let trash = s.upsert_mailbox(acc, "[Gmail]/Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let mairie = s.upsert_mailbox(acc, "Mairie", Some("/"), None, true).unwrap();
        let a = s.insert_header(acc, all, 1, &header("a@x", None, "Façade", 5), FLAGS, 10).unwrap();
        let in_inbox = s.insert_header(acc, inbox, 5, &header("a@x", None, "Façade", 5), FLAGS, 10).unwrap();
        s.insert_header(acc, mairie, 7, &header("a@x", None, "Façade", 5), FLAGS, 10).unwrap();
        // Only in the inbox: older than All Mail's window.
        s.insert_header(acc, inbox, 6, &header("old@x", None, "Ancien", 1), FLAGS, 10).unwrap();
        let binned = s.insert_header(acc, trash, 3, &header("t@x", None, "Jeté", 4), FLAGS, 10).unwrap();
        let raw = b"Subject: Facade\r\n\r\nBonjour";
        s.store_body(in_inbox, raw, &crate::mime::body_info(raw)).unwrap();

        // Not while the queue still names copies.
        let op = s.push_op(acc, r#"{"kind":"store","mailbox":"INBOX","uid":5,"flag":"\\Seen","value":true}"#, "lu", None).unwrap();
        assert!(!s.convert_to_labels(acc).unwrap());
        s.finish_op(op).unwrap();
        assert!(s.convert_to_labels(acc).unwrap());
        assert!(s.labels_model(acc).unwrap());

        let state = |id| s.mailbox_state(id).unwrap().is_label;
        assert_eq!((state(inbox), state(mairie), state(all), state(trash)), (true, true, false, false));
        let left: Vec<i64> = s.mailbox_message_ids(all).unwrap().into_iter().chain(s.mailbox_message_ids(trash).unwrap()).collect();
        assert_eq!(left, vec![a, binned]);
        assert_eq!(s.message_labels(&[a]).unwrap()[&a], vec![inbox, mairie]);
        assert_eq!(s.body(a).unwrap().as_deref(), Some(&raw[..]), "the inbox copy's body passed on");
        let listed = s.list_threads(&[inbox], &[], ThreadFilter::All, false, None, 50, 0).unwrap();
        assert_eq!(listed.iter().map(|t| (t.latest_id, t.mailbox_id)).collect::<Vec<_>>(), vec![(a, inbox)]);

        // The server's labels then replace them, except while a change is on its way.
        assert!(s.set_server_labels(all, &[(1, vec![mairie])], &HashSet::new(), s.flags_tick()).unwrap());
        assert!(!s.set_server_labels(all, &[(1, vec![inbox])], &HashSet::from([1]), s.flags_tick()).unwrap());
        assert_eq!(s.message_labels(&[a]).unwrap()[&a], vec![mairie]);
    }

    #[test]
    fn a_folder_left_as_a_bare_name_on_the_server_loses_its_messages() {
        let (s, acc, ..) = setup();
        let clients = s.upsert_mailbox(acc, "Clients", Some("/"), None, true).unwrap();
        s.insert_header(acc, clients, 1, &header("c@x", None, "Devis", 5), FLAGS, 10).unwrap();
        // Deleted elsewhere while « Clients/2024 » remains: listed \Noselect.
        assert_eq!(s.upsert_mailbox(acc, "Clients", Some("/"), None, false).unwrap(), clients);
        assert!(s.mailbox_message_ids(clients).unwrap().is_empty());
        assert_eq!(s.mailbox_state(clients).unwrap().uidvalidity, None);
    }

    #[test]
    fn a_failed_recount_keeps_the_account_for_a_retry() {
        let (s, acc, inbox, _) = setup();
        s.insert_header(acc, inbox, 1, &header("m@x", None, "Devis", 5), FLAGS, 10).unwrap();
        s.db().execute_batch("CREATE TRIGGER block BEFORE DELETE ON contacts BEGIN SELECT RAISE(ABORT, 'x'); END;").unwrap();
        assert!(s.delete_account(acc).is_err());
        assert!(s.list_accounts().unwrap().iter().any(|a| a.id == acc), "still there: removed again later");
        s.db().execute_batch("DROP TRIGGER block;").unwrap();
        s.delete_account(acc).unwrap();
        assert!(s.search_contacts("example.org", &[], 10).unwrap().is_empty(), "the people of its first steps forgotten too");
        assert_eq!(s.setting(&format!("removing_contacts:{acc}")).unwrap(), None);
    }

    #[test]
    fn a_gmail_account_without_all_mail_goes_back_to_folders() {
        let (s, acc, inbox, sent) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let mairie = s.upsert_mailbox(acc, "Mairie", Some("/"), None, true).unwrap();
        s.insert_header(acc, all, 1, &header("a@x", None, "Façade", 5), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 5, &header("a@x", None, "Façade", 5), FLAGS, 10).unwrap();
        let mut state = s.mailbox_state(inbox).unwrap();
        (state.uidvalidity, state.uidnext) = (Some(1), Some(6));
        s.save_mailbox_state(&state).unwrap();
        assert!(s.convert_to_labels(acc).unwrap());
        assert!(!s.revert_labels_model(acc).unwrap(), "All Mail still there");

        // All Mail hidden from IMAP (a Gmail setting): the folders hold the mail again.
        s.retain_mailboxes(acc, &[inbox, sent, mairie]).unwrap();
        assert!(s.revert_labels_model(acc).unwrap());
        assert!(!s.labels_model(acc).unwrap());
        let state = s.mailbox_state(inbox).unwrap();
        assert!(!state.is_label && !s.mailbox_state(mairie).unwrap().is_label);
        assert_eq!((state.uidvalidity, state.uidnext), (None, None), "read again from the start");
        assert!(!s.revert_labels_model(acc).unwrap());
    }

    #[test]
    fn a_gmail_sent_label_makes_correspondents() {
        let (s, acc, _, sent) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        s.set_label_folder(sent, true).unwrap();
        let to = vec![Address { name: Some("Fournisseur".into()), email: "supplier@ex.org".into() }];
        let mine = HeaderInfo { from: Some(Address { name: None, email: "y@example.org".into() }), to, ..header("s@x", None, "Commande", 5) };
        s.insert_header(acc, all, 7, &mine, FLAGS, 10).unwrap();
        let written = || s.read().query_row("SELECT written FROM contacts WHERE email = 'supplier@ex.org'", [], |r| r.get::<_, i64>(0)).optional().unwrap();
        assert_eq!(written(), None);
        assert!(s.set_server_labels(all, &[(7, vec![sent])], &HashSet::new(), s.flags_tick()).unwrap());
        assert!(s.correspondents(acc).unwrap().contains("supplier@ex.org"));
        assert_eq!(written(), Some(1));
        // The same labels read again count nothing twice.
        s.set_server_labels(all, &[(7, vec![sent])], &HashSet::new(), s.flags_tick()).unwrap();
        assert_eq!(written(), Some(1));

        // Removing the account takes them away.
        s.delete_account(acc).unwrap();
        assert_eq!(written(), None);
    }

    #[test]
    fn a_gmail_label_shows_and_counts_as_the_users_own() {
        let (s, acc, _, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let mairie = s.upsert_mailbox(acc, "Mairie", Some("/"), None, true).unwrap();
        s.set_label_folder(mairie, true).unwrap();
        let in_all = s.insert_header(acc, all, 3, &header("seminaire@x", None, "Réunion", 5), FLAGS, 10).unwrap();
        s.relabel(&[(in_all, vec![mairie])]).unwrap();
        let row = &s.list_threads(&[all], &[], ThreadFilter::All, false, None, 50, 0).unwrap()[0];
        assert_eq!(row.labels, [mairie], "All Mail shows the label");

        // Labelled Mairie: the user's own; a message only in All Mail is not.
        s.insert_header(acc, all, 4, &header("seul@x", None, "Réunion seule", 6), FLAGS, 10).unwrap();
        let subjects = |q: &str| s.search(acc, q, 50, false).unwrap().into_iter().map(|m| m.subject).collect::<Vec<_>>();
        assert_eq!(subjects("Réunion has:userlabels"), ["Réunion"]);
        assert_eq!(subjects("Réunion sans-étiquette"), ["Réunion seule"]);
        assert_eq!(s.labelled(&[in_all]).unwrap(), HashSet::from([in_all]));
    }

    #[test]
    fn folder_messages_from_one_sender() {
        let (s, acc, inbox, sent) = setup();
        let trash = s.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let other = |mid: &str, date| HeaderInfo { from: Some(Address { name: None, email: "x@example.org".into() }), ..header(mid, None, "x", date) };
        let old = s.insert_header(acc, trash, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        let new = s.insert_header(acc, trash, 2, &header("b@x", None, "b", 3), FLAGS, 10).unwrap();
        s.insert_header(acc, trash, 3, &other("c@x", 2), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 4, &header("d@x", None, "d", 4), FLAGS, 10).unwrap();
        let ids = |list: Vec<MessageSummary>| list.into_iter().map(|m| m.id).collect::<Vec<_>>();
        // Only that sender, only these folders, newest first, the address in any case.
        assert_eq!(ids(s.folder_messages_from(&[trash], "F@Example.org", 10).unwrap()), vec![new, old]);
        assert_eq!(ids(s.folder_messages_from(&[trash], "f@example.org", 1).unwrap()), vec![new]);
        assert!(s.folder_messages_from(&[sent], "f@example.org", 10).unwrap().is_empty());
    }

    #[test]
    fn pinned_messages_counted_everywhere_but_bins() {
        let (s, acc, inbox, _) = setup();
        let archive = s.upsert_mailbox(acc, "INBOX.Archive", Some("."), Some(Role::Archive), true).unwrap();
        let pinned = ServerFlags { flagged: true, ..FLAGS };
        s.insert_header(acc, archive, 1, &header("a@x", None, "a", 1), pinned, 10).unwrap();
        s.insert_header(acc, archive, 2, &header("b@x", None, "b", 2), pinned, 10).unwrap();
        s.insert_header(acc, inbox, 3, &header("c@x", None, "c", 3), FLAGS, 10).unwrap();
        let (count, list) = s.flagged_messages(6).unwrap();
        assert_eq!((count, list.len()), (2, 2));
        let (count, list) = s.flagged_messages(1).unwrap();
        assert_eq!((count, list.len()), (2, 1));
    }

    #[test]
    fn accounts_keep_the_order_chosen() {
        let (s, first, _, _) = setup();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let add = |email: &str| {
            s.insert_account(&NewAccount {
                auth: NewAuth::default(),
                name: "Y".into(), email: email.into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv.clone(),
            })
            .unwrap()
        };
        let second = add("b@example.org");
        let third = add("c@example.org");
        s.reorder_accounts(&[third, first]).unwrap();
        let order: Vec<i64> = s.list_accounts().unwrap().into_iter().map(|a| a.id).collect();
        assert_eq!(order, vec![third, first, second]);
    }

    #[test]
    fn screener_holds_unknown_senders_only() {
        let (s, acc, inbox, sent) = setup();
        let archive = s.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let held = s.upsert_mailbox(acc, "New senders", Some("/"), Some(Role::Screener), true).unwrap();
        let refused = s.upsert_mailbox(acc, "Screened out", Some("/"), Some(Role::Refused), true).unwrap();
        let from = |email: &str, mid: &str| HeaderInfo { from: Some(Address { name: None, email: email.into() }), ..header(mid, None, mid, 100) };
        let mut uid = 0;
        let mut put = |mailbox, email: &str, mid: &str| {
            uid += 1;
            s.insert_header(acc, mailbox, uid, &from(email, mid), FLAGS, 10).unwrap()
        };
        put(archive, "Known@Example.org", "old@x");
        put(refused, "bad@example.org", "bad-old@x");
        // A new sender's first message waits in "New senders"; its All Mail copy does not make
        // the sender known, nor does it make the next one known.
        put(held, "waiting@example.org", "w1@x");
        put(all, "waiting@example.org", "w1@x");
        let known = put(inbox, "known@example.org", "k@x");
        let fresh = put(inbox, "new@example.org", "n@x");
        put(all, "new@example.org", "n@x");
        let again = put(inbox, "waiting@example.org", "w2@x");
        let bad = put(inbox, "bad@example.org", "b@x");
        let spam = put(inbox, "promo@spam.example", "p@x");
        // Mail from oneself passes once proven: authenticated by the account's usual server, or
        // with a copy in Sent. A pass verdict while that server is unknown proves nothing (it may
        // be the sender's own header).
        let mine = {
            let mut h = from("y@example.org", "y@x");
            h.signals.dmarc = Some(true);
            s.insert_header(acc, inbox, 1000, &h, FLAGS, 10).unwrap()
        };
        let noted = {
            let h = from("y@example.org", "note@x");
            s.insert_header(acc, sent, 1001, &h, FLAGS, 10).unwrap();
            s.insert_header(acc, inbox, 1001, &h, FLAGS, 10).unwrap()
        };
        let accepted = put(inbox, "friend@example.org", "f@x");
        s.set_screening(acc, "@spam.example", Some("block")).unwrap();
        s.set_screening(acc, "friend@example.org", Some("accept")).unwrap();
        let own: HashSet<String> = ["y@example.org".to_string()].into();
        let ids = [known, fresh, again, bad, spam, mine, noted, accepted];
        let mut verdicts = s.screen_verdicts(acc, &ids, 0, &own).unwrap();
        verdicts.sort_by_key(|(id, _)| *id);
        assert_eq!(verdicts, vec![(fresh, Verdict::Hold), (again, Verdict::Hold), (bad, Verdict::Refuse), (spam, Verdict::Refuse), (mine, Verdict::Hold)]);
        // Once the usual server is learned, its pass verdict proves the message; another
        // server's does not.
        for n in 0..5 {
            let mut h = from(&format!("list@other{n}.example"), &format!("l{n}@x"));
            h.signals.authserv = Some("mx.example.org".into());
            s.insert_header(acc, archive, 2000 + n, &h, FLAGS, 10).unwrap();
        }
        let signed = |mid: &str, server: &str, uid| {
            let mut h = from("y@example.org", mid);
            h.signals.dmarc = Some(true);
            h.signals.authserv = Some(server.into());
            s.insert_header(acc, inbox, uid, &h, FLAGS, 10).unwrap()
        };
        let genuine = signed("g@x", "mx.example.org", 3000);
        let forged = signed("f@x", "forged.example", 3001);
        assert_eq!(s.screen_verdicts(acc, &[genuine, forged], 0, &own).unwrap(), vec![(forged, Verdict::Hold)]);
        // Messages dated before the screener was turned on are left alone.
        assert!(s.screen_verdicts(acc, &ids, 200, &own).unwrap().is_empty());
        assert_eq!(s.messages_from_pattern(acc, "@example.org", Role::Screener).unwrap().len(), 1);
        let senders = s.held_senders(Some(acc)).unwrap();
        assert_eq!((senders[0].1.as_str(), senders[0].3), ("waiting@example.org", 1));
    }

    #[test]
    fn screener_is_not_fooled_by_a_borrowed_address() {
        let (s, acc, inbox, sent) = setup();
        let archive = s.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let with = |email: Option<&str>, mid: &str, edit: &dyn Fn(&mut HeaderInfo)| {
            let mut h = HeaderInfo { from: email.map(|e| Address { name: None, email: e.into() }), ..header(mid, None, mid, 100) };
            edit(&mut h);
            h
        };
        let mut uid = 10;
        let mut put = |mailbox, h: &HeaderInfo| {
            uid += 1;
            s.insert_header(acc, mailbox, uid, h, FLAGS, 10).unwrap()
        };
        // A correspondent the user knows, and one they accepted.
        put(archive, &with(Some("bank@example.org"), "old@x", &|_| {}));
        s.set_screening(acc, "friend@example.org", Some("accept")).unwrap();
        // The user's own message, kept in Sent.
        put(sent, &with(Some("y@example.org"), "mine@x", &|h| h.subject = "Devis".into()));

        let genuine = put(inbox, &with(Some("bank@example.org"), "g@x", &|_| {}));
        let failed = put(inbox, &with(Some("bank@example.org"), "f@x", &|h| h.signals.dmarc = Some(false)));
        let masked = put(inbox, &with(Some("friend@example.org"), "m@x", &|h| h.signals.spoofed_name = true));
        let nobody = put(inbox, &with(None, "n@x", &|_| {}));
        // Someone who received the user's message sends one "from" them, under its Message-ID.
        // (sent again from elsewhere, it never has the same size: other Received lines)
        let forged = s.insert_header(acc, inbox, 99, &with(Some("y@example.org"), "mine@x", &|h| h.subject = "Facture urgente".into()), FLAGS, 2400).unwrap();
        let own: HashSet<String> = ["y@example.org".to_string()].into();
        let mut verdicts = s.screen_verdicts(acc, &[genuine, failed, masked, nobody, forged], 0, &own).unwrap();
        verdicts.sort_by_key(|(id, _)| *id);
        assert_eq!(verdicts, vec![(failed, Verdict::Hold), (masked, Verdict::Hold), (nobody, Verdict::Hold), (forged, Verdict::Hold)]);
    }

    #[test]
    fn screener_accepts_what_comes_back_and_replies_to_sent_mail() {
        let (s, acc, inbox, sent) = setup();
        let held = s.upsert_mailbox(acc, "New senders", Some("/"), Some(Role::Screener), true).unwrap();
        let mut uid = 100;
        let mut put = |mailbox, email: &str, mid: &str, received: i64, irt: Option<&str>| {
            uid += 1;
            let h = HeaderInfo { from: Some(Address { name: None, email: email.into() }), received_at: Some(received), ..header(mid, irt, mid, 50) };
            s.insert_header(acc, mailbox, uid, &h, FLAGS, 10).unwrap()
        };
        let own: HashSet<String> = ["y@example.org".to_string()].into();
        let verdict = |id: i64| s.screen_verdicts(acc, &[id], 0, &own).unwrap().into_iter().map(|(_, v)| v).next();

        // Held here, moved back to the inbox by another client before "New senders" was synced
        // again: the same message (same arrival date) accepts its sender.
        put(held, "stranger@example.net", "s1@x", 100, None);
        let back = put(inbox, "stranger@example.net", "s1@x", 100, None);
        assert_eq!(verdict(back), Some(Verdict::Accept("stranger@example.net".into())));
        // The sender sending it again under the same Message-ID is a new delivery: held.
        let resent = put(inbox, "stranger@example.net", "s1@x", 300, None);
        assert_eq!(verdict(resent), Some(Verdict::Hold));

        // Remembered after the folder's copy is gone, unless a copy is still on its way there.
        let first = put(inbox, "other@example.net", "o1@x", 100, None);
        s.move_local(first, held).unwrap();
        s.remember_screened(&[first]).unwrap();
        let returned = put(inbox, "other@example.net", "o1@x", 100, None);
        assert_eq!(verdict(returned), Some(Verdict::Hold), "this device's own move, still in flight");
        s.delete_message(first).unwrap();
        assert_eq!(verdict(returned), Some(Verdict::Accept("other@example.net".into())));
        s.forget_screened(&[returned]).unwrap();
        assert_eq!(verdict(returned), Some(Verdict::Hold));

        // Sent from facteur: the recipients and the replies to it are known at once, before
        // Sent is synced; a reply to a message in Sent too.
        s.record_sent(acc, "mine-1@example.org", &["Bob@Example.net".into()]).unwrap();
        let bob = put(inbox, "bob@example.net", "b1@x", 100, None);
        let colleague = put(inbox, "carol@example.net", "c1@x", 100, Some("mine-1@example.org"));
        put(sent, "y@example.org", "mine-2@example.org", 100, None);
        let later = put(inbox, "dave@example.net", "d1@x", 100, Some("mine-2@example.org"));
        let unrelated = put(inbox, "erin@example.net", "e1@x", 100, Some("elsewhere@x"));
        assert_eq!((verdict(bob), verdict(colleague), verdict(later)), (None, None, None));
        assert_eq!(verdict(unrelated), Some(Verdict::Hold));
    }

    #[test]
    fn screener_dates_own_addresses_and_domain_blocks() {
        let (s, acc, inbox, sent) = setup();
        let mut uid = 0;
        let mut put = |mailbox, email: &str, mid: &str, date: i64, received: Option<i64>| {
            uid += 1;
            let h = HeaderInfo { from: Some(Address { name: None, email: email.into() }), received_at: received, ..header(mid, None, mid, date) };
            s.insert_header(acc, mailbox, uid, &h, FLAGS, 10).unwrap()
        };
        let own: HashSet<String> = ["Y@example.org".to_string()].into();
        let verdicts = |ids: &[i64], since| {
            let mut v = s.screen_verdicts(acc, ids, since, &own).unwrap();
            v.sort_by_key(|(id, _)| *id);
            v
        };
        // The server's arrival date counts, not the Date header the sender wrote.
        let backdated = put(inbox, "a@example.net", "a@x", 50, Some(500));
        let postdated = put(inbox, "b@example.net", "b@x", 900, Some(100));
        assert_eq!(verdicts(&[backdated, postdated], 200), vec![(backdated, Verdict::Hold)]);

        // From one's own address (+tag too): unproven, it is held; with a copy in Sent, it passes.
        let spoofed = put(inbox, "y+tag@example.org", "sp@x", 100, None);
        let to_self = put(inbox, "y@example.org", "self@x", 100, None);
        put(sent, "y@example.org", "self@x", 100, None);
        assert_eq!(verdicts(&[spoofed, to_self], 0), vec![(spoofed, Verdict::Hold)]);

        // A domain block spares the people written to and addresses with their own decision.
        s.set_screening(acc, "@corp.example", Some("block")).unwrap();
        s.set_screening(acc, "carol@corp.example", Some("accept")).unwrap();
        s.record_sent(acc, "m@example.org", &["alice@corp.example".into()]).unwrap();
        let alice = put(inbox, "alice@corp.example", "al@x", 100, None);
        let bob = put(inbox, "bob@corp.example", "bo@x", 100, None);
        let carol = put(inbox, "carol@corp.example", "ca@x", 100, None);
        assert_eq!(verdicts(&[alice, bob, carol], 0), vec![(bob, Verdict::Refuse)]);
        assert!(s.spared_by_domain(acc, "alice@corp.example", true).unwrap());
        assert!(!s.spared_by_domain(acc, "alice@corp.example", false).unwrap());
        assert!(s.spared_by_domain(acc, "carol@corp.example", false).unwrap());
        assert!(!s.spared_by_domain(acc, "bob@corp.example", true).unwrap());
        assert_eq!(base_address("Yann+News@Example.org"), "yann@example.org");
        assert_eq!(base_address("+x@example.org"), "+x@example.org");
    }

    #[test]
    fn sender_category_is_put_back() {
        let (s, acc, inbox, _) = setup();
        let h = HeaderInfo { from: Some(Address { name: None, email: "news@shop.example".into() }), ..header("n@x", None, "n", 1) };
        let id = s.insert_header(acc, inbox, 1, &h, FLAGS, 10).unwrap();
        let before = s.message_category(id).unwrap();
        let snapshot = s.sender_category_snapshot("News@Shop.example").unwrap();
        s.set_sender_category("news@shop.example", Category::Notification).unwrap();
        assert_eq!(s.message_category(id).unwrap(), Category::Notification);
        s.restore_sender_category(&snapshot).unwrap();
        assert_eq!(s.sender_category("news@shop.example").unwrap(), None);
        assert_eq!(s.message_category(id).unwrap(), before);
    }

    #[test]
    fn unpinned_message_leaves_the_pinned_folder() {
        let (s, acc, inbox, _) = setup();
        s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let starred = s.upsert_mailbox(acc, "[Gmail]/Starred", Some("/"), Some(Role::Flagged), true).unwrap();
        let pinned = ServerFlags { flagged: true, ..FLAGS };
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), pinned, 10).unwrap();
        let copy = s.insert_header(acc, starred, 5, &header("a@x", None, "a", 1), pinned, 10).unwrap();
        let other = s.insert_header(acc, starred, 6, &header("b@x", None, "b", 2), pinned, 10).unwrap();
        let mut state = s.mailbox_state(starred).unwrap();
        state.server_count = Some(2);
        s.save_mailbox_state(&state).unwrap();
        s.set_count_base(starred, 0, false).unwrap();
        // Unpinned (the copy too, as the server does): the Pinned folder's copy goes.
        s.set_flags_local(&[id, copy], FlagKind::Flagged, false).unwrap();
        assert_eq!(s.drop_unpinned(acc).unwrap(), 1);
        assert!(s.summary(copy).is_err());
        assert!(s.summary(other).is_ok());
        assert!(!s.summary(id).unwrap().flags.flagged);
        let shown = |id| s.list_mailboxes(acc).unwrap().into_iter().find(|m| m.id == id).unwrap().server_total;
        assert_eq!(shown(starred), Some(1));
        // Moved into another folder: its count follows before any sync.
        let banque = s.upsert_mailbox(acc, "Banque", Some("/"), None, true).unwrap();
        let mut state = s.mailbox_state(banque).unwrap();
        state.server_count = Some(0);
        s.save_mailbox_state(&state).unwrap();
        s.set_count_base(banque, 0, false).unwrap();
        s.move_local(id, banque).unwrap();
        assert_eq!(shown(banque), Some(1));
    }

    #[test]
    fn spam_senders_are_never_suggested() {
        let (s, acc, inbox, _) = setup();
        let junk = s.upsert_mailbox(acc, "Junk", Some("/"), Some(Role::Junk), true).unwrap();
        let from = |email: &str, id: &str| HeaderInfo { from: Some(Address { name: Some("Banque".into()), email: email.into() }), ..header(id, None, "Alerte", 1) };
        for i in 0..5 {
            s.insert_header(acc, junk, i + 1, &from("secure@evil.example", &format!("{i}@x")), FLAGS, 10).unwrap();
        }
        s.insert_header(acc, inbox, 1, &from("conseiller@banque.example", "ok@x"), FLAGS, 10).unwrap();
        let found: Vec<String> = s.search_contacts("banque", &[], 10).unwrap().into_iter().map(|a| a.email).collect();
        assert_eq!(found, ["conseiller@banque.example"]);
    }

    #[test]
    fn a_book_that_failed_keeps_its_contacts() {
        use crate::contacts::Contact;
        let (s, acc, _, _) = setup();
        let card = |id: &str, email: &str| Contact { remote_id: id.into(), name: Some("X".into()), emails: vec![email.into()] };
        // Stored under their absolute URL, as `caldav::parse_cards` gives them.
        let (work, home) = ("https://dav.example.org/ab/work/", "https://dav.example.org/ab/home/");
        let other = card("https://dav.example.org/ab/work2/c9.vcf", "x@ex.org");
        s.replace_address_book(acc, "y@example.org", &[card(&format!("{work}c1.vcf"), "w@ex.org"), card(&format!("{home}c2.vcf"), "h@ex.org"), other]).unwrap();
        // The work book failed this time: its contacts go on with the home book's new ones.
        let mut now = vec![card(&format!("{home}c3.vcf"), "n@ex.org")];
        now.extend(s.address_book_under(acc, work).unwrap());
        s.replace_address_book(acc, "y@example.org", &now).unwrap();
        let mut left: Vec<String> = s.search_contacts("ex.org", &[], 10).unwrap().into_iter().map(|a| a.email).collect();
        left.sort();
        assert_eq!(left, ["n@ex.org", "w@ex.org"]);
    }

    #[test]
    fn a_one_letter_last_piece_is_never_a_prefix() {
        assert_eq!(fts_word("facture"), "\"facture\"*");
        assert_eq!(fts_word("paul@g"), "\"paul@g\"", "the star would apply to `g` alone");
        assert_eq!(fts_word("rendez-v"), "\"rendez-v\"");
        assert_eq!(fts_word("paul@gm"), "\"paul@gm\"*");
        assert_eq!(fts_word("x"), "\"x\"");
        assert_eq!(fts_word("a\"b"), "\"a\"\"b\"", "no FTS5 syntax through quotes");
    }

    #[test]
    fn a_flag_changed_while_the_server_flags_are_read_is_not_put_back() {
        let (s, acc, inbox, _) = setup();
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        // The sync asks for the flags; meanwhile the message is pinned here, and its change
        // already left the queue.
        let since = s.flags_tick();
        s.set_flags_local(&[id], FlagKind::Flagged, true).unwrap();
        s.update_flags(inbox, &[(1, ServerFlags { flagged: false, ..FLAGS })], &HashSet::new(), since).unwrap();
        assert!(s.summary(id).unwrap().flags.flagged, "the older answer does not unpin it");
        // The next read, asked after the change, is the server's word.
        s.update_flags(inbox, &[(1, ServerFlags { flagged: false, ..FLAGS })], &HashSet::new(), s.flags_tick()).unwrap();
        assert!(!s.summary(id).unwrap().flags.flagged);
    }

    #[test]
    fn mark_all_read_during_a_flag_read_is_not_undone() {
        let (s, acc, inbox, _) = setup();
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        let since = s.flags_tick();
        s.mark_account_seen(acc, false).unwrap();
        s.update_flags(inbox, &[(1, FLAGS)], &HashSet::new(), since).unwrap();
        assert!(s.summary(id).unwrap().flags.seen, "the older answer does not make it unread again");
    }

    #[test]
    fn flag_changed_during_a_move_reaches_the_server() {
        let (s, acc, inbox, _) = setup();
        let archive = s.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.move_local(id, archive).unwrap();
        // Opened (so read) during the move: no UID, nothing to send yet.
        s.set_flags_local(&[id], FlagKind::Seen, true).unwrap();
        assert!(s.pending_ops(acc).unwrap().is_empty());
        // Arrival confirmed by the sync: the server says unread, the local choice stays.
        let adopted = s.insert_header(acc, archive, 7, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        assert_eq!(adopted, id);
        assert!(s.summary(id).unwrap().flags.seen);
        let ops = s.pending_ops(acc).unwrap();
        assert_eq!(ops.len(), 1);
        let op: serde_json::Value = serde_json::from_str(&s.op_payload(ops[0].id).unwrap().unwrap()).unwrap();
        assert_eq!((op["kind"].as_str(), op["mailbox"].as_str(), op["uid"].as_u64(), op["value"].as_bool()), (Some("store"), Some("Archive"), Some(7), Some(true)));
    }

    #[test]
    fn sync_does_not_overwrite_flags_waiting_to_be_sent() {
        let (s, acc, inbox, _) = setup();
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), ServerFlags { seen: true, ..FLAGS }, 10).unwrap();
        s.set_flag(id, FlagKind::Seen, false).unwrap();
        // The server does not know yet: it says read.
        let keep: HashSet<u32> = [1].into();
        s.update_flags(inbox, &[(1, ServerFlags { seen: true, ..FLAGS })], &keep, s.flags_tick()).unwrap();
        assert!(!s.summary(id).unwrap().flags.seen, "the local change stays");
        s.update_flags(inbox, &[(1, ServerFlags { seen: true, ..FLAGS })], &HashSet::new(), s.flags_tick()).unwrap();
        assert!(s.summary(id).unwrap().flags.seen);
    }

    #[test]
    fn thread_numbers_are_never_reused() {
        let (s, acc, inbox, _) = setup();
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        let reply = s.insert_header(acc, inbox, 2, &header("r@x", Some("a@x"), "Re: a", 2), FLAGS, 10).unwrap();
        let thread = s.summary(a).unwrap().thread_id;
        // The last one inserted goes: SQLite hands its id to the next one.
        s.delete_message(reply).unwrap();
        s.delete_message(a).unwrap();
        let b = s.insert_header(acc, inbox, 3, &header("b@x", None, "b", 3), FLAGS, 10).unwrap();
        assert_ne!(s.summary(b).unwrap().thread_id, thread, "a new thread, not that of a vanished message");
    }

    #[test]
    fn huge_references_header_does_not_block_the_sync() {
        let (s, acc, inbox, _) = setup();
        let mut h = header("big@x", None, "piège", 1);
        h.references = (0..40_000).map(|i| format!("r{i}@x")).collect();
        let id = s.insert_header(acc, inbox, 1, &h, FLAGS, 10).unwrap();
        assert_eq!(s.summary(id).unwrap().subject, "piège");
    }

    #[test]
    fn placeholders_wait_for_moves_seen_before_the_sync() {
        let (s, acc, inbox, _) = setup();
        let archive = s.upsert_mailbox(acc, "Archive", Some("/"), Some(Role::Archive), true).unwrap();
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.move_local(id, archive).unwrap();
        // Move completed during the sync, after it read the folder: wait.
        s.drop_orphan_placeholders(archive, true).unwrap();
        assert!(s.summary(id).is_ok());
        // No move on its way before or after: the server never got it.
        s.drop_orphan_placeholders(archive, false).unwrap();
        assert!(s.summary(id).is_err());
        // A move back to the inbox that never arrived: no thread left the inbox by it.
        let back = s.insert_header(acc, archive, 2, &header("b@x", None, "b", 1), FLAGS, 10).unwrap();
        s.move_local(back, inbox).unwrap();
        let thread = s.summary(back).unwrap().thread_id;
        s.drop_orphan_placeholders(inbox, false).unwrap();
        let handled: i64 = s.read().query_row("SELECT COUNT(*) FROM handled WHERE thread_id = ?1", [thread], |r| r.get(0)).unwrap();
        assert_eq!(handled, 0);
    }

    #[test]
    fn queued_draft_saves_keep_their_old_copy_out() {
        let (s, acc, _, _) = setup();
        s.push_op(acc, r#"{"kind":"save_draft","mailbox":"Drafts","message_id":"a@x","raw":"","source":[7,null]}"#, "", None).unwrap();
        assert_eq!(s.in_flight_uids(acc, "Drafts").unwrap(), HashSet::from([7]), "not fetched back while the save waits");
        assert!(s.failed_op_uids(acc, "Drafts").unwrap().is_empty());
    }

    #[test]
    fn emptying_the_trash_turns_moves_into_deletions() {
        let (s, acc, _, _) = setup();
        s.push_op(acc, r#"{"kind":"move_set","mailbox":"INBOX","uids":[4,5],"dest":"Trash","uidvalidity":9}"#, "", Some(now() + 60)).unwrap();
        s.push_op(acc, r#"{"kind":"move_set","mailbox":"INBOX","uids":[6],"dest":"Archive"}"#, "", None).unwrap();
        assert_eq!(s.moves_into_become_deletes(acc, "Trash").unwrap(), 1);
        let ops = s.pending_ops(acc).unwrap();
        assert_eq!((ops[0].kind.as_str(), ops[0].not_before), ("delete_set", None), "to replay at once");
        let op: serde_json::Value = serde_json::from_str(&s.op_payload(ops[0].id).unwrap().unwrap()).unwrap();
        assert_eq!(op["uids"], serde_json::json!([4, 5]));
        assert_eq!(ops[1].kind, "move_set", "the other moves are unchanged");
        assert_eq!(s.in_flight_uids(acc, "INBOX").unwrap(), [4, 5, 6].into());
    }

    #[test]
    fn queued_draft_deletions_keep_sent_drafts_out() {
        let (s, acc, _, _) = setup();
        s.push_op(acc, r#"{"kind":"save_draft","mailbox":"Drafts","message_id":"<a@x>","raw":""}"#, "", None).unwrap();
        s.push_op(acc, r#"{"kind":"delete_draft","mailbox":"Drafts","message_id":"<a@x>"}"#, "", None).unwrap();
        s.push_op(acc, r#"{"kind":"delete_draft","mailbox":"Drafts","message_id":"<b@x>"}"#, "", None).unwrap();
        // A cancelled send: saved again after its deletion, the draft is to come back.
        s.push_op(acc, r#"{"kind":"save_draft","mailbox":"Drafts","message_id":"<b@x>","raw":""}"#, "", None).unwrap();
        s.push_op(acc, r#"{"kind":"delete_draft","mailbox":"Other","message_id":"<c@x>"}"#, "", None).unwrap();
        assert_eq!(s.pending_draft_deletes(acc, "Drafts").unwrap(), ["<a@x>".to_string()].into());
    }

    #[test]
    fn copies_of_a_message_share_one_body() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        let b = s.insert_header(acc, all, 9, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        let info = crate::mime::body_info(b"Subject: a\r\n\r\nBonjour");
        s.store_body(a, b"Subject: a\r\n\r\nBonjour", &info).unwrap();
        assert_eq!(s.body(b).unwrap().as_deref(), Some(&b"Subject: a\r\n\r\nBonjour"[..]));
        assert!(s.missing_bodies(all, 10, u32::MAX, 0).unwrap().is_empty(), "nothing to download for the other copy");
        assert_eq!(s.coverage(acc).unwrap().bodies, 2);
    }

    #[test]
    fn similar_messages_can_bring_their_whole_conversations() {
        let (s, acc, inbox, sent) = setup();
        let from = |email: &str, mid: &str, reply_to: Option<&str>, date| HeaderInfo {
            from: Some(Address { name: None, email: email.into() }),
            ..header(mid, reply_to, "Façade", date)
        };
        let theirs = s.insert_header(acc, inbox, 1, &from("cedric@x", "c1@x", None, 1), FLAGS, 10).unwrap();
        let mine = s.insert_header(acc, sent, 1, &from("y@example.org", "y1@x", Some("c1@x"), 2), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 2, &from("autre@x", "z@x", None, 3), FLAGS, 10).unwrap();
        let by = |whole| {
            let mut ids = s.similar(&SimilarCriteria { from_address: Some("cedric@x".into()), whole_threads: whole, ..Default::default() }, 10).unwrap().ids;
            ids.sort();
            ids
        };
        assert_eq!(by(false), [theirs]);
        assert_eq!(by(true), [theirs, mine], "the reply comes with its conversation, nothing else");
    }

    #[test]
    fn similar_subject_ignores_case_accents_and_doubled_spaces() {
        let (s, acc, inbox, _) = setup();
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "Re: nettoyage façade  mairie", 1), FLAGS, 10).unwrap();
        let r = s.similar(&SimilarCriteria { subject: Some("Nettoyage facade mairie".into()), ..Default::default() }, 10).unwrap();
        assert_eq!(r.ids, [a]);
        assert_eq!(fold("a \t  b"), "a b");
    }

    #[test]
    fn readers_can_compute_account_stats() {
        // On disk, so reads go through the read-only connections.
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = s
            .insert_account(&NewAccount { auth: NewAuth::default(), name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(), imap: srv.clone(), smtp: srv })
            .unwrap();
        let inbox = s.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        assert_eq!(s.account_stats(acc, "y@example.org", 100).unwrap().total, 1);
        assert_eq!(s.account_stats(acc, "y@example.org", 100).unwrap().total, 1, "temporary table rebuilt");
    }

    #[test]
    fn the_french_no_identifier_marker_becomes_english() {
        let (s, acc, inbox, _) = setup();
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.db().execute("UPDATE messages SET signals = '{\"authserv\":\"(sans identifiant)\",\"dkim\":true}' WHERE id = ?1", [id]).unwrap();
        let migration = MIGRATIONS.iter().find(|m| m.contains("'(no identifier)'")).unwrap();
        s.db().execute_batch(migration).unwrap();
        let signals: String = s.db().query_row("SELECT signals FROM messages WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&signals).unwrap();
        assert_eq!((json["authserv"].as_str(), json["dkim"].as_bool()), (Some("(no identifier)"), Some(true)));
    }

    #[test]
    fn a_mostly_empty_file_is_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        s.db().execute_batch("CREATE TABLE junk(b BLOB); WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000) INSERT INTO junk SELECT zeroblob(4000) FROM n;").unwrap();
        assert_eq!(s.compact_above(0).unwrap(), None, "nothing free yet");
        s.db().execute_batch("DROP TABLE junk;").unwrap();
        let freed = s.compact_above(0).unwrap().expect("compacted");
        assert!(freed > 2000 * 4000, "the file itself is shorter: {freed}");
        assert_eq!(s.db().query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(s.db().query_row("PRAGMA temp_store", [], |r| r.get::<_, i64>(0)).unwrap(), 2, "back to memory");
        // The readers still see the rewritten file, and map it again.
        assert_eq!(s.read().query_row("SELECT count(*) FROM accounts", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(s.readers.iter().all(|r| r.lock().unwrap().query_row("PRAGMA mmap_size", [], |r| r.get::<_, i64>(0)).unwrap() == READER_MMAP));
        assert_eq!(s.compact_above(0).unwrap(), None);

        // A reader busy: no compaction now (Windows would not shorten a file it maps), nor a wait.
        s.db().execute_batch("CREATE TABLE junk(b BLOB); WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000) INSERT INTO junk SELECT zeroblob(4000) FROM n; DROP TABLE junk;").unwrap();
        if let Some(reader) = s.readers.first() {
            let _busy = reader.lock().unwrap();
            assert_eq!(s.compact_above(0).unwrap(), None);
        }
        assert!(s.compact_above(0).unwrap().is_some());
    }

    #[test]
    fn threads_across_mailboxes_and_out_of_order() {
        let (s, acc, inbox, sent) = setup();
        // The reply arrives before the original message.
        let reply = s.insert_header(acc, inbox, 2, &header("b@x", Some("a@x"), "Re: Réunion", 200), FLAGS, 10).unwrap();
        let root = s.insert_header(acc, inbox, 1, &header("a@x", None, "Réunion", 100), FLAGS, 10).unwrap();
        let mine = s.insert_header(acc, sent, 1, &header("c@x", Some("b@x"), "Re: Réunion", 300), FLAGS, 10).unwrap();

        let t = s.summary(root).unwrap().thread_id;
        assert_eq!(s.summary(reply).unwrap().thread_id, t);
        assert_eq!(s.summary(mine).unwrap().thread_id, t);

        let threads = s.list_threads(&[inbox], &[], ThreadFilter::All, false, None, 50, 0).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].count, 2);
        assert_eq!(threads[0].latest_id, reply);
        assert_eq!(threads[0].subject, "Re: Réunion", "subject of the latest message");
        assert_eq!(threads[0].message_ids, [root, reply]);
        assert_eq!(threads[0].participants.len(), 1, "same sender counted once");
        assert!(s.list_threads(&[inbox], &[], ThreadFilter::Unread, false, None, 50, 0).unwrap().len() == 1);
        assert!(s.list_threads(&[inbox], &[], ThreadFilter::Flagged, false, None, 50, 0).unwrap().is_empty());
        assert_eq!(s.thread_messages(acc, t, None).unwrap().len(), 3);
    }

    #[test]
    fn pinned_first_and_woken_threads_on_top() {
        let (s, acc, inbox, _) = setup();
        let old = s.insert_header(acc, inbox, 1, &header("a@x", None, "Épinglé", 100), ServerFlags { flagged: true, ..FLAGS }, 10).unwrap();
        s.insert_header(acc, inbox, 2, &header("b@x", None, "Revenu", 200), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 3, &header("c@x", None, "Récent", 300), FLAGS, 10).unwrap();
        let subjects = |pinned| {
            s.list_threads(&[inbox], &[], ThreadFilter::All, pinned, None, 50, 0).unwrap().into_iter().map(|t| t.subject).collect::<Vec<_>>()
        };
        assert_eq!(subjects(false), ["Récent", "Revenu", "Épinglé"]);
        assert_eq!(subjects(true), ["Épinglé", "Récent", "Revenu"]);

        let back = s.find_by_message_id(acc, "b@x").unwrap().unwrap();
        let thread = s.summary(back).unwrap().thread_id;
        s.snooze(acc, thread, &["b@x".into()], 1_000).unwrap();
        let listed = s.list_threads(&[inbox], &[], ThreadFilter::All, false, None, 50, 0).unwrap();
        assert_eq!(listed[1].snoozed_until, Some(1_000));
        s.woke(acc, thread, 400).unwrap();
        let listed = s.list_threads(&[inbox], &[], ThreadFilter::All, true, None, 50, 0).unwrap();
        assert_eq!(listed.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(), ["Épinglé", "Revenu", "Récent"]);
        assert!(listed[1].woken && listed[1].activity == 400 && listed[1].snoozed_until.is_none());
        assert_eq!(s.summary(old).unwrap().flags.flagged, true);
    }

    #[test]
    fn readers_see_committed_writes_and_contacts_follow() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("t.db")).unwrap();
        assert_eq!(s.readers.len(), READERS);
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let acc = s
            .insert_account(&NewAccount {
                auth: NewAuth::default(),
                name: "Y".into(), email: "y@example.org".into(), username: "y".into(), password: String::new(),
                imap: srv.clone(), smtp: srv,
            })
            .unwrap();
        let inbox = s.upsert_mailbox(acc, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let sent = s.upsert_mailbox(acc, "Sent", Some("/"), Some(Role::Sent), true).unwrap();
        // Written by the writer, read at once by a reader.
        assert_eq!(s.list_accounts().unwrap().len(), 1);
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "Salut", 100), FLAGS, 10).unwrap();
        assert_eq!(s.summary(id).unwrap().subject, "Salut");

        let mut mine = header("b@x", None, "Re", 200);
        mine.from = Some(Address { name: None, email: "y@example.org".into() });
        mine.to = vec![Address { name: Some("Marc Leroy".into()), email: "Marc@Ex.org".into() }];
        s.insert_header(acc, sent, 1, &mine, FLAGS, 10).unwrap();
        let found = s.search_contacts("leroy", &[], 5).unwrap();
        assert_eq!((found[0].email.as_str(), found[0].name.as_deref()), ("marc@ex.org", Some("Marc Leroy")));
        // Someone the user wrote to comes before someone who only wrote to them.
        let all = s.search_contacts("@", &["y@example.org".into()], 5).unwrap();
        assert_eq!(all.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["marc@ex.org", "f@example.org"]);
        // Read by a reader connection, which has `fold` too: accents and capitals ignored.
        for q in ["FRANÇ", "franc", "Franç"] {
            assert_eq!(s.search_contacts(q, &[], 5).unwrap().len(), 1, "{q}");
        }
    }

    #[test]
    fn sync_side_effects_are_safe() {
        let (s, acc, inbox, sent) = setup();
        let h = header("a@x", None, "Bonjour", 100);
        let (id, new) = s.insert_headers(acc, inbox, &[(5, h.clone(), FLAGS, 10)]).unwrap()[0];
        assert!(new);
        // Two syncs crossing: no uniqueness error, no duplicate "new".
        assert_eq!(s.insert_headers(acc, inbox, &[(5, h, FLAGS, 10)]).unwrap()[0], (id, false));

        // UIDs targeted by a queued operation: the sync must not bring them back.
        let op = serde_json::json!({ "kind": "move_set", "mailbox": "INBOX", "uids": [5, 6], "dest": "Archive" });
        s.push_op(acc, &op.to_string(), "", None).unwrap();
        s.push_op(acc, &serde_json::json!({ "kind": "delete_set", "mailbox": "Sent", "uids": [9] }).to_string(), "", None).unwrap();
        assert_eq!(s.in_flight_uids(acc, "INBOX").unwrap(), [5, 6].into_iter().collect());

        // Undo is all or nothing: one operation already gone, and nothing moves.
        s.move_local(id, sent).unwrap();
        let texts = [op.to_string(), serde_json::json!({ "kind": "delete_set", "mailbox": "Sent", "uids": [9] }).to_string()];
        let ops: Vec<(i64, String)> = s.pending_ops(acc).unwrap().iter().map(|p| p.id).zip(texts).collect();
        assert_eq!(s.undo(&[ops[0].clone(), (9999, String::new())], &[(id, inbox, Some(5))], &[], &[], false).unwrap(), Undone::TooLate);
        assert_eq!(s.location(id).unwrap().mailbox_id, sent);
        // Without the replay lock: only while all are still held (these never were).
        assert_eq!(s.undo(&ops, &[(id, inbox, Some(5))], &[], &[], true).unwrap(), Undone::Released);
        assert_eq!(s.location(id).unwrap().mailbox_id, sent);
        assert_eq!(s.pending_ops(acc).unwrap().len(), 2);
        assert_eq!(s.undo(&ops, &[(id, inbox, Some(5))], &[], &[], false).unwrap(), Undone::Done);
        assert_eq!((s.location(id).unwrap().mailbox_id, s.location(id).unwrap().uid), (inbox, Some(5)));
        assert!(s.pending_ops(acc).unwrap().is_empty());
        let text = serde_json::json!({ "kind": "delete_set", "mailbox": "Sent", "uids": [9] }).to_string();
        let held = s.push_op(acc, &text, "", Some(now() + 60)).unwrap();
        assert_eq!(s.undo(&[(held, text.clone())], &[], &[], &[], true).unwrap(), Undone::Done);
        assert!(s.pending_ops(acc).unwrap().is_empty());

        // Replayed early, its id taken by the next operation queued: that one is not undone.
        let first = s.push_op(acc, &text, "", Some(now() + 60)).unwrap();
        s.finish_op(first).unwrap();
        let pin = r#"{"kind":"store","mailbox":"Archive","uids":[3],"flag":"\\Flagged","add":true}"#;
        assert_eq!(s.push_op(acc, pin, "", None).unwrap(), first, "SQLite reuses the id");
        assert_eq!(s.undo(&[(first, text.clone())], &[], &[], &[], false).unwrap(), Undone::TooLate);
        assert_eq!(s.pending_ops(acc).unwrap().len(), 1, "the pin stays queued");

        // A replay cut midway may have moved it on the server: too late; refused, it did nothing.
        let cut = s.push_op(acc, &text, "", None).unwrap();
        s.fail_replayed(cut, &text, "connection lost", 10).unwrap();
        assert_eq!(s.undo(&[(cut, text.clone())], &[], &[], &[], false).unwrap(), Undone::TooLate);
        let started = s.push_op(acc, &text, "", None).unwrap();
        s.mark_op_started(started).unwrap();
        assert_eq!(s.undo(&[(started, text.clone())], &[], &[], &[], false).unwrap(), Undone::TooLate);
        let refused = s.push_op(acc, &text, "", None).unwrap();
        s.refuse_replayed(refused, &text, "NO [LIMIT]", 10).unwrap();
        assert_eq!(s.undo(&[(refused, text.clone())], &[], &[], &[], false).unwrap(), Undone::TooLate, "a first chunk may have gone");
        let untried = s.push_op(acc, &text, "", None).unwrap();
        assert_eq!(s.undo(&[(untried, text)], &[], &[], &[], false).unwrap(), Undone::Done);
    }

    #[test]
    fn a_replay_finishes_its_own_operation_not_the_one_that_took_its_id() {
        let (s, acc, _, _) = setup();
        let d1 = r#"{"kind":"save_draft","raw":"D1"}"#;
        let d2 = r#"{"kind":"save_draft","raw":"D2"}"#;
        // D1 replays; meanwhile the composer saves again: D1's row goes, D2 takes its id.
        let id = s.push_op(acc, d1, "", None).unwrap();
        assert_eq!(s.replay_payload(id, now()).unwrap().as_deref(), Some(d1));
        s.finish_op(id).unwrap();
        assert_eq!(s.push_op(acc, d2, "", None).unwrap(), id, "SQLite reuses the id");
        // D1's upload ends, or fails: D2 stays queued, untouched.
        s.finish_replayed(id, d1).unwrap();
        assert!(!s.fail_replayed(id, d1, "cut", 1).unwrap() && !s.refuse_replayed(id, d1, "NO", 1).unwrap());
        let left = s.pending_ops(acc).unwrap();
        assert_eq!((left.len(), left[0].attempts), (1, 0));
        assert_eq!(s.op_payload(id).unwrap().as_deref(), Some(d2));
        s.finish_replayed(id, d2).unwrap();
        assert!(s.pending_ops(acc).unwrap().is_empty());
        // An id taken by an operation still held for undo is not replayed early.
        let held = s.push_op(acc, d1, "", Some(now() + 60)).unwrap();
        assert_eq!(s.replay_payload(held, now()).unwrap(), None);
    }

    #[test]
    fn sends_waiting_offline_keep_their_tries_and_go_once_back() {
        let (s, acc, _, _) = setup();
        let draft = Draft { account_id: acc, to: vec!["a@x".into()], subject: "Train".into(), ..Default::default() };
        let id = s.queue_outgoing(&draft, 100, false, None).unwrap();
        let fresh = s.queue_outgoing(&draft, 5000, false, None).unwrap();
        s.claim_outgoing(150).unwrap().unwrap();
        // Offline three times: no attempt the server saw.
        for _ in 0..3 {
            s.outgoing_failed(id, "offline", Some(9000), true).unwrap();
        }
        assert_eq!(s.outgoing_tries(id).unwrap(), (0, 3));
        // Signed in again or back online: due now; a send never tried keeps its own time.
        assert_eq!(s.retry_outgoing_now(Some(acc), 200).unwrap(), 1);
        assert_eq!(s.next_outgoing().unwrap(), Some(200));
        assert_eq!(s.retry_outgoing_now(None, 200).unwrap(), 0);
        assert!(s.claim_outgoing(4000).unwrap().is_some_and(|(claimed, _, _)| claimed == id));
        assert!(s.claim_outgoing(4000).unwrap().is_none(), "{fresh} waits for its time");
        // An account whose lanes are taken: its due message stays pending, cancellable.
        let due = s.queue_outgoing(&draft, 4100, false, None).unwrap();
        assert!(s.claim_outgoing_except(4200, &[acc]).unwrap().is_none());
        let (claimed, _, _) = s.claim_outgoing(4200).unwrap().unwrap();
        s.unclaim_outgoing(claimed).unwrap();
        assert!(s.take_outgoing(due).unwrap().is_some(), "still cancellable");
    }

    #[test]
    fn outbox_claim_and_cancel() {
        let (s, acc, _, _) = setup();
        let draft = Draft { account_id: acc, to: vec!["a@x".into()], subject: "Bonjour".into(), ..Default::default() };
        let later = s.queue_outgoing(&draft, 500, true, Some(86_400)).unwrap();
        let soon = s.queue_outgoing(&draft, 100, false, None).unwrap();
        assert_eq!(s.next_outgoing().unwrap(), Some(100));
        assert!(s.claim_outgoing(50).unwrap().is_none(), "nothing due");

        let (id, claimed, remind) = s.claim_outgoing(150).unwrap().unwrap();
        assert_eq!((id, claimed.subject.as_str(), remind), (soon, "Bonjour", None));
        assert!(s.take_outgoing(soon).unwrap().is_none(), "too late to cancel a send in progress");
        assert!(s.claim_outgoing(150).unwrap().is_none(), "already claimed");

        s.outgoing_failed(soon, "réseau", Some(200), false).unwrap();
        assert_eq!(s.outbox().unwrap()[0].state, "pending");
        s.claim_outgoing(250).unwrap().unwrap();
        assert_eq!(s.interrupted_outgoing().unwrap(), 1);
        let interrupted = s.outbox().unwrap().into_iter().find(|o| o.id == soon).unwrap();
        assert_eq!(interrupted.state, "failed");
        // No copy in Sent (facteur did not get to file one, and this server does not): Sent
        // cannot tell whether it went.
        assert!(interrupted.error.unwrap().contains("destinataire"));

        assert_eq!(s.take_outgoing(later).unwrap().unwrap().0.to, ["a@x"]);
        assert_eq!(s.outbox().unwrap().len(), 1);
    }

    #[test]
    fn a_send_that_may_have_gone_stays_tried_through_retry() {
        let (s, acc, _, _) = setup();
        let draft = Draft { account_id: acc, to: vec!["a@x".into()], subject: "Devis".into(), ..Default::default() };
        // Cut by a quit while sending, then retried and failing offline.
        let cut = s.queue_outgoing(&draft, 100, false, None).unwrap();
        s.claim_outgoing(150).unwrap().unwrap();
        s.interrupted_outgoing().unwrap();
        assert!(s.reschedule_outgoing(cut, 200).unwrap());
        s.claim_outgoing(250).unwrap().unwrap();
        s.outgoing_failed(cut, "offline", Some(9000), true).unwrap();
        assert!(s.take_outgoing(cut).unwrap().unwrap().1, "edited, it gets a new Message-ID");
        // Offline only, never reached a server: the same Message-ID is fine.
        let offline = s.queue_outgoing(&draft, 100, false, None).unwrap();
        s.claim_outgoing(150).unwrap().unwrap();
        s.outgoing_failed(offline, "offline", Some(9000), true).unwrap();
        assert!(!s.take_outgoing(offline).unwrap().unwrap().1);
        // Retry starts the offline count again too.
        let again = s.queue_outgoing(&draft, 100, false, None).unwrap();
        s.claim_outgoing(150).unwrap().unwrap();
        s.outgoing_failed(again, "offline", Some(9000), true).unwrap();
        s.reschedule_outgoing(again, 200).unwrap();
        assert_eq!(s.outgoing_tries(again).unwrap(), (0, 0));
    }

    #[test]
    fn reply_ids_fall_back_on_in_reply_to() {
        let (s, acc, inbox, _) = setup();
        // In-Reply-To only, no References (some clients): it stands for them.
        let only_irt = HeaderInfo { references: Vec::new(), ..header("b@x", Some("a@x"), "Re: x", 10) };
        let id = s.insert_header(acc, inbox, 1, &only_irt, FLAGS, 10).unwrap();
        assert_eq!(s.message_ids(id).unwrap(), (Some("b@x".to_string()), vec!["a@x".to_string()]));
        let root = s.insert_header(acc, inbox, 2, &header("r@x", None, "x", 5), FLAGS, 10).unwrap();
        assert_eq!(s.message_ids(root).unwrap(), (Some("r@x".to_string()), Vec::new()));
    }

    #[test]
    fn an_automatic_reply_does_not_resolve_a_follow_up() {
        let (s, acc, inbox, sent) = setup();
        let mine = HeaderInfo { from: Some(Address { name: None, email: "Y@example.org".into() }), ..header("devis@x", None, "Devis", 1_000) };
        s.insert_header(acc, sent, 1, &mine, FLAGS, 10).unwrap();
        s.add_reminder(acc, "devis@x", "Devis", &["f@example.org".into()], 1_000, 2_000).unwrap();
        let away = b"Message-ID: <ooo@x>\r\nFrom: f@example.org\r\nIn-Reply-To: <devis@x>\r\nReferences: <devis@x>\r\nAuto-Submitted: auto-replied\r\nSubject: Absence\r\nDate: Thu, 1 Jan 1970 00:20:00 +0000\r\n\r\nJe suis absente.";
        let away = HeaderInfo { received_at: Some(1_200), ..crate::mime::parse_headers(away, 1_200) };
        s.insert_header(acc, inbox, 2, &away, FLAGS, 10).unwrap();
        assert_eq!(s.check_reminders(2_500).unwrap().len(), 1, "an out-of-office reply is no answer");
    }

    #[test]
    fn reminders_resolve_on_reply() {
        let (s, acc, inbox, sent) = setup();
        let mine = |mid: &str, irt: Option<&str>, date| HeaderInfo {
            from: Some(Address { name: None, email: "Y@example.org".into() }),
            ..header(mid, irt, "Devis", date)
        };
        s.insert_header(acc, sent, 1, &mine("devis@x", None, 1_000), FLAGS, 10).unwrap();
        s.add_reminder(acc, "devis@x", "Devis", &["f@example.org".into()], 1_000, 2_000).unwrap();
        s.add_reminder(acc, "autre@x", "Autre", &[], 1_000, 2_000).unwrap();
        assert!(s.check_reminders(1_500).unwrap().is_empty(), "not due yet");
        // My own follow-up in the thread does not count as a reply.
        s.insert_header(acc, sent, 2, &mine("relance@x", Some("devis@x"), 1_800), FLAGS, 10).unwrap();

        // Nor the sent message itself (its Sent copy, appended later), nor one written from an alias.
        s.set_identities(acc, &[crate::model::Identity { email: "Alias@Example.org".into(), ..Default::default() }]).unwrap();
        let copy = HeaderInfo {
            from: Some(Address { name: None, email: "removed-alias@example.org".into() }),
            received_at: Some(1_100),
            ..header("devis@x", None, "Devis", 1_000)
        };
        s.insert_header(acc, inbox, 7, &copy, FLAGS, 10).unwrap();
        let from_alias = HeaderInfo {
            from: Some(Address { name: None, email: "alias@example.org".into() }),
            received_at: Some(1_900),
            ..header("alias@x", Some("devis@x"), "Re: Devis", 1_900)
        };
        s.insert_header(acc, sent, 3, &from_alias, FLAGS, 10).unwrap();

        let due = s.check_reminders(2_500).unwrap();
        assert_eq!(due.len(), 2);
        let quote = due.iter().find(|f| f.subject == "Devis").unwrap();
        assert_eq!((quote.mailbox_id, quote.to.as_slice()), (Some(sent), ["f@example.org".to_string()].as_slice()));
        assert!(s.check_reminders(2_600).unwrap().is_empty(), "reported only once");

        let reply = HeaderInfo { received_at: Some(3_000), ..header("rep@x", Some("relance@x"), "Re: Devis", 3_000) };
        s.insert_header(acc, inbox, 1, &reply, FLAGS, 10).unwrap();
        s.check_reminders(3_100).unwrap();
        assert_eq!(s.follow_ups().unwrap().iter().map(|f| f.subject.as_str()).collect::<Vec<_>>(), ["Autre"]);
    }

    #[test]
    fn usual_authentication_server_is_learned() {
        let (s, acc, inbox, _) = setup();
        let h = |uid: u32, server: &str| {
            let mut info = header(&format!("{uid}@x"), None, "x", uid as i64);
            info.from = Some(Address { name: None, email: format!("s@d{}.example", uid % 7) });
            info.signals.authserv = Some(server.into());
            s.insert_header(acc, inbox, uid, &info, FLAGS, 10).unwrap();
        };
        for uid in 1..=4 {
            h(uid, "mx.google.com");
        }
        assert_eq!(s.usual_authserv(acc).unwrap(), None, "not enough evidence yet");
        h(5, "mx.google.com");
        h(6, "faux");
        assert_eq!(s.usual_authserv(acc).unwrap().as_deref(), Some("mx.google.com"));
    }

    #[test]
    fn imported_old_mail_does_not_set_the_usual_authentication_server() {
        let (s, acc, inbox, _) = setup();
        let archive = s.upsert_mailbox(acc, "Old Gmail", Some("/"), None, true).unwrap();
        let h = |mailbox: i64, uid: u32, date: i64, server: &str| {
            let mut info = header(&format!("{mailbox}-{uid}@x"), None, "x", date);
            info.from = Some(Address { name: None, email: format!("s@d{}.example", uid % 7) });
            info.signals.authserv = Some(server.into());
            s.insert_header(acc, mailbox, uid, &info, FLAGS, 10).unwrap();
        };
        // Only an archive: it is all there is to learn from.
        for uid in 1..=600 {
            h(archive, uid, uid as i64, "mx.google.com");
        }
        assert_eq!(s.usual_authserv(acc).unwrap().as_deref(), Some("mx.google.com"));
        // Synced after the inbox, yet the inbox's own provider wins.
        for uid in 1..=10 {
            h(inbox, uid, 10_000 + uid as i64, "mx.fastmail.com");
        }
        for uid in 601..=1200 {
            h(archive, uid, uid as i64, "mx.google.com");
        }
        assert_eq!(s.usual_authserv(acc).unwrap().as_deref(), Some("mx.fastmail.com"));
    }

    #[test]
    fn junk_does_not_teach_the_usual_authentication_server() {
        let (s, acc, _, _) = setup();
        let junk = s.upsert_mailbox(acc, "Junk", Some("/"), Some(Role::Junk), true).unwrap();
        let trash = s.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let other = s.upsert_mailbox(acc, "Lists", Some("/"), None, true).unwrap();
        let h = |mailbox: i64, uid: u32, date: i64, received: i64, server: &str| {
            let mut info = HeaderInfo { received_at: Some(received), ..header(&format!("{mailbox}-{uid}@x"), None, "x", date) };
            info.from = Some(Address { name: None, email: format!("s@d{}.example", uid % 7) });
            info.signals.authserv = Some(server.into());
            s.insert_header(acc, mailbox, uid, &info, FLAGS, 10).unwrap();
        };
        // Forged verdicts filed by the provider in junk and bin: nothing learned from them.
        for uid in 1..=30 {
            h(junk, uid, 1_000, 1_000, "mx.trusted.example");
            h(trash, uid, 1_000, 1_000, "mx.trusted.example");
        }
        assert_eq!(s.usual_authserv(acc).unwrap(), None);
        // A Date written in the future does not make a message the most recently received.
        for uid in 1..=500 {
            h(other, uid, 9_000_000_000, 100, "mx.forged.example");
        }
        for uid in 501..=800 {
            h(other, uid, 5_000, 5_000 + uid as i64, "mx.provider.example");
        }
        assert_eq!(s.usual_authserv(acc).unwrap().as_deref(), Some("mx.provider.example"));
    }

    #[test]
    fn unknown_senders_do_not_teach_the_usual_authentication_server() {
        let (s, acc, inbox, _) = setup();
        let screener = s.upsert_mailbox(acc, "New senders", Some("/"), Some(Role::Screener), true).unwrap();
        let refused = s.upsert_mailbox(acc, "Screened out", Some("/"), Some(Role::Refused), true).unwrap();
        let h = |mailbox: i64, uid: u32, from: String, server: &str| {
            let mut info = HeaderInfo { received_at: Some(1_000 + uid as i64), ..header(&format!("{mailbox}-{uid}@x"), None, "x", 1_000) };
            info.from = Some(Address { name: None, email: from });
            info.signals.authserv = Some(server.into());
            s.insert_header(acc, mailbox, uid, &info, FLAGS, 10).unwrap();
        };
        // Planted verdicts held by the screener, from many domains: nothing learned from them.
        for uid in 1..=30 {
            h(screener, uid, format!("a@d{uid}.example"), "mx.evil.example");
            h(refused, uid, format!("a@d{uid}.example"), "mx.evil.example");
        }
        assert_eq!(s.usual_authserv(acc).unwrap(), None);
        // One sender alone in the inbox, however many messages: nothing either.
        for uid in 1..=30 {
            h(inbox, uid, "a@evil.example".into(), "mx.evil.example");
        }
        assert_eq!(s.usual_authserv(acc).unwrap(), None);
        // Then several senders' mail carrying the provider's verdict.
        for uid in 31..=100 {
            h(inbox, uid, format!("b@d{}.example", uid % 5), "mx.provider.example");
        }
        assert_eq!(s.usual_authserv(acc).unwrap().as_deref(), Some("mx.provider.example"));
    }

    #[test]
    fn smart_inbox_filters_and_sender_choice() {
        let (s, acc, inbox, sent) = setup();
        let with = |mid: &str, email: &str, category: Category, date: i64| HeaderInfo {
            from: Some(Address { name: None, email: email.into() }),
            category,
            ..header(mid, None, mid, date)
        };
        s.insert_header(acc, inbox, 1, &with("perso@x", "ami@x.fr", Category::People, 100), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 2, &with("news@x", "infos@shop.fr", Category::Newsletter, 200), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 3, &with("alerte@x", "no-reply@banque.fr", Category::Notification, 300), FLAGS, 10).unwrap();
        let subjects = |c: Option<Category>| {
            s.list_threads(&[inbox], &[], ThreadFilter::All, false, c, 50, 0).unwrap().into_iter().map(|t| t.subject).collect::<Vec<_>>()
        };
        assert_eq!(subjects(Some(Category::People)), ["perso@x"]);
        assert_eq!(subjects(Some(Category::Newsletter)), ["news@x"]);
        assert_eq!(subjects(None).len(), 3);
        assert!(s.category_unread(&[inbox]).unwrap().values().all(|n| *n == 1));

        // The user files a sender under another category: cached and future messages.
        s.set_sender_category("INFOS@shop.fr", Category::People).unwrap();
        assert_eq!(subjects(Some(Category::People)), ["news@x", "perso@x"]);
        s.insert_header(acc, inbox, 4, &with("news2@x", "infos@shop.fr", Category::Newsletter, 400), FLAGS, 10).unwrap();
        assert_eq!(subjects(Some(Category::People))[0], "news2@x");

        // Someone the user wrote to is a person, even through a bulk mailing.
        let mut mine = with("moi@x", "y@example.org", Category::People, 50);
        mine.to = vec![Address { name: None, email: "asso@club.fr".into() }];
        s.insert_header(acc, sent, 1, &mine, FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 5, &with("club@x", "asso@club.fr", Category::Newsletter, 500), FLAGS, 10).unwrap();
        assert_eq!(subjects(Some(Category::People))[0], "club@x");
    }

    #[test]
    fn done_view_leaves_out_threads_back_in_inbox() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        // Done: only in All Mail. Still in the inbox: in both.
        s.insert_header(acc, all, 1, &header("done@x", None, "Traité", 100), FLAGS, 10).unwrap();
        s.insert_header(acc, all, 2, &header("open@x", None, "En cours", 200), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 1, &header("open@x", None, "En cours", 200), FLAGS, 10).unwrap();
        let done = s.list_threads(&[all], &[inbox], ThreadFilter::All, false, None, 50, 0).unwrap();
        assert_eq!(done.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(), ["Traité"]);
    }

    #[test]
    fn done_view_sorts_by_when_threads_left_the_inbox() {
        let (s, acc, inbox, _) = setup();
        let archive = s.upsert_mailbox(acc, "Archives", Some("/"), Some(Role::Archive), true).unwrap();
        let old = s.insert_header(acc, inbox, 1, &header("old@x", None, "Ancien", 100), FLAGS, 10).unwrap();
        s.insert_header(acc, archive, 1, &header("new@x", None, "Récent", 200), FLAGS, 10).unwrap();
        let done = || s.list_threads(&[archive], &[inbox], ThreadFilter::All, false, None, 50, 0).unwrap();
        assert_eq!(done().iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(), ["Récent"]);
        // Archived now: before the thread whose last message is newer.
        s.move_local(old, archive).unwrap();
        let listed = done();
        assert_eq!(listed.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(), ["Ancien", "Récent"]);
        assert!(listed[0].activity >= now() - 5 && listed[0].date == 100);

        // A label taken off (Gmail): the same.
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let labels = s.upsert_mailbox(acc, "Labels", Some("/"), None, true).unwrap();
        s.set_label_folder(labels, true).unwrap();
        let x = s.insert_header(acc, all, 1, &header("x@x", None, "Libellé", 50), FLAGS, 10).unwrap();
        s.set_labels(x, &[inbox]).unwrap();
        let handled = |id: i64| -> Option<i64> {
            s.read()
                .query_row("SELECT h.at FROM handled h JOIN messages m ON m.thread_id = h.thread_id WHERE m.id = ?1", [id], |r| r.get(0))
                .optional()
                .unwrap()
        };
        s.db().execute("DELETE FROM handled", []).unwrap();
        s.set_labels(x, &[labels]).unwrap();
        assert!(handled(x).is_some());

        // Nothing written for an account being removed.
        s.delete_account(acc).unwrap();
        assert_eq!(s.read().query_row("SELECT COUNT(*) FROM handled", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn a_folder_reset_or_an_undo_is_no_thread_handled() {
        let (s, acc, inbox, _) = setup();
        let archive = s.upsert_mailbox(acc, "Archives", Some("/"), Some(Role::Archive), true).unwrap();
        let handled = || s.read().query_row("SELECT COUNT(*) FROM handled", [], |r| r.get::<_, i64>(0)).unwrap();
        // The inbox recreated on the server (UIDVALIDITY changed): its rows go, nothing is handled.
        s.insert_header(acc, inbox, 1, &header("a@x", None, "Gardé", 100), FLAGS, 10).unwrap();
        s.clear_mailbox(inbox).unwrap();
        assert_eq!(handled(), 0);
        // Moved back to the inbox from Done, then undone: it keeps no new handled date.
        let id = s.insert_header(acc, archive, 1, &header("b@x", None, "Traité", 100), FLAGS, 10).unwrap();
        let text = serde_json::json!({ "kind": "move_set", "mailbox": "Archives", "dest": "INBOX", "uids": [1] }).to_string();
        let op = s.push_op(acc, &text, "", Some(now() + 60)).unwrap();
        s.move_local(id, inbox).unwrap();
        assert_eq!(s.undo(&[(op, text)], &[(id, archive, Some(1))], &[], &[], false).unwrap(), Undone::Done);
        assert_eq!((s.location(id).unwrap().mailbox_id, handled()), (archive, 0));
        // A real archive still is.
        s.move_local(id, inbox).unwrap();
        s.move_local(id, archive).unwrap();
        assert_eq!(handled(), 1);
    }

    #[test]
    fn a_large_account_is_removed_in_steps() {
        let (s, acc, inbox, _) = setup();
        for uid in 1..=2500 {
            s.insert_header(acc, inbox, uid, &header(&format!("m{uid}@x"), None, "Sujet", i64::from(uid)), FLAGS, 10).unwrap();
        }
        s.delete_account(acc).unwrap();
        let left = |table: &str| s.read().query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!((left("messages"), left("accounts"), left("handled"), left("handled_quiet")), (0, 0, 0, 0));
    }

    #[test]
    fn unified_inbox_merges_accounts() {
        let (s, acc, inbox, _) = setup();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let other = s
            .insert_account(&NewAccount {
                name: "Z".into(), email: "z@example.org".into(), username: "z".into(), password: String::new(),
                imap: srv.clone(), smtp: srv, auth: NewAuth::Password,
            })
            .unwrap();
        let other_inbox = s.upsert_mailbox(other, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        s.insert_header(acc, inbox, 1, &header("a@x", None, "Ancien", 100), FLAGS, 10).unwrap();
        s.insert_header(other, other_inbox, 1, &header("b@x", None, "Récent", 200), FLAGS, 10).unwrap();

        let threads = s.list_threads(&[inbox, other_inbox], &[], ThreadFilter::All, false, None, 50, 0).unwrap();
        assert_eq!(threads.iter().map(|t| t.subject.as_str()).collect::<Vec<_>>(), ["Récent", "Ancien"]);
        assert_eq!((threads[0].account_id, threads[0].mailbox_id), (other, other_inbox));
    }

    #[test]
    fn dashboard_counts() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        let starred = ServerFlags { flagged: true, ..FLAGS };
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "Suivi", 500), starred, 10).unwrap();
        // Same message in All Mail: counted once, on the INBOX side.
        s.insert_header(acc, all, 1, &header("a@x", None, "Suivi", 500), starred, 10).unwrap();
        s.insert_header(acc, inbox, 2, &header("b@x", None, "Ancien", 100), FLAGS, 10).unwrap();

        let (count, flagged) = s.flagged_messages(6).unwrap();
        assert_eq!(count, 1);
        assert_eq!(flagged.iter().map(|m| m.id).collect::<Vec<_>>(), [a]);
        assert_eq!(s.inbox_ids().unwrap(), [inbox]);

        // Arrived today (INTERNALDATE) but dated yesterday by the sender, then archived: counted
        // once, whether in INBOX, in All Mail or both.
        let late = HeaderInfo { received_at: Some(1000), ..header("c@x", None, "En retard", 10) };
        s.insert_header(acc, all, 2, &late, FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 3, &late, FLAGS, 10).unwrap();
        assert_eq!(s.received_since(900).unwrap(), 1);
    }

    #[test]
    fn contacts_prefer_people_we_wrote_to() {
        let (s, acc, inbox, sent) = setup();
        let mut from_marie = header("1@x", None, "a", 100);
        from_marie.from = Some(Address { name: Some("Marie Martin".into()), email: "marie@ex.org".into() });
        s.insert_header(acc, inbox, 1, &from_marie, FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 2, &HeaderInfo { message_id: Some("2@x".into()), ..from_marie.clone() }, FLAGS, 10).unwrap();
        let mut to_mario = header("3@x", None, "b", 50);
        to_mario.from = Some(Address { name: None, email: "y@example.org".into() });
        to_mario.to = vec![Address { name: Some("Mario".into()), email: "Mario@ex.org".into() }];
        s.insert_header(acc, sent, 1, &to_mario, FLAGS, 10).unwrap();

        let found = s.search_contacts("mar", &["y@example.org".into()], 5).unwrap();
        assert_eq!(found.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["mario@ex.org", "marie@ex.org"]);
        assert_eq!(found[1].name.as_deref(), Some("Marie Martin"));
        assert!(s.search_contacts("example", &["y@example.org".into()], 5).unwrap().is_empty(), "oneself left out");
    }

    #[test]
    fn a_removed_account_takes_its_correspondents_away() {
        let (s, acc, inbox, sent) = setup();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let other = s
            .insert_account(&NewAccount {
                auth: NewAuth::default(),
                name: "Z".into(), email: "z@example.org".into(), username: "z".into(), password: String::new(),
                imap: srv.clone(), smtp: srv,
            })
            .unwrap();
        let other_inbox = s.upsert_mailbox(other, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let from = |email: &str, id: &str| HeaderInfo { from: Some(Address { name: None, email: email.into() }), ..header(id, None, "a", 100) };
        s.insert_header(acc, inbox, 1, &from("marie@ex.org", "1@x"), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 2, &from("both@ex.org", "2@x"), FLAGS, 10).unwrap();
        let mut to_mario = header("3@x", None, "b", 50);
        to_mario.to = vec![Address { name: Some("Mario".into()), email: "mario@ex.org".into() }];
        s.insert_header(acc, sent, 1, &to_mario, FLAGS, 10).unwrap();
        s.insert_header(other, other_inbox, 1, &from("both@ex.org", "4@x"), FLAGS, 10).unwrap();
        s.insert_header(other, other_inbox, 2, &from("zoe@ex.org", "5@x"), FLAGS, 10).unwrap();
        // Written to from the account removed, and known to the other: no longer "written to".
        let mut to_zoe = header("6@x", None, "c", 60);
        to_zoe.to = vec![Address { name: None, email: "zoe@ex.org".into() }];
        s.insert_header(acc, sent, 2, &to_zoe, FLAGS, 10).unwrap();
        assert_eq!(s.search_contacts("ex.org", &[], 10).unwrap().len(), 4);

        s.delete_account(acc).unwrap();
        let mut left: Vec<String> = s.search_contacts("ex.org", &[], 10).unwrap().into_iter().map(|a| a.email).collect();
        left.sort();
        assert_eq!(left, ["both@ex.org", "zoe@ex.org"]);
        let counts = |email: &str| s.read().query_row("SELECT written, seen FROM contacts WHERE email = ?1", [email], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).unwrap();
        assert_eq!((counts("zoe@ex.org"), counts("both@ex.org")), ((0, 1), (0, 1)));
    }

    #[test]
    fn a_removal_cut_halfway_still_forgets_the_people_of_its_first_steps() {
        let (s, acc, inbox, _) = setup();
        let from = |email: &str, id: &str| HeaderInfo { from: Some(Address { name: None, email: email.into() }), ..header(id, None, "a", 100) };
        let first = s.insert_header(acc, inbox, 1, &from("marie@ex.org", "1@x"), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 2, &from("paul@ex.org", "2@x"), FLAGS, 10).unwrap();
        // The first attempt noted its correspondents, deleted Marie's message, then stopped.
        s.removal_correspondents(acc).unwrap();
        s.db().execute("DELETE FROM messages WHERE id = ?1", [first]).unwrap();
        s.delete_account(acc).unwrap();
        assert!(s.search_contacts("ex.org", &[], 10).unwrap().is_empty());
        assert_eq!(s.setting(&format!("removing_contacts:{acc}")).unwrap(), None);
    }

    #[test]
    fn address_books_name_and_suggest_people() {
        use crate::contacts::Contact;
        let (s, acc, inbox, sent) = setup();
        let mut from_marie = header("1@x", None, "a", 100);
        from_marie.from = Some(Address { name: Some("M. Martin".into()), email: "marie@ex.org".into() });
        s.insert_header(acc, inbox, 1, &from_marie, FLAGS, 10).unwrap();
        let mut to_mario = header("3@x", None, "b", 50);
        to_mario.from = Some(Address { name: None, email: "y@example.org".into() });
        to_mario.to = vec![Address { name: Some("Mario".into()), email: "mario@ex.org".into() }];
        s.insert_header(acc, sent, 1, &to_mario, FLAGS, 10).unwrap();
        let book = [
            Contact { remote_id: "c1".into(), name: Some("Marie Martin".into()), emails: vec!["marie@ex.org".into()] },
            Contact { remote_id: "c2".into(), name: Some("Martine Durand".into()), emails: vec!["martine@ex.org".into(), "m.durand@work.org".into()] },
        ];
        s.replace_address_book(acc, "y@example.org", &book).unwrap();
        s.replace_address_book(acc, "someone.else@example.org", &[]).unwrap();
        assert_eq!(s.search_contacts("durand", &[], 5).unwrap().len(), 2, "another account's fetch is not written");
        let found = s.search_contacts("mar", &[], 5).unwrap();
        // Written to first, then the address book's people, then the others.
        let emails: Vec<&str> = found.iter().map(|a| a.email.as_str()).collect();
        assert_eq!(emails[..2], ["mario@ex.org", "marie@ex.org"]);
        let mut rest = emails[2..].to_vec();
        rest.sort();
        assert_eq!(rest, ["m.durand@work.org", "martine@ex.org"], "found by the contact's name too");
        assert_eq!(found[1].name.as_deref(), Some("Marie Martin"), "the address book's name");
        assert_eq!(s.search_contacts("durand", &[], 5).unwrap().len(), 2, "every address of a contact");

        s.set_address_book_state(acc, CalendarState::Ok, None, true).unwrap();
        let status = s.address_book_status().unwrap();
        assert_eq!((status[0].contacts, status[0].state), (2, CalendarState::Ok));
        let synced = status[0].last_sync;
        s.set_address_book_state(acc, CalendarState::Error, Some("down"), false).unwrap();
        let status = s.address_book_status().unwrap();
        assert_eq!((status[0].last_sync, status[0].message.as_deref()), (synced, Some("down")), "the last sync stays");

        s.replace_address_book(acc, "y@example.org", &book[..1]).unwrap();
        assert!(s.search_contacts("durand", &[], 5).unwrap().is_empty(), "replaced");
    }

    #[test]
    fn recent_recipients_are_the_latest_people_written_to() {
        let (s, acc, inbox, sent) = setup();
        let to = |email: &str, name: Option<&str>| Address { name: name.map(Into::into), email: email.into() };
        let mut old = header("1@x", None, "a", 10);
        old.to = vec![to("Ann@ex.org", Some("Ann")), to("bob@ex.org", None)];
        s.insert_header(acc, sent, 1, &old, FLAGS, 10).unwrap();
        let mut new = header("2@x", None, "b", 20);
        new.to = vec![to("carl@ex.org", None), to("y@example.org", None)];
        s.insert_header(acc, sent, 2, &new, FLAGS, 10).unwrap();
        let mut newest = header("4@x", None, "d", 25);
        newest.cc = vec![to("ann@ex.org", None)];
        s.insert_header(acc, sent, 3, &newest, FLAGS, 10).unwrap();
        // Received mail does not count, only what we sent.
        let mut received = header("3@x", None, "c", 30);
        received.to = vec![to("dora@ex.org", None)];
        s.insert_header(acc, inbox, 1, &received, FLAGS, 10).unwrap();

        let found = s.recent_recipients(&["Y@example.org".into()], 5).unwrap();
        // Newest first, each address once, our own left out, the known name kept.
        assert_eq!(found.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(), ["ann@ex.org", "carl@ex.org", "bob@ex.org"]);
        assert_eq!(found[0].name.as_deref(), Some("Ann"));
        assert_eq!(s.recent_recipients(&[], 1).unwrap().len(), 1);
    }

    #[test]
    fn only_the_latest_draft_version_is_queued() {
        let (s, acc, _, _) = setup();
        let save = |raw: &str| format!(r#"{{"kind":"save_draft","mailbox":"Drafts","message_id":"d1@x","raw":"{raw}"}}"#);
        // An older save that failed goes too: retried later, it would put back an old text.
        let failed = s.push_op(acc, &save("v0"), "", None).unwrap();
        assert!(s.fail_replayed(failed, &save("v0"), "too large", 0).unwrap());
        s.push_op(acc, &save("v1"), "", None).unwrap();
        s.push_op(acc, r#"{"kind":"save_draft","mailbox":"Drafts","message_id":"autre@x","raw":"a"}"#, "", None).unwrap();
        s.drop_pending_draft(acc, "d1@x").unwrap();
        s.push_op(acc, &save("v2"), "", None).unwrap();
        let ops: Vec<String> = s.pending_ops(acc).unwrap().into_iter().map(|p| s.op_payload(p.id).unwrap().unwrap()).collect();
        assert_eq!(ops.len(), 2, "{ops:?}");
        assert!(ops.iter().any(|o| o.contains("autre@x")), "another draft is left alone");
        assert!(ops.iter().any(|o| o.contains("v2")) && !ops.iter().any(|o| o.contains("v1") || o.contains("v0")));
    }

    #[test]
    fn blocked_senders_are_found_in_inbox() {
        let (s, acc, inbox, sent) = setup();
        let mut spam = header("s@x", None, "Promo", 100);
        spam.from = Some(Address { name: None, email: "Promo@Spam.example".into() });
        let id = s.insert_header(acc, inbox, 1, &spam, FLAGS, 10).unwrap();
        s.insert_header(acc, sent, 1, &HeaderInfo { message_id: Some("t@x".into()), ..spam.clone() }, FLAGS, 10).unwrap();
        assert!(s.inbox_from_blocked(acc).unwrap().is_empty());
        s.block_sender(" promo@spam.example ").unwrap();
        assert_eq!(s.inbox_from_blocked(acc).unwrap(), [id], "inbox only, case-insensitive");
        assert_eq!(s.blocked_senders().unwrap(), ["promo@spam.example"]);
        s.unblock_sender("PROMO@spam.example").unwrap();
        assert!(s.blocked_senders().unwrap().is_empty());
    }

    #[test]
    fn batched_lookups_agree_with_the_single_ones() {
        let (s, acc, inbox, sent) = setup();
        let held = s.upsert_mailbox(acc, "New senders", Some("/"), Some(Role::Screener), true).unwrap();
        let from = |email: &str, name: Option<&str>, mid: &str, date| HeaderInfo {
            from: Some(Address { name: name.map(Into::into), email: email.into() }),
            ..header(mid, None, mid, date)
        };
        let a1 = s.insert_header(acc, held, 1, &from("Ann@Ex.org", None, "a1@x", 10), FLAGS, 10).unwrap();
        let a2 = s.insert_header(acc, held, 2, &from("ann@ex.org", Some("Ann"), "a2@x", 30), FLAGS, 10).unwrap();
        let b = s.insert_header(acc, held, 3, &from("bob@ex.org", None, "b@x", 20), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 1, &from("ann@ex.org", None, "a3@x", 5), FLAGS, 10).unwrap();
        // One row per sender of the screener, the latest first, with its latest message.
        let senders = s.held_senders(Some(acc)).unwrap();
        assert_eq!(senders, vec![(acc, String::from("ann@ex.org"), Some(String::from("Ann")), 2, a2), (acc, String::from("bob@ex.org"), None, 1, b)]);
        assert_eq!(s.held_senders(None).unwrap().len(), 2);

        let emails = vec!["ANN@ex.org".to_string(), "bob@ex.org".into(), "nobody@ex.org".into()];
        let latest = s.latest_from_many(&emails).unwrap();
        assert_eq!(latest.len(), 2);
        for e in &emails[..2] {
            assert_eq!(latest.get(e).copied(), s.latest_from(e).unwrap(), "{e}");
        }
        s.block_sender("bob@ex.org").unwrap();
        let many = s.trust_inputs_many(&[a1, b, 999]).unwrap();
        assert_eq!(many.len(), 2);
        for id in [a1, b] {
            let (inputs, sender) = &many[&id];
            let one = s.trust_inputs(id).unwrap();
            assert_eq!((inputs.account_id, &inputs.from, inputs.role), (one.account_id, &one.from, one.role));
            let email = one.from.unwrap();
            let (wrote, seen) = s.sender_context(acc, &email, id).unwrap();
            assert_eq!(*sender, Some((wrote, seen, s.is_blocked(&email).unwrap())), "{email}");
        }
        assert_eq!(many[&a1].1.unwrap().1, 2, "two other messages of hers");

        assert_eq!(s.uid_stats(held).unwrap(), (3, Some(3)));
        assert_eq!(s.uid_stats(sent).unwrap(), (0, None));
        assert_eq!(s.uids_below(held, 3).unwrap(), HashSet::from([1, 2]));
    }

    #[test]
    fn queued_operations_are_listed_without_their_payloads() {
        let (s, acc, ..) = setup();
        let op = r#"{"kind":"move_set","mailbox":"INBOX","uids":[2,3],"dest":"Archive"}"#;
        let id = s.push_op(acc, op, "", None).unwrap();
        let other = s.push_op(acc, r#"{"kind":"move","mailbox":"INBOX","uid":7,"dest":"Archive"}"#, "", None).unwrap();
        s.push_op(acc, r#"{"kind":"store","mailbox":"INBOX","uid":7,"flag":"\\Seen","value":true}"#, "", None).unwrap();
        let row = s.op_row(id).unwrap().unwrap();
        assert_eq!((row.kind.as_str(), row.size), ("move_set", op.len() as i64));
        assert!(s.op_row(12345).unwrap().is_none());
        assert_eq!(s.op_rows().unwrap().len(), 3);
        assert_eq!(s.move_payloads(acc, "INBOX", other, false, false).unwrap(), [op]);
        assert!(!s.draft_present(acc, "<d@x>").unwrap());
        s.push_op(acc, r#"{"kind":"save_draft","mailbox":"Drafts","message_id":"<d@x>","raw":""}"#, "", None).unwrap();
        assert!(s.draft_present(acc, "<d@x>").unwrap());
    }

    #[test]
    fn replies_to_an_original_never_received_are_one_thread() {
        let (s, acc, inbox, _) = setup();
        let r1 = s.insert_header(acc, inbox, 1, &header("r1@x", Some("root@x"), "Re: Devis", 10), FLAGS, 10).unwrap();
        let r2 = s.insert_header(acc, inbox, 2, &header("r2@x", Some("root@x"), "Re: Devis", 20), FLAGS, 10).unwrap();
        let thread = |id: i64| s.read().query_row("SELECT thread_id FROM messages WHERE id = ?1", [id], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(thread(r1), thread(r2));
        // In-Reply-To alone, no References.
        let mut bare = header("r3@x", Some("root@x"), "Re: Devis", 30);
        bare.references.clear();
        let r3 = s.insert_header(acc, inbox, 3, &bare, FLAGS, 10).unwrap();
        assert_eq!(thread(r3), thread(r1));
        let other = s.insert_header(acc, inbox, 4, &header("o@x", Some("elsewhere@x"), "Autre", 40), FLAGS, 10).unwrap();
        assert_ne!(thread(other), thread(r1));
    }

    #[test]
    fn sender_lookups_use_the_address_indexes() {
        let (s, ..) = setup();
        let plan = |sql: &str| -> String {
            let db = s.db();
            let mut st = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            st.query_map([], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect::<Vec<_>>().join(" | ")
        };
        let blocked = plan(
            "SELECT m.id FROM blocked_senders s
             CROSS JOIN messages m ON lower(m.from_addr) = lower(s.email) AND m.account_id = 1
             JOIN mailboxes b ON b.id = m.mailbox_id
             WHERE b.role = 'inbox' AND m.uid IS NOT NULL",
        );
        assert!(blocked.contains("SEARCH m USING INDEX messages_from") && !blocked.contains("SCAN m"), "{blocked}");
        let latest = plan(
            "SELECT j.value, (SELECT m.id FROM messages m WHERE lower(m.from_addr) = lower(j.value) ORDER BY m.date DESC LIMIT 1)
             FROM json_each('[\"a@x\"]') j",
        );
        assert!(latest.contains("messages_from_date") && !latest.contains("SCAN m"), "{latest}");
    }

    #[test]
    fn account_stats_count_each_message_once() {
        let (s, acc, inbox, sent) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        let now = 1_792_000_000;
        let mut from_marie = header("1@x", None, "a", now - 3600);
        from_marie.from = Some(Address { name: Some("Marie".into()), email: "marie@ex.org".into() });
        s.insert_header(acc, inbox, 1, &from_marie, FLAGS, 100).unwrap();
        // Same message in All Mail: counted once.
        s.insert_header(acc, all, 1, &from_marie, FLAGS, 100).unwrap();
        let mut mine = header("2@x", None, "b", now - 7200);
        mine.from = Some(Address { name: None, email: "y@example.org".into() });
        mine.to = vec![Address { name: Some("Marie".into()), email: "marie@ex.org".into() }];
        s.insert_header(acc, sent, 1, &mine, ServerFlags { seen: true, ..FLAGS }, 50).unwrap();

        let st = s.account_stats(acc, "y@example.org", now).unwrap();
        assert_eq!((st.total, st.received, st.sent, st.unread), (2, 1, 1, 1));
        assert_eq!((st.received_30d, st.sent_30d), (1, 1));
        assert_eq!(st.months.len(), 12);
        assert_eq!(st.months.iter().map(|m| m.received + m.sent).sum::<i64>(), 2);
        assert_eq!(st.weekdays.iter().sum::<i64>(), 1);
        assert_eq!(st.top_senders[0].email, "marie@ex.org");
        assert_eq!((st.top_recipients[0].email.as_str(), st.top_recipients[0].count), ("marie@ex.org", 1));

        // Inbox only: the All Mail copy stays unread.
        assert_eq!(s.mark_account_seen(acc, true).unwrap(), ["INBOX"]);
        assert!(s.mark_account_seen(acc, true).unwrap().is_empty());
        assert_eq!(s.mark_account_seen(acc, false).unwrap(), ["[Gmail]/All"], "folders that still had unread messages");
        assert_eq!(s.account_stats(acc, "y@example.org", now).unwrap().unread, 0);
    }

    #[test]
    fn similar_messages_by_criteria() {
        let (s, acc, inbox, _) = setup();
        let mut promo = header("p1@x", None, "Soldes d'hiver -50%", 100);
        promo.from = Some(Address { name: None, email: "news@mail.shop.example".into() });
        promo.list_id = Some("promo.shop.example".into());
        let a = s.insert_header(acc, inbox, 1, &promo, FLAGS, 10).unwrap();
        let b = s
            .insert_header(acc, inbox, 2, &HeaderInfo { message_id: Some("p2@x".into()), subject: "Soldes d'été".into(), date: 300, ..promo.clone() }, FLAGS, 10)
            .unwrap();
        s.insert_header(acc, inbox, 3, &header("o@x", None, "Autre", 100), FLAGS, 10).unwrap();
        s.store_body(a, b"raw", &BodyInfo {
            preview: String::new(), text: "Code promo réservé aux membres".into(), has_attachments: false,
            link_domains: vec!["shop.example".into(), "tracker.example".into()], list_id: None,
        })
        .unwrap();

        let by = |c: SimilarCriteria| s.similar(&c, 10).unwrap();
        // Words of the message text, in any order, prefixes accepted.
        assert_eq!(by(SimilarCriteria { body: Some("membres promo".into()), ..Default::default() }).ids, [a]);
        assert_eq!(by(SimilarCriteria { body: Some("membre\"".into()), ..Default::default() }).ids, [a], "quotes are harmless");
        assert_eq!(by(SimilarCriteria { body: Some("promo : membres".into()), ..Default::default() }).ids, [a], "standalone punctuation is ignored");
        assert_eq!(by(SimilarCriteria { body: Some("membres - €".into()), ..Default::default() }).ids, [a]);
        assert!(by(SimilarCriteria { body: Some(" : ".into()), ..Default::default() }).ids.is_empty(), "no word: nothing, never everything");
        assert!(by(SimilarCriteria { body: Some("absent".into()), ..Default::default() }).ids.is_empty());
        assert_eq!(by(SimilarCriteria { from_domain: Some("shop.example".into()), ..Default::default() }).count, 2, "subdomain included");
        assert_eq!(by(SimilarCriteria { subject: Some("SOLDES".into()), ..Default::default() }).count, 2);
        assert_eq!(by(SimilarCriteria { subject: Some("50%".into()), ..Default::default() }).count, 1, "literal %");
        assert_eq!(by(SimilarCriteria { link_domains: vec!["tracker.example".into()], ..Default::default() }).ids, [a]);
        assert_eq!(by(SimilarCriteria { list_id: Some("promo.shop.example".into()), ..Default::default() }).count, 2);
        let both = by(SimilarCriteria { from_domain: Some("shop.example".into()), subject: Some("été".into()), ..Default::default() });
        assert_eq!(both.ids, [b], "every criterion must match");
        let shop = || SimilarCriteria { from_domain: Some("shop.example".into()), ..Default::default() };
        assert_eq!(by(SimilarCriteria { date_from: Some(200), ..shop() }).ids, [b]);
        assert_eq!(by(SimilarCriteria { date_to: Some(100), ..shop() }).ids, [a], "bound included");

        // A domain announced by a link: messages linking to it or sent from it, the one open aside.
        let other = s.insert_header(acc, inbox, 4, &header("o2@x", None, "Autre", 100), FLAGS, 10).unwrap();
        assert_eq!(s.domain_seen("shop.example", other).unwrap(), 2, "from a subdomain");
        assert_eq!(s.domain_seen("shop.example", a).unwrap(), 1);
        assert_eq!(s.domain_seen("tracker.example", other).unwrap(), 1, "by its links");
        assert_eq!(s.domain_seen("tracker.example", a).unwrap(), 0);
        assert_eq!(s.domain_seen("hop.example", other).unwrap(), 0, "not a mere suffix");
        assert_eq!(s.domain_seen("sh_p.example", other).unwrap(), 0, "`_` is literal");
    }

    #[test]
    fn held_or_refused_mail_does_not_vouch_for_a_link_domain() {
        let (s, acc, inbox, _) = setup();
        let held = s.upsert_mailbox(acc, "New senders", Some("/"), Some(Role::Screener), true).unwrap();
        let refused = s.upsert_mailbox(acc, "Screened out", Some("/"), Some(Role::Refused), true).unwrap();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let links = |id| {
            s.store_body(id, b"raw", &BodyInfo {
                preview: String::new(), text: String::new(), has_attachments: false,
                link_domains: vec!["evil.example".into()], list_id: None,
            })
            .unwrap()
        };
        let open = s.insert_header(acc, inbox, 1, &header("open@x", None, "Accéder", 100), FLAGS, 10).unwrap();
        // Primed from the screener, from "Screened out", and through a Gmail message held by its label.
        for (b, uid, mid) in [(held, 1, "p1@x"), (refused, 1, "p2@x")] {
            links(s.insert_header(acc, b, uid, &header(mid, None, "Bonjour", 50), FLAGS, 10).unwrap());
        }
        let labelled = s.insert_header(acc, all, 1, &header("p3@x", None, "Bonjour", 50), FLAGS, 10).unwrap();
        links(labelled);
        s.relabel(&[(labelled, vec![held])]).unwrap();
        assert_eq!(s.link_domain_context("evil.example", open).unwrap(), (0, 0, false));
        assert_eq!(s.domain_seen("evil.example", open).unwrap(), 0);
        // Mail the user let in does count.
        links(s.insert_header(acc, inbox, 2, &header("ok@x", None, "Bonjour", 60), FLAGS, 10).unwrap());
        assert_eq!(s.link_domain_context("evil.example", open).unwrap(), (1, 1, false));
        assert_eq!(s.domain_seen("evil.example", open).unwrap(), 1);
    }

    #[test]
    fn coverage_against_server_counts() {
        let (s, acc, inbox, sent) = setup();
        let id = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 2, &header("b@x", None, "b", 2), FLAGS, 10).unwrap();
        s.store_body(id, b"raw", &BodyInfo {
            preview: String::new(), text: String::new(), has_attachments: false, link_domains: Vec::new(), list_id: None,
        })
        .unwrap();
        let c = s.coverage(acc).unwrap();
        assert_eq!((c.local, c.bodies, c.server), (2, 1, None), "folders never synced: total unknown");

        for (mailbox, count) in [(inbox, 5), (sent, 0)] {
            let mut state = s.mailbox_state(mailbox).unwrap();
            state.server_count = Some(count);
            s.save_mailbox_state(&state).unwrap();
        }
        assert_eq!(s.coverage(acc).unwrap().server, Some(5));
        // A message being moved (no UID) does not count as present.
        s.move_local(id, sent).unwrap();
        assert_eq!(s.coverage(acc).unwrap().local, 1);
    }

        #[test]
    fn failed_ops_wait_for_the_user() {
        let (s, acc, _, _) = setup();
        s.push_op(acc, "{}", "Déplacer vers Archive · Facture", None).unwrap();
        let id = s.pending_ops(acc).unwrap()[0].id;
        for _ in 0..5 {
            s.fail_op(id, "NO refusé", 5).unwrap();
        }
        assert!(s.pending_ops(acc).unwrap().is_empty(), "no longer replayed automatically");
        let failed = s.failed_ops().unwrap();
        assert_eq!((failed[0].label.as_str(), failed[0].error.as_str()), ("Déplacer vers Archive · Facture", "NO refusé"));
        assert_eq!(s.ops_counts().unwrap(), [(acc, 0, 1)]);
        assert_eq!(s.retry_op(id).unwrap(), acc);
        assert_eq!(s.pending_ops(acc).unwrap().len(), 1);
    }

    #[test]
    fn move_placeholder_is_adopted() {
        let (s, acc, inbox, sent) = setup();
        let id = s.insert_header(acc, inbox, 7, &header("m@x", None, "Bonjour", 100), FLAGS, 10).unwrap();
        s.store_body(id, b"raw", &BodyInfo { preview: "p".into(), text: "contenu".into(), has_attachments: false, link_domains: Vec::new(), list_id: None }).unwrap();
        s.move_local(id, sent).unwrap();
        assert!(s.mailbox_uids(inbox).unwrap().is_empty());
        // The destination folder's sync sees the same message arrive with a new UID.
        let same = s.insert_header(acc, sent, 42, &header("m@x", None, "Bonjour", 100), FLAGS, 10).unwrap();
        assert_eq!(same, id);
        assert_eq!(s.body(id).unwrap().as_deref(), Some(&b"raw"[..]));
    }

    #[test]
    fn placeholder_adopts_only_the_same_message() {
        let (s, acc, inbox, sent) = setup();
        let id = s.insert_header(acc, inbox, 7, &header("dup@x", None, "Un", 100), FLAGS, 10).unwrap();
        s.move_local(id, sent).unwrap();
        // Another message sharing the Message-ID (another date) arrives first: a row of its own,
        // the moved one keeps waiting.
        let other = s.insert_header(acc, sent, 41, &header("dup@x", None, "Deux", 200), FLAGS, 10).unwrap();
        assert_ne!(other, id);
        assert_eq!(s.location(id).unwrap().uid, None);
        // Same date and sender, another size: the same message all the same.
        assert_eq!(s.insert_header(acc, sent, 42, &header("dup@x", None, "Un", 100), FLAGS, 12).unwrap(), id);
        assert_eq!(s.location(id).unwrap().uid, Some(42));
    }

    #[test]
    fn mark_all_read_reaches_messages_being_moved() {
        let (s, acc, inbox, sent) = setup();
        let id = s.insert_header(acc, sent, 7, &header("m@x", None, "a", 100), FLAGS, 10).unwrap();
        s.move_local(id, inbox).unwrap();
        assert_eq!(s.mark_account_seen(acc, true).unwrap(), ["INBOX"]);
        // It arrives above the cutoff of the folder's "mark all as read", still unread on the
        // server: the change is sent for it on its own.
        assert_eq!(s.insert_header(acc, inbox, 50, &header("m@x", None, "a", 100), FLAGS, 10).unwrap(), id);
        assert!(s.summary(id).unwrap().flags.seen);
        assert!(s.pending_flag_uids(acc, "INBOX").unwrap().contains(&50));
    }

    #[test]
    fn vanished_folders_hand_their_bodies_over() {
        let (s, acc, inbox, sent) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        let label = s.upsert_mailbox(acc, "Projets", Some("/"), None, true).unwrap();
        let a = s.insert_header(acc, label, 1, &header("p@x", None, "p", 5), FLAGS, 10).unwrap();
        let b = s.insert_header(acc, all, 9, &header("p@x", None, "p", 5), FLAGS, 10).unwrap();
        s.store_body(a, b"raw-p", &raw_info("p")).unwrap();
        assert_eq!(s.retain_mailboxes(acc, &[inbox, sent, all]).unwrap(), 1);
        assert_eq!(s.body(b).unwrap().as_deref(), Some(&b"raw-p"[..]));
        // Same for a folder deleted in facteur.
        let other = s.upsert_mailbox(acc, "Autre", Some("/"), None, true).unwrap();
        let c = s.insert_header(acc, other, 1, &header("q@x", None, "q", 6), FLAGS, 10).unwrap();
        let d = s.insert_header(acc, all, 10, &header("q@x", None, "q", 6), FLAGS, 10).unwrap();
        s.store_body(c, b"raw-q", &raw_info("q")).unwrap();
        s.delete_mailbox(other).unwrap();
        assert_eq!(s.body(d).unwrap().as_deref(), Some(&b"raw-q"[..]));
        assert!(s.missing_bodies(all, 10, u32::MAX, 0).unwrap().is_empty());
    }

    #[test]
    fn pinning_a_labelled_message_changes_its_starred_label() {
        let (s, acc, _, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let starred = s.upsert_mailbox(acc, "[Gmail]/Starred", Some("/"), Some(Role::Flagged), true).unwrap();
        s.set_label_folder(starred, true).unwrap();
        let id = s.insert_header(acc, all, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.set_flags_local(&[id], FlagKind::Flagged, true).unwrap();
        assert_eq!(s.message_labels(&[id]).unwrap().get(&id), Some(&vec![starred]));
        s.set_flags_local(&[id], FlagKind::Flagged, false).unwrap();
        assert!(s.message_labels(&[id]).unwrap().get(&id).is_none_or(|l| l.is_empty()));
    }

    #[test]
    fn gmail_threads_decide_conversations() {
        let (s, acc, _, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let thread = |id| s.summary(id).unwrap().thread_id;
        // Joined by their references, apart for Gmail; a third one Gmail puts with the first.
        let a = s.insert_header(acc, all, 1, &header("a@x", None, "Devis", 1), FLAGS, 10).unwrap();
        let b = s.insert_header(acc, all, 2, &header("b@x", Some("a@x"), "Re: Devis", 2), FLAGS, 10).unwrap();
        let c = s.insert_header(acc, all, 3, &header("c@x", None, "Autre sujet", 3), FLAGS, 10).unwrap();
        assert_eq!(thread(a), thread(b));
        assert_eq!(s.uids_without_gmail_thread(all, 10).unwrap(), [3, 2, 1]);
        let first = thread(a);
        assert!(s.set_gmail_threads(acc, all, &[(1, 100), (2, 200), (3, 100)]).unwrap());
        assert_eq!(thread(a), first, "the oldest message keeps its number");
        assert_eq!(thread(c), first);
        assert_ne!(thread(b), first, "another Gmail thread: another number");
        assert!(s.uids_without_gmail_thread(all, 10).unwrap().is_empty());
        // A later reply referencing both stays where Gmail puts it, and merges neither.
        let d = s.insert_header(acc, all, 4, &header("d@x", Some("b@x"), "Re: Re: Devis", 4), FLAGS, 10).unwrap();
        s.set_gmail_threads(acc, all, &[(4, 200)]).unwrap();
        assert_eq!(thread(d), thread(b));
        assert_eq!(thread(a), first);
        // Gmail's 64 bits come back the same.
        assert!(s.set_gmail_threads(acc, all, &[(1, u64::MAX)]).unwrap());
        assert_ne!(thread(a), first);
    }

    #[test]
    fn settling_releases_only_the_moves_of_those_messages() {
        let (s, acc, _, _) = setup();
        let later = now() + 60;
        let push = |op: &crate::ops::Op| s.push_op(acc, &serde_json::to_string(op).unwrap(), "x", Some(later)).unwrap();
        let mine = push(&crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![3, 4], dest: "Archive".into(), uidvalidity: None });
        let other = push(&crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![5], dest: "Archive".into(), uidvalidity: None });
        let moved: HashSet<(String, u32)> = [("INBOX".to_string(), 4)].into_iter().collect();
        assert_eq!(s.release_moves_of(acc, &moved, &[]).unwrap(), 1);
        let held = |id| s.pending_ops(acc).unwrap().into_iter().find(|p| p.id == id).unwrap().not_before;
        assert_eq!(held(mine), None);
        assert_eq!(held(other), Some(later), "another undo window is left alone");
        // Released behind an action still undoable: that one goes first, the later one waits.
        let after = push(&crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![7], dest: "Archive".into(), uidvalidity: None });
        let mine = push(&crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![6], dest: "Archive".into(), uidvalidity: None });
        let last = push(&crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![8], dest: "Archive".into(), uidvalidity: None });
        let moved: HashSet<(String, u32)> = [("INBOX".to_string(), 6)].into_iter().collect();
        assert_eq!(s.release_moves_of(acc, &moved, &[]).unwrap(), 3);
        assert_eq!((held(other), held(after), held(mine), held(last)), (None, None, None, Some(later)));
        // A screener move (no undo window) into "New senders", behind one: that one goes.
        let undoable = push(&crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![9], dest: "Archive".into(), uidvalidity: None });
        let screened = crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![10], dest: "New".into(), uidvalidity: None };
        s.push_op(acc, &serde_json::to_string(&screened).unwrap(), "x", None).unwrap();
        assert_eq!(s.release_moves_of(acc, &HashSet::new(), &["Elsewhere".to_string()]).unwrap(), 0);
        assert_eq!(s.release_moves_of(acc, &HashSet::new(), &["New".to_string()]).unwrap(), 2);
        assert_eq!((held(undoable), held(last)), (None, None));
    }

    #[test]
    fn refused_moves_are_known_and_retries_remember_their_tries() {
        let (s, acc, inbox, _) = setup();
        let id = s.insert_header(acc, inbox, 5, &header("r@x", None, "r", 1), FLAGS, 10).unwrap();
        let op = crate::ops::Op::MoveSet { mailbox: "INBOX".into(), uids: vec![5], dest: "Junk".into(), uidvalidity: None };
        let op = s.push_op(acc, &serde_json::to_string(&op).unwrap(), "x", None).unwrap();
        let tried = |op| s.pending_ops(acc).unwrap().iter().any(|p| p.id == op && p.tried);
        assert!(!tried(op));
        s.mark_op_started(op).unwrap();
        assert!(tried(op));
        for _ in 0..5 {
            s.fail_op(op, "NO", 5).unwrap();
        }
        // Failed: no longer in flight (the message comes back), but known as refused.
        assert!(s.in_flight_uids(acc, "INBOX").unwrap().is_empty());
        assert_eq!(s.failed_op_uids(acc, "INBOX").unwrap(), [5].into_iter().collect());
        assert_eq!(s.failed_op_ids(&[id]).unwrap(), [id].into_iter().collect());
        // The user retries: attempts start over, the earlier try is remembered.
        s.retry_op(op).unwrap();
        assert!(tried(op));
        assert!(s.failed_op_uids(acc, "INBOX").unwrap().is_empty());
        // A try that failed on the network counts as one too.
        let other = s.push_op(acc, r#"{"kind":"move","mailbox":"INBOX","uid":6,"dest":"Junk"}"#, "x", None).unwrap();
        s.fail_op(other, "cut", 10).unwrap();
        s.retry_op(other).unwrap();
        assert!(tried(other));
    }

    #[test]
    fn rows_added_outside_a_sync_leave_the_server_total_alone() {
        let (s, acc, inbox, _) = setup();
        for uid in [8, 9] {
            s.insert_header(acc, inbox, uid, &header(&format!("{uid}@x"), None, "x", uid as i64), FLAGS, 10).unwrap();
        }
        let mut state = s.mailbox_state(inbox).unwrap();
        state.server_count = Some(10);
        s.save_mailbox_state(&state).unwrap();
        s.set_count_base(inbox, 0, false).unwrap();
        let total = || s.list_mailboxes(acc).unwrap().into_iter().find(|m| m.id == inbox).unwrap().server_total;
        assert_eq!(total(), Some(10));
        // Older history loaded: the server's count already had it.
        s.insert_header(acc, inbox, 2, &header("2@x", None, "x", 2), FLAGS, 10).unwrap();
        s.raise_count_base(inbox, 1).unwrap();
        assert_eq!(total(), Some(10));
    }

    #[test]
    fn gmail_copies_are_found_once() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All Mail", Some("/"), Some(Role::All), true).unwrap();
        let both = s.insert_header(acc, all, 9, &header("m@x", None, "Facture", 100), FLAGS, 10).unwrap();
        let mine = s.insert_header(acc, inbox, 1, &header("m@x", None, "Facture", 100), FLAGS, 10).unwrap();
        // Same Message-ID, another message (another date): both kept.
        let other = s.insert_header(acc, all, 10, &header("m@x", None, "Facture", 50), FLAGS, 10).unwrap();
        let only = s.insert_header(acc, all, 11, &header("n@x", None, "Facture", 10), FLAGS, 10).unwrap();
        let ids = |q: &str, limit: usize| s.search(acc, q, limit, false).unwrap().into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids("facture", 50), [mine, other, only], "the inbox's copy stands for the message");
        assert_eq!(ids("de:françoise", 50), [mine, other, only]);
        assert_eq!(ids("facture", 2), [mine, other]);
        assert_eq!(ids("dans:all facture", 50).len(), 3, "All Mail asked for: its own copies");
        // The body held by the All Mail copy only, the bin and junk searched too: the inbox's
        // copy still stands for the message (the one a server hit is shown as).
        s.store_body(both, b"raw", &BodyInfo { preview: String::new(), text: "Échéance".into(), has_attachments: false, link_domains: Vec::new(), list_id: None }).unwrap();
        let with_bins = s.search(acc, "echeance", 50, true).unwrap().into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(with_bins, [mine]);
        assert_eq!(ids("echeance", 50), [mine]);
    }

    #[test]
    fn a_role_word_also_names_a_plain_folder_called_so() {
        let (s, acc, inbox, _) = setup();
        let junk = s.upsert_mailbox(acc, "[Gmail]/Spam", Some("/"), Some(Role::Junk), true).unwrap();
        let plain = s.upsert_mailbox(acc, "Spam", Some("/"), None, true).unwrap();
        let mut found = s.folders_matching(acc, "spam").unwrap();
        found.sort_unstable();
        assert_eq!(found, [junk, plain]);
        assert_eq!(s.folders_matching(acc, "indésirables").unwrap(), [junk]);
        assert_eq!(s.folders_matching(acc, "boîte").unwrap(), [inbox]);
    }

    #[test]
    fn full_text_search() {
        let (s, acc, inbox, _) = setup();
        let id = s.insert_header(acc, inbox, 1, &header("m@x", None, "Facture d'octobre", 100), FLAGS, 10).unwrap();
        s.store_body(id, b"raw", &BodyInfo { preview: String::new(), text: "Échéance le 31".into(), has_attachments: false, link_domains: Vec::new(), list_id: None }).unwrap();
        assert_eq!(s.search(acc, "factur", 50, false).unwrap().len(), 1);
        assert_eq!(s.search(acc, "echeance", 50, false).unwrap().len(), 1, "accent-insensitive");
        assert_eq!(s.search(acc, "francoise", 50, false).unwrap().len(), 1, "searches the sender");
        assert_eq!(s.search(acc, "\"OR inexistant", 50, false).unwrap().len(), 0);
        // One letter: a whole word, not the prefix of every term; an unfinished operator: nothing.
        assert_eq!(s.search(acc, "e", 50, false).unwrap().len(), 0);
        assert_eq!(s.search(acc, "d", 50, false).unwrap().len(), 1, "\"d'octobre\"");
        assert_eq!(s.search(acc, "is:p", 50, false).unwrap().len(), 0);
        assert_eq!(s.search(acc, "factur is:p", 50, false).unwrap().len(), 1);
        // Operators, alone or with text.
        assert_eq!(s.search(acc, "de:françoise", 50, false).unwrap().len(), 1);
        assert_eq!(s.search(acc, "de:inconnu", 50, false).unwrap().len(), 0);
        assert_eq!(s.search(acc, "avec:f@example.org", 50, false).unwrap().len(), 1, "from them");
        assert_eq!(s.search(acc, "avec:inconnu@x.fr", 50, false).unwrap().len(), 0);
        assert_eq!(s.search(acc, "objet:octobre echeance", 50, false).unwrap().len(), 1);
        assert_eq!(s.search(acc, "objet:octobre pj", 50, false).unwrap().len(), 0, "no attachment");
        assert_eq!(s.search(acc, "non-lu dans:boîte", 50, false).unwrap().len(), 1);
        assert_eq!(s.search(acc, "dans:envoyés", 50, false).unwrap().len(), 0);
        // Bin: left out by default, included on request or when targeted.
        let trash = s.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        s.move_local(id, trash).unwrap();
        assert_eq!(s.search(acc, "factur", 50, false).unwrap().len(), 0);
        assert_eq!(s.search(acc, "factur", 50, true).unwrap().len(), 1);
        assert_eq!(s.search(acc, "factur dans:corbeille", 50, false).unwrap().len(), 1);
        assert_eq!(s.search(acc, "avant:1970-01-01", 50, false).unwrap().len(), 0);
        s.delete_message(id).unwrap();
        assert_eq!(s.search(acc, "factur", 50, false).unwrap().len(), 0);
    }

    #[test]
    fn operators_ignore_accents_and_capitals() {
        let (s, acc, inbox, _) = setup();
        let mut h = header("e@x", None, "RÉUNION d'Été", 100);
        h.from = Some(Address { name: Some("Émilie Durand".into()), email: "emilie@example.org".into() });
        h.to = vec![Address { name: Some("Ærøskøbing Ève".into()), email: "eve@example.org".into() }];
        s.insert_header(acc, inbox, 1, &h, FLAGS, 10).unwrap();
        for q in ["de:Émilie", "de:émilie", "de:emilie", "de:EMILIE", "objet:réunion", "objet:reunion", "objet:été", "à:ève", "avec:eve"] {
            assert_eq!(s.search(acc, q, 50, false).unwrap().len(), 1, "{q}");
        }
        assert_eq!(s.search(acc, "de:emilia", 50, false).unwrap().len(), 0);
        assert_eq!(fold("ÉLODIE Ærø"), "elodie ærø");
        assert_eq!(fold("Plain ASCII"), "plain ascii");
    }

    #[test]
    fn recipient_operators_match_addresses_not_their_json() {
        let (s, acc, inbox, _) = setup();
        let mut h = header("m@x", None, "Bonjour", 100);
        h.to = vec![Address { name: None, email: "paul@x.fr".into() }];
        h.cc = vec![Address { name: Some("Marie Curie".into()), email: "mc@y.fr".into() }];
        s.insert_header(acc, inbox, 1, &h, FLAGS, 10).unwrap();
        // "name", "email", "null" are the JSON's, not the message's.
        for q in ["à:mai", "à:nam", "to:null", "avec:ema", "avec:{"] {
            assert_eq!(s.search(acc, q, 50, false).unwrap().len(), 0, "{q}");
        }
        for q in ["à:paul", "à:x.fr", "à:marie", "à:mc@y", "avec:curie", "avec:paul@x.fr"] {
            assert_eq!(s.search(acc, q, 50, false).unwrap().len(), 1, "{q}");
        }
    }

    #[test]
    fn body_search_finds_copies_in_other_folders() {
        // Gmail: the same message in INBOX and "Work", its body stored under INBOX only.
        let (s, acc, inbox, _) = setup();
        let work = s.upsert_mailbox(acc, "Work", Some("/"), None, true).unwrap();
        let held = s.insert_header(acc, inbox, 1, &header("m@x", None, "Hello", 100), FLAGS, 10).unwrap();
        s.store_body(held, b"raw", &BodyInfo { preview: String::new(), text: "Invoice attached".into(), has_attachments: false, link_domains: Vec::new(), list_id: None }).unwrap();
        let copy = s.insert_header(acc, work, 1, &header("m@x", None, "Hello", 100), FLAGS, 10).unwrap();
        let ids = |q: &str, bins| s.search(acc, q, 50, bins).unwrap().into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids("invoice dans:work", false), [copy]);
        assert_eq!(ids("invoice dans:boîte", false), [held]);
        assert_eq!(ids("hello dans:work", false), [copy], "header words: its own row");
        assert_eq!(ids("invoice", false), [held], "shown once");
        assert_eq!(ids("invoice", true), [held], "no folder restriction: the plain index join");
        assert!(ids("absent dans:work", false).is_empty());
    }

    fn raw_info(text: &str) -> BodyInfo {
        BodyInfo { preview: text.into(), text: text.into(), has_attachments: false, link_domains: vec!["ex.org".into()], list_id: None }
    }

    #[test]
    fn copies_must_match_on_more_than_the_message_id() {
        let (s, acc, inbox, _) = setup();
        let drafts = s.upsert_mailbox(acc, "Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        // Two versions of a draft on the server, one Message-ID.
        let v1 = s.insert_header(acc, drafts, 1, &header("d@x", None, "Brouillon", 10), FLAGS, 100).unwrap();
        let v2 = s.insert_header(acc, drafts, 2, &header("d@x", None, "Brouillon", 10), FLAGS, 140).unwrap();
        s.store_body(v1, b"v1", &raw_info("v1")).unwrap();
        assert_eq!(s.body(v2).unwrap(), None, "never the other version's body");
        assert_eq!(s.missing_bodies(drafts, 10, u32::MAX, 0).unwrap().len(), 1, "v2 still to download");
        // A third row, same Message-ID but another sender: not shared at insert time either.
        let mut forged = header("d@x", None, "Brouillon", 10);
        forged.from = Some(Address { name: None, email: "other@x".into() });
        let f = s.insert_header(acc, inbox, 3, &forged, FLAGS, 100).unwrap();
        assert_eq!(s.body(f).unwrap(), None);
    }

    #[test]
    fn copies_inserted_later_share_the_body_at_once() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.store_body(a, b"raw", &raw_info("Bonjour")).unwrap();
        let b = s.insert_header(acc, all, 9, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        assert!(s.missing_bodies(all, 10, u32::MAX, 0).unwrap().is_empty(), "shared without a folder rescan");
        assert_eq!(s.summary(b).unwrap().preview, "Bonjour");
        assert_eq!(s.body(b).unwrap().as_deref(), Some(&b"raw"[..]));
    }

    #[test]
    fn deleting_the_copy_that_holds_the_body_keeps_it_for_the_others() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        let body_of = |id| s.body(id).unwrap();
        // Gmail archive: the INBOX copy (which holds the body) disappears from INBOX.
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "Facture", 1), FLAGS, 10).unwrap();
        let b = s.insert_header(acc, all, 9, &header("a@x", None, "Facture", 1), FLAGS, 10).unwrap();
        s.store_body(a, b"raw-a", &raw_info("échéance octobre")).unwrap();
        s.delete_uids(inbox, &[1]).unwrap();
        assert_eq!(body_of(b).as_deref(), Some(&b"raw-a"[..]));
        assert!(s.missing_bodies(all, 10, u32::MAX, 0).unwrap().is_empty());
        assert_eq!(s.search(acc, "echeance", 10, false).unwrap().iter().map(|m| m.id).collect::<Vec<_>>(), [b], "indexed text follows");
        assert_eq!(s.similar(&SimilarCriteria { link_domains: vec!["ex.org".into()], ..Default::default() }, 10).unwrap().ids, [b]);

        // Same through a folder emptied or recreated on the server.
        let trash = s.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let c = s.insert_header(acc, trash, 1, &header("c@x", None, "c", 2), FLAGS, 10).unwrap();
        let d = s.insert_header(acc, all, 10, &header("c@x", None, "c", 2), FLAGS, 10).unwrap();
        s.store_body(c, b"raw-c", &raw_info("c")).unwrap();
        s.purge_local(trash).unwrap();
        assert_eq!(body_of(d).as_deref(), Some(&b"raw-c"[..]));
        let e = s.insert_header(acc, inbox, 2, &header("e@x", None, "e", 3), FLAGS, 10).unwrap();
        let f = s.insert_header(acc, all, 11, &header("e@x", None, "e", 3), FLAGS, 10).unwrap();
        s.store_body(e, b"raw-e", &raw_info("e")).unwrap();
        s.clear_mailbox(inbox).unwrap();
        assert_eq!(body_of(f).as_deref(), Some(&b"raw-e"[..]));

        // Gmail archive seen from All Mail: the moved row (placeholder) gives way to the copy
        // already there, and its body goes to that copy.
        let g = s.insert_header(acc, inbox, 3, &header("g@x", None, "g", 4), FLAGS, 10).unwrap();
        let h = s.insert_header(acc, all, 12, &header("g@x", None, "g", 4), FLAGS, 10).unwrap();
        s.store_body(g, b"raw-g", &raw_info("g")).unwrap();
        s.move_local(g, all).unwrap();
        s.drop_orphan_placeholders(all, false).unwrap();
        assert!(s.summary(g).is_err());
        assert_eq!(body_of(h).as_deref(), Some(&b"raw-g"[..]));
    }

    #[test]
    fn copies_left_without_any_body_are_downloaded_again() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        // Shared by an earlier version on the Message-ID alone (another size).
        let b = s.insert_header(acc, all, 9, &header("a@x", None, "a", 1), FLAGS, 99).unwrap();
        s.store_body(a, b"raw", &raw_info("x")).unwrap();
        s.db().execute("UPDATE messages SET has_body = 1 WHERE id = ?1", [b]).unwrap();
        s.delete_uids(inbox, &[1]).unwrap();
        assert_eq!(s.missing_bodies(all, 10, u32::MAX, 0).unwrap().iter().map(|m| m.0).collect::<Vec<_>>(), [b]);
    }

    #[test]
    fn failed_ops_no_longer_hold_messages_back() {
        let (s, acc, inbox, _) = setup();
        let trash = s.upsert_mailbox(acc, "Trash", Some("/"), Some(Role::Trash), true).unwrap();
        let mv = s.push_op(acc, r#"{"kind":"move_set","mailbox":"INBOX","uids":[4],"dest":"Trash"}"#, "", None).unwrap();
        let purge = s.push_op(acc, r#"{"kind":"purge","mailbox":"Trash","below":30}"#, "", None).unwrap();
        s.push_op(acc, r#"{"kind":"purge","mailbox":"Junk","below":90}"#, "", None).unwrap();
        s.push_op(acc, r#"{"kind":"mark_all_seen","mailbox":"INBOX","below":12}"#, "", None).unwrap();
        assert_eq!(s.purge_pending_below(acc, "Trash").unwrap(), Some(30), "this folder's purge only");
        assert_eq!(s.mark_all_seen_below(acc, "INBOX").unwrap(), Some(12));
        let mut folders = s.op_folders(mv).unwrap();
        folders.sort();
        assert_eq!(folders, [inbox, trash], "source and destination to read again");
        assert!(!s.fail_op(mv, "NO", 2).unwrap());
        assert!(s.fail_op(mv, "NO", 2).unwrap(), "failed now");
        s.fail_op(purge, "NO", 1).unwrap();
        assert!(s.in_flight_uids(acc, "INBOX").unwrap().is_empty());
        assert_eq!(s.purge_pending_below(acc, "Trash").unwrap(), None);
    }

    #[test]
    fn undoable_moves_keep_their_placeholder() {
        let (s, acc, inbox, _) = setup();
        let all = s.upsert_mailbox(acc, "[Gmail]/All", Some("/"), Some(Role::All), true).unwrap();
        s.insert_header(acc, all, 5, &header("b@x", None, "b", 2), FLAGS, 10).unwrap();
        let moved = s.insert_header(acc, inbox, 2, &header("b@x", None, "b", 2), FLAGS, 10).unwrap();
        s.move_local(moved, all).unwrap();
        let text = r#"{"kind":"move_set","mailbox":"INBOX","uids":[2],"dest":"[Gmail]/All"}"#.to_string();
        let op = s.push_op(acc, &text, "", Some(now() + 60)).unwrap();
        s.drop_orphan_placeholders(all, false).unwrap();
        assert!(s.summary(moved).is_ok(), "the move can still be undone");
        assert_eq!(s.undo(&[(op, text.clone())], &[(moved, inbox, Some(2))], &[], &[], false).unwrap(), Undone::Done);
        assert_eq!(s.location(moved).unwrap().mailbox_id, inbox);

        // The moved row has gone meanwhile: the undo reports it and changes nothing.
        s.move_local(moved, all).unwrap();
        let op = s.push_op(acc, &text, "", None).unwrap();
        s.delete_message(moved).unwrap();
        assert_eq!(s.undo(&[(op, text)], &[(moved, inbox, Some(2))], &[], &[], false).unwrap(), Undone::TooLate);
        assert_eq!(s.pending_ops(acc).unwrap().len(), 1, "still queued");
    }

    #[test]
    fn arrivals_and_bodiless_among_ids() {
        let (s, acc, inbox, _) = setup();
        // Server arrival (INTERNALDATE) first, the sender's Date when it is unknown.
        let old = s.insert_header(acc, inbox, 1, &HeaderInfo { received_at: Some(100), ..header("a@x", None, "a", 5_000) }, FLAGS, 10).unwrap();
        let new = s.insert_header(acc, inbox, 2, &HeaderInfo { received_at: Some(2_000), ..header("b@x", None, "b", 50) }, FLAGS, 10).unwrap();
        let undated = s.insert_header(acc, inbox, 3, &HeaderInfo { received_at: None, ..header("c@x", None, "c", 3_000) }, FLAGS, 10).unwrap();
        assert_eq!(s.arrived_since(&[old, new, undated], 1_000).unwrap(), HashSet::from([new, undated]));
        assert!(s.arrived_since(&[], 0).unwrap().is_empty());
        s.store_body(new, b"raw", &BodyInfo { preview: String::new(), text: String::new(), has_attachments: false, link_domains: Vec::new(), list_id: None }).unwrap();
        assert_eq!(s.without_body(&[old, new]).unwrap(), HashSet::from([old]));
    }

    #[test]
    fn uid_lookup_among_reads_only_those_asked() {
        let (s, acc, inbox, sent) = setup();
        let a = s.insert_header(acc, inbox, 3, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 4, &header("b@x", None, "b", 2), FLAGS, 10).unwrap();
        s.insert_header(acc, sent, 5, &header("c@x", None, "c", 3), FLAGS, 10).unwrap();
        assert_eq!(s.mailbox_uids_among(inbox, &[3, 5, 9]).unwrap(), HashMap::from([(3, a)]));
        assert!(s.mailbox_uids_among(inbox, &[]).unwrap().is_empty());
    }

    #[test]
    fn usual_authentication_server_walks_the_arrival_index() {
        let (s, ..) = setup();
        let db = s.db();
        let mut st = db.prepare(&format!("EXPLAIN QUERY PLAN {USUAL_AUTHSERV}")).unwrap();
        let plan = st.query_map([1], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect::<Vec<_>>().join(" | ");
        // No sort of the whole account: the 500 latest are read in index order.
        assert!(plan.contains("messages_account_arrival") && !plan.contains("SCAN m"), "{plan}");
        let inner = plan.split("SCAN (subquery").next().unwrap();
        assert!(!inner.contains("TEMP B-TREE"), "only the 500 rows' grouping sorts: {plan}");
        // The inbox pass reads the inbox's rows, not every arrival of the account.
        let mut st = db.prepare(&format!("EXPLAIN QUERY PLAN {USUAL_AUTHSERV_INBOX}")).unwrap();
        let plan = st.query_map([1], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect::<Vec<_>>().join(" | ");
        let inner = plan.split("SCAN (subquery").next().unwrap();
        assert!(inner.contains("messages_box_arrival") && !inner.contains("SCAN m") && !inner.contains("TEMP B-TREE"), "the 500 latest in index order: {plan}");
    }

    #[test]
    fn placeholder_lookups_use_the_message_id_index() {
        let (s, ..) = setup();
        let plan = |sql: &str| -> String {
            let db = s.db();
            let mut st = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            st.query_map([], |r| r.get::<_, String>(3)).unwrap().map(|r| r.unwrap()).collect::<Vec<_>>().join(" | ")
        };
        let adopt = plan("SELECT id FROM messages WHERE account_id = 1 AND message_id = 'x' AND mailbox_id = 1 AND uid IS NULL");
        assert!(adopt.contains("messages_msgid") || adopt.contains("sqlite_autoindex_messages_1"), "{adopt}");
        let copy = plan(&format!(
            "SELECT m.id, (SELECT o.id FROM messages o WHERE {SAME_COPY} AND o.mailbox_id = m.mailbox_id AND o.uid IS NOT NULL LIMIT 1)
             FROM messages m WHERE m.mailbox_id = 1 AND m.uid IS NULL AND m.message_id IS NOT NULL"
        ));
        // (account_id, message_id) or (account_id, date), depending on the statistics: never a
        // pass over the folder for each placeholder.
        assert!(copy.contains("SEARCH o USING INDEX messages_") && !copy.contains("SCAN o"), "{copy}");
        let mailboxes = plan("SELECT (SELECT COUNT(*) FROM messages m WHERE m.mailbox_id = 1 AND m.seen = 0) FROM mailboxes");
        assert!(mailboxes.contains("messages_unread"), "{mailboxes}");
        // At every sync of a server without CONDSTORE: indexes only, no folder rows read.
        let known = plan(&UID_COUNT.replace("?1", "1"));
        assert!(known.contains("COVERING INDEX"), "{known}");
        let unseen = plan(&UID_UNSEEN.replace("?1", "1"));
        assert!(unseen.contains("messages_unread"), "{unseen}");
    }

    #[test]
    fn stats_leave_drafts_out_before_counting_each_message_once() {
        let (s, acc, _, sent) = setup();
        let drafts = s.upsert_mailbox(acc, "Drafts", Some("/"), Some(Role::Drafts), true).unwrap();
        let mut mine = header("m@x", None, "Envoi", 1_000);
        mine.from = Some(Address { name: None, email: "y@example.org".into() });
        // The draft copy comes first (lowest id), the sent message after.
        s.insert_header(acc, drafts, 1, &mine, FLAGS, 10).unwrap();
        s.insert_header(acc, sent, 1, &mine, FLAGS, 10).unwrap();
        let st = s.account_stats(acc, "y@example.org", 2_000).unwrap();
        assert_eq!((st.total, st.sent), (1, 1));
    }

    #[test]
    fn snoozes_belong_to_their_account() {
        let (s, acc, inbox, _) = setup();
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let other = s
            .insert_account(&NewAccount { name: "Z".into(), email: "z@example.org".into(), username: "z".into(), password: String::new(), imap: srv.clone(), smtp: srv, auth: NewAuth::Password })
            .unwrap();
        let other_inbox = s.upsert_mailbox(other, "INBOX", Some("/"), Some(Role::Inbox), true).unwrap();
        let a = s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        let b = s.insert_header(other, other_inbox, 1, &header("b@x", None, "b", 2), FLAGS, 10).unwrap();
        // Thread numbers from before the dedicated counter: the same in both accounts.
        s.db().execute("UPDATE messages SET thread_id = 7 WHERE id IN (?1, ?2)", [a, b]).unwrap();
        s.snooze(acc, 7, &[], 5_000).unwrap();
        let theirs = s.list_threads(&[other_inbox], &[], ThreadFilter::All, false, None, 10, 0).unwrap();
        assert_eq!(theirs[0].snoozed_until, None);
        let mine = s.list_threads(&[inbox], &[], ThreadFilter::All, false, None, 10, 0).unwrap();
        assert_eq!(mine[0].snoozed_until, Some(5_000));
    }

    #[test]
    fn folder_counts_and_cutoffs() {
        let (s, acc, inbox, sent) = setup();
        s.insert_header(acc, inbox, 1, &header("a@x", None, "a", 1), FLAGS, 10).unwrap();
        s.insert_header(acc, inbox, 7, &header("b@x", None, "b", 2), ServerFlags { seen: true, ..FLAGS }, 10).unwrap();
        let boxes = s.list_mailboxes(acc).unwrap();
        let ib = boxes.iter().find(|m| m.id == inbox).unwrap();
        assert_eq!((ib.total, ib.unread), (2, 1));
        assert_eq!(boxes.iter().find(|m| m.id == sent).unwrap().total, 0);
        // UIDNEXT seen earlier lags behind what the cache holds.
        assert_eq!(s.uid_cutoff(inbox).unwrap(), Some(8));
        let mut state = s.mailbox_state(inbox).unwrap();
        state.uidnext = Some(20);
        s.save_mailbox_state(&state).unwrap();
        assert_eq!(s.uid_cutoff(inbox).unwrap(), Some(20));
        assert_eq!(s.uid_cutoff(sent).unwrap(), None);
        assert_eq!(s.mailbox_uid_list(inbox, Some(1)).unwrap(), [7]);
        // An empty list of folders to keep never wipes the account.
        assert_eq!(s.retain_mailboxes(acc, &[]).unwrap(), 0);
        assert_eq!(s.list_mailboxes(acc).unwrap().len(), 2);
    }

    #[test]
    fn event_reminders_fall_due_once() {
        use crate::calendar::{Instance, RemoteCalendar};
        let (s, acc, _, _) = setup();
        let cal = RemoteCalendar { remote_id: "a".into(), name: "a".into(), color: None, read_only: false, enabled: true, ctag: None };
        let (rows, _) = s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal]).unwrap();
        let ev = |uid: &str, start: i64, reminders: Vec<i64>| Instance {
            uid: uid.into(), summary: uid.into(), start, end: start + 3600, busy: true, reminders, ..Default::default()
        };
        let guests = vec![crate::model::EventAttendee { email: "marie@example.org".into(), response: Some("accepted".into()), ..Default::default() }];
        let meeting = Instance { attendees: guests.clone(), meeting: Some("https://meet.google.com/x".into()), ..ev("meeting", 10_000, vec![10, 60]) };
        let declined = Instance { response: Some("declined".into()), ..ev("declined", 10_000, vec![10]) };
        let cancelled = Instance { status: Some("cancelled".into()), ..ev("cancelled", 10_000, vec![10]) };
        s.replace_calendar_events(rows[0].id, acc, "a", &[meeting, declined, cancelled, ev("silent", 10_000, vec![])], None).unwrap();

        // Ten minutes before (9 400) falls in this check; the hour before (6 400) came earlier.
        let due = s.event_alarms(9_000, 9_400).unwrap();
        assert_eq!(due.iter().map(|(e, at)| (e.uid.as_str(), *at)).collect::<Vec<_>>(), [("meeting", 9_400)]);
        let (e, _) = &due[0];
        assert_eq!((e.attendees.clone(), e.meeting.as_deref(), e.reminders.clone()), (guests, Some("https://meet.google.com/x"), vec![10, 60]));
        assert!(s.event_alarms(9_400, 9_900).unwrap().is_empty(), "not twice");
        assert_eq!(s.event_alarms(6_000, 6_400).unwrap().len(), 1);
    }

    #[test]
    fn calendar_events_of_a_window() {
        use crate::calendar::{Instance, RemoteCalendar};
        let (s, acc, _, _) = setup();
        let cal = |id: &str, enabled: bool| RemoteCalendar {
            remote_id: id.into(), name: id.into(), color: Some("#0082c9".into()), read_only: false, enabled, ctag: None,
        };
        let (rows, changed) = s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal("a", true), cal("b", false)]).unwrap();
        assert!(changed);
        assert!(!s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal("a", true), cal("b", false)]).unwrap().1, "the same list again");
        let (a, b) = (rows[0].id, rows[1].id);
        assert_eq!((rows[0].enabled, rows[1].enabled), (true, false));
        let ev = |uid: &str, start: i64, end: i64| Instance {
            uid: uid.into(), instance: String::new(), summary: uid.into(), location: None, description: None,
            start, end, all_day: false, status: None, organizer: None, url: None, busy: true, ..Default::default()
        };
        s.replace_calendar_events(a, acc, "a", &[ev("late", 150, 160), ev("before", 10, 100), ev("across", 90, 120), ev("point", 100, 100), ev("after", 200, 210)], Some("t1")).unwrap();
        s.replace_calendar_events(b, acc, "b", &[ev("hidden", 110, 120)], None).unwrap();
        let uids = |from, to| s.calendar_events(from, to).unwrap().into_iter().map(|e| e.uid).collect::<Vec<_>>();
        // Overlapping [100, 200): ends at 100 is out, starts at 200 is out, zero-length at 100 in;
        // the hidden calendar's events are not listed; by start.
        assert_eq!(uids(100, 200), ["across", "point", "late"]);
        let first = &s.calendar_events(100, 200).unwrap()[0];
        assert_eq!((first.calendar_id, first.account_id), (a, acc));

        // Shown: its events are listed. Hidden: forgotten, fetched again when shown.
        s.set_calendar_enabled(b, true).unwrap();
        assert_eq!(uids(100, 200), ["across", "point", "hidden", "late"]);
        assert_eq!(s.set_calendar_enabled(a, false).unwrap(), acc);
        assert_eq!(uids(0, 1000), ["hidden"]);

        // A calendar gone from the provider goes with its events; the choice to show one stays.
        let (rows, _) = s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal("b", false)]).unwrap();
        assert_eq!(rows.iter().map(|r| (r.id, r.enabled)).collect::<Vec<_>>(), [(b, true)]);
        assert_eq!(s.calendars().unwrap().len(), 1);
        s.replace_calendar_events(b, acc, "b", &[ev("again", 1, 2)], Some("t2")).unwrap();
        assert_eq!(uids(0, 1000), ["again"]);

        // Ignored while a sync was fetching: what that sync brings back is not written, and its
        // events have no calendar left to go to.
        s.set_calendar_ignored(acc, true).unwrap();
        assert_eq!(s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal("b", true)]).unwrap(), (Vec::new(), false));
        assert!(!s.replace_calendar_events(b, acc, "b", &[ev("late", 1, 2)], None).unwrap());
        // Nor once another account's calendar has been handed the freed id.
        let srv = ServerConfig { host: "localhost".into(), port: 143, security: Security::None };
        let other = s
            .insert_account(&NewAccount {
                auth: NewAuth::default(),
                name: "O".into(), email: "other@example.org".into(), username: "o".into(), password: String::new(),
                imap: srv.clone(), smtp: srv,
            })
            .unwrap();
        let taken = s.sync_calendar_list(other, CalendarSource::Caldav, &[cal("c", true)]).unwrap().0;
        assert_eq!(taken[0].id, a, "a freed id handed out again");
        assert!(!s.replace_calendar_events(a, acc, "a", &[ev("late", 1, 2)], Some("stale")).unwrap());
        assert!(!s.set_calendar_error(a, acc, "a", Some("stale")).unwrap());
        assert!(uids(0, 1000).is_empty());
        assert_eq!(s.calendars().unwrap()[0].error, None);
        let ctag: Option<String> = s.read().query_row("SELECT ctag FROM calendars WHERE id = ?1", [a], |r| r.get(0)).unwrap();
        assert_eq!(ctag, None);
        s.set_calendar_ignored(acc, false).unwrap();
        assert_eq!(s.calendars().unwrap().len(), 1);
        assert_eq!(s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal("b", true)]).unwrap().0.len(), 1, "used again");
    }

    #[test]
    fn a_caldav_calendar_is_read_again_only_where_it_changed() {
        use crate::calendar::{Instance, RemoteCalendar};
        let (s, acc, _, _) = setup();
        let cal = RemoteCalendar { remote_id: "https://dav.example/c/".into(), name: "c".into(), color: None, read_only: false, enabled: true, ctag: None };
        let c = s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal]).unwrap().0[0].id;
        let ev = |href: &str, uid: &str, start: i64| Instance {
            uid: uid.into(), summary: uid.into(), start, end: start + 10, busy: true, href: Some(href.into()), ..Default::default()
        };
        let (a, b) = ("https://dav.example/c/a.ics".to_string(), "https://dav.example/c/b.ics".to_string());
        let listed = vec![(a.clone(), "\"1\"".to_string()), (b.clone(), "\"1\"".to_string())];
        let all: HashSet<String> = [a.clone(), b.clone()].into();
        s.update_calendar_objects(c, acc, "https://dav.example/c/", &listed, &all, &[ev(&a, "a", 100), ev(&b, "b", 200)], None).unwrap();
        assert_eq!(s.caldav_etags(c).unwrap().len(), 2);
        let ids = || s.calendar_events(0, 1000).unwrap().into_iter().map(|e| (e.uid, e.id)).collect::<Vec<_>>();
        let before = ids();
        // b changed (a new ETag), a did not: only b is read again; a keeps its row.
        let listed = vec![(a.clone(), "\"1\"".to_string()), (b.clone(), "\"2\"".to_string())];
        let known = s.caldav_etags(c).unwrap();
        let to_read: HashSet<String> = listed.iter().filter(|(h, e)| known.get(h) != Some(e)).map(|(h, _)| h.clone()).collect();
        assert_eq!(to_read, [b.clone()].into());
        let mut moved = ev(&b, "b", 250);
        moved.summary = "b (moved)".into();
        assert!(s.update_calendar_objects(c, acc, "https://dav.example/c/", &listed, &to_read, &[moved], None).unwrap());
        let after = s.calendar_events(0, 1000).unwrap();
        assert_eq!(after[0].id, before[0].1, "a untouched");
        assert_eq!((after[1].summary.as_str(), after[1].start), ("b (moved)", 250));
        assert_eq!(after[1].id, before[1].1, "b read again keeps its row: a form open on it still saves");
        // a deleted on the server: no longer listed, its events go.
        let listed = vec![(b.clone(), "\"2\"".to_string())];
        assert!(s.update_calendar_objects(c, acc, "https://dav.example/c/", &listed, &HashSet::new(), &[], None).unwrap());
        assert_eq!(s.calendar_events(0, 1000).unwrap().iter().map(|e| e.uid.as_str()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(s.caldav_etags(c).unwrap().len(), 1);
    }

    #[test]
    fn a_caldav_calendar_shown_again_is_read_whole() {
        use crate::calendar::RemoteCalendar;
        let (s, acc, _, _) = setup();
        let cal = RemoteCalendar { remote_id: "https://dav.example/c/".into(), name: "c".into(), color: None, read_only: false, enabled: true, ctag: None };
        let c = s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal]).unwrap().0[0].id;
        let listed = vec![("https://dav.example/c/a.ics".to_string(), "\"1\"".to_string())];
        s.update_calendar_objects(c, acc, "https://dav.example/c/", &listed, &HashSet::new(), &[], None).unwrap();
        s.set_setting(&format!("caldav_whole:{c}"), "1792800000").unwrap();
        s.set_calendar_enabled(c, false).unwrap();
        assert!(s.caldav_etags(c).unwrap().is_empty(), "no ETag left to skip its unchanged objects");
        assert_eq!(s.setting(&format!("caldav_whole:{c}")).unwrap(), None);
    }

    #[test]
    fn calendar_events_keep_their_rows() {
        use crate::calendar::{Instance, RemoteCalendar};
        let (s, acc, _, _) = setup();
        let cal = RemoteCalendar { remote_id: "a".into(), name: "a".into(), color: None, read_only: false, enabled: true, ctag: None };
        let a = s.sync_calendar_list(acc, CalendarSource::Google, &[cal]).unwrap().0[0].id;
        let ev = |uid: &str, instance: &str, start: i64| Instance {
            uid: uid.into(), instance: instance.into(), summary: uid.into(), location: None, description: None,
            start, end: start + 10, all_day: false, status: None, organizer: None, url: None, busy: true, ..Default::default()
        };
        assert!(s.replace_calendar_events(a, acc, "a", &[ev("x", "1", 100), ev("x", "2", 200), ev("y", "", 300)], None).unwrap());
        let ids = |s: &Store| s.calendar_events(0, 1000).unwrap().into_iter().map(|e| (e.uid, e.instance, e.id)).collect::<Vec<_>>();
        let before = ids(&s);
        // Fetched again unchanged: nothing written, same rows.
        assert!(!s.replace_calendar_events(a, acc, "a", &[ev("x", "1", 100), ev("x", "2", 200), ev("y", "", 300)], None).unwrap());
        assert_eq!(ids(&s), before);
        // One moved, one gone: the others keep their ids.
        let mut moved = ev("y", "", 300);
        moved.summary = "y (moved)".into();
        moved.busy = false;
        assert!(s.replace_calendar_events(a, acc, "a", &[ev("x", "1", 100), moved], None).unwrap());
        let after = s.calendar_events(0, 1000).unwrap();
        assert_eq!(after.iter().map(|e| e.id).collect::<Vec<_>>(), [before[0].2, before[2].2]);
        assert_eq!((after[1].summary.as_str(), after[1].busy), ("y (moved)", false));
        // An error, then a fetch that goes through: the error goes away.
        assert!(s.set_calendar_error(a, acc, "a", Some("gone")).unwrap());
        assert!(!s.set_calendar_error(a, acc, "a", Some("gone")).unwrap());
        assert_eq!(s.calendars().unwrap()[0].error.as_deref(), Some("gone"));
        assert!(s.replace_calendar_events(a, acc, "a", &[ev("x", "1", 100), ev("y", "", 300)], None).unwrap(), "the error cleared is a change");
        assert_eq!(s.calendars().unwrap()[0].error, None);
    }

    #[test]
    fn what_facteur_may_edit() {
        use crate::calendar::{Instance, RemoteCalendar};
        let (s, acc, _, _) = setup();
        let cal = |id: &str, read_only: bool| RemoteCalendar { remote_id: id.into(), name: id.into(), color: None, read_only, enabled: true, ctag: None };
        let rows = s.sync_calendar_list(acc, CalendarSource::Caldav, &[cal("mine", false), cal("shared", true)]).unwrap().0;
        let (mine, shared) = (rows[0].id, rows[1].id);
        let ev = |uid: &str, own: bool, recurring: bool, href: Option<&str>| Instance {
            uid: uid.into(), start: 100, end: 200, own, recurring, href: href.map(str::to_string), ..Default::default()
        };
        s.replace_calendar_events(mine, acc, "mine", &[
            ev("single", true, false, Some("https://dav.example.org/c/single.ics")),
            Instance { response: Some("needs_action".into()), ..ev("invited", false, false, Some("https://dav.example.org/c/invited.ics")) },
            ev("series", true, true, Some("https://dav.example.org/c/series.ics")),
            ev("nowhere", true, false, None),
        ], None).unwrap();
        s.replace_calendar_events(shared, acc, "shared", &[ev("theirs", true, false, Some("https://dav.example.org/s/a.ics"))], None).unwrap();
        let editable = |s: &Store| {
            let mut v: Vec<_> = s.calendar_events(0, 1000).unwrap().into_iter().filter(|e| e.editable).map(|e| e.uid).collect();
            v.sort();
            v
        };
        // A CalDAV series is edited too; someone else's invitation is answered, not edited.
        assert_eq!(editable(&s), ["series", "single"]);
        let respondable: Vec<_> = s.calendar_events(0, 1000).unwrap().into_iter().filter(|e| e.respondable).map(|e| e.uid).collect();
        assert_eq!(respondable, ["invited"]);
        let invited = s.calendar_events(0, 1000).unwrap().into_iter().find(|e| e.uid == "invited").unwrap();
        s.set_event_response(invited.id, "accepted", false).unwrap();
        assert_eq!(s.event_target(invited.id).unwrap().event.response.as_deref(), Some("accepted"));

        let single = s.calendar_events(0, 1000).unwrap().into_iter().find(|e| e.uid == "single").unwrap();
        let target = s.event_target(single.id).unwrap();
        assert_eq!((target.calendar_remote_id.as_str(), target.source, target.href.as_deref()), ("mine", CalendarSource::Caldav, Some("https://dav.example.org/c/single.ics")));
        assert!(s.calendar_target(shared).unwrap().read_only);

        // Written by facteur, then deleted.
        let id = s.put_calendar_event(mine, &Instance { summary: "new".into(), ..ev("new", true, false, Some("https://dav.example.org/c/new.ics")) }).unwrap();
        assert_eq!(editable(&s), ["new", "series", "single"]);
        assert_eq!(s.put_calendar_event(mine, &Instance { summary: "renamed".into(), ..ev("new", true, false, Some("https://dav.example.org/c/new.ics")) }).unwrap(), id, "the same row");
        s.delete_calendar_event(id).unwrap();
        assert_eq!(editable(&s), ["series", "single"]);
    }

    #[test]
    fn whole_days_read_in_the_local_zone() {
        use crate::calendar::{Instance, RemoteCalendar};
        use chrono::TimeZone;
        let (s, acc, _, _) = setup();
        let cal = RemoteCalendar { remote_id: "a".into(), name: "a".into(), color: None, read_only: false, enabled: true, ctag: None };
        let a = s.sync_calendar_list(acc, CalendarSource::Google, &[cal]).unwrap().0[0].id;
        let day = |d: u32| chrono::Utc.with_ymd_and_hms(2026, 12, d, 0, 0, 0).unwrap().timestamp();
        let local = |d: u32| chrono::Local.with_ymd_and_hms(2026, 12, d, 0, 0, 0).earliest().unwrap().timestamp();
        let e = Instance {
            uid: "noel".into(), instance: String::new(), summary: "Noël".into(), location: None, description: None,
            start: day(24), end: day(26), all_day: true, status: None, organizer: None, url: None, busy: false, ..Default::default()
        };
        s.replace_calendar_events(a, acc, "a", &[e], None).unwrap();
        let got = s.calendar_events(local(25), local(26)).unwrap();
        assert_eq!(got.iter().map(|e| (e.start, e.end)).collect::<Vec<_>>(), [(local(24), local(26))]);
        assert!(s.calendar_events(local(26), local(27)).unwrap().is_empty());
        assert!(s.calendar_events(local(23), local(24)).unwrap().is_empty());
    }

    #[test]
    fn calendar_settings_of_an_account() {
        let (s, acc, _, _) = setup();
        assert_eq!(s.calendar_account(acc).unwrap(), CalendarAccount::default());
        assert!(s.set_calendar_home(acc, Some(("https://dav.example.org/home/", "example.org")), 0).unwrap());
        assert!(s.set_calendar_state(acc, CalendarState::Ok, None, 0).unwrap());
        assert!(!s.set_calendar_state(acc, CalendarState::Ok, None, 0).unwrap(), "unchanged");
        let ok = s.calendar_account(acc).unwrap();
        assert!(ok.last_sync.is_some() && ok.state == Some(CalendarState::Ok));
        assert_eq!(ok.home_site.as_deref(), Some("example.org"));
        assert!(s.set_calendar_state(acc, CalendarState::Error, Some("down"), 0).unwrap());
        let err = s.calendar_account(acc).unwrap();
        assert_eq!((err.last_sync, err.message.as_deref()), (ok.last_sync, Some("down")));
        // A new address forgets the home found before, and moves the generation on.
        s.set_caldav_url(acc, Some("https://cal.example.org/")).unwrap();
        let manual = s.calendar_account(acc).unwrap();
        assert_eq!((manual.caldav_url.as_deref(), manual.home_url.as_deref(), manual.home_site.as_deref(), manual.state), (Some("https://cal.example.org/"), None, None, None));
        assert_eq!(manual.generation, 1);
        // A sync started before the address was saved: what it found is dropped.
        assert!(!s.set_calendar_home(acc, Some(("https://old.example.org/", "example.org")), 0).unwrap());
        assert!(!s.set_calendar_state(acc, CalendarState::Error, Some("old"), 0).unwrap());
        assert_eq!(s.calendar_account(acc).unwrap(), manual);
        assert!(s.set_calendar_home(acc, Some(("https://cal.example.org/home/", "example.org")), 1).unwrap());
        // Partial: some calendars synced, the time moves on.
        assert!(s.set_calendar_state(acc, CalendarState::Partial, Some("calendars not synced: b"), 1).unwrap());
        assert!(s.calendar_account(acc).unwrap().last_sync.is_some());
    }
}
