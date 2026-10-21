# facteur

Desktop mail client: Rust engine, Nuxt interface inside Tauri.

```
crates/core   engine: SQLite cache (FTS5), IMAP sync, SMTP, offline operation queue
crates/cli    command-line test bench for the engine
src-tauri     Tauri application: exposes the engine to the interface
ui            Nuxt 4 + Nuxt UI interface (SPA)
```

## Prerequisites (Linux / WSL)

```bash
sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
pnpm install
```

## Publishing a release

```bash
scripts/release.sh 0.3.0                    # signed facteur.exe, Linux Flatpak, GitHub release
scripts/release.sh 0.3.0 --notes notes.md   # with release notes
scripts/release.sh 0.3.0 --android          # + the signed Android APK
```

Everything is built locally. The script bumps the version, builds `facteur.exe`, signs it with the
update key (`~/.tauri/facteur.key` and its password `~/.tauri/facteur.key.password`, outside the
repository), builds the Flatpak bundle offline as Flathub does (`scripts/flatpak-sources.sh` lists
the crates and pnpm packages from the lockfiles, then `scripts/build-flatpak.sh`: needs
`flatpak-builder`, the GNOME SDK and the `rust-stable` and `node24` extensions it names), writes
`update.json`, then commits, tags
`vX.Y.Z`, pushes and creates the release with `gh`. The `.github/workflows/pages.yml` workflow
(no compilation) copies the latest release's files to https://shu-app.com/facteur/, so that
the updater reads a stable address and the download page needs no GitHub API call. The exe reads
`download/update.json` there, downloads the new exe, checks its signature, replaces itself and
restarts (`src-tauri/src/selfupdate.rs`).

The APK is signed with `~/.tauri/facteur-android.jks` (settings in
`~/.tauri/facteur-android.properties`).

**Back up these keys**: if the update key is lost, already distributed exes can no longer update;
losing the Android key forces users to uninstall the app to move to a new signature.

## Windows executable

From WSL, with Rust (MSVC), Visual Studio Build Tools and Node installed on the Windows side:

```bash
scripts/build-windows.sh          # standalone facteur.exe (~45 MB, embedded interface)
scripts/build-windows.sh --setup  # + NSIS installer
```

The local AI (llama.cpp, built in) also needs LLVM for libclang (`winget install LLVM.LLVM`) and
CMake (the one in Visual Studio's Build Tools is found). The exe only needs WebView2, which ships
with Windows 10/11. Data lives in
`%APPDATA%\dev.facteur.mail`, passwords in Windows Credential Manager.

## Google and Microsoft sign-in (OAuth2)

Gmail and Outlook no longer accept a password in an application: facteur opens the browser on
the provider's page, catches the redirect on a local port and keeps only the refresh token, in
the system keyring.

The application's credentials are published in `crates/core/src/oauth.rs`, as Thunderbird does:
for an installed application they are not secret (every distributed executable contains them);
security relies on PKCE and the local redirect. A fork can use its own by setting
`FACTEUR_GOOGLE_CLIENT_ID`, `FACTEUR_GOOGLE_CLIENT_SECRET` and `FACTEUR_MICROSOFT_CLIENT_ID` at
compile time (in a `.env` at the root for `scripts/build-windows.sh`).

Creating the credentials:

- **Google**: console.cloud.google.com → APIs & Services → enable the Gmail, Google Calendar and
  People APIs → OAuth consent screen (External type; in testing mode, add the testers'
  addresses) → Credentials → Create OAuth client ID of type "Desktop app". Scopes:
  `https://mail.google.com/` (restricted: beyond 100 users, the application must be verified by
  Google), `https://www.googleapis.com/auth/calendar`,
  `https://www.googleapis.com/auth/contacts.readonly`, `openid`, `email`, `profile`.
- **Microsoft**: portal.azure.com → Microsoft Entra ID → App registrations → personal and work
  accounts, "Mobile and desktop applications" platform with the redirect URI
  `http://localhost`. No client secret. Delegated permissions: Office 365 Exchange Online
  `IMAP.AccessAsUser.All` and `SMTP.Send`; Microsoft Graph `Calendars.Read`, and, asked for only
  when used, `Calendars.ReadWrite` and `Contacts.Read`; `offline_access`.

## Running

```bash
pnpm dev          # full application (Tauri + Nuxt with hot reload)
pnpm build        # distributable packages
cargo test -p facteur-core
```

The engine alone, without the interface:

```bash
cargo run -p facteur-cli -- add --name Me --email me@example.com --user me@example.com --pass … \
  --imap imap.example.com:993:tls --smtp smtp.example.com:465:tls
cargo run -p facteur-cli -- sync 1
cargo run -p facteur-cli -- boxes 1
```

## How it works

- **Sync**: per folder, `UID SEARCH ALL` to detect deletions, headers of new messages (the 1000
  most recent on the first pass), flags via CONDSTORE when available.
  Recent bodies are prefetched; the others are downloaded when opened. IDLE on INBOX.
- **Offline**: actions (read, pinned, move, delete, copy to Sent) apply to the cache immediately,
  then are replayed on the server from the `pending_ops` table.
- **Threads**: grouped by `References` / `In-Reply-To`, across all folders.
- **Security**: HTML sanitized (ammonia) then shown in an iframe without scripts; remote images
  blocked by default. Passwords and tokens only in the system keyring (Windows Credential
  Manager, Secret Service, Android Keystore): without one, adding an account fails rather than
  writing a secret to disk.

## Shortcuts

`c` compose · `j`/`k` next/previous thread · `/` search · `e` archive · `#` delete ·
`s` pin · `u` unread · `l` labels (Gmail) · `r` reply · `a` reply all · `f` forward · `⌘/Ctrl+Enter` send

## License

Licensed under either of the following, at your option:

- [MIT](LICENSE-MIT)
- [Apache 2.0](LICENSE-APACHE)

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
facteur by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
