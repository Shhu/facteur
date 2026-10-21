# Messages, memberships and providers

Design document. Status: **implemented** (October 20, 2026), except the Graph backend (step 6).

Goal: move from "one row per (message, IMAP folder)" to "one message per account, with its
memberships", and route each provider (generic IMAP, Gmail, Microsoft) through a backend of its
own behind a common interface. The work splits into steps that can ship separately (see
[Steps](#steps)).

## Why

### Gmail is modeled backwards

On Gmail, a label is not a folder: it is a property of the message. Yet facteur syncs each label
as an IMAP folder, which creates one row per label for the same message ("copies"). A whole
chunk of the code exists to stitch these copies back together:

- `SAME_COPY` (store.rs:603), the definition of "two rows are the same message" (same
  Message-ID, size, date and sender). It is used in 14 functions.
- Bodies: a single copy holds the blob, the others read it through `SAME_COPY`. Before deleting
  a row, `rehome_bodies` / `fix_bodiless` (store.rs:1577-1675) move the body, the FTS text and
  the links to a surviving copy. `share_known_bodies` is a repair pass.
- `gmail_copies_leaving` (store.rs:1544) hides the copies of a trashed message until their
  folders are resynced.
- `copy_many_local` (store.rs:3331) and `Undo.copies`: from "All Mail", a move becomes a copy;
  otherwise the label is removed.
- Placeholder copies (`uid` NULL) and their adoption (`insert_header_tx`), with
  `drop_orphan_placeholders`, the in-memory `replaced` table and `current_ids`
  (store.rs:3248-3277). They exist because an id changes when a moved row gives way to the copy
  already present.
- Picking the copy to act on: `similar()` (store.rs:2588), `preferred_copy`,
  `one_per_message`, `thread_messages(prefer)`, `flagged_messages`, `received_since`, and
  `SimilarResult.copies` on the interface side.
- `set_flags_local(siblings)`: a flag is written on every copy. `drop_unpinned` mimics the
  Starred folder.
- `labels_of` and `LABELLED` rebuild the labels by cross-checking the copies.
- The server gets queried once per label (see below), and the counters (`coverage`, the sum of
  unread messages in the "mark all as read" menu) count a message several times.

Most of the Gmail bugs in 0.4 and 0.5 came from there: a message left in the list after being
trashed, Important removed by the similar messages screen, a copy duplicated in Receipts, a move
refused after a rowid was reused.

### The number of commands costs more than the volume

Measurements from October 19, 2026 (perf build, first sync after launch):

| Account | Folders | SELECT | UID search | Full sync |
|---|---|---|---|---|
| Gmail A | 13 | 150–340 ms | 280–390 ms | 13 s |
| Gmail B (18 messages) | 8 | 0.3–2.1 s | 0.3–1.7 s | 22 s (5.4 s of which connecting) |
| Gmail C | 23 | **1.5–5.7 s** | **3.3–6.8 s** | > 140 s |
| OVH | 9 | ≈ 120 ms | ≈ 240 ms | 5.6 s |
| Dovecot | ≈ 30 | ≈ 50 ms | ≈ 100 ms | 11 s |

Gmail C's folders hold a few dozen messages, and the database never waits: it is the server that
answers slowly, on every command (probably throttling by Gmail, with other clients connected to
the account). At launch, facteur sends 3 commands per folder (SELECT, `UID SEARCH ALL`, all
flags), because the periodic full check (`FULL_LIST_EVERY`, `FULL_FLAGS_EVERY`,
sync.rs:318-325) is kept in memory only: every restart redoes it everywhere. With 200 labels,
even a fast server would pay for 200 SELECTs.

### Microsoft

Microsoft sign-in, Outlook calendars and Outlook contacts work (OAuth, then IMAP for the mail and
Graph for the rest). Microsoft works with folders (a message is in a single folder), plus
categories (color tags) that are only visible through the Graph API. Outlook.com's IMAP server has
few extensions: no `CONDSTORE`, no `NOTIFY`, no `LIST-STATUS`.

## The target model

### The three cases

| | Where the message is | Tags | Stable identity |
|---|---|---|---|
| Classic IMAP (OVH, Dovecot, Exchange over IMAP) | 1 folder | IMAP keywords at best | (folder, UID), changes on every move |
| Gmail | N labels + Trash/Spam | the labels | `X-GM-MSGID` |
| Microsoft Graph (later) | 1 folder | categories | Graph immutable id |

### A message, its handle, its labels

```
messages        one row per message and per account. (mailbox_id, uid) is its handle: the
                folder through which facteur addresses it to the server. Flags, body, category,
                signals, thread_id: once only. Later gm_msgid / gm_thrid (Gmail).
labels          (mailbox_id, message_id): the message's labels, folders where it is filed
                without a UID of its own. Empty outside Gmail.
mailboxes       + is_label: a label folder lists its messages through `labels` only.
tags            (message_id, tag): IMAP keywords, Outlook categories (later).
```

**Central invariant: every message has exactly one handle**, its `mailbox_id` and `uid`
columns. The schema enforces it by itself: a message has only one row. Every command to the
server goes through the handle.

- Classic IMAP: the handle is its folder, and nothing else changes. Tables, indexes and
  queries stay as they are today.
- Gmail: the handle is in "All Mail", Trash or Spam (the only three synced folders). `INBOX`,
  `[Gmail]/Important`, `Receipts`… are label folders, filled from `X-GM-LABELS`.

*Why not a general memberships table?* It could have been considered, but every IMAP account
would have changed model, with every query and every covering index rewritten, for a case
(several folders per message) that only exists at Gmail. With `labels`, only the view queries
("the messages of folder X", "in the inbox") learn to read labels as well. Measurements on the
real database give identical or faster plans.

What this changes for Gmail:

- **A message's id no longer changes** when its labels change. This removes `replaced`,
  `current_ids`, most of the `settle()` calls, and the adoption of placeholder copies.
- Flags, body, FTS text, links, signals and category exist only once.
- A message's labels are a simple query (`message_labels`), without cross-checking.

Two SQL helpers serve as the foundation (store.rs):

- `Boxes::of(ids)` splits folders into real folders and label folders.
  - `holds(alias)`: "the message is in one of them".
  - `pool(cols)`: the messages of these folders, with the folder each one is listed from (`box`).
- `in_folder_where(alias, cond)`: "the message is in a folder matching `cond`" (a role),
  written as a single list of ids so that SQLite starts from the folders index.

### Threads

- Gmail: `X-GM-THRID` gives the thread. It is converted into a local `thread_id` through a
  mapping table, and threads no longer need to be stitched together as messages arrive.
- Classic IMAP and Microsoft over IMAP: the current `assign_thread`, unchanged (References,
  In-Reply-To, Message-ID).

## The backends

An enum rather than a dynamic trait (async in Rust, few variants):

```rust
enum Provider { Imap(ImapQuirks), Gmail }
```

Detection through the `X-GM-EXT-1` capability (it covers Google Workspace on a custom domain),
no longer through the host or the presence of a folder with the `all` role (`has_all_mail`). It
is stored on the account. Microsoft over IMAP is an `Imap` with its quirks (`ImapQuirks`: no
`CONDSTORE`, rate limiting, Sent saved by the server, which `server_saves_sent` already knows).

Each backend provides three things:

1. **Sync**: which folders to read, how to detect changes, how to fill the memberships.
2. **The plan of an action**: for "move", "archive", "trash", "label", "mark", it returns the
   local changes (memberships, flags) and the operations to queue.
3. **The execution of an operation** from the queue.

The interface, undo and rules speak only in actions and message ids. They no longer need to
know which provider they are talking to.

### Generic IMAP

The current sync, with two improvements useful to everyone:

- **`LIST-STATUS`** (RFC 5819) when the server announces it: a single command gives `MESSAGES`,
  `UIDNEXT`, `UIDVALIDITY` and `HIGHESTMODSEQ` for every folder. Only the folders that changed
  are opened. Without `LIST-STATUS`, one `STATUS` per folder, lighter than a SELECT.
- **Full check** kept in the database (no longer in memory) and done only when needed. With
  `CONDSTORE`, a folder whose UIDNEXT, EXISTS and HIGHESTMODSEQ have not moved has nothing to
  check. The check stays periodic (every 6 h, flags every hour without `CONDSTORE`), and is
  forced after an operation is given up (`recheck_op_folders`).
- Later, `QRESYNC` (Dovecot) to receive deleted UIDs without rereading the list.

Actions: `UID MOVE` (or COPY + EXPUNGE), STORE and EXPUNGE as today. A move updates the handle
in place.

### Gmail

Sync:

- Three folders only: "All Mail", Trash, Spam. Headers are read with
  `X-GM-MSGID X-GM-THRID X-GM-LABELS`.
- Label and flag changes arrive through `CHANGEDSINCE` on "All Mail". **To be checked with a
  test**: Gmail must increment the MODSEQ of a message whose labels change.
- **Complete inbox, whatever the window**: `UID SEARCH X-GM-LABELS \Inbox` on "All Mail" brings
  back inbox messages older than the latest 1000.
- **Sparsely filled open label**: `X-GM-RAW "label:…"`, then the missing headers (the
  equivalent of `load_older` for a label).
- Label counters come from local memberships. Server totals come from a `LIST-STATUS`, which is
  quite likely to be available at Gmail.
- Listening: IDLE on "All Mail" rather than on INBOX (arrivals, sent messages, labels).

Actions on the handle (UID in "All Mail"):

| Action | Gmail |
|---|---|
| Move from X to Y | `+X-GM-LABELS (Y) -X-GM-LABELS (X)` |
| Label Y (new) | `+X-GM-LABELS (Y)` |
| Archive | `-X-GM-LABELS (\Inbox)` |
| Trash | `UID MOVE` to Trash (Gmail then removes all labels) |
| Spam | `UID MOVE` to Spam |
| Restore from trash | `UID MOVE` Trash → INBOX, handle in Trash |
| Pin | `\Flagged` (= Starred at Gmail), no more Starred folder to mimic |
| Delete a label | `DELETE` the label: the messages stay in "All Mail" |

Undo plays the reverse operation on the same ids, and `Undo.copies` goes away. A new queue
operation `Labels { uids, add, remove, uidvalidity }` targets "All Mail".

Drafts and Sent: these are labels (`\Draft`, `\Sent`). `SaveDraft` stays an APPEND to Drafts.
`DeleteDraft` goes through the handle.

### Microsoft

- **First over IMAP**: generic backend, existing OAuth, quirks noted in `ImapQuirks`. Without
  `CONDSTORE`, flags are reread over a window (`RECENT_FLAGS`), as today. Exchange limits the
  rate: fewer simultaneous connections, and no `LIST-STATUS`, hence one `STATUS` per folder.
- **Later, over Graph** if we want categories (into `tags`), immutable ids and per-folder change
  tracking. It is then a third backend, and the Microsoft calendar (ROADMAP) would come with it.

## What goes away

| Today | After |
|---|---|
| `SAME_COPY` and its 14 users | removed |
| `rehome_bodies`, `fix_bodiless`, `share_known_bodies` | one body per message |
| `gmail_copies_leaving(_where)` | trash removes the labels |
| `copy_many_local`, `Undo.copies` | one more label |
| Gmail placeholder copies, `drop_orphan_placeholders` on the Gmail side | a label |
| `replaced`, `current_ids` | stable ids |
| cross-checked `labels_of`, `LABELLED` | `message_labels`, `EXISTS` on `labels` |
| copy choice (`similar`, `preferred_copy`, `one_per_message`, `thread(prefer)`, `SimilarResult.copies`) | a single message |
| `set_flags_local(siblings)`, `drop_unpinned` | single flag |
| deduplication in `account_stats`, `received_since`, `flagged_messages`, `coverage` | not applicable |
| one SELECT per label on every sync | 3 folders (Gmail), `LIST-STATUS` (IMAP) |

## What is affected

Inventory from October 19, 2026, approximate figures:

- **store.rs**:
  - about 200 public functions;
  - about 65 functions touch `mailbox_id` and some forty the UID;
  - 49 queries take the folder's role from the row (`JOIN mailboxes`);
  - 82 tests.
  - Themes: sync by (folder, UID), per-folder counters, thread lists, roles (trust, usual
    senders, screener, drafts), moves and undo, bodies.
- **Operation queue** (ops.rs): every operation targets (folder name, UID, UIDVALIDITY), which
  stays true through the handle. The "in flight" queries (`in_flight_uids`,
  `failed_op_uids`, `pending_flag_uids`, `release_moves_of`, `moves_pending_into`) keep their
  shape. Additions: `Labels`, and per-backend execution.
- **Engine**: `move_inner`, `archive`, `trash`, `report_spam`, `to_inbox`, rules, the
  screener, snooze, `mark_account_read`, drafts, `delete_folder`, `empty_folder`,
  `find_similar`, `search_server` (the Gmail case is already there: "All Mail" only).
- **Interface**:
  - `mailbox_id` on `MessageSummary` / `ThreadSummary` becomes "the folder being viewed from".
  - `ThreadView` already filters by folder (`here`): it will filter by membership.
  - Label chips come directly from the message.
  - `cleanup.vue` loses `copies`.
  - The page for deleting a Gmail folder no longer needs to move messages.
  - A `label:` operator becomes possible.
  - Drag and drop and undo keep the same interface.
- **Backup**: it reads the server, not the cache, so it is not affected. But today it saves a
  Gmail message once per label. A separate step would have it read "All Mail", Trash and Spam,
  with the labels in the index, which restore would reapply.

## Cache migration

On update, with the operation queue empty (flushed beforehand, otherwise the migration waits):

1. **All accounts**: add the `labels` table (empty) and `mailboxes.is_label` (false).
   Nothing else moves: a pure SQL migration, with no change in behavior.
2. **Gmail accounts**, on the first sync with the Gmail backend:
   - the rows of "All Mail", Trash and Spam become the messages (known handles, ids kept);
   - a FETCH `X-GM-MSGID X-GM-THRID X-GM-LABELS` on these handles fills in the exact
     memberships;
   - label rows that have a `SAME_COPY` among them are merged: body, local state and referenced
     ids (reminders, DKIM signals) move to the message;
   - the others (old messages outside the window, present only in a label) are deleted and will
     come back on demand.
   - Snoozes, reminders, screener and `sent_ids` are keyed by RFC Message-ID and do not move.

The database should shrink noticeably on Gmail accounts.

## Steps

Each step can ship on its own and keeps the app usable.

0. **Sync fix** ✅ (October 19, 2026):
   - full check stored in the database (every 6 h; flags every hour without `CONDSTORE`);
   - `LIST-STATUS`, with unchanged folders left closed;
   - `\NonExistent` treated as `\NoSelect`.

   Gmail C goes from more than 140 s to 6.9 s at launch. This work is taken over as is by the
   IMAP backend.
1. **Labels in the store** ✅ (October 19, 2026). `labels` table and `mailboxes.is_label`, then
   the view queries go through `Boxes` and `in_folder_where`:
   - lists (`list_threads`, the "Done" view), folder counters, categories, the folder details;
   - the `dans:` search, the thread seen from a folder;
   - "mark all as read" (locally), the screener (inbox, Sent, screened out);
   - held senders, snooze, drafts, pinned messages, recent recipients.

   No folder is a label yet: behavior does not change. Two tests check each view with label
   folders.

   **Left for step 3**, because they assume Gmail copies and change meaning with the new
   model:
   - `MarkAllSeen` on the handle (the whole of "All Mail");
   - the screener: `moved_back`, `elsewhere`, `held_leftovers`;
   - `VOUCHES`, `received_since`, `USUAL_AUTHSERV`, `account_stats`, `trust_inputs`;
   - the deduplication in `flagged_messages` and `thread_messages`;
   - `labels_of` and `LABELLED`, replaced by `message_labels`.
2. **Provider layer** ✅ (October 19, 2026):
   - the `provider` module with `Provider { Imap, Gmail }`. The provider is detected through
     `X-GM-EXT-1` on every sync and stored on the account (`accounts.provider`); before the
     first connection, an account with "All Mail" is taken for Gmail;
   - `plan_move` decides what a move does: messages moved, copies (the Gmail label added from
     "All Mail"), commands per source folder. `move_inner` now only applies the plan. Rules,
     the screener, archiving, trash and spam all go through it;
   - the other Gmail cases in the engine (flags on all copies, purge, folder details) ask for
     the provider instead of looking for an "All Mail" folder;
   - the `Labels { mailbox, uids, add, remove }` operation (`UID STORE ±X-GM-LABELS`), ready but
     not used yet, with `provider::gmail_label` to name system labels (`\Inbox`, `\Sent`,
     `\Important`…) and the others. **To be checked on Gmail**: personal labels do expect the
     IMAP name (modified UTF-7).

   Behavior does not change: the Gmail plan reproduces today's copies, until step 3.
3. **Gmail backend** (October 19, 2026, in testing):
   - **Conversion** on the first full sync of a Gmail account, only with an empty queue
     (`convert_to_labels`):
     - each copy in a label folder becomes a label of its "All Mail" row, which keeps the body,
       the DKIM verdict and the category of the inbox copy;
     - copies with no row in "All Mail" go away, and the inbox is completed right after;
     - label folder counters are forgotten, a full reread of labels is forced.
   - **Sync**:
     - only "All Mail", Trash and Spam are opened, "All Mail" first;
     - headers and flags arrive with `X-GM-LABELS`;
     - a sync request for a label (or for the inbox) syncs "All Mail";
     - label totals come from `LIST-STATUS`.
   - **Completion**: the inbox, the screener, screened out, "Later" and pinned messages are
     searched through `X-GM-LABELS` (the 1000 most recent each). An open label loads its older
     messages with the same search.
   - **Arrivals**: they are classified by label (inbox: notification; Sent, Drafts, screened
     out: not "received"). A notification opens the thread from the inbox. Recent bodies in
     "All Mail" are prefetched.
   - **Actions**: `provider::plan_labels` reproduces Gmail's "Move to".
     - Trash and spam: change of folder, labels removed.
     - Restoring from the trash: back in "All Mail" with the label.
     - Otherwise: destination label added. "Transit" labels are removed: screener, screened
       out, "Later", the source label, and the inbox when moving from it or from no specific
       folder.
     - The interface passes the source folder (`move_to(…, from)`), undo restores the labels.
     - "Mark all as read" on the inbox targets the server's INBOX folder.
     - Deleting a label leaves its messages in "All Mail".
   - **Queries**: links, recent arrivals and the screener (`elsewhere`) take labels into
     account.

   **Backup** of `facteur.db` before the first launch: `sauvegarde-avant-libelles/`.

   **3d (October 20, 2026)**:
   - Conversion: a failed action no longer blocks it (only actions in progress count).
   - Removed: `copy_many_local` and `Undo.copies`, the Gmail branch of `plan_move` (a Gmail
     account not yet converted refuses moves), `gmail_copies_leaving`,
     `preferred_copy`, `replaced` and the replacement of a moved row by its copy,
     `share_known_bodies`, flags copied onto the copies, the Gmail filter in `similar`.
   - `LABELLED`, `labels_of` and `VOUCHES` read the `labels` table.
   - Starred: pinning adds or removes the Starred label right away; a read of the labels does
     not undo it while the flag is in progress.
   - Statistics: Gmail drafts (label) are excluded. Trust: copies of the same message do not
     count as mail already received.
   - Threads: `X-GM-THRID` read through a raw FETCH (async-imap does not expose it) during the
     "All Mail" sync, 5000 messages per pass; `gmail_threads` table (Gmail thread → local
     number), `messages.gm_thrid`. `assign_thread` no longer merges a thread decided by Gmail.

   **Remaining**: sharing bodies between copies (`SAME_COPY` in `rehome_bodies`,
   `fix_bodiless`, `body()`, `store_body`, full-text search). It is still used for the real
   copies on IMAP servers (a message sent to oneself, in the inbox and in Sent); removing it
   would download their body twice.
4. **Interface** ✅ (October 20, 2026):
   - "Move" stays Gmail's "Move to"; "Labels" (button, L key) ticks and unticks the user's
     labels on the selection or the open thread, without moving it (`set_labels`,
     `provider::plan_relabel`, undoable). A box is half-ticked when only some of the messages
     have the label.
   - The open thread shows its labels as chips, each with a cross to remove it.
   - Labels offered (`Mailbox.user_label`, `provider::user_label`): the user's own only, not the
     inbox, Sent or Important (hidden: Gmail sets it on its own), nor facteur's folders (new
     senders, screened out, "Later").
   - `label:` (and `libellé:`) is a synonym of `dans:`.
   - Label counts already come from local memberships and `LIST-STATUS`.
5. **Microsoft over IMAP** ✅ (October 20, 2026) on Outlook.com: application id, OAuth sign-in,
   sync with `STATUS` folder by folder (neither `LIST-STATUS` nor `CONDSTORE`),
   "User is authenticated but not connected" treated as a temporary refusal, calendar through
   Graph. A Microsoft account opened with another address keeps the address entered (the token
   and the Graph profile only give the identifier). Remaining: a Microsoft 365 account.
6. **Later**: `QRESYNC`, Graph backend for mail (categories, immutable ids). Done in the
   meantime: backup links a message already saved in another folder instead of downloading it
   again, and the Outlook calendar goes through Graph.

## Open questions

- **Important** (`\Important`): a label like any other in the sidebar, or hidden? It serves as
  rank 4 among move targets today.
- **The "All Mail" window**: 1000 messages, like the other folders? The inbox is completed
  separately, but a rarely used label will only show its recent messages until it is opened.
- **The "move" gesture on Gmail from the inbox**: remove `\Inbox` and add Y (the equivalent of
  "Move to" in Gmail), or only add Y ("Label")? The model allows both, and the interface needs
  two distinct gestures.
- **`CHANGEDSINCE` and labels**: to be checked on Gmail before step 3. Otherwise, `X-GM-LABELS`
  has to be reread periodically over a window.
- **Exchange over IMAP**: to be checked on a real account (capabilities, connection limits,
  `MOVE` behavior).
