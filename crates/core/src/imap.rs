use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use async_imap::types::{Flag, NameAttribute};
use futures::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, pki_types::ServerName};

use crate::error::{Error, Result};
use crate::model::{Role, Security, ServerConfig};
use crate::store::ServerFlags;

#[derive(Debug)]
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

/// Handed to lettre for SMTP, connected (and encrypted, for implicit TLS) here.
impl lettre::transport::smtp::client::AsyncTokioStream for Stream {
    fn peer_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        match self {
            Stream::Plain(tcp) => tcp.peer_addr(),
            Stream::Tls(tls) => tls.get_ref().0.peer_addr(),
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

pub type Session = async_imap::Session<Stream>;

#[derive(Clone)]
pub enum Credential {
    Password(String),
    Token(String),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Credential::Password(_) => "Credential::Password(…)",
            Credential::Token(_) => "Credential::Token(…)",
        })
    }
}

/// A second challenge after the XOAUTH2 response is an encoded error: answering it empty
/// makes the server state it.
struct XOAuth2<'a> {
    user: &'a str,
    token: &'a str,
    sent: bool,
}

impl async_imap::Authenticator for XOAuth2<'_> {
    type Response = Vec<u8>;

    fn process(&mut self, _challenge: &[u8]) -> Vec<u8> {
        if self.sent {
            return Vec::new();
        }
        self.sent = true;
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token).into_bytes()
    }
}

fn tls_connector() -> TlsConnector {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
    });
    TlsConnector::from(config.clone())
}

pub(crate) async fn tls(host: &str, tcp: TcpStream) -> Result<Stream> {
    let name = ServerName::try_from(host.to_string()).map_err(|e| Error::Tls(e.to_string()))?;
    let s = tls_connector().connect(name, tcp).await.map_err(tls_error)?;
    Ok(Stream::Tls(Box::new(s)))
}

/// A connection the server cut during the handshake is a network failure, tried again on a new
/// one (some servers reset a connection when they get several at once); a certificate refused
/// stays a TLS error, never retried.
fn tls_error(e: std::io::Error) -> Error {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionReset | ConnectionAborted | UnexpectedEof | BrokenPipe => Error::Io(e),
        _ => Error::Tls(e.to_string()),
    }
}

/// Bounded to one minute: a server that accepts the connection then goes silent must not
/// block facteur.
pub async fn connect(server: &ServerConfig, username: &str, credential: &Credential) -> Result<Session> {
    tokio::time::timeout(std::time::Duration::from_secs(60), connect_inner(server, username, credential))
        .await
        .map_err(|_| Error::Other(crate::tr!("{} ne répond plus pendant la connexion", "{} stopped responding while connecting", server.host)))?
}

async fn connect_inner(server: &ServerConfig, username: &str, credential: &Credential) -> Result<Session> {
    let mut steps = crate::perf::Steps::new(|| format!("{} sign-in", server.host));
    // Not even connected (offline, name not found, nothing answering): no session the server
    // dropped, which would count towards offering the single-connection mode.
    let tcp = tokio::time::timeout(std::time::Duration::from_secs(20), tcp_connect(&server.host, server.port))
        .await
        .map_err(|_| Error::Unreachable(crate::tr!("délai dépassé en se connectant à {}", "timed out connecting to {}", server.host)))?
        .map_err(|e| Error::Unreachable(format!("{}: {e}", server.host)))?;
    keepalive(&tcp);
    steps.mark("tcp");

    let mut client = match server.security {
        Security::Tls => async_imap::Client::new(tls(&server.host, tcp).await?),
        Security::None => async_imap::Client::new(Stream::Plain(tcp)),
        Security::StartTls => {
            let mut plain = async_imap::Client::new(Stream::Plain(tcp));
            plain.read_response().await?;
            plain.run_command_and_check_ok("STARTTLS", None).await?;
            let Stream::Plain(tcp) = plain.into_inner() else { unreachable!() };
            // No new greeting after STARTTLS.
            let client = async_imap::Client::new(tls(&server.host, tcp).await?);
            steps.mark("starttls");
            let session = authenticate(client, username, credential).await;
            steps.mark("auth");
            return session;
        }
    };
    steps.mark("tls");
    client.read_response().await?;
    steps.mark("greeting");
    let session = authenticate(client, username, credential).await;
    steps.mark("auth");
    session
}

/// The server's addresses raced in the manner of Happy Eyeballs (RFC 8305): IPv6 and IPv4
/// alternating, one more attempt every 300 ms while the earlier ones go on, the first connected
/// kept. Tried one after the other, an address whose packets are dropped (IPv6 on some networks)
/// would take the whole time allowed.
pub(crate) async fn tcp_connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut addresses = interleaved(tokio::net::lookup_host((host, port)).await?.collect()).into_iter();
    let mut tries = tokio::task::JoinSet::new();
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, format!("no address for {host}"));
    loop {
        if let Some(address) = addresses.next() {
            tries.spawn(TcpStream::connect(address));
        } else if tries.is_empty() {
            return Err(last);
        }
        let more = addresses.len() > 0;
        tokio::select! {
            Some(done) = tries.join_next() => match done {
                Ok(Ok(tcp)) => return Ok(tcp),
                Ok(Err(e)) => last = e,
                Err(e) => last = std::io::Error::other(e),
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)), if more => {}
        }
    }
}

/// The resolver's order, the two families alternating from the first one's.
fn interleaved(addresses: Vec<std::net::SocketAddr>) -> Vec<std::net::SocketAddr> {
    let Some(first) = addresses.first().map(|a| a.is_ipv6()) else { return addresses };
    let (mut same, mut other): (std::collections::VecDeque<_>, std::collections::VecDeque<_>) =
        addresses.into_iter().partition(|a| a.is_ipv6() == first);
    let mut out = Vec::with_capacity(same.len() + other.len());
    while !same.is_empty() || !other.is_empty() {
        out.extend(same.pop_front());
        out.extend(other.pop_front());
    }
    out
}

/// A connection that died silently (laptop asleep, Wi-Fi switched, NAT entry dropped) then
/// fails within a couple of minutes instead of leaving a command, or IDLE, waiting for its own
/// timeout.
fn keepalive(tcp: &TcpStream) {
    use std::time::Duration;
    let params = socket2::TcpKeepalive::new().with_time(Duration::from_secs(60));
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", target_os = "windows"))]
    let params = params.with_interval(Duration::from_secs(15));
    if let Err(e) = socket2::SockRef::from(tcp).set_tcp_keepalive(&params) {
        tracing::debug!("TCP keepalive: {e}");
    }
}

async fn authenticate(client: async_imap::Client<Stream>, username: &str, credential: &Credential) -> Result<Session> {
    let (result, what) = match credential {
        Credential::Password(password) => (client.login(username, password).await, ("identifiant ou mot de passe", "Username or password")),
        Credential::Token(token) => (
            client.authenticate("XOAUTH2", XOAuth2 { user: username, token, sent: false }).await,
            ("jeton de connexion", "Sign-in token"),
        ),
    };
    result.map_err(|(e, _)| match e {
        // A passing refusal (server busy, too many connections) is not the credentials' fault:
        // they must not be set aside waiting for the user.
        async_imap::error::Error::No(msg) | async_imap::error::Error::Bad(msg) if transient_refusal(&msg) => {
            Error::Refused(crate::tr!("connexion refusée pour le moment par le serveur IMAP ({})", "sign-in refused for now by the IMAP server ({})", server_reason(&msg)))
        }
        async_imap::error::Error::No(msg) | async_imap::error::Error::Bad(msg) => {
            let what = if crate::i18n::english() { what.1 } else { what.0 };
            Error::Auth(crate::tr!("{what} refusé par le serveur IMAP ({})", "{what} rejected by the IMAP server ({})", server_reason(&msg)))
        }
        e => e.into(),
    })
}

/// Response codes (RFC 5530) of a refusal for a passing reason. Outlook gives no code: "User
/// is authenticated but not connected" follows an accepted token while its mailbox is briefly
/// out of reach (or one connection too many). Gmail gives its limits as an [ALERT]: too many
/// connections (another app, a phone), or the day's bandwidth used up.
pub(crate) fn transient_refusal(msg: &str) -> bool {
    let upper = msg.to_ascii_uppercase();
    [
        "[UNAVAILABLE]", "[INUSE]", "[LIMIT]", "[SERVERBUG]", "[THROTTLED]", "AUTHENTICATED BUT NOT CONNECTED",
        "TOO MANY SIMULTANEOUS CONNECTIONS", "EXCEEDED COMMAND OR BANDWIDTH LIMITS",
    ]
    .iter()
    .any(|code| upper.contains(code))
}

/// A passing refusal that signing in again clears when it lasts (Outlook's token bound to a
/// mailbox it no longer reaches); waiting is the only cure for the others.
pub(crate) fn cured_by_signing_in(msg: &str) -> bool {
    msg.to_ascii_uppercase().contains("AUTHENTICATED BUT NOT CONNECTED")
}

/// async-imap renders NO/BAD as `code: None, info: Some("[CODE] text")`: keep only the text.
pub(crate) fn server_reason(msg: &str) -> String {
    let info = msg
        .split_once("info: Some(\"")
        .and_then(|(_, rest)| rest.rsplit_once("\")"))
        .map(|(text, _)| text)
        .unwrap_or(msg);
    let text = match info.strip_prefix('[') {
        Some(rest) => rest.split_once("] ").map(|(_, t)| t).unwrap_or(info),
        None => info,
    };
    let text = text.trim().trim_end_matches('.');
    if text.is_empty() { crate::tr!("réponse vide", "empty response") } else { text.to_string() }
}

pub struct ListedMailbox {
    pub name: String,
    pub delimiter: Option<String>,
    pub role: Option<Role>,
    pub selectable: bool,
}

/// Enough to tell a folder has not changed since the last sync without selecting it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderStatus {
    pub messages: Option<u32>,
    pub uidnext: Option<u32>,
    pub uidvalidity: Option<u32>,
    pub highestmodseq: Option<u64>,
    /// Without CONDSTORE, the only sign that a message was read elsewhere.
    pub unseen: Option<u32>,
}

/// For servers that do not give the status with the listing.
pub async fn folder_status_of(session: &mut Session, name: &str, items: &str) -> Result<FolderStatus> {
    let m = session.status(name, items).await?;
    Ok(FolderStatus { messages: Some(m.exists), uidnext: m.uid_next, uidvalidity: m.uid_validity, highestmodseq: m.highest_modseq, unseen: m.unseen })
}

/// [`folder_status_of`] for several folders, their STATUS sent one after the other without
/// waiting for the answers (pipelining, RFC 3501 5.5): one round trip per batch instead of one
/// per folder (37 folders on Outlook: about 4 s). A folder the server refuses is left out.
pub async fn folder_statuses(session: &mut Session, names: &[String], items: &str) -> Result<HashMap<String, FolderStatus>> {
    use async_imap::imap_proto::{MailboxDatum, Response};
    let mut out = HashMap::new();
    for batch in names.chunks(STATUS_BATCH) {
        let mut waiting = HashSet::new();
        for name in batch {
            waiting.insert(session.run_command(format!("STATUS {} {items}", quoted(name))).await?.0);
        }
        while !waiting.is_empty() {
            let Some(response) = session.read_response().await? else {
                return Err(Error::Imap(async_imap::error::Error::ConnectionLost));
            };
            match response.parsed() {
                Response::Done { tag, .. } => {
                    waiting.remove(&tag.0);
                }
                Response::MailboxData(MailboxDatum::Status { mailbox, status }) => {
                    out.insert(mailbox.to_string(), folder_status(status));
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

/// Commands sent ahead of their answers: few enough for any server to queue.
const STATUS_BATCH: usize = 25;

/// An IMAP quoted string.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

pub async fn list_mailboxes(session: &mut Session) -> Result<Vec<ListedMailbox>> {
    Ok(list_with_status(session, None).await?.0)
}

/// `status` needs LIST-STATUS. A folder the server gives no status for is missing from the map
/// and gets selected as usual.
pub async fn list_with_status(session: &mut Session, status: Option<&str>) -> Result<(Vec<ListedMailbox>, HashMap<String, FolderStatus>)> {
    use async_imap::types::UnsolicitedResponse;
    // STATUS answers come through the unsolicited channel, which holds only 100: drain it as the
    // listing goes so none is dropped on accounts with hundreds of labels. Leftovers from earlier
    // commands are discarded first.
    let unsolicited = session.unsolicited_responses.clone();
    while unsolicited.try_recv().is_ok() {}
    let mut statuses = HashMap::new();
    let mut aside = Vec::new();
    let take = |statuses: &mut HashMap<String, FolderStatus>, aside: &mut Vec<UnsolicitedResponse>| {
        while let Ok(response) = unsolicited.try_recv() {
            match response {
                UnsolicitedResponse::Status { mailbox, attributes } => {
                    statuses.insert(mailbox, folder_status(&attributes));
                }
                other => aside.push(other),
            }
        }
    };
    let pattern = match status {
        Some(items) => format!("\"*\" RETURN ({items})"),
        None => "*".to_string(),
    };
    let mut names = Vec::new();
    {
        let mut listing = session.list(Some(""), Some(&pattern)).await?;
        while let Some(name) = listing.try_next().await? {
            names.push(name);
            take(&mut statuses, &mut aside);
        }
    }
    take(&mut statuses, &mut aside);
    // Every server has an INBOX. async-imap ends a listing quietly when the server answers NO or
    // closes the connection: an empty listing is a failure, never an empty account.
    if names.is_empty() {
        return Err(empty_listing(session, aside).await);
    }
    // A listing cut at a line boundary (BYE then a clean close) also ends quietly: the session
    // must still answer for it to be the whole list.
    if let Err(e) = still_open(session).await {
        tracing::info!("folder listing ({} folders) followed by a dead connection: {e}", names.len());
        return Err(Error::Dropped(crate::tr!("la connexion s'est fermée pendant la liste des dossiers", "the connection closed during the folder listing")));
    }
    let listed = names
        .iter()
        .map(|n| {
            let attrs = n.attributes();
            let declared = attrs.iter().find_map(|a| match a {
                NameAttribute::Sent => Some(Role::Sent),
                NameAttribute::Drafts => Some(Role::Drafts),
                NameAttribute::Trash => Some(Role::Trash),
                NameAttribute::Junk => Some(Role::Junk),
                NameAttribute::Archive => Some(Role::Archive),
                NameAttribute::All => Some(Role::All),
                NameAttribute::Flagged => Some(Role::Flagged),
                NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\Important") => Some(Role::Important),
                _ => None,
            });
            // `\NonExistent` (RFC 5258) implies `\NoSelect`: Gmail sends it for its `[Gmail]` container
            // in an extended listing (LIST-STATUS) instead of `\NoSelect`.
            let unselectable = attrs.iter().any(|a| match a {
                NameAttribute::NoSelect => true,
                NameAttribute::Extension(e) => e.eq_ignore_ascii_case("\\NonExistent"),
                _ => false,
            });
            (n.name().to_string(), n.delimiter().map(str::to_string), declared, !unselectable)
        })
        .collect();
    Ok((assign_roles(listed), statuses))
}

fn folder_status(attributes: &[async_imap::imap_proto::StatusAttribute]) -> FolderStatus {
    use async_imap::imap_proto::StatusAttribute as A;
    let mut s = FolderStatus::default();
    for a in attributes {
        match a {
            A::Messages(n) => s.messages = Some(*n),
            A::UidNext(n) => s.uidnext = Some(*n),
            A::UidValidity(n) => s.uidvalidity = Some(*n),
            A::HighestModSeq(n) => s.highestmodseq = Some(*n),
            A::Unseen(n) => s.unseen = Some(*n),
            _ => {}
        }
    }
    s
}

/// async-imap's own `noop` ends quietly on a closed connection; this one fails, proving the
/// session's previous answer was not cut short by the connection going away.
pub async fn still_open(session: &mut Session) -> Result<()> {
    crate::error::bounded(20, "NOOP", async { Ok(session.run_command_and_check_ok("NOOP").await?) }).await
}

/// async-imap returns what it read up to the tagged reply without checking OK/NO/BAD, and ends
/// quietly when the connection closes: a sync taking that for the server's list would wipe the
/// folder's cache. `exists` > 0 (for a search like ALL) makes an empty answer a failure too:
/// once NOOP shows the connection is fine, it is the server refusing this folder's search, so a
/// folder-level NO lets an account sync skip the folder instead of treating it as a drop.
pub async fn uid_search_checked(session: &mut Session, query: &str, exists: u32) -> Result<HashSet<u32>> {
    let uids = session.uid_search(query).await?;
    still_open(session).await?;
    if uids.is_empty() && exists > 0 {
        return Err(Error::Imap(async_imap::error::Error::No(crate::tr!(
            "le serveur n'a renvoyé aucun message d'un dossier qui en compte {}",
            "the server returned no message from a folder holding {}",
            exists
        ))));
    }
    Ok(uids)
}

/// async-imap does not expose `X-GM-THRID`, so the FETCH is sent raw and read here up to its
/// tagged end. Other untagged answers (EXISTS) are dropped: the sync reads the folder state
/// itself.
/// `false` with the list: Gmail refused some of the messages ("Some messages could not be
/// FETCHed"), the others are given.
pub async fn gmail_threads(session: &mut Session, set: &str) -> Result<(Vec<(u32, u64)>, bool)> {
    use async_imap::imap_proto::{AttributeValue, Response, Status};
    let tag = session.run_command(format!("UID FETCH {set} (UID X-GM-THRID)")).await?;
    let mut out = Vec::new();
    loop {
        let Some(answer) = session.read_response().await? else {
            return Err(Error::Imap(async_imap::error::Error::ConnectionLost));
        };
        match answer.parsed() {
            Response::Fetch(_, attrs) => {
                let uid = attrs.iter().find_map(|a| if let AttributeValue::Uid(u) = a { Some(*u) } else { None });
                let thread = attrs.iter().find_map(|a| if let AttributeValue::GmailThrId(t) = a { Some(*t) } else { None });
                if let (Some(uid), Some(thread)) = (uid, thread) {
                    out.push((uid, thread));
                }
            }
            Response::Done { tag: done, status, outcome } if *done == tag => {
                return match status {
                    Status::Ok => Ok((out, true)),
                    Status::No => Ok((out, false)),
                    _ => Err(Error::Imap(async_imap::error::Error::Bad(outcome.information.as_deref().unwrap_or("X-GM-THRID").to_string()))),
                };
            }
            _ => {}
        }
    }
}

/// A command read up to its tagged end. async-imap's own STORE and EXPUNGE streams end quietly on
/// a refusal (NO, BAD) or a connection closed before the end, which an operation would then take
/// for done; here those are errors. Untagged answers are dropped: the sync reads the state.
pub async fn run_checked(session: &mut Session, command: String) -> Result<()> {
    use async_imap::imap_proto::{Response, Status};
    let tag = session.run_command(command).await?;
    loop {
        let Some(answer) = session.read_response().await? else {
            return Err(Error::Imap(async_imap::error::Error::ConnectionLost));
        };
        match answer.parsed() {
            Response::Done { tag: done, status, outcome } if *done == tag => {
                let text = outcome.information.as_deref().unwrap_or_default().to_string();
                return match status {
                    Status::Ok => Ok(()),
                    Status::Bad => Err(Error::Imap(async_imap::error::Error::Bad(text))),
                    _ => Err(Error::Imap(async_imap::error::Error::No(text))),
                };
            }
            Response::Data { status: Status::Bye, .. } => return Err(Error::Imap(async_imap::error::Error::ConnectionLost)),
            _ => {}
        }
    }
}

/// UID SEARCH read up to its tagged end: a refusal or a cut connection is an error even when the
/// right answer could be empty (where [`uid_search_checked`] cannot tell).
pub async fn uid_search_strict(session: &mut Session, query: &str) -> Result<Vec<u32>> {
    use async_imap::imap_proto::{MailboxDatum, Response, Status};
    let tag = session.run_command(format!("UID SEARCH {query}")).await?;
    let mut out = Vec::new();
    loop {
        let Some(answer) = session.read_response().await? else {
            return Err(Error::Imap(async_imap::error::Error::ConnectionLost));
        };
        match answer.parsed() {
            Response::MailboxData(MailboxDatum::Search(uids)) => out.extend(uids.iter().copied()),
            Response::Done { tag: done, status, outcome } if *done == tag => {
                if *status != Status::Ok {
                    return Err(Error::Imap(async_imap::error::Error::No(outcome.information.as_deref().unwrap_or("SEARCH").to_string())));
                }
                return Ok(out);
            }
            Response::Data { status: Status::Bye, .. } => return Err(Error::Imap(async_imap::error::Error::ConnectionLost)),
            _ => {}
        }
    }
}

/// The last untagged BYE, NO or BAD async-imap set aside, if any.
pub fn parting_words(session: &mut Session) -> Option<String> {
    parting_words_in(std::iter::from_fn(|| session.unsolicited_responses.try_recv().ok()))
}

fn parting_words_in(responses: impl Iterator<Item = async_imap::types::UnsolicitedResponse>) -> Option<String> {
    use async_imap::imap_proto::{Response, Status};
    let mut said = None;
    for response in responses {
        if let async_imap::types::UnsolicitedResponse::Other(data) = response
            && let Response::Data { status: Status::Bye | Status::No | Status::Bad, outcome } = data.parsed()
            && let Some(text) = outcome.information.as_deref()
            && !text.trim().is_empty()
        {
            said = Some(text.trim().to_string());
        }
    }
    said
}

async fn empty_listing(session: &mut Session, aside: Vec<async_imap::types::UnsolicitedResponse>) -> crate::Error {
    let said = parting_words_in(aside.into_iter()).or_else(|| parting_words(session));
    let alive = still_open(session).await.is_ok();
    let text = match said {
        Some(text) if !text.is_empty() => text,
        _ if !alive => crate::tr!("la connexion s'est fermée pendant la liste des dossiers", "the connection closed during the folder listing"),
        _ => crate::tr!("le serveur a renvoyé une liste de dossiers vide", "the server sent an empty folder list"),
    };
    tracing::info!("empty folder listing (connection {}): {text}", if alive { "still open" } else { "closed" });
    crate::Error::Dropped(text)
}

/// SPECIAL-USE roles win; a name-based guess only fills a role no folder declares, and only once.
/// Otherwise a plain "Brouillons" folder left by another client would sit next to Gmail's real
/// "[Gmail]/Brouillons" as a second Drafts folder.
fn assign_roles(listed: Vec<(String, Option<String>, Option<Role>, bool)>) -> Vec<ListedMailbox> {
    let declared: HashSet<Role> = listed.iter().filter_map(|(_, _, role, _)| *role).collect();
    let mut guessed = HashSet::new();
    listed
        .into_iter()
        .map(|(name, delimiter, role, selectable)| {
            // With an All Mail (Gmail), archiving is taking the inbox label off: a user label named
            // "Archive" is a label like any other, not where archived mail goes.
            let role = role.or_else(|| {
                guess_role(&name)
                    .filter(|r| !(*r == Role::Archive && declared.contains(&Role::All)))
                    .filter(|r| *r == Role::Inbox || (!declared.contains(r) && guessed.insert(*r)))
            });
            ListedMailbox { name, delimiter, role, selectable }
        })
        .collect()
}

fn guess_role(name: &str) -> Option<Role> {
    let leaf = crate::utf7::decode(name.rsplit(['/', '.']).next().unwrap_or(name)).to_lowercase();
    if name.eq_ignore_ascii_case("INBOX") {
        return Some(Role::Inbox);
    }
    // Common names in several languages, so a known folder is never recreated under another name
    // whatever facteur's language.
    Some(match leaf.as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" | "envoyés" | "envoyes" | "éléments envoyés" | "messages envoyés"
        | "gesendet" | "gesendete objekte" | "gesendete elemente" | "enviados" | "elementos enviados" | "inviata" | "posta inviata"
        | "verzonden" | "verzonden items" => Role::Sent,
        "drafts" | "draft" | "brouillons" | "brouillon" | "entwürfe" | "entwurf" | "borradores" | "bozze" | "concepten" => Role::Drafts,
        "trash" | "bin" | "deleted" | "deleted items" | "deleted messages" | "corbeille" | "éléments supprimés"
        | "messages supprimés" | "papierkorb" | "gelöschte objekte" | "gelöschte elemente" | "papelera" | "elementos eliminados"
        | "cestino" | "posta eliminata" | "prullenbak" | "verwijderde items" => Role::Trash,
        "junk" | "spam" | "junk e-mail" | "junk email" | "junk mail" | "bulk mail" | "courrier indésirable" | "indésirables"
        | "pourriel" | "pourriels" | "junk-e-mail" | "spamverdacht" | "correo no deseado" | "posta indesiderata"
        | "ongewenste e-mail" => Role::Junk,
        "archive" | "archives" | "archiv" | "archivo" | "archivio" | "archief" => Role::Archive,
        "plus tard" | "snoozed" => Role::Snoozed,
        "nouveaux expéditeurs" | "new senders" | "screener" => Role::Screener,
        "refusés" | "refuses" | "screened out" => Role::Refused,
        _ => return None,
    })
}

pub fn server_flags<'a>(flags: impl Iterator<Item = Flag<'a>>) -> ServerFlags {
    let mut f = ServerFlags { seen: false, flagged: false, answered: false, draft: false };
    for flag in flags {
        match flag {
            Flag::Seen => f.seen = true,
            Flag::Flagged => f.flagged = true,
            Flag::Answered => f.answered = true,
            Flag::Draft => f.draft = true,
            _ => {}
        }
    }
    f
}

pub fn uid_set(uids: &[u32]) -> String {
    uid_sets(uids, usize::MAX).pop().unwrap_or_default()
}

/// [`uid_set`] cut into sets of at most about `max` characters: a command line of hundreds of
/// kilobytes is refused by many servers (Dovecot: 64 KiB).
pub fn uid_sets(uids: &[u32], max: usize) -> Vec<String> {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut sets = Vec::new();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        while i + 1 < sorted.len() && sorted[i + 1] == sorted[i] + 1 {
            i += 1;
        }
        let range = if sorted[i] == start { start.to_string() } else { format!("{start}:{}", sorted[i]) };
        if !out.is_empty() && out.len() + 1 + range.len() > max {
            sets.push(std::mem::take(&mut out));
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(&range);
        i += 1;
    }
    if !out.is_empty() || sets.is_empty() {
        sets.push(out);
    }
    sets
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_gmail_label_named_archive_is_no_archive_folder() {
        let listed = |all: bool| {
            let mut l = vec![("INBOX".to_string(), Some("/".to_string()), None, true), ("Archive".to_string(), Some("/".to_string()), None, true)];
            if all {
                l.push(("[Gmail]/All Mail".to_string(), Some("/".to_string()), Some(Role::All), true));
            }
            assign_roles(l).into_iter().find(|m| m.name == "Archive").unwrap().role
        };
        assert_eq!(listed(true), None, "Gmail: archived means out of the inbox, in All Mail");
        assert_eq!(listed(false), Some(Role::Archive), "elsewhere, the folder of that name");
    }

    #[tokio::test]
    async fn failing_to_connect_at_all_is_no_dropped_session() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let server = ServerConfig { host: "127.0.0.1".into(), port, security: Security::None };
        let Err(e) = connect(&server, "u", &Credential::Password("p".into())).await else { panic!("nothing listens") };
        assert!(matches!(e, Error::Unreachable(_)) && !e.is_connection_drop(), "{e:?}");
    }

    #[test]
    fn long_uid_sets_are_cut() {
        assert_eq!(crate::imap::uid_set(&[5, 1, 2, 3, 9]), "1:3,5,9");
        assert_eq!(crate::imap::uid_set(&[]), "");
        let scattered: Vec<u32> = (0..20_000).map(|n| n * 2).collect();
        let sets = uid_sets(&scattered, 8000);
        assert!(sets.len() > 1 && sets.iter().all(|s| s.len() <= 8000));
        assert_eq!(sets.join(","), crate::imap::uid_set(&scattered));
    }

    #[test]
    fn address_families_alternate() {
        let a = |s: &str| s.parse::<std::net::SocketAddr>().unwrap();
        let got = interleaved(vec![a("[2001:db8::1]:993"), a("[2001:db8::2]:993"), a("[2001:db8::3]:993"), a("192.0.2.1:993")]);
        assert_eq!(got, vec![a("[2001:db8::1]:993"), a("192.0.2.1:993"), a("[2001:db8::2]:993"), a("[2001:db8::3]:993")]);
        assert!(interleaved(vec![]).is_empty());
    }

    #[tokio::test]
    async fn raced_connect_reaches_a_listening_address() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_connect("localhost", port).await.is_ok());
        assert!(tcp_connect("127.0.0.1", 1).await.is_err());
    }

    #[test]
    fn a_handshake_cut_is_tried_again_a_refused_certificate_is_not() {
        use std::io::{Error as Io, ErrorKind};
        assert!(tls_error(Io::from(ErrorKind::ConnectionReset)).is_connection_drop());
        assert!(tls_error(Io::from(ErrorKind::UnexpectedEof)).is_connection_drop());
        let refused = tls_error(Io::new(ErrorKind::InvalidData, "invalid peer certificate: UnknownIssuer"));
        assert!(!refused.is_connection_drop() && refused.to_string().starts_with("TLS"));
    }

    #[test]
    fn declared_special_folders_win_over_name_guesses() {
        let row = |name: &str, role: Option<Role>| (name.to_string(), Some("/".to_string()), role, true);
        let roles: Vec<Option<Role>> = assign_roles(vec![
            row("INBOX", None),
            row("Brouillons", None),
            row("[Gmail]/Brouillons", Some(Role::Drafts)),
            row("Archives", None),
            row("Archive", None),
        ])
        .into_iter()
        .map(|m| m.role)
        .collect();
        assert_eq!(roles, [Some(Role::Inbox), None, Some(Role::Drafts), Some(Role::Archive), None], "one folder per role");
    }

    #[test]
    fn special_folders_are_recognised_whatever_their_language() {
        for (name, role) in [
            ("INBOX.Brouillons", Role::Drafts),
            ("Drafts", Role::Drafts),
            ("INBOX/Entwürfe", Role::Drafts),
            ("Junk Mail", Role::Junk),
            ("Indésirables", Role::Junk),
            ("Bin", Role::Trash),
            ("Papelera", Role::Trash),
            ("Plus tard", Role::Snoozed),
            ("Nouveaux exp&AOk-diteurs", Role::Screener),
            ("Screened out", Role::Refused),
            ("Envoy&AOk-s", Role::Sent),
        ] {
            assert_eq!(guess_role(name), Some(role), "{name}");
        }
        assert_eq!(guess_role("Banque"), None);
    }

    #[test]
    fn uid_set() {
        assert_eq!(super::uid_set(&[9, 1, 2, 3, 7, 10]), "1:3,7,9:10");
        assert_eq!(super::uid_set(&[5]), "5");
    }

    #[test]
    fn server_reason_drops_the_code() {
        assert_eq!(
            super::server_reason("code: None, info: Some(\"[AUTHENTICATIONFAILED] Authentication failed.\")"),
            "Authentication failed"
        );
        assert_eq!(super::server_reason("code: None, info: Some(\"LOGIN failed.\")"), "LOGIN failed");
        assert_eq!(super::server_reason("Invalid credentials"), "Invalid credentials");
        assert_eq!(super::server_reason("code: None, info: None"), "code: None, info: None");
    }

    #[test]
    fn a_busy_server_is_not_a_refused_password() {
        assert!(super::transient_refusal("code: None, info: Some(\"[UNAVAILABLE] Try again later\")"));
        assert!(super::transient_refusal("code: None, info: Some(\"[limit] Too many connections\")"));
        assert!(!super::transient_refusal("code: None, info: Some(\"[AUTHENTICATIONFAILED] Authentication failed.\")"));
        assert!(super::transient_refusal("code: None, info: Some(\"User is authenticated but not connected.\")"));
        assert!(super::transient_refusal("code: None, info: Some(\"[ALERT] Too many simultaneous connections. (Failure)\")"));
        assert!(super::transient_refusal("code: None, info: Some(\"[ALERT] Account exceeded command or bandwidth limits. (Failure)\")"));
        assert!(!super::cured_by_signing_in("Too many simultaneous connections"));
    }

    /// A signed-in session on a scripted server: each step answers the next command (`{tag}`
    /// standing for its tag), then closes the connection when its flag says so.
    pub(crate) async fn scripted(script: Vec<(&'static str, bool)>) -> Session {
        scripted_with_log(script).await.0
    }

    /// [`scripted`], with the commands the server received after signing in (tags left out).
    pub(crate) async fn scripted_with_log(script: Vec<(&'static str, bool)>) -> (Session, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received = log.clone();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            w.write_all(b"* OK ready\r\n").await.unwrap();
            let mut signed_in = false;
            for (reply, close) in std::iter::once(("{tag} OK signed in", false)).chain(script) {
                let Ok(Some(line)) = lines.next_line().await else { return };
                let tag = line.split(' ').next().unwrap_or("*").to_string();
                if signed_in {
                    received.lock().unwrap().push(line.split_once(' ').map_or(line.clone(), |(_, c)| c.to_string()));
                }
                signed_in = true;
                let _ = w.write_all(format!("{}\r\n", reply.replace("{tag}", &tag)).as_bytes()).await;
                if close {
                    return;
                }
            }
            while let Ok(Some(_)) = lines.next_line().await {}
        });
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut client = async_imap::Client::new(Stream::Plain(tcp));
        client.read_response().await.unwrap();
        (authenticate(client, "u", &Credential::Password("p".into())).await.unwrap(), log)
    }

    #[tokio::test]
    async fn folder_statuses_are_asked_all_at_once() {
        let (mut session, log) = scripted_with_log(vec![
            ("* STATUS INBOX (MESSAGES 3 UIDNEXT 9 UIDVALIDITY 1 UNSEEN 1)\r\n{tag} OK done", false),
            ("{tag} NO [NONEXISTENT] gone", false),
            ("* STATUS \"Clients/Jill & Joy\" (MESSAGES 0 UIDNEXT 1 UIDVALIDITY 2 UNSEEN 0)\r\n{tag} OK done", false),
        ])
        .await;
        let names = ["INBOX".to_string(), "Old".into(), "Clients/Jill & Joy".into()];
        let found = folder_statuses(&mut session, &names, "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN)").await.unwrap();
        assert_eq!((found["INBOX"].messages, found["INBOX"].unseen), (Some(3), Some(1)));
        assert_eq!(found["Clients/Jill & Joy"].uidvalidity, Some(2));
        assert!(!found.contains_key("Old"), "refused: asked again alone");
        assert_eq!(log.lock().unwrap()[2], "STATUS \"Clients/Jill & Joy\" (MESSAGES UIDNEXT UIDVALIDITY UNSEEN)");
    }

    #[tokio::test]
    async fn a_refused_or_cut_search_is_not_an_empty_folder() {
        let mut ok = scripted(vec![("* SEARCH 3 4\r\n{tag} OK done", false), ("{tag} OK", false)]).await;
        assert_eq!(uid_search_checked(&mut ok, "ALL", 2).await.unwrap(), HashSet::from([3, 4]));
        // Refused: async-imap gives an empty set. The connection still answers, so it is this
        // folder's refusal, which an account sync skips, not a drop.
        let mut refused = scripted(vec![("{tag} NO [UNAVAILABLE] Try again", false), ("{tag} OK", false)]).await;
        let refusal = uid_search_checked(&mut refused, "ALL", 5).await.unwrap_err();
        assert!(matches!(refusal, Error::Imap(async_imap::error::Error::No(_))) && !refusal.is_connection_drop());
        // Cut at a line boundary: an empty set too, even for a folder said to be empty.
        let mut cut = scripted(vec![("* BYE going away", true)]).await;
        assert!(uid_search_checked(&mut cut, "ALL", 0).await.is_err_and(|e| e.is_connection_drop()));
    }

    #[tokio::test]
    async fn gmail_threads_are_read_from_the_raw_answers() {
        let answers = "* 1 FETCH (X-GM-THRID 1278455344230334865 UID 10)\r\n* 4 EXISTS\r\n* 2 FETCH (UID 11 X-GM-THRID 7)\r\n{tag} OK done";
        let (mut session, log) = scripted_with_log(vec![(answers, false)]).await;
        assert_eq!(gmail_threads(&mut session, "10:11").await.unwrap(), (vec![(10, 1278455344230334865), (11, 7)], true));
        assert_eq!(log.lock().unwrap().last().map(String::as_str), Some("UID FETCH 10:11 (UID X-GM-THRID)"));
        let mut refused = scripted(vec![("{tag} BAD unknown item", false)]).await;
        assert!(gmail_threads(&mut refused, "1").await.is_err());
        // Gmail refusing some messages: the others are given, the answer marked partial.
        let partial = "* 1 FETCH (X-GM-THRID 7 UID 10)\r\n{tag} NO Some messages could not be FETCHed (Failure)";
        let mut session = scripted(vec![(partial, false)]).await;
        assert_eq!(gmail_threads(&mut session, "10:11").await.unwrap(), (vec![(10, 7)], false));
    }

    #[tokio::test]
    async fn a_refused_or_cut_store_is_an_error() {
        let mut done = scripted(vec![("* 3 FETCH (FLAGS (\\Seen))\r\n{tag} OK done", false)]).await;
        assert!(run_checked(&mut done, "UID STORE 7 +FLAGS.SILENT (\\Seen)".into()).await.is_ok());
        let mut refused = scripted(vec![("{tag} NO Request is throttled", false)]).await;
        assert!(matches!(run_checked(&mut refused, "UID STORE 7 +FLAGS.SILENT (\\Seen)".into()).await, Err(Error::Imap(async_imap::error::Error::No(_)))));
        // Closed before its end: not done.
        let mut cut = scripted(vec![("* BYE going away", true)]).await;
        assert!(run_checked(&mut cut, "EXPUNGE".into()).await.is_err_and(|e| e.is_connection_drop()));
    }

    #[tokio::test]
    async fn a_strict_search_tells_empty_from_refused() {
        let mut empty = scripted(vec![("* SEARCH\r\n{tag} OK done", false)]).await;
        assert_eq!(uid_search_strict(&mut empty, "DELETED").await.unwrap(), Vec::<u32>::new());
        let mut some = scripted(vec![("* SEARCH 4 9\r\n{tag} OK done", false)]).await;
        assert_eq!(uid_search_strict(&mut some, "DELETED").await.unwrap(), vec![4, 9]);
        let mut refused = scripted(vec![("{tag} NO busy", false)]).await;
        assert!(uid_search_strict(&mut refused, "DELETED").await.is_err());
    }

    #[tokio::test]
    async fn a_folder_listing_cut_after_inbox_is_an_error() {
        let mut cut = scripted(vec![("* LIST () \"/\" INBOX", true)]).await;
        assert!(matches!(list_mailboxes(&mut cut).await, Err(Error::Dropped(_))));
        let mut whole = scripted(vec![("* LIST () \"/\" INBOX\r\n* LIST (\\Sent) \"/\" Sent\r\n{tag} OK done", false), ("{tag} OK", false)]).await;
        assert_eq!(list_mailboxes(&mut whole).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_listing_with_status_gives_each_folder_state() {
        let mut lines = vec![
            "* LIST () \"/\" INBOX".to_string(),
            "* STATUS INBOX (MESSAGES 3 UIDNEXT 9 UIDVALIDITY 7 HIGHESTMODSEQ 41)".to_string(),
            "* LIST (\\HasChildren \\NonExistent) \"/\" \"[Gmail]\"".to_string(),
            "* LIST (\\HasNoChildren \\Important) \"/\" \"[Gmail]/Wichtig\"".to_string(),
        ];
        // More than the 100 unsolicited answers async-imap keeps: none is lost.
        for i in 0..150 {
            lines.push(format!("* LIST () \"/\" \"Label {i}\""));
            lines.push(format!("* STATUS \"Label {i}\" (MESSAGES {i} UIDNEXT 2 UIDVALIDITY 1)"));
        }
        lines.push("{tag} OK done".to_string());
        let reply: &'static str = Box::leak(lines.join("\r\n").into_boxed_str());
        let mut session = scripted(vec![(reply, false), ("{tag} OK", false)]).await;
        let (listed, status) = list_with_status(&mut session, Some("STATUS (MESSAGES UIDNEXT UIDVALIDITY HIGHESTMODSEQ)")).await.unwrap();
        assert_eq!(listed.len(), 153);
        assert!(!listed.iter().find(|l| l.name == "[Gmail]").unwrap().selectable, "\\NonExistent cannot be selected");
        assert_eq!(listed.iter().find(|l| l.name == "[Gmail]/Wichtig").unwrap().role, Some(Role::Important), "Important in German");
        assert_eq!(status.len(), 151);
        assert_eq!(status["INBOX"], FolderStatus { messages: Some(3), uidnext: Some(9), uidvalidity: Some(7), highestmodseq: Some(41), unseen: None });
        assert_eq!(status["Label 149"].messages, Some(149));
        assert_eq!(status["Label 149"].highestmodseq, None);
    }
}
