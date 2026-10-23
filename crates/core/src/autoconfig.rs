//! Server discovery, in the order of the major mail clients: the provider's autoconfig,
//! Mozilla's ISPDB, MX -> the host's ISPDB entry, RFC 6186 SRV records, then guessing with a
//! real connection. Only the address's domain leaves the machine, as in Thunderbird.

use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesOrdered;
use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

use crate::error::{Error, Result};
use crate::imap::{Stream, tls};
use crate::model::{Security, ServerConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discovered {
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    /// The provider may require the local part alone.
    pub username: String,
    pub source: Source,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Autoconfig,
    Ispdb,
    Mx,
    Srv,
    Guess,
}

const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn discover(email: &str) -> Result<Discovered> {
    let email = email.trim();
    let domain = email
        .rsplit_once('@')
        .map(|(_, d)| d.trim().to_ascii_lowercase())
        .filter(|d| !d.is_empty() && d.contains('.'))
        .ok_or_else(|| Error::Other(crate::tr!("adresse invalide", "invalid address")))?;

    let http = http_client()?;

    // Trust order: the provider over https, then Mozilla's ISPDB, and only then the file served
    // in plain text (forgeable on public Wi-Fi: kept only if it names servers of the domain
    // itself, with encryption).
    let ((secure, plain), central) = tokio::join!(autoconfig_direct(&http, &domain, email), ispdb(&http, &domain, email));
    let plain = plain.filter(|f| {
        let ours = |host: &str| registrable(&host.to_ascii_lowercase()) == registrable(&domain);
        ours(&f.imap.host) && ours(&f.smtp.host) && f.imap.security != Security::None && f.smtp.security != Security::None
    });
    if let Some(found) = secure.or(central).or(plain) {
        return Ok(found);
    }

    let resolver = TokioResolver::builder_tokio().ok().and_then(|b| b.build().ok());

    // The MX says who really hosts this domain's mail.
    if let Some(resolver) = &resolver {
        for candidate in mx_provider_domains(resolver, &domain).await {
            if candidate == domain {
                continue;
            }
            if let Some(mut found) = ispdb(&http, &candidate, email).await {
                found.source = Source::Mx;
                return Ok(found);
            }
        }
    }

    // SRV (RFC 6186): a target outside the domain is not kept: a forged DNS answer would
    // otherwise send the password to another server.
    let (mut imap, mut smtp) = match &resolver {
        Some(r) => srv(r, &domain).await,
        None => (None, None),
    };
    let in_domain = |c: &Option<ServerConfig>| c.as_ref().is_some_and(|c| registrable(&c.host.to_ascii_lowercase()) == registrable(&domain));
    if !in_domain(&imap) {
        imap = None;
    }
    if !in_domain(&smtp) {
        smtp = None;
    }
    // As soon as one half comes from guessing, the guess is what the user is shown.
    let source = if imap.is_some() && smtp.is_some() { Source::Srv } else { Source::Guess };

    if imap.is_none() || smtp.is_none() {
        let (gi, gs) = tokio::join!(
            async { if imap.is_none() { guess_imap(&domain).await } else { None } },
            async { if smtp.is_none() { guess_smtp(&domain).await } else { None } }
        );
        imap = imap.or(gi);
        smtp = smtp.or(gs);
    }

    match (imap, smtp) {
        (Some(imap), Some(smtp)) => Ok(Discovered { imap, smtp, username: email.to_string(), source, provider: None }),
        _ => Err(Error::NotFound(crate::tr!("aucun serveur de courrier trouvé pour {domain}", "no mail server found for {domain}"))),
    }
}

fn http_client() -> Result<reqwest::Client> {
    // reqwest is built without a default crypto provider: install the engine's.
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("facteur")
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

async fn fetch_config(http: &reqwest::Client, url: &str, email: &str) -> Option<Discovered> {
    let resp = http.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    let found = parse_autoconfig(&text, email);
    if found.is_some() {
        debug!(url, "autoconfig found");
    }
    found
}

/// Returns the https result and, separately, the plain-text one (many ISPs only serve it that
/// way; Thunderbird accepts it).
async fn autoconfig_direct(http: &reqwest::Client, domain: &str, email: &str) -> (Option<Discovered>, Option<Discovered>) {
    let encoded = urlencode(email);
    let urls = [
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={encoded}"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml?emailaddress={encoded}"),
        format!("http://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={encoded}"),
    ];
    // In parallel: a host that does not answer must not delay the others.
    let mut results = futures::future::join_all(urls.iter().map(|url| fetch_config(http, url, email))).await;
    let tag = |found: Option<Discovered>| {
        found.map(|mut f| {
            f.source = Source::Autoconfig;
            f
        })
    };
    let plain = tag(results.pop().flatten());
    (tag(results.into_iter().flatten().next()), plain)
}

/// Only the domain goes in the URL; the address only fills the username template locally.
async fn ispdb(http: &reqwest::Client, domain: &str, email: &str) -> Option<Discovered> {
    let url = format!("https://autoconfig.thunderbird.net/v1.1/{domain}");
    let mut found = fetch_config(http, &url, email).await?;
    found.source = Source::Ispdb;
    Some(found)
}

/// The first IMAP and first SMTP server are enough: they are listed most recommended first.
fn parse_autoconfig(xml: &str, email: &str) -> Option<Discovered> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let server = |tag: &str, kind: &str| {
        doc.descendants()
            .filter(|n| n.has_tag_name(tag) && n.attribute("type") == Some(kind))
            .find_map(|n| {
                let child = |name: &str| n.children().find(|c| c.has_tag_name(name)).and_then(|c| c.text()).map(str::trim);
                let host = child("hostname")?.to_string();
                let port = child("port")?.parse().ok()?;
                // An unknown value is not "no encryption": the server is dropped.
                let security = match child("socketType")? {
                    "SSL" => Security::Tls,
                    "STARTTLS" => Security::StartTls,
                    "plain" => Security::None,
                    _ => return None,
                };
                let username = child("username").unwrap_or("%EMAILADDRESS%").to_string();
                Some((ServerConfig { host, port, security }, username))
            })
    };
    let (imap, username) = server("incomingServer", "imap")?;
    let (smtp, _) = server("outgoingServer", "smtp")?;
    let provider = doc
        .descendants()
        .find(|n| n.has_tag_name("displayName"))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let username = if email.is_empty() { username } else { fill_template(&username, email) };
    Some(Discovered { imap, smtp, username, source: Source::Autoconfig, provider })
}

pub fn fill_template(template: &str, email: &str) -> String {
    let (local, domain) = email.rsplit_once('@').unwrap_or((email, ""));
    template
        .replace("%EMAILADDRESS%", email)
        .replace("%EMAILLOCALPART%", local)
        .replace("%EMAILDOMAIN%", domain)
}

/// TXT records of `name`: `None` when the lookup failed (no answer, timeout), empty when the name
/// has none.
pub(crate) async fn txt(name: &str) -> Option<Vec<String>> {
    let resolver = TokioResolver::builder_tokio().ok()?.build().ok()?;
    match tokio::time::timeout(Duration::from_secs(3), resolver.txt_lookup(name)).await.ok()? {
        Ok(lookup) => Some(
            lookup
                .answers()
                .iter()
                .filter_map(|r| match &r.data {
                    RData::TXT(txt) => Some(txt.txt_data.iter().map(|part| String::from_utf8_lossy(part).into_owned()).collect()),
                    _ => None,
                })
                .collect(),
        ),
        Err(e) if e.is_no_records_found() || e.is_nx_domain() => Some(vec![]),
        Err(_) => None,
    }
}

/// Domains to try in the ISPDB from the MX, most specific first.
async fn mx_provider_domains(resolver: &TokioResolver, domain: &str) -> Vec<String> {
    let Ok(lookup) = tokio::time::timeout(Duration::from_secs(3), resolver.mx_lookup(domain)).await else {
        return vec![];
    };
    let Ok(lookup) = lookup else { return vec![] };
    let mut mx: Vec<_> = lookup
        .answers()
        .iter()
        .filter_map(|r| match &r.data {
            RData::MX(mx) => Some((mx.preference, mx.exchange.to_utf8().trim_end_matches('.').to_ascii_lowercase())),
            _ => None,
        })
        .collect();
    mx.sort();
    let mut out = vec![];
    for (_, host) in mx {
        let sld = registrable(&host);
        // A "." MX (RFC 7505) announces a domain without mail.
        if sld.is_empty() {
            continue;
        }
        // A few hosts whose MX domain does not carry the configuration's name.
        let known = match sld.as_str() {
            "google.com" | "googlemail.com" => Some("gmail.com"),
            "outlook.com" => Some("outlook.com"),
            "yahoodns.net" => Some("yahoo.com"),
            "zoho.eu" => Some("zoho.com"),
            _ => None,
        };
        for d in [known.map(str::to_string), Some(sld)].into_iter().flatten() {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    debug!(domain, ?out, "domains derived from the MX");
    out
}

/// Public mail providers: anyone can get an address there. A decision about such a domain would
/// cover strangers (the screener, image approvals), and its logo would be the provider's.
pub const PUBLIC_DOMAINS: &[&str] = &[
    // Google, Microsoft, Yahoo, Apple, AOL
    "gmail.com", "googlemail.com", "outlook.com", "outlook.fr", "outlook.de", "outlook.es", "outlook.it", "outlook.be",
    "hotmail.com", "hotmail.fr", "hotmail.de", "hotmail.es", "hotmail.it", "hotmail.be", "hotmail.co.uk", "live.com",
    "live.fr", "live.de", "live.be", "live.co.uk", "live.ca", "msn.com", "passport.com", "windowslive.com",
    "yahoo.com", "yahoo.fr", "yahoo.de", "yahoo.es", "yahoo.it", "yahoo.co.uk", "yahoo.ca", "yahoo.co.jp",
    "yahoo.com.br", "ymail.com", "rocketmail.com", "icloud.com", "me.com", "mac.com", "aol.com", "aol.fr", "aim.com",
    // Privacy-minded and independent providers
    "proton.me", "protonmail.com", "protonmail.ch", "pm.me", "tutanota.com", "tutanota.de", "tuta.io", "tuta.com",
    "mailbox.org", "posteo.de", "posteo.net", "fastmail.com", "fastmail.fm", "hey.com", "disroot.org", "riseup.net",
    "zoho.com", "zohomail.com", "zohomail.eu", "hushmail.com", "runbox.com", "startmail.com", "mailfence.com",
    "duck.com", "simplelogin.com", "simplelogin.fr", "anonaddy.me", "addy.io", "mail.com", "email.com", "usa.com",
    "post.com", "inbox.com", "lycos.com", "excite.com", "rediffmail.com",
    // France, Belgium, Switzerland
    "orange.fr", "wanadoo.fr", "free.fr", "sfr.fr", "neuf.fr", "cegetel.net", "club-internet.fr", "noos.fr",
    "laposte.net", "bbox.fr", "numericable.fr", "aliceadsl.fr", "infonie.fr", "voila.fr", "netcourrier.com",
    "skynet.be", "telenet.be", "bluewin.ch", "gmx.ch", "hispeed.ch",
    // Germany, Austria, Italy, Spain, Netherlands, UK, Nordics, Eastern Europe
    "gmx.com", "gmx.fr", "gmx.de", "gmx.net", "gmx.at", "web.de", "t-online.de", "freenet.de", "arcor.de",
    "libero.it", "virgilio.it", "tiscali.it", "alice.it", "tin.it", "email.it", "terra.es", "telefonica.net",
    "ziggo.nl", "kpnmail.nl", "planet.nl", "home.nl", "btinternet.com", "sky.com", "virginmedia.com", "talktalk.net",
    "ntlworld.com", "blueyonder.co.uk", "seznam.cz", "centrum.cz", "wp.pl", "o2.pl", "onet.pl", "interia.pl",
    "abv.bg", "freemail.hu", "telia.com", "online.no",
    // Russia, Asia, Americas, elsewhere
    "yandex.com", "yandex.ru", "ya.ru", "mail.ru", "inbox.ru", "list.ru", "bk.ru", "rambler.ru", "ukr.net",
    "qq.com", "foxmail.com", "163.com", "126.com", "yeah.net", "sina.com", "sohu.com", "aliyun.com", "naver.com",
    "daum.net", "hanmail.net", "nate.com", "comcast.net", "verizon.net", "att.net", "sbcglobal.net", "bellsouth.net",
    "charter.net", "cox.net", "earthlink.net", "optonline.net", "shaw.ca", "rogers.com", "sympatico.ca",
    "bigpond.com", "optusnet.com.au", "xtra.co.nz", "uol.com.br", "bol.com.br", "terra.com.br",
];

pub fn is_public_domain(host: &str) -> bool {
    let host = host.trim().trim_matches('.').to_ascii_lowercase();
    PUBLIC_DOMAINS.contains(&host.as_str()) || PUBLIC_DOMAINS.contains(&registrable(&host).as_str())
}

/// Multi-tenant domains where anyone gets a subdomain, missing from the Public Suffix List (and
/// abused for phishing pages): each tenant counts as its own domain.
const SHARED_SUFFIXES: &[&str] = &[
    "onmicrosoft.com", "mail.onmicrosoft.com", "sharepoint.com", "weebly.com", "godaddysites.com", "glitch.me",
    "mailchimpsites.com", "jimdosite.com", "webnode.page", "zohosites.com", "strikingly.com", "000webhostapp.com",
    "firebasestorage.app", "hubspotpagebuilder.com", "typeform.com", "wordpress.com", "page.link", "railway.app",
];

/// Registrable domain ("eTLD+1") from the Public Suffix List, private section included:
/// `aspmx.l.google.com` → `google.com`, `mail.example.co.uk` → `example.co.uk`, but
/// `evil.github.io` and `contoso.onmicrosoft.com` stay distinct sites. A host that is itself a
/// public suffix (or a single label) is returned as is.
pub(crate) fn registrable(host: &str) -> String {
    let host = host.trim().trim_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    let host = labels.join(".");
    if labels.len() < 2 {
        return host;
    }
    // Longest suffix first (`mail.onmicrosoft.com` before `onmicrosoft.com`).
    if let Some(suffix) = SHARED_SUFFIXES.iter().filter(|s| host.ends_with(&format!(".{s}"))).max_by_key(|s| s.len()) {
        let label = host[..host.len() - suffix.len() - 1].rsplit('.').next().unwrap_or_default();
        return format!("{label}.{suffix}");
    }
    match psl::domain_str(&host) {
        Some(domain) => domain.to_string(),
        None => host,
    }
}

/// RFC 6186: `_imaps`/`_submissions` = TLS, `_imap`/`_submission` = STARTTLS.
async fn srv(resolver: &TokioResolver, domain: &str) -> (Option<ServerConfig>, Option<ServerConfig>) {
    let one = |name: String, security: Security| async move {
        let lookup = tokio::time::timeout(Duration::from_secs(3), resolver.srv_lookup(name)).await.ok()?.ok()?;
        let mut records: Vec<_> = lookup
            .answers()
            .iter()
            .filter_map(|r| match &r.data {
                RData::SRV(s) => Some((s.priority, s.target.to_utf8().trim_end_matches('.').to_string(), s.port)),
                _ => None,
            })
            // A "." target explicitly means "no service".
            .filter(|(_, target, port)| !target.is_empty() && *port > 0)
            .collect();
        records.sort();
        let (_, host, port) = records.into_iter().next()?;
        Some(ServerConfig { host, port, security })
    };
    let (imaps, imap, smtps, smtp) = tokio::join!(
        one(format!("_imaps._tcp.{domain}"), Security::Tls),
        one(format!("_imap._tcp.{domain}"), Security::StartTls),
        one(format!("_submissions._tcp.{domain}"), Security::Tls),
        one(format!("_submission._tcp.{domain}"), Security::StartTls),
    );
    (imaps.or(imap), smtps.or(smtp))
}

#[derive(Clone, Copy, PartialEq)]
enum Proto {
    Imap,
    Smtp,
}

/// Only what really speaks IMAP or SMTP is kept.
async fn guess_imap(domain: &str) -> Option<ServerConfig> {
    let hosts = [format!("imap.{domain}"), format!("mail.{domain}"), domain.to_string()];
    guess(Proto::Imap, &hosts, &[(993, Security::Tls), (143, Security::StartTls)]).await
}

async fn guess_smtp(domain: &str) -> Option<ServerConfig> {
    let hosts = [format!("smtp.{domain}"), format!("mail.{domain}"), domain.to_string()];
    guess(Proto::Smtp, &hosts, &[(465, Security::Tls), (587, Security::StartTls)]).await
}

async fn guess(proto: Proto, hosts: &[String], ports: &[(u16, Security)]) -> Option<ServerConfig> {
    let candidates: Vec<ServerConfig> = hosts
        .iter()
        .flat_map(|h| ports.iter().map(move |&(port, security)| ServerConfig { host: h.clone(), port, security }))
        .collect();
    // All probes start together; answers are read in order of preference, stopping at the first
    // that answers, without waiting for the others to time out.
    let mut probes: FuturesOrdered<_> = candidates
        .into_iter()
        .map(|c| async move {
            let ok = probe(proto, &c).await;
            (ok, c)
        })
        .collect();
    while let Some((ok, c)) = probes.next().await {
        if ok {
            return Some(c);
        }
    }
    None
}

async fn probe(proto: Proto, server: &ServerConfig) -> bool {
    let ok = tokio::time::timeout(PROBE_TIMEOUT, probe_inner(proto, server)).await.unwrap_or(false);
    debug!(host = server.host, port = server.port, ?proto, ok, "probe");
    ok
}

impl std::fmt::Debug for Proto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Proto::Imap => "imap",
            Proto::Smtp => "smtp",
        })
    }
}

async fn probe_inner(proto: Proto, server: &ServerConfig) -> bool {
    let Ok(tcp) = crate::imap::tcp_connect(&server.host, server.port).await else { return false };
    match server.security {
        Security::Tls => match tls(&server.host, tcp).await {
            Ok(mut s) => greeting(&mut s, proto).await.is_some(),
            Err(_) => false,
        },
        Security::StartTls => {
            let mut s = Stream::Plain(tcp);
            if greeting(&mut s, proto).await.is_none() {
                return false;
            }
            // Only check that STARTTLS is offered; the real connection does the rest.
            let (cmd, expect) = match proto {
                Proto::Imap => ("a1 STARTTLS\r\n", "a1 OK"),
                Proto::Smtp => ("EHLO facteur\r\n", "STARTTLS"),
            };
            if s.write_all(cmd.as_bytes()).await.is_err() {
                return false;
            }
            read_until(&mut s, |text| text.contains(expect) || ends_reply(proto, text)).await.is_some_and(|t| t.contains(expect))
        }
        Security::None => false,
    }
}

async fn greeting(s: &mut Stream, proto: Proto) -> Option<String> {
    let text = read_until(s, |t| t.contains('\n')).await?;
    let ok = match proto {
        Proto::Imap => text.starts_with("* OK") || text.starts_with("* PREAUTH"),
        Proto::Smtp => text.starts_with("220"),
    };
    ok.then_some(text)
}

/// A multi-line SMTP reply ends with `250 `; a tagged IMAP one with `a1 `.
fn ends_reply(proto: Proto, text: &str) -> bool {
    match proto {
        Proto::Imap => text.lines().any(|l| l.starts_with("a1 ")),
        Proto::Smtp => text.lines().any(|l| l.len() >= 4 && l.starts_with("250") && !l.starts_with("250-")),
    }
}

async fn read_until(s: &mut Stream, done: impl Fn(&str) -> bool) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = s.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if done(&text) || buf.len() > 16 * 1024 {
            return Some(text.into_owned());
        }
    }
}

pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Exemple</displayName>
    <incomingServer type="pop3"><hostname>pop.example.com</hostname><port>995</port><socketType>SSL</socketType><username>%EMAILADDRESS%</username></incomingServer>
    <incomingServer type="imap">
      <hostname>imap.example.com</hostname><port>143</port><socketType>STARTTLS</socketType>
      <authentication>password-cleartext</authentication><username>%EMAILLOCALPART%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.com</hostname><port>465</port><socketType>SSL</socketType><username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn reads_the_mozilla_format() {
        let d = parse_autoconfig(XML, "jean@example.com").unwrap();
        assert_eq!(d.imap.host, "imap.example.com");
        assert_eq!(d.imap.port, 143);
        assert_eq!(d.imap.security, Security::StartTls);
        assert_eq!(d.smtp.host, "smtp.example.com");
        assert_eq!(d.smtp.security, Security::Tls);
        assert_eq!(d.username, "jean");
        assert_eq!(d.provider.as_deref(), Some("Exemple"));
    }

    #[test]
    fn keeps_the_template_without_an_address() {
        let d = parse_autoconfig(XML, "").unwrap();
        assert_eq!(d.username, "%EMAILLOCALPART%");
        assert_eq!(fill_template(&d.username, "jean@example.com"), "jean");
    }

    #[test]
    fn registrable_domain() {
        assert_eq!(registrable("aspmx.l.google.com"), "google.com");
        assert_eq!(registrable("mx1.mail.ovh.net"), "ovh.net");
        assert_eq!(registrable("mail.example.co.uk"), "example.co.uk");
        assert_eq!(registrable("example.fr"), "example.fr");
        assert_eq!(registrable("Mail.Impots.Gouv.FR."), "impots.gouv.fr");
        assert_eq!(registrable("cdn.tracker.example"), "tracker.example");
        // Private section of the list: each customer is its own site.
        assert_eq!(registrable("login.evil.pages.dev"), "evil.pages.dev");
        assert_eq!(registrable("someone.github.io"), "someone.github.io");
        assert_eq!(registrable("contoso.onmicrosoft.com"), "contoso.onmicrosoft.com");
        assert_eq!(registrable("app.herokuapp.com"), "app.herokuapp.com");
        assert_eq!(registrable("github.io"), "github.io");
        assert_eq!(registrable("x.contoso.mail.onmicrosoft.com"), "contoso.mail.onmicrosoft.com");
        assert_eq!(registrable("tenant-my.sharepoint.com"), "tenant-my.sharepoint.com");
        assert_eq!(registrable("onmicrosoft.com"), "onmicrosoft.com");
        assert_eq!(registrable("localhost"), "localhost");
    }

    #[test]
    fn end_of_smtp_reply() {
        assert!(!ends_reply(Proto::Smtp, "250-mail.example.com\r\n250-STARTTLS\r\n"));
        assert!(ends_reply(Proto::Smtp, "250-mail.example.com\r\n250-STARTTLS\r\n250 OK\r\n"));
        assert!(ends_reply(Proto::Imap, "* OK ready\r\na1 BAD\r\n"));
    }
}
