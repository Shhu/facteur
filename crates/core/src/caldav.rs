//! CalDAV (RFC 4791) calendars and CardDAV (RFC 6352) address books of password accounts.
//!
//! Only https. Discovery goes without the password. The password is then sent only to the site
//! (registrable domain) of the IMAP server or of the address's known provider, or to the site of
//! an address the user entered; never to another one a redirection or an answer points at.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::{StatusCode, Url};
use roxmltree::Node;

use crate::autoconfig::registrable;
use crate::calendar::{Instance, RemoteCalendar, hex_color};
use crate::error::{Error, Result};

const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const CALSERVER: &str = "http://calendarserver.org/ns/";
const APPLE: &str = "http://apple.com/ns/ical/";
const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";

/// Largest answer accepted to a PROPFIND, and to the events of one calendar.
const MAX_PROPFIND: usize = 4 * 1024 * 1024;
const MAX_REPORT: usize = 32 * 1024 * 1024;

/// Providers without a well-known address on the mail domain (registrable domain → service).
const PROVIDERS: &[(&str, &str)] = &[
    ("icloud.com", "https://caldav.icloud.com/"),
    ("me.com", "https://caldav.icloud.com/"),
    ("mac.com", "https://caldav.icloud.com/"),
    ("fastmail.com", "https://caldav.fastmail.com/"),
    ("fastmail.fm", "https://caldav.fastmail.com/"),
    ("yahoo.com", "https://caldav.calendar.yahoo.com/"),
    ("posteo.de", "https://posteo.de:8443/"),
];

const PRINCIPAL_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:current-user-principal/><c:calendar-home-set/><d:resourcetype/></d:prop>
</d:propfind>"#;

const HOME_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><c:calendar-home-set/></d:prop>
</d:propfind>"#;

const CALENDARS_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/">
  <d:prop>
    <d:resourcetype/><d:displayname/><a:calendar-color/><c:supported-calendar-component-set/>
    <cs:getctag/><d:sync-token/><d:current-user-privilege-set/>
  </d:prop>
</d:propfind>"#;

/// Known CardDAV services, by the site of their CalDAV one (so of a site already trusted).
const CARD_PROVIDERS: &[(&str, &str)] = &[
    ("icloud.com", "https://contacts.icloud.com/"),
    ("fastmail.com", "https://carddav.fastmail.com/"),
    ("yahoo.com", "https://carddav.address.yahoo.com/"),
    ("posteo.de", "https://posteo.de:8843/"),
];

const CARD_PRINCIPAL_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop><d:current-user-principal/><card:addressbook-home-set/><d:resourcetype/></d:prop>
</d:propfind>"#;

const CARD_HOME_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop><card:addressbook-home-set/></d:prop>
</d:propfind>"#;

const BOOKS_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/></d:prop></d:propfind>"#;

const CARD_LIST_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>"#;

/// Cards asked for this many at a time. Only the properties used are asked for, but a server may
/// ignore that and send whole vCards, photos included: each answer stays bounded.
const CARDS_PER_REQUEST: usize = 100;

fn cards_body(hrefs: &[String]) -> String {
    let escape = |h: &str| h.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let list: String = hrefs.iter().map(|h| format!("<d:href>{}</d:href>", escape(h))).collect();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav">
  <d:prop><card:address-data><card:prop name="FN"/><card:prop name="N"/><card:prop name="EMAIL"/></card:address-data></d:prop>
  {list}
</card:addressbook-multiget>"#
    )
}

/// Where to look for the CardDAV service of an account: the CalDAV address the user entered (one
/// DAV server often serves both), the known providers, and the well-known addresses on the mail
/// and IMAP domains. Only those on a trusted site are asked (see [`Client::find_address_book`]).
pub fn card_candidates(email: &str, imap_host: &str, caldav: Option<&Url>) -> Vec<Url> {
    let domain = email.rsplit_once('@').map(|(_, d)| d.trim().to_ascii_lowercase()).unwrap_or_default();
    let imap_host = imap_host.trim().trim_end_matches('.').to_ascii_lowercase();
    let mut out: Vec<String> = caldav.map(Url::to_string).into_iter().collect();
    for s in auto_sites(email, &imap_host) {
        out.extend(CARD_PROVIDERS.iter().filter(|(d, _)| *d == s).map(|(_, url)| url.to_string()));
    }
    for host in [domain.clone(), registrable(&imap_host), imap_host] {
        if !host.is_empty() {
            out.push(format!("https://{host}/.well-known/carddav"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.into_iter().filter(|u| seen.insert(u.clone())).filter_map(|u| Url::parse(&u).ok()).collect()
}

/// Where to look for the CalDAV service of an address: known providers, then the well-known
/// address on the mail domain, on the IMAP server's domain, and on the IMAP server itself.
pub fn candidates(email: &str, imap_host: &str) -> Vec<Url> {
    let domain = email.rsplit_once('@').map(|(_, d)| d.trim().to_ascii_lowercase()).unwrap_or_default();
    let imap_host = imap_host.trim().trim_end_matches('.').to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    for site in [registrable(&domain), registrable(&imap_host)] {
        out.extend(PROVIDERS.iter().filter(|(d, _)| *d == site).map(|(_, url)| url.to_string()));
    }
    for host in [domain.clone(), registrable(&imap_host), imap_host] {
        if !host.is_empty() {
            out.push(format!("https://{host}/.well-known/caldav"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.into_iter().filter(|u| seen.insert(u.clone())).filter_map(|u| Url::parse(&u).ok()).collect()
}

fn site(host: &str) -> String {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() { bare.to_ascii_lowercase() } else { registrable(host) }
}

pub fn site_of(url: &Url) -> Option<String> {
    url.host_str().map(site)
}

fn provider_sites() -> Vec<String> {
    PROVIDERS.iter().filter_map(|(_, u)| Url::parse(u).ok().as_ref().and_then(site_of)).collect()
}

/// Sites the password may go to without the user naming them: the IMAP server's (it already
/// gets that password), and the CalDAV service of the address's known provider. A CalDAV server
/// found anywhere else is only used once the user enters its address.
pub fn auto_sites(email: &str, imap_host: &str) -> Vec<String> {
    let domain = email.rsplit_once('@').map(|(_, d)| d.trim().to_ascii_lowercase()).unwrap_or_default();
    let imap_host = imap_host.trim().trim_end_matches('.').to_ascii_lowercase();
    let mut out = Vec::new();
    if !imap_host.is_empty() {
        out.push(site(&imap_host));
    }
    for s in [registrable(&domain), registrable(&imap_host)] {
        out.extend(PROVIDERS.iter().filter(|(d, _)| *d == s).filter_map(|(_, u)| Url::parse(u).ok().as_ref().and_then(site_of)));
    }
    out.sort();
    out.dedup();
    out.retain(|s| !s.is_empty());
    out
}

pub fn check_url(url: &str) -> Result<Url> {
    let parsed = Url::parse(url.trim()).map_err(|e| Error::Other(crate::tr!("adresse CalDAV invalide : {e}", "invalid CalDAV address: {e}")))?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err(Error::Other(crate::tr!("adresse CalDAV refusée : https exigé", "CalDAV address refused: https is required")));
    }
    Ok(parsed)
}

/// Who a request goes to: with the password (trusted sites only), or without it, within the
/// sites given (discovery).
#[derive(Clone, Copy)]
enum Access<'a> {
    Password,
    Anonymous(&'a [String]),
}

struct Answer {
    /// After redirections.
    url: Url,
    status: StatusCode,
    /// The `DAV` headers, lowercase: `calendar-access` (RFC 4791 §5.1), `addressbook` (RFC 6352 §6.1).
    dav: String,
    etag: Option<String>,
    body: String,
}

impl Answer {
    fn is_caldav(&self) -> bool {
        self.dav.contains("calendar-access")
            || (self.status == StatusCode::MULTI_STATUS
                && parse_principal(&self.body, &self.url).is_ok_and(|p| p.home.is_some() || p.calendar))
    }

    fn is_carddav(&self) -> bool {
        self.dav.contains("addressbook")
            || (self.status == StatusCode::MULTI_STATUS
                && parse_card_principal(&self.body, &self.url).is_ok_and(|p| p.home.is_some() || p.calendar))
    }
}

#[derive(Debug, PartialEq)]
pub enum Discovery {
    Found(Url),
    /// A CalDAV service on a site the password does not go to unasked: the user has to enter
    /// its address.
    Elsewhere(Url),
    Nothing,
}

/// No `Debug`: it holds the password.
pub struct Client {
    http: reqwest::Client,
    username: String,
    password: String,
    trusted: Vec<String>,
}

impl Client {
    pub fn new(username: &str, password: &str) -> Result<Self> {
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            // Followed by hand: reqwest turns a redirected PROPFIND into a GET.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("facteur")
            .build()
            .map_err(|e| Error::Other(e.to_string()))?;
        Ok(Client { http, username: username.to_string(), password: password.to_string(), trusted: Vec::new() })
    }

    pub fn trust(&mut self, url: &Url) {
        if let Some(site) = site_of(url) {
            self.trust_site(&site);
        }
    }

    pub fn trust_site(&mut self, site: &str) {
        let site = site.trim().to_ascii_lowercase();
        if !site.is_empty() && !self.trusted.contains(&site) {
            self.trusted.push(site);
        }
    }

    pub fn trusts(&self, url: &Url) -> bool {
        url.scheme() == "https" && url.host_str().is_some_and(|h| self.trusted.contains(&site(h)))
    }

    /// Redirects followed by hand: 5 at most, https only; with the password, within the trusted
    /// sites only; without it, within the sites given.
    async fn send(&self, method: &str, url: &Url, depth: &str, body: &str, access: Access<'_>, max: usize) -> Result<Answer> {
        let headers = [("Depth", depth), ("Content-Type", "application/xml; charset=utf-8")];
        self.send_with(method, url, &headers, body, access, max).await
    }

    async fn send_with(&self, method: &str, url: &Url, headers: &[(&str, &str)], body: &str, access: Access<'_>, max: usize) -> Result<Answer> {
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| Error::Other(e.to_string()))?;
        let mut url = url.clone();
        for _ in 0..6 {
            let allowed = match access {
                Access::Password => self.trusts(&url),
                Access::Anonymous(sites) => {
                    url.scheme() == "https" && (self.trusts(&url) || site_of(&url).is_some_and(|s| sites.contains(&s)))
                }
            };
            if url.scheme() != "https" || !allowed {
                return Err(Error::Other(crate::tr!("CalDAV : adresse refusée ({url})", "CalDAV: address refused ({url})")));
            }
            let mut req = self.http.request(method.clone(), url.clone()).body(body.to_string());
            for (name, value) in headers {
                req = req.header(*name, *value);
            }
            if matches!(access, Access::Password) {
                req = req.basic_auth(&self.username, Some(&self.password));
            }
            let resp = req.send().await.map_err(|e| Error::Other(crate::tr!("serveur d'agenda injoignable : {e}", "calendar server unreachable: {e}")))?;
            let status = resp.status();
            if status.is_redirection() {
                let next = resp.headers().get("location").and_then(|l| l.to_str().ok()).and_then(|l| url.join(l).ok());
                if let Some(next) = next {
                    url = next;
                    continue;
                }
            }
            let dav = resp.headers().get_all("dav").iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(",").to_ascii_lowercase();
            let etag = resp.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string);
            let bytes = crate::http::read_capped(resp, max).await?;
            return Ok(Answer { url, status, dav, etag, body: String::from_utf8_lossy(&bytes).into_owned() });
        }
        Err(Error::Other(crate::tr!("CalDAV : trop de redirections", "CalDAV: too many redirections")))
    }

    async fn dav(&self, method: &str, url: &Url, depth: &str, body: &str, max: usize) -> Result<(Url, String)> {
        let answer = checked(self.send(method, url, depth, body, Access::Password, max).await?)?;
        Ok((answer.url, answer.body))
    }

    /// The calendar object at `href`, and its ETag.
    pub async fn object(&self, href: &Url) -> Result<(String, Option<String>)> {
        let answer = checked(self.send_with("GET", href, &[], "", Access::Password, MAX_PROPFIND).await?)?;
        Ok((answer.body, answer.etag))
    }

    /// `condition`: `If-Match` with the version the change was made on (refused if the object
    /// changed since), `If-None-Match: *` to create (refused if one is there), or none for a
    /// server that gave no version.
    async fn put(&self, href: &Url, ics: &str, condition: Option<(&str, &str)>) -> Result<Answer> {
        let mut headers = vec![("Content-Type", "text/calendar; charset=utf-8")];
        headers.extend(condition);
        self.send_with("PUT", href, &headers, ics, Access::Password, MAX_PROPFIND).await
    }

    /// Already gone counts as deleted.
    pub async fn delete(&self, href: &Url) -> Result<()> {
        match self.send_with("DELETE", href, &[], "", Access::Password, MAX_PROPFIND).await.and_then(checked) {
            Ok(_) | Err(Error::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// A new event in `calendar`, under a name of its own.
    /// A new event in `calendar`, under a name made of its UID: sent again after an uncertain
    /// answer, the server refuses the copy (412), which counts as done.
    pub async fn create(&self, calendar: &Url, uid: &str, ics: &str) -> Result<Url> {
        let base = if calendar.path().ends_with('/') { calendar.clone() } else { Url::parse(&format!("{calendar}/")).map_err(|e| Error::Other(e.to_string()))? };
        let href = base.join(&format!("{uid}.ics")).map_err(|e| Error::Other(e.to_string()))?;
        let answer = self.put(&href, ics, Some(("If-None-Match", "*"))).await?;
        if answer.status != StatusCode::PRECONDITION_FAILED {
            checked(answer)?;
        }
        Ok(href)
    }

    /// The calendar object at `href` changed by `change` on its latest version, and written back
    /// unless the object changed meanwhile. `None`: `change` refused it.
    pub async fn rewrite(&self, href: &Url, change: impl FnOnce(&str) -> Option<String>) -> Result<Option<String>> {
        let (ics, etag) = self.object(href).await?;
        let Some(changed) = change(&ics) else { return Ok(None) };
        checked(self.put(href, &changed, etag.as_deref().map(|tag| ("If-Match", tag))).await?)?;
        Ok(Some(changed))
    }

    /// Without the password: a server counts only when it says it is one (`DAV: calendar-access`
    /// or calendar properties), and redirects stay within the candidate's site, the trusted ones
    /// and the known providers. The home is then asked for with the password, on a trusted site.
    pub async fn discover(&self, candidates: &[Url]) -> Result<Discovery> {
        let providers = provider_sites();
        let mut elsewhere = None;
        for start in candidates {
            let Some(own) = site_of(start) else { continue };
            let allowed: Vec<String> = std::iter::once(own).chain(providers.iter().cloned()).collect();
            let endpoint = match self.send("PROPFIND", start, "0", PRINCIPAL_BODY, Access::Anonymous(&allowed), MAX_PROPFIND).await {
                Ok(answer) if answer.is_caldav() => answer.url,
                // A bare 401 (no `DAV` header: Nextcloud and other sabre/dav servers) says
                // nothing yet: an OPTIONS request may tell. Any page of a site can answer 401,
                // so the password goes only to a server that says it is CalDAV.
                Ok(answer) if answer.status == StatusCode::UNAUTHORIZED && self.says(&answer.url, &allowed, "calendar-access").await => {
                    answer.url
                }
                Ok(answer) => {
                    tracing::debug!("CalDAV: {start} answered {} (not CalDAV)", answer.status);
                    continue;
                }
                Err(e) => {
                    tracing::debug!("CalDAV: {start}: {e}");
                    continue;
                }
            };
            if !self.trusts(&endpoint) {
                tracing::debug!("CalDAV: {endpoint} is not on a trusted site: the password is not sent");
                elsewhere.get_or_insert(endpoint);
                continue;
            }
            match self.home(&endpoint).await {
                Ok(Some(home)) => return Ok(Discovery::Found(home)),
                Ok(None) => {}
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(e) => tracing::debug!("CalDAV: {endpoint}: {e}"),
            }
        }
        Ok(elsewhere.map_or(Discovery::Nothing, Discovery::Elsewhere))
    }

    /// Whether the server sends invitations and their answers itself (RFC 6638 §2.1).
    pub async fn auto_schedules(&self, calendar: &Url) -> bool {
        self.send_with("OPTIONS", calendar, &[], "", Access::Password, MAX_PROPFIND).await.is_ok_and(|a| a.dav.contains("calendar-auto-schedule"))
    }

    /// Whether OPTIONS answers with `capability` in its `DAV` header.
    async fn says(&self, url: &Url, allowed: &[String], capability: &str) -> bool {
        self.send("OPTIONS", url, "0", "", Access::Anonymous(allowed), MAX_PROPFIND).await.is_ok_and(|a| a.dav.contains(capability))
    }

    /// From the server's root, a principal, the home itself, or one calendar (then listed
    /// alone). A home on a site not trusted is refused.
    pub async fn home(&self, url: &Url) -> Result<Option<Url>> {
        let (base, xml) = self.dav("PROPFIND", url, "0", PRINCIPAL_BODY, MAX_PROPFIND).await?;
        let found = parse_principal(&xml, &base)?;
        let home = if let Some(home) = found.home {
            Some(home)
        } else if found.calendar {
            Some(base)
        } else if let Some(principal) = found.principal {
            let (base, xml) = self.dav("PROPFIND", &principal, "0", HOME_BODY, MAX_PROPFIND).await?;
            parse_principal(&xml, &base)?.home
        } else {
            None
        };
        match home {
            Some(home) if !self.trusts(&home) => Err(Error::Other(crate::tr!(
                "CalDAV : agenda sur un autre site refusé ({home})",
                "CalDAV: calendar home on another site refused ({home})"
            ))),
            home => Ok(home),
        }
    }

    /// The first address book home found among `candidates`, as [`Client::discover`] does for
    /// calendars: asked without the password first, which then goes only to a trusted server that
    /// said it is CardDAV (a site's web pages never get it). `None`: no CardDAV service there.
    pub async fn find_address_book(&self, candidates: &[Url]) -> Result<Option<Url>> {
        let providers: Vec<String> = CARD_PROVIDERS.iter().filter_map(|(_, u)| Url::parse(u).ok().as_ref().and_then(site_of)).collect();
        for start in candidates.iter().filter(|u| self.trusts(u)) {
            let Some(own) = site_of(start) else { continue };
            let allowed: Vec<String> = std::iter::once(own).chain(providers.iter().cloned()).collect();
            let endpoint = match self.send("PROPFIND", start, "0", CARD_PRINCIPAL_BODY, Access::Anonymous(&allowed), MAX_PROPFIND).await {
                Ok(answer) if answer.is_carddav() => answer.url,
                Ok(answer) if answer.status == StatusCode::UNAUTHORIZED && self.says(&answer.url, &allowed, "addressbook").await => answer.url,
                Ok(answer) => {
                    tracing::debug!("CardDAV: {start} answered {} (not CardDAV)", answer.status);
                    continue;
                }
                Err(e) => {
                    tracing::debug!("CardDAV: {start}: {e}");
                    continue;
                }
            };
            if !self.trusts(&endpoint) {
                continue;
            }
            match self.address_book_home(&endpoint).await {
                Ok(Some(home)) => return Ok(Some(home)),
                Ok(None) => {}
                Err(e @ Error::Auth(_)) => return Err(e),
                Err(e) => tracing::debug!("CardDAV: {endpoint}: {e}"),
            }
        }
        Ok(None)
    }

    async fn address_book_home(&self, url: &Url) -> Result<Option<Url>> {
        let (base, xml) = self.dav("PROPFIND", url, "0", CARD_PRINCIPAL_BODY, MAX_PROPFIND).await?;
        let found = parse_card_principal(&xml, &base)?;
        let home = if let Some(home) = found.home {
            Some(home)
        } else if found.calendar {
            Some(base)
        } else if let Some(principal) = found.principal {
            let (base, xml) = self.dav("PROPFIND", &principal, "0", CARD_HOME_BODY, MAX_PROPFIND).await?;
            parse_card_principal(&xml, &base)?.home
        } else {
            None
        };
        Ok(home.filter(|h| self.trusts(h)))
    }

    /// The address books of a home (the home itself when it is one).
    pub async fn address_books(&self, home: &Url) -> Result<Vec<Url>> {
        let (base, xml) = self.dav("PROPFIND", home, "1", BOOKS_BODY, MAX_PROPFIND).await?;
        let doc = document(&xml)?;
        Ok(responses(&doc)
            .into_iter()
            .filter(|(_, props)| props.iter().any(|p| is(p, DAV, "resourcetype") && child(*p, CARDDAV, "addressbook").is_some()))
            .filter_map(|(href, _)| base.join(&href).ok())
            .filter(|u| self.trusts(u))
            .collect())
    }

    /// The book's cards are listed first, then read by batches.
    pub async fn cards(&self, book: &Url) -> Result<Vec<crate::contacts::Contact>> {
        let (_, xml) = self.dav("PROPFIND", book, "1", CARD_LIST_BODY, MAX_PROPFIND).await?;
        let hrefs = card_hrefs(&xml)?;
        let mut out = Vec::new();
        for chunk in hrefs.chunks(CARDS_PER_REQUEST) {
            let (base, xml) = self.dav("REPORT", book, "1", &cards_body(chunk), MAX_REPORT).await?;
            out.extend(parse_cards(&xml, &base)?);
        }
        Ok(out)
    }

    pub async fn calendars(&self, home: &Url) -> Result<Vec<RemoteCalendar>> {
        let (base, xml) = self.dav("PROPFIND", home, "1", CALENDARS_BODY, MAX_PROPFIND).await?;
        parse_calendars(&xml, &base)
    }

    /// The server is asked to expand recurring events; those it leaves whole are expanded by
    /// [`crate::calendar::instances`].
    pub async fn events(&self, calendar: &Url, from: i64, to: i64, me: &[String]) -> Result<Vec<Instance>> {
        let utc = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.format("%Y%m%dT%H%M%SZ").to_string()).unwrap_or_default();
        let (start, end) = (utc(from), utc(to));
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data><c:expand start="{start}" end="{end}"/></c:calendar-data></d:prop>
  <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="{start}" end="{end}"/></c:comp-filter></c:comp-filter></c:filter>
</c:calendar-query>"#
        );
        let (base, xml) = self.dav("REPORT", calendar, "1", &body, MAX_REPORT).await?;
        Ok(parse_calendar_data(&xml)?
            .iter()
            .flat_map(|(href, ics)| {
                let href = base.join(href).ok().map(|u| u.to_string());
                crate::calendar::instances(ics, from, to, me).into_iter().map(move |i| Instance { href: href.clone(), ..i })
            })
            .collect())
    }
}

fn is(n: &Node, ns: &str, name: &str) -> bool {
    n.is_element() && n.tag_name().name() == name && n.tag_name().namespace() == Some(ns)
}

fn child<'a, 'i>(n: Node<'a, 'i>, ns: &str, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| is(c, ns, name))
}

fn text(n: Node) -> String {
    n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>().trim().to_string()
}

fn href(n: Node, base: &Url) -> Option<Url> {
    let h = text(child(n, DAV, "href")?);
    (!h.is_empty()).then(|| base.join(&h).ok()).flatten()
}

fn document(xml: &str) -> Result<roxmltree::Document<'_>> {
    roxmltree::Document::parse(xml).map_err(|e| Error::Other(crate::tr!("réponse CalDAV illisible : {e}", "unreadable CalDAV answer: {e}")))
}

fn responses<'a, 'i>(doc: &'a roxmltree::Document<'i>) -> Vec<(String, Vec<Node<'a, 'i>>)> {
    doc.descendants()
        .filter(|n| is(n, DAV, "response"))
        .map(|r| {
            let href = child(r, DAV, "href").map(text).unwrap_or_default();
            let props = r
                .children()
                .filter(|n| is(n, DAV, "propstat"))
                .filter(|ps| child(*ps, DAV, "status").map(text).is_none_or(|s| s.contains(" 200")))
                .filter_map(|ps| child(ps, DAV, "prop"))
                .flat_map(|p| p.children().filter(|n| n.is_element()))
                .collect();
            (href, props)
        })
        .collect()
}

#[derive(Debug, Default, PartialEq)]
struct Principal {
    principal: Option<Url>,
    home: Option<Url>,
    calendar: bool,
}

fn parse_principal(xml: &str, base: &Url) -> Result<Principal> {
    let doc = document(xml)?;
    let mut out = Principal::default();
    for (_, props) in responses(&doc) {
        for p in props {
            if is(&p, DAV, "current-user-principal") {
                out.principal = out.principal.or_else(|| href(p, base));
            } else if is(&p, CALDAV, "calendar-home-set") {
                out.home = out.home.or_else(|| href(p, base));
            } else if is(&p, DAV, "resourcetype") && child(p, CALDAV, "calendar").is_some() {
                out.calendar = true;
            }
        }
    }
    Ok(out)
}

/// The cards of a book's listing: entries with an ETag, not collections.
fn card_hrefs(xml: &str) -> Result<Vec<String>> {
    let doc = document(xml)?;
    Ok(responses(&doc)
        .into_iter()
        .filter(|(_, props)| {
            props.iter().any(|p| is(p, DAV, "getetag"))
                && !props.iter().any(|p| is(p, DAV, "resourcetype") && child(*p, DAV, "collection").is_some())
        })
        .map(|(href, _)| href)
        .filter(|h| !h.is_empty())
        .collect())
}

fn parse_cards(xml: &str, base: &Url) -> Result<Vec<crate::contacts::Contact>> {
    let doc = document(xml)?;
    Ok(responses(&doc)
        .into_iter()
        .flat_map(|(href, props)| {
            let href = base.join(&href).map(|u| u.to_string()).unwrap_or(href);
            props
                .into_iter()
                .filter(|p| is(p, CARDDAV, "address-data"))
                .flat_map(|p| crate::contacts::parse_vcards(&href, &text(p)))
                .collect::<Vec<_>>()
        })
        .collect())
}

/// As [`parse_principal`], for CardDAV; `calendar`: the address is an address book itself.
fn parse_card_principal(xml: &str, base: &Url) -> Result<Principal> {
    let doc = document(xml)?;
    let mut out = Principal::default();
    for (_, props) in responses(&doc) {
        for p in props {
            if is(&p, DAV, "current-user-principal") {
                out.principal = out.principal.or_else(|| href(p, base));
            } else if is(&p, CARDDAV, "addressbook-home-set") {
                out.home = out.home.or_else(|| href(p, base));
            } else if is(&p, DAV, "resourcetype") && child(p, CARDDAV, "addressbook").is_some() {
                out.calendar = true;
            }
        }
    }
    Ok(out)
}

fn parse_calendars(xml: &str, base: &Url) -> Result<Vec<RemoteCalendar>> {
    let doc = document(xml)?;
    let mut out = Vec::new();
    for (href, props) in responses(&doc) {
        let prop = |ns: &str, name: &str| props.iter().copied().find(|p| is(p, ns, name));
        if prop(DAV, "resourcetype").and_then(|r| child(r, CALDAV, "calendar")).is_none() {
            continue;
        }
        // Task lists and journals are not shown.
        if let Some(set) = prop(CALDAV, "supported-calendar-component-set")
            && !set.children().filter(|c| is(c, CALDAV, "comp")).any(|c| c.attribute("name").is_some_and(|n| n.eq_ignore_ascii_case("VEVENT")))
        {
            continue;
        }
        let Ok(url) = base.join(&href) else { continue };
        let read_only = prop(DAV, "current-user-privilege-set").is_some_and(|set| {
            !set.descendants()
                .filter(|n| n.is_element() && n.tag_name().namespace() == Some(DAV))
                .any(|n| matches!(n.tag_name().name(), "write" | "write-content" | "all"))
        });
        let name = prop(DAV, "displayname").map(text).filter(|n| !n.is_empty()).unwrap_or_else(|| {
            url.path_segments().and_then(|mut s| s.rfind(|p| !p.is_empty())).map(crate::oauth::urldecode).unwrap_or_default()
        });
        let ctag = prop(CALSERVER, "getctag").or(prop(DAV, "sync-token")).map(text).filter(|t| !t.is_empty());
        out.push(RemoteCalendar {
            remote_id: url.to_string(),
            name,
            color: prop(APPLE, "calendar-color").and_then(|c| hex_color(&text(c))),
            read_only,
            enabled: true,
            ctag,
        });
    }
    Ok(out)
}

/// Each calendar object, with its address.
fn parse_calendar_data(xml: &str) -> Result<Vec<(String, String)>> {
    let doc = document(xml)?;
    Ok(responses(&doc)
        .into_iter()
        .flat_map(|(href, props)| {
            props.into_iter().filter(|p| is(p, CALDAV, "calendar-data")).map(text).map(move |ics| (href.clone(), ics)).collect::<Vec<_>>()
        })
        .filter(|(_, ics)| !ics.is_empty())
        .collect())
}

fn checked(answer: Answer) -> Result<Answer> {
    let status = answer.status;
    match status.as_u16() {
        200..=299 => Ok(answer),
        401 | 403 => Err(Error::Auth(crate::tr!(
            "identifiants refusés par le serveur d'agenda ({status})",
            "credentials refused by the calendar server ({status})"
        ))),
        404 | 410 => Err(Error::NotFound(answer.url.to_string())),
        412 => Err(Error::Other(crate::tr!(
            "l'événement a changé entre-temps sur le serveur : réessayez après la synchro",
            "the event changed on the server meanwhile: try again after the sync"
        ))),
        _ => Err(Error::Other(crate::tr!("serveur d'agenda : {status}", "calendar server: {status}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_for_an_address() {
        let list: Vec<String> = candidates("y@example.org", "mail.example.org").iter().map(Url::to_string).collect();
        assert_eq!(list, ["https://example.org/.well-known/caldav", "https://mail.example.org/.well-known/caldav"]);
        let list: Vec<String> = candidates("y@me.com", "imap.mail.me.com").iter().map(Url::to_string).collect();
        assert_eq!(list[0], "https://caldav.icloud.com/");
    }

    #[test]
    fn only_https_addresses() {
        assert!(check_url("http://dav.example.org/").is_err());
        assert!(check_url("https://dav.example.org/remote.php/dav").is_ok());
    }

    #[test]
    fn password_stays_within_the_site() {
        let mut c = Client::new("y", "secret").unwrap();
        c.trust(&Url::parse("https://caldav.icloud.com/").unwrap());
        assert!(c.trusts(&Url::parse("https://p42-caldav.icloud.com/123/calendars/").unwrap()));
        assert!(!c.trusts(&Url::parse("https://evil.example/").unwrap()));
        assert!(!c.trusts(&Url::parse("http://caldav.icloud.com/").unwrap()));
        c.trust(&Url::parse("https://192.168.1.10/dav").unwrap());
        assert!(!c.trusts(&Url::parse("https://10.0.1.10/dav").unwrap()));
    }

    #[test]
    fn password_goes_to_the_mail_server_or_the_known_provider() {
        assert_eq!(auto_sites("me@shop.fr", "ssl0.ovh.net"), ["ovh.net"]);
        assert_eq!(auto_sites("y@example.org", "mail.example.org"), ["example.org"]);
        assert_eq!(auto_sites("y@me.com", "imap.mail.me.com"), ["icloud.com", "me.com"]);
        assert_eq!(auto_sites("y@fastmail.com", "imap.fastmail.com"), ["fastmail.com"]);
        assert!(provider_sites().contains(&"icloud.com".to_string()));
    }

    #[tokio::test]
    async fn discovery_stays_within_the_sites_allowed() {
        let c = Client::new("me@shop.fr", "secret").unwrap();
        // Refused before anything is sent: another site, plain http, or with the password to
        // a site not trusted.
        let evil = Url::parse("https://attacker.example/.well-known/caldav").unwrap();
        assert!(c.send("PROPFIND", &evil, "0", "", Access::Anonymous(&["shop.fr".into()]), 1024).await.is_err());
        let http = Url::parse("http://shop.fr/.well-known/caldav").unwrap();
        assert!(c.send("PROPFIND", &http, "0", "", Access::Anonymous(&["shop.fr".into()]), 1024).await.is_err());
        let own = Url::parse("https://shop.fr/.well-known/caldav").unwrap();
        assert!(c.send("PROPFIND", &own, "0", "", Access::Password, 1024).await.is_err());
    }

    #[test]
    fn a_bare_401_is_not_caldav() {
        let url = Url::parse("https://attacker.example/").unwrap();
        let answer = |status: StatusCode, calendar_access: bool, body: &str| {
            let dav = if calendar_access { "1, 3, calendar-access".to_string() } else { String::new() };
            Answer { url: url.clone(), status, dav, etag: None, body: body.into() }
        };
        assert!(!answer(StatusCode::UNAUTHORIZED, false, "").is_caldav());
        assert!(!answer(StatusCode::UNAUTHORIZED, false, "").is_carddav(), "a web page asking for a password is not CardDAV");
        let card = Answer { url: url.clone(), status: StatusCode::UNAUTHORIZED, dav: "1, 3, addressbook".into(), etag: None, body: String::new() };
        assert!(card.is_carddav() && !card.is_caldav());
        assert!(answer(StatusCode::UNAUTHORIZED, true, "").is_caldav());
        // A WebDAV server that is not CalDAV.
        let dav = r#"<multistatus xmlns="DAV:"><response><href>/</href><propstat><prop><resourcetype><collection/></resourcetype></prop>
            <status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        assert!(!answer(StatusCode::MULTI_STATUS, false, dav).is_caldav());
        let cal = r#"<multistatus xmlns="DAV:"><response><href>/p/</href><propstat><prop>
            <C:calendar-home-set xmlns:C="urn:ietf:params:xml:ns:caldav"><href>/cal/</href></C:calendar-home-set>
            </prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        assert!(answer(StatusCode::MULTI_STATUS, false, cal).is_caldav());
        assert!(!answer(StatusCode::OK, false, cal).is_caldav());
    }

    #[test]
    fn reads_principal_and_home() {
        let base = Url::parse("https://cloud.example.org/remote.php/dav/").unwrap();
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">
 <d:response><d:href>/remote.php/dav/</d:href>
  <d:propstat><d:prop><d:current-user-principal><d:href>/remote.php/dav/principals/users/y/</d:href></d:current-user-principal>
   <d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><cal:calendar-home-set/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
</d:multistatus>"#;
        let p = parse_principal(xml, &base).unwrap();
        assert_eq!(p.principal.unwrap().as_str(), "https://cloud.example.org/remote.php/dav/principals/users/y/");
        assert!(p.home.is_none() && !p.calendar);
        let xml = r#"<multistatus xmlns="DAV:"><response><href>/p/</href><propstat><prop>
            <C:calendar-home-set xmlns:C="urn:ietf:params:xml:ns:caldav"><href>https://p42-caldav.icloud.com/123/calendars/</href></C:calendar-home-set>
            </prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;
        assert_eq!(parse_principal(xml, &base).unwrap().home.unwrap().as_str(), "https://p42-caldav.icloud.com/123/calendars/");
    }

    #[test]
    fn reads_the_calendars_of_a_home() {
        let base = Url::parse("https://cloud.example.org/remote.php/dav/calendars/y/").unwrap();
        let xml = r##"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:x1="http://apple.com/ns/ical/">
 <d:response><d:href>/remote.php/dav/calendars/y/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/y/personal/</d:href>
  <d:propstat><d:prop>
   <d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:displayname>Personnel</d:displayname>
   <x1:calendar-color>#0082C9FF</x1:calendar-color><cs:getctag>http://sabre.io/ns/sync/42</cs:getctag>
   <cal:supported-calendar-component-set><cal:comp name="VEVENT"/></cal:supported-calendar-component-set>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:current-user-privilege-set>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/y/tasks/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype>
   <cal:supported-calendar-component-set><cal:comp name="VTODO"/></cal:supported-calendar-component-set></d:prop>
   <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/remote.php/dav/calendars/y/holidays%20fr/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/><cal:calendar/></d:resourcetype><d:sync-token>t9</d:sync-token>
   <d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop>
   <d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:displayname/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
</d:multistatus>"##;
        let list = parse_calendars(xml, &base).unwrap();
        assert_eq!(list.len(), 2, "the home itself and the task list are left out");
        assert_eq!(list[0].remote_id, "https://cloud.example.org/remote.php/dav/calendars/y/personal/");
        assert_eq!((list[0].name.as_str(), list[0].color.as_deref(), list[0].read_only), ("Personnel", Some("#0082c9"), false));
        assert_eq!(list[0].ctag.as_deref(), Some("http://sabre.io/ns/sync/42"));
        assert_eq!((list[1].name.as_str(), list[1].read_only, list[1].ctag.as_deref()), ("holidays fr", true, Some("t9")));
    }

    #[test]
    fn where_to_look_for_contacts() {
        let list: Vec<String> = card_candidates("y@me.com", "imap.mail.me.com", None).iter().map(Url::to_string).collect();
        assert_eq!(list[0], "https://contacts.icloud.com/");
        let manual = Url::parse("https://cloud.example.org/remote.php/dav").unwrap();
        let list: Vec<String> = card_candidates("y@example.org", "mail.example.org", Some(&manual)).iter().map(Url::to_string).collect();
        assert_eq!(list, ["https://cloud.example.org/remote.php/dav", "https://example.org/.well-known/carddav", "https://mail.example.org/.well-known/carddav"]);
    }

    #[test]
    fn reads_an_address_book_home_and_its_cards() {
        let base = Url::parse("https://cloud.example.org/remote.php/dav/").unwrap();
        let xml = r#"<d:multistatus xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:response><d:href>/p/</d:href><d:propstat><d:prop>
            <card:addressbook-home-set><d:href>/remote.php/dav/addressbooks/users/y/</d:href></card:addressbook-home-set>
            </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
        assert_eq!(parse_card_principal(xml, &base).unwrap().home.unwrap().as_str(), "https://cloud.example.org/remote.php/dav/addressbooks/users/y/");
        let xml = "<d:multistatus xmlns:d=\"DAV:\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\"><d:response><d:href>/ab/y/c1.vcf</d:href>
            <d:propstat><d:prop><card:address-data>BEGIN:VCARD\r\nFN:Marie\r\nEMAIL:marie@example.org\r\nEND:VCARD\r\n</card:address-data></d:prop>
            <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>";
        let doc = document(xml).unwrap();
        let (href, props) = responses(&doc).remove(0);
        let cards = crate::contacts::parse_vcards(&href, &text(props.into_iter().find(|p| is(p, CARDDAV, "address-data")).unwrap()));
        assert_eq!((cards[0].remote_id.as_str(), cards[0].name.as_deref(), cards[0].emails[0].as_str()), ("/ab/y/c1.vcf", Some("Marie"), "marie@example.org"));
    }

    #[test]
    fn lists_the_cards_of_a_book() {
        let xml = r#"<d:multistatus xmlns:d="DAV:">
 <d:response><d:href>/ab/y/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getetag>"c"</d:getetag></d:prop>
  <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/ab/y/a&amp;b.vcf</d:href><d:propstat><d:prop><d:resourcetype/><d:getetag>"1"</d:getetag></d:prop>
  <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let hrefs = card_hrefs(xml).unwrap();
        assert_eq!(hrefs, ["/ab/y/a&b.vcf"], "the book itself left out");
        assert!(cards_body(&hrefs).contains("<d:href>/ab/y/a&amp;b.vcf</d:href>"));
    }

    #[test]
    fn reads_expanded_events() {
        let xml = "<?xml version=\"1.0\"?>
<d:multistatus xmlns:d=\"DAV:\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\">
 <d:response><d:href>/cal/y/personal/weekly.ics</d:href><d:propstat><d:prop><d:getetag>\"1\"</d:getetag>
  <cal:calendar-data><![CDATA[BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:weekly\r\nRECURRENCE-ID:20261020T120000Z\r\n\
DTSTART:20261020T120000Z\r\nDTEND:20261020T130000Z\r\nSUMMARY:Réunion\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly\r\n\
RECURRENCE-ID:20261027T130000Z\r\nDTSTART:20261027T130000Z\r\nDTEND:20261027T140000Z\r\nSUMMARY:Réunion\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n]]></cal:calendar-data>
 </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
 <d:response><d:href>/cal/y/personal/gone.ics</d:href><d:propstat><d:prop><cal:calendar-data/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
</d:multistatus>";
        let data = parse_calendar_data(xml).unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0].0, "/cal/y/personal/weekly.ics");
        let events = crate::calendar::instances(&data[0].1, 1_792_000_000, 1_794_000_000, &[]);
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].summary.as_str(), events[0].start, events[0].end - events[0].start), ("Réunion", 1_792_497_600, 3600));
        assert_ne!(events[0].instance, events[1].instance);
    }
}
