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

- Done: templates and quick replies, muting a conversation.
- "Done": sort by the date a thread was handled (not only by the date of its last message).

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
- Not yet: reminders set from facteur (the provider's are used), a time zone per event.
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
  llama.cpp's ggml so that the program holds one ggml).
- The Flatpak is built without it for now: llama.cpp needs libclang there (the SDK's LLVM
  extension).
- Next: the graphics card (Vulkan), short reply suggestions, sorting help, search by meaning.

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

## Distribution

- Done: self-updating standalone exe (minisign signature), GitHub releases built locally,
  site on GitHub Pages (Home, Features, What's new, Download pages since 0.7.0).
- Google verification of the application (leaving testing mode): brand verified and published
  (2026-10-21) with the `shhu.github.io` links, which redirect to `shu-app.com`. Left: the
  restricted Gmail scopes' own verification (security assessment); moving the console links to
  `shu-app.com` means verifying that domain in Search Console and a new brand verification.
- Public since 0.8.1 (fresh history, 2026-10-21). Left: `contacts.readonly` on the Google
  consent screen.
- Flathub: app id `com.shu_app.facteur`, offline build ready (`scripts/flatpak-sources.sh`,
  `scripts/flathub-manifest.sh`); submit the manifest to github.com/flathub/flathub, then verify
  the app on shu-app.com. Then the site's Linux section points to Flathub.
