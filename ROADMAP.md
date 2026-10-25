# Roadmap

What remains to be done, by theme. Decisions already made are noted under each item.

## Security and privacy

- **Encrypted backup (optional)**: today the backup is in plain text (`.eml` files in Maildir,
  SQLite index with senders and subjects, visible folder names).
  - Encryption per destination, with a passphrase chosen by the user.
  - Standard [age](https://age-encryption.org) format (key derived with scrypt): one
    `.eml.age` file per message, decryptable without facteur using the `age` tool.
  - Index encrypted too, decrypted only in memory (SQLite `serialize`/`deserialize`).
  - Folder names replaced by identifiers in the directory tree, real names in the index.
  - Check file to verify the passphrase without storing it; in-place conversion of an
    existing backup; clear warning: forgotten passphrase = lost backup.
  - Advanced restore unchanged once the passphrase is entered.
  - To decide: passphrase in the system keyring (automatic backup possible, proposed as the
    default) or asked every time; hiding folder names (proposed: yes).
- **Encrypted local cache**: `facteur.db` holds all mail in plain text (SQLCipher, passphrase at
  startup or in the keyring). Heavier; meanwhile, BitLocker protects the computer.
- **Message encryption**: PGP / S/MIME.
- **App lock**: password or Windows Hello.

## Accounts

- Done: one message per account with its memberships (folders, or Gmail labels ticked from
  the "Labels" button), one backend per provider. Remaining: Microsoft mail through Graph
  (categories): [docs/design/providers.md](docs/design/providers.md).
- **Microsoft sign-in (Outlook, Hotmail)**: done and verified on an Outlook.com account
  (2026-10-20), calendar included. A Microsoft account opened with another address (Gmail…) keeps
  the address entered: neither the token nor the Graph profile gives the mailbox's address. Still
  to try: a Microsoft 365 (work) account.

## Sorting and reading

- Done: templates and quick replies, muting a conversation; the "Done" view sorted by when each
  thread left the inbox (archived in facteur or elsewhere, a label taken off), or by its last
  message when that is later or the thread was archived before facteur kept track.

## Mobile and sync

- Test the Android APK; Google sign-in (redirect) and DNS discovery on mobile.
- Pairing with the PC by QR code (accounts and secrets encrypted).
- Encrypted sync of settings between devices, via a hidden IMAP folder.
- Mobile interface (stacked screens, swipe gestures) and background notifications.
- Signed release APK.

## Calendar and contacts

- Done: Google, CalDAV and Outlook (Graph) calendars; events created, edited and deleted from
  facteur, one date of a series or the whole series (times moved in the series' own zone);
  invitations answered from the calendar. Google calendars are only reread when something
  changed (or every 6 hours, as the window moves).
- Done: address books, read only (Google contacts, Outlook contacts, CardDAV): their names and
  addresses in the composer's suggestions; large CardDAV address books fetched in batches
  (`addressbook-multiget`).
- Done: repeating events created from facteur (every day, week, month or year, every n, until a
  date or so many times); a whole series moved to other days, turned into whole days (or back),
  given another rule or made a single event, and a single event made a series. A rule facteur cannot show (BYSETPOS, several days
  of the month…) is kept as it is; changing it is done in its own calendar app.
- Done: reminders (a notification at the reminders each event has in Google, Outlook or CalDAV,
  "Join" for its video meeting); guests and their answers shown, and invited from facteur
  (Google and Outlook send the invitations; CalDAV through the server's scheduling, or by mail
  when it has none).
- Done: reminders set from facteur (on screen; Outlook keeps one per event, the earliest), and a
  time zone per event: chosen in the form (its times typed in that zone, the computer's time
  shown beside), read from Google, Outlook and CalDAV, shown in the event's card when it differs
  from the computer's. A series moved to another zone is written again in it.
- Done: Outlook calendars only reread when a light listing (identifiers and change dates)
  differs from the last one. Graph's `calendarView/delta` would avoid that listing too, but it
  only exists for the main calendar.

## Local AI (opt-in)

- Done: off by default, turned on in the settings (and off again, the model deleted); always on
  the computer (llama.cpp built in), never through an external server. Models offered by size,
  tested with facteur's instructions: Qwen 3.5 2B ("Light", 1.3 GB), 4B ("Balanced", 2.7 GB)
  and 9B ("Precise", 5.7 GB), Apache 2.0, downloaded once from Hugging Face (pinned revision,
  checked hash), the one fitting the computer's memory pointed out; or any GGUF file of one's own.
  A thread summed up; an appointment found in a message and offered as a calendar event (the
  model copies the email's words for the day and time, facteur reads them: "jeudi à 14h",
  "demain soir vers 19h").
- Done: dictation (Whisper large-v3 turbo, 0.57 GB, turned on apart): a microphone button in
  the composer, the voice transcribed on the computer (whisper.cpp v1.9.5, built against
  llama.cpp's ggml so that the program holds one ggml). The language spoken is found by the model
  or set in the settings, never taken from facteur's own language (forced to another one, Whisper
  translates). Drafts saved while dictating or typing are uploaded once, 10 seconds after the
  last save, instead of at every pause (each upload made Gmail read all of All Mail again); reading a Gmail draft or sent message
  again no longer searches All Mail for the inbox's older messages.
- Done: the graphics card through Vulkan, on Windows and in the Flatpak (the model as far as the card's memory goes,
  the rest on the processor; wholly on the processor without a Vulkan driver, or when the card
  fails a request), dictation included.
- Next: short reply suggestions, sorting help, search by meaning.

## Search

- Done: operators and their values (contacts, folders, dates) offered in the Ctrl+K palette as
  under the search fields; server search shared between three connections on accounts with many
  folders (one on an account limited to a single connection).
- Prefix full-text index (FTS5 `prefix`): only if searches show up slow in the diagnostics.
  Words are already matched as prefixes ("fact" finds "facture"); the index would mostly speed up
  two- or three-letter words, at the cost of rebuilding the whole full-text table once (a long
  first start) and a larger database.

## Robustness

- Done: **an OVH account dropping facteur's sessions**: silent drops of listening and sync, on any
  folder. Not caused by Spark; with the single-connection mode turned on (October 2026) the drops
  stopped: a low session cap on that OVH offer (MXPLAN 1). facteur now offers that mode by itself
  when an account's server keeps dropping its connections (not the network, not a wake from
  sleep).
- Done: the size the SMTP server announces (`SIZE`) checked before sending (and given with the
  message, so a server refuses it before receiving it); queued operations' labels kept in both
  languages; calendar and address book problems written again when the language changes;
  correspondents of a removed account gone from the composer's suggestions.
- Done: errors of queued operations kept in both languages too (the server's own words as they
  came).
- Done (October 2026): an audit in ten domains (sync, queue, concurrency, SQLite, network,
  interface, hostile mail, MIME, calendars, lifecycle), 44 findings, all fixed with their tests.
  The last three since: the composer writes a long reply's HTML out after a pause or when saving
  or sending, not at every key; a CalDAV calendar is read again only for the objects whose ETag
  changed, in batches (no answer of the whole calendar, no 32 MiB limit); the one-time Gmail
  label conversion goes in short steps, and resumes when cut.
- Done (October 2026): a second audit in the same ten domains, 38 findings, all fixed, with
  regression tests where a test can reach them. Among them: the local AI in a process of its own
  (`facteur --ai-worker`: a graphics driver failing ends it, not facteur, and the processor takes
  over), mail text never read as the model's control tokens and the summary in bullet points
  only (checked against the real model with injected mails), crafted invitations bounded, sends
  made offline kept waiting instead of given up, long jobs giving the one connection over between
  folders, queued operations finished only if still theirs.
- Done (October 2026): a third audit in the same ten domains, 32 findings, all fixed, with
  regression tests where a test can reach them. Among them: an account's addresses raced IPv6
  and IPv4 (Happy Eyeballs) for IMAP and SMTP, a stalled SMTP server no longer holding a send
  for good, a long backup or history fetch giving the one connection over to the sync, the
  listening connections logged out of at quit, the local AI stopped at once (turned off, at
  quit, or hung past 15 minutes) and a crash of the graphics card remembered across launches,
  CalDAV events read as stored (their zone kept), alarms facteur does not show (by mail, at the
  end) kept when the reminders change, crafted subjects and invitations bounded, scripts in SVG
  and Python attachments never opened from facteur, large folders deleted in steps, the
  database compacted with reads going on, the full-text index checked after the window shows,
  and an Outlook account's folders checked a few per pass rather than all at each launch.
- Done (October 2026): a fourth audit, 37 findings, all fixed, with regression tests where a test
  can reach them. Among them: crafted invitations bounded for good (periods without a day,
  zones of many observances, occurrences and texts per object), CalDAV series with rules read
  here in part (BYSETPOS) expanded by the server again, a reminder change no longer mailing the
  guests, alarms by date kept, Google default mail reminders kept, departures searched strictly
  and only when cheaper than the whole list, the folders' STATUS asked at once with a fallback
  for slow servers, a folder's unread count differing from STATUS rereading its flags, failed
  draft saves superseded, undo refused once a replay was tried, model downloads resumed across
  tries, the local AI refused on processors without AVX2 (instead of crashing and blaming the
  card), dictation falling back to the processor, the search index checked without holding the
  writer, contacts counted again for the people of a removed account, and the list keeping the
  rows that did not change.
- Done (October 2026): a fifth and last audit, 21 findings, all fixed, with regression tests where
  a test can reach them. Among them: a draft reopened before its upload shows the version saved,
  the old copy of a reopened draft deleted only once its new version is in, a crafted subject of
  reply prefixes read in linear time, the unread count checked at each sync through indexes only,
  a removal of an account cut halfway still forgetting its people, a model download kept on a
  passing server error and its part hashed off the async workers, an SMTP certificate refused
  told as such, a CalDAV series given another rule telling its guests, Google and Outlook series
  rebuilt without losing rooms and reminders, guests past the 200 shown kept and told, past
  overrides no longer hiding a series' current occurrences, restore never expunging other
  clients' deleted messages, links whose text is hidden by colour, size, position or generated
  by a style asking first, the message list no longer rebuilt at each sync, the microphone
  released when audio fails to start, forced flag checks not waiting their turn, AI requests left
  by the interface not run, dictation going before them, and no AI process started after quit.
- Done (October 2026): a deep pass by fifteen specialists, 51 findings, all fixed, with
  regression tests where a test can reach them. Among them: crafted formatting tags no longer
  blowing up the HTML parser, a charset given only in the HTML (or none) read right, replies to
  an original never received kept in one thread, Gmail server searches in a label read through
  All Mail, a message restored from the bin into a label kept while its move waits, a Gmail
  account without All Mail in IMAP moving its mail, arrivals left to the sync by server search, a
  flag changed during the server's flag read kept, a send that may have gone given a new
  Message-ID when edited even after Retry, sends retried with a fresh offline count, connect
  failures no longer counted as dropped sessions, autoconfig refusing https-to-http redirects,
  keychain reads bounded and not asked again every minute, the search index rebuilt from the
  messages, indexes rebuilt after a damage report, a backup index's damage kept apart from the
  database, a refused folder no longer stopping a backup, weekly series without a named day no
  longer rebuilt on another day, an event saved again after a failure taking the new version,
  CalDAV calendars changing often still read whole as the window moves, sender DNS records looked
  up only for trusted senders, reordered (bidi) link text and blocked images' alt text checked,
  long program extensions blocked, the open thread and short lists no longer re-rendered at each
  sync, read changes and moves reflected past the first page, the single-instance lock in the
  Flatpak, and the local AI's download and turn-off races.
- Done (October 2026): a seventh pass by the same fifteen specialists, 49 findings, all fixed,
  thirteen of them gaps in the sixth pass's own fixes, with regression tests where a test can
  reach them. Among them: a damaged unique index no longer keeping facteur from starting (its
  duplicates removed, a failing REINDEX offered as damage), the search index rebuild resumable and
  safe beside the sync, the HTML parser guard bounding formatting tags left open times what
  follows, links whose image or leading symbols hid the domain checked again, Gmail's sent
  message no longer taken for a deleted draft, the calendar re-read whole when shown again, an
  occurrence edited from a series moved once, alarms at a date ringing once, weekly series
  starting on a chosen day, bottom-posted replies kept for the AI, blank text parts falling back
  to the HTML, a Gmail label named Archive left a label, history fetch not re-syncing All Mail per
  label, keychain and Outlook sign-in retries, scheduled sends waited for at quit, and the count
  of a folder kept right when mail arrives during its sync.
- Done (October 2026): an eighth pass by the same fifteen specialists, 40 findings, each checked
  again by a reviewer asked to refute it, all fixed test first. Among them: the HTML guard
  reading tags as the parser does (closing tags in attributes, comments or styles no longer
  counted, tag names read once, nesting and stray end tags bounded), link text compared on every
  domain it shows, a Gmail account whose All Mail is hidden from IMAP going back to folders, mail
  sent from Gmail making correspondents and contacts (and leaving with its account), an
  unreadable flag answer no longer stopping an account's sync, server search syncing the folders
  of recent matches, a draft reopened after its upload showing the version saved, no Drafts
  folder created before the folder list, follow-ups kept through out-of-office replies, a folder
  locked for a while failing the backup rather than skipped for a day, a restore cut by a quit
  going on in the same folder, the occurrence opened keeping its own day in a series change,
  alarms at a time on whole days, the calendar token of a removed account forgotten, installing
  one AI model keeping the other's cut download, and the message list and sidebar no longer
  rebuilt whole during a sync.
- Done (October 2026): a ninth pass by the same fifteen specialists, 36 findings, each checked
  again by a reviewer asked to refute it, all fixed test first. Among them: the duplicate cleanup
  after a damage report taking bodies and labels with the copies, a full disk or busy file at
  that check no longer offered as damage, a folder left as a bare name on the server emptied, a
  cached message with unreadable flags found and left out, Gmail's Important label known in every
  language, a draft deleted or saved again while a save or deletion of it waits staying as the
  user left it, an operation being sent no longer said discarded, accented recipient domains sent
  in ASCII, the machine's name kept out of EHLO, invitations that may have gone said so, a reply
  sent late flagging only the message it answers, executables in archives and PDF actions found
  however hidden, an attribution-heavy mail no longer slow to open, ordinary unclosed formatting
  kept, a restore to the original folders skipping what moved since the backup, a failed
  self-update restart putting the previous exe back, all month names and abbreviations read for
  appointments, and a model download stopping at once when asked.

## Distribution

- Done: self-updating standalone exe (minisign signature), GitHub releases built locally,
  site on GitHub Pages (Home, Features, What's new, Download pages since 0.7.0).
- Google verification of the application (leaving testing mode): brand verified and published
  (2026-10-21) with the `shhu.github.io` links, which redirect to `shu-app.com`. Left: the
  restricted Gmail scopes' own verification (security assessment); moving the console links to
  `shu-app.com` means verifying that domain in Search Console and a new brand verification.
- Public since 0.8.1 (fresh history, 2026-10-21). Left: `contacts.readonly` on the Google
  consent screen.
- Linux: signed Flatpak repository on shu-app.com, shared with Atelier (`flatpak/publish.sh` in
  Shhu/shhu.github.io, run by release.sh; key in `~/.tauri/shu-flatpak-gpg`). Flathub set aside:
  its rules exclude AI-written manifests and submissions, and young projects.
