//! DKIM checked locally (RFC 6376, Stalwart's `mail-auth`) when the provider's verdict cannot be
//! used: none written, written by a server other than the account's usual one, or no DKIM result.
//!
//! Only the DNS TXT lookup of a signature's key goes out, through the system resolver: never the
//! report address (`_report._domainkey`, RFC 6651) nor third-party authorisations (ATPS, RFC
//! 6541). A key is looked up once per few hours, a few per message at most, and never when its
//! name holds the account's own address (a selector made for one recipient would tell the sender
//! the message was just opened). Keys are only looked up when a message arrives: on opening,
//! only already known keys are used, so a selector unique to one message can never time its
//! opening.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use hickory_resolver::TokioResolver;
use mail_auth::common::crypto::{Algorithm, CryptoError};
use mail_auth::common::resolver::ToFqdn;
use mail_auth::dkim::DkimError;
use mail_auth::{AuthenticatedMessage, DkimResult, DnsError, Error, MessageAuthenticator, Parameters, ResolverCache, Txt};

use crate::trust::{Context, DkimLocal, Signals};

/// How long a key, or its absence, is kept.
const KEY_TTL: Duration = Duration::from_secs(4 * 3600);
/// Longest wait for the keys of one message (a resolver may hang).
const TIMEOUT: Duration = Duration::from_secs(6);
/// Keys looked up per message, at most (a message may carry dozens of signatures).
const MAX_LOOKUPS: usize = 3;
/// A message older than this whose signature does not match the key published today is not
/// called forged: senders replace their keys, and the old one may be gone.
const KEY_ROTATION: i64 = 30 * 86_400;
const MAX_KEYS: usize = 1000;

/// Not for junk, spam, blocked or already untrustworthy senders: the key lookup reaches the
/// sender's DNS servers.
pub fn wanted(s: &Signals, c: &Context) -> bool {
    let usable = match s.dkim {
        None => false,
        Some(true) => c.foreign_results.is_none(),
        // An announced failure counts even from an unknown server.
        Some(false) => true,
    };
    !usable && s.dkim_local.is_none() && !s.spam_flag && !c.in_junk && !c.blocked && !crate::trust::evaluate(s, c).is_low()
}

/// Keys of the signing domains by DNS name (`selector._domainkey.domain.`), with their expiry.
#[derive(Default)]
pub struct Keys {
    entries: Mutex<HashMap<Box<str>, (Txt, Instant)>>,
}

impl Keys {
    fn get<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entries = self.entries.lock().unwrap();
        entries.get(name).filter(|(_, until)| *until > Instant::now()).map(|(txt, _)| txt.clone())
    }

    fn put(&self, name: Box<str>, record: Txt) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= MAX_KEYS {
            let now = Instant::now();
            entries.retain(|_, (_, until)| *until > now);
            if entries.len() >= MAX_KEYS {
                entries.clear();
            }
        }
        entries.insert(name, (record, Instant::now() + KEY_TTL));
    }

    #[cfg(test)]
    fn publish(&self, name: &str, record: &str) {
        use mail_auth::common::parse::TxtRecordParser;
        let key = mail_auth::common::verify::DomainKey::parse(record.as_bytes()).expect("key");
        self.put(name.to_fqdn().into_owned().into_boxed_str(), Txt::DomainKey(std::sync::Arc::new(key)));
    }
}

/// The key cache as `mail-auth` sees it while checking one message: any name other than the
/// keys facteur agreed to look up gets an answer at once ("not looked up"), so `mail-auth`
/// never sends a query for it.
struct Gate<'a> {
    keys: &'a Keys,
    allowed: HashSet<Box<str>>,
    /// Only the keys already in the cache: a key not there is "not looked up" too.
    cached_only: bool,
}

impl ResolverCache<Box<str>, Txt> for Gate<'_> {
    fn get<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let skipped = || Some(Txt::Error(Error::Io("not looked up by facteur".into())));
        if !self.allowed.contains(name) {
            return skipped();
        }
        match self.keys.get(name) {
            None if self.cached_only => skipped(),
            known => known,
        }
    }

    fn remove<Q>(&self, name: &Q) -> Option<Txt>
    where
        Box<str>: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.keys.entries.lock().unwrap().remove(name).map(|(txt, _)| txt)
    }

    // The key's own TTL is not used: kept a few hours whatever it says (fewer lookups).
    fn insert(&self, key: Box<str>, value: Txt, _valid_until: Instant) {
        if self.allowed.contains(&key) && !self.cached_only {
            self.keys.put(key, value);
        }
    }
}

#[derive(Default)]
pub struct Verifier {
    resolver: OnceLock<Option<MessageAuthenticator>>,
    keys: Keys,
}

impl Verifier {
    /// `None`: no usable system configuration (no public resolver is used in its place).
    fn authenticator(&self) -> Option<&MessageAuthenticator> {
        self.resolver
            .get_or_init(|| TokioResolver::builder_tokio().ok().and_then(|b| b.build().ok()).map(MessageAuthenticator))
            .as_ref()
    }

    /// `None` when no verdict can be given (no signature, key not found, DNS unreachable or too
    /// slow, expired, `l=`, RSA-SHA1 or unsupported). `lookup`: only for a message that just
    /// arrived; when it is being opened, a lookup would tell the sender when it was read.
    pub async fn verify(&self, raw: &[u8], from_domain: &str, own: &[String], sent_at: i64, lookup: bool) -> Option<DkimLocal> {
        let auth = self.authenticator()?;
        verify_with(auth, &self.keys, raw, from_domain, own, sent_at, crate::store::now(), lookup).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn verify_with(auth: &MessageAuthenticator, keys: &Keys, raw: &[u8], from_domain: &str, own: &[String], sent_at: i64, now: i64, lookup: bool) -> Option<DkimLocal> {
    let from = crate::autoconfig::registrable(&from_domain.trim_end_matches('.').to_ascii_lowercase());
    let aligned = |d: &str| !from.is_empty() && crate::autoconfig::registrable(&d.trim_end_matches('.').to_ascii_lowercase()) == from;
    // Being opened: without a known key for one of its signatures there is no verdict to give,
    // so the body is not hashed for nothing (read from the headers alone).
    if !lookup {
        let signatures = header_signatures(raw);
        let allowed = allowed_keys(signatures.iter().map(|s| (s.s.as_str(), s.d.as_str())), &aligned, own);
        if !allowed.iter().any(|name| matches!(keys.get(&**name), Some(Txt::DomainKey(_)))) {
            return None;
        }
    }
    // Hashes the whole body: off the async worker for a large message.
    let message = crate::rt::offload(|| AuthenticatedMessage::parse(raw))?;
    if message.dkim_headers.is_empty() {
        return None;
    }
    let allowed = allowed_keys(message.dkim_headers.iter().map(|h| (h.header.s.as_str(), h.header.d.as_str())), &aligned, own);
    // A mailing list or a forwarder (ARC) may have changed the message on the way, a footer or a
    // subject tag: its broken signature is then no sign of forgery (RFC 6376, 6.1).
    let relayed = message.headers.iter().any(|(name, _)| {
        let name = std::str::from_utf8(name).unwrap_or("").trim().trim_end_matches(':');
        ["List-Id", "List-Post", "Mailing-List", "ARC-Seal"].iter().any(|h| name.eq_ignore_ascii_case(h))
    });
    let gate = Gate { keys, allowed, cached_only: !lookup };
    let outputs = tokio::time::timeout(TIMEOUT, auth.verify_dkim(Parameters::new(&message).with_txt_cache(&gate))).await.ok()?;

    let rank = |l: &DkimLocal| match (l.pass, l.aligned) {
        (true, true) => 3,
        (true, false) => 2,
        (false, true) => 1,
        (false, false) => 0,
    };
    let mut best: Option<DkimLocal> = None;
    for output in &outputs {
        let Some(signature) = output.signature() else { continue };
        // RSA-SHA1 no longer proves anything (RFC 8301).
        if signature.a == Algorithm::RsaSha1 {
            continue;
        }
        let pass = match output.result() {
            DkimResult::Pass => true,
            // The body is not the one signed: whatever the key, the message was changed.
            DkimResult::Neutral(Error::Dkim(DkimError::FailedBodyHashMatch)) if !relayed => false,
            DkimResult::Fail(Error::Crypto(CryptoError::FailedVerification)) if !relayed && now - sent_at < KEY_ROTATION => false,
            DkimResult::PermError(err @ Error::Dns(DnsError::RecordNotFound(_))) if lookup => {
                // No such key: not asked again for a while either.
                keys.put(key_name(&signature.s, &signature.d), Txt::Error(err.clone()));
                continue;
            }
            _ => continue,
        };
        let candidate = DkimLocal { pass, domain: signature.d.trim_end_matches('.').to_ascii_lowercase(), aligned: aligned(&signature.d) };
        if best.as_ref().is_none_or(|b| rank(&candidate) > rank(b)) {
            best = Some(candidate);
        }
    }
    best
}

/// The keys that may be looked up for these signatures (selector, domain): those of the From
/// domain first, a few at most, never one naming the recipient.
fn allowed_keys<'a>(signatures: impl Iterator<Item = (&'a str, &'a str)>, aligned: &dyn Fn(&str) -> bool, own: &[String]) -> HashSet<Box<str>> {
    let mut names: Vec<(bool, Box<str>)> = signatures
        .map(|(s, d)| (aligned(d), key_name(s, d)))
        .filter(|(_, name)| !names_recipient(name, own))
        .collect();
    names.sort_by_key(|(first, _)| !*first);
    let mut allowed = HashSet::new();
    for (_, name) in names {
        if allowed.len() < MAX_LOOKUPS {
            allowed.insert(name);
        }
    }
    allowed
}

/// The DKIM signatures of the message's header block, parsed as `mail-auth` does (those with
/// `l=` left out), without reading its body.
fn header_signatures(raw: &[u8]) -> Vec<mail_auth::dkim::Signature> {
    let mut signatures = Vec::new();
    let mut field = |bytes: &[u8]| {
        let Some(colon) = bytes.iter().position(|&b| b == b':') else { return };
        let name = std::str::from_utf8(&bytes[..colon]).unwrap_or("").trim();
        if name.eq_ignore_ascii_case("DKIM-Signature")
            && let Ok(signature) = mail_auth::dkim::Signature::parse(&bytes[colon + 1..])
            && signature.l == 0
        {
            signatures.push(signature);
        }
    };
    let (mut start, mut pos) = (None, 0);
    while pos < raw.len() {
        let end = raw[pos..].iter().position(|&b| b == b'\n').map_or(raw.len(), |i| pos + i + 1);
        let line = &raw[pos..end];
        if line == b"\n" || line == b"\r\n" {
            break;
        }
        // A line that does not start with a space begins a new field (otherwise: folded).
        if !matches!(line[0], b' ' | b'\t') {
            if let Some(from) = start {
                field(&raw[from..pos]);
            }
            start = Some(pos);
        }
        pos = end;
    }
    if let Some(from) = start {
        field(&raw[from..pos]);
    }
    signatures
}

/// DNS name of a signature's key, as `mail-auth` asks for it (lowercase, final dot).
fn key_name(selector: &str, domain: &str) -> Box<str> {
    format!("{selector}._domainkey.{domain}").to_fqdn().into_owned().into_boxed_str()
}

/// The key's DNS name holds one of the account's addresses (its local part, separators aside):
/// a selector made for this recipient, whose lookup would tell the sender who opened the
/// message, and when. Short local parts are left out (« info », « mail » are common selectors).
fn names_recipient(name: &str, own: &[String]) -> bool {
    let squash = |s: &str| s.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_ascii_lowercase();
    let name = squash(name);
    own.iter().any(|address| {
        let local = squash(address.split('@').next().unwrap_or(""));
        local.len() >= 5 && name.contains(&local)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_resolver::config::ResolverConfig;
    use hickory_resolver::net::runtime::TokioRuntimeProvider;

    /// RFC 8463, appendix A: one message signed with Ed25519 and with RSA, and both keys.
    const ED25519_KEY: &str = "v=DKIM1; k=ed25519; p=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    const ED25519_SEED: &str = "nWGxne/9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A=";
    const RSA_KEY: &str = "v=DKIM1; k=rsa; p=MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDkHlOQoBTzWRiGs5V6NpP3idY6Wk08a5qhdR6wy5bdOKb2jLQiY/J16JYi0Qvx/byYzCNb3W91y3FutACDfzwQ/BC/e/8uBsCR+yz1Lxj+PL6lHvqMKrM3rG4hstT5QjvHO9PzoxZyVYLzBfO2EeC3Ip3G+2kryOTIKT+l/K4w3QIDAQAB";
    const ED25519_SIGNATURE: &str = "DKIM-Signature: v=1; a=ed25519-sha256; c=relaxed/relaxed;\r\n \
        d=football.example.com; i=@football.example.com;\r\n \
        q=dns/txt; s=brisbane; t=1528637909; h=from : to :\r\n \
        subject : date : message-id : from : subject : date;\r\n \
        bh=2jUSOH9NhtVGCQWNr9BrIAPreKQjO6Sn7XIkfJVOzv8=;\r\n \
        b=/gCrinpcQOoIfuHNQIbq4pgh9kyIK3AQUdt9OdqQehSwhEIug4D11Bus\r\n \
        Fa3bT3FY5OsU7ZbnKELq+eXdp1Q1Dw==\r\n";
    const RSA_SIGNATURE: &str = "DKIM-Signature: v=1; a=rsa-sha256; c=relaxed/relaxed;\r\n \
        d=football.example.com; i=@football.example.com;\r\n \
        q=dns/txt; s=test; t=1528637909; h=from : to : subject :\r\n \
        date : message-id : from : subject : date;\r\n \
        bh=2jUSOH9NhtVGCQWNr9BrIAPreKQjO6Sn7XIkfJVOzv8=;\r\n \
        b=F45dVWDfMbQDGHJFlXUNB2HKfbCeLRyhDXgFpEL8GwpsRe0IeIixNTe3\r\n \
        DhCVlUrSjV4BwcVcOF6+FF3Zo9Rpo1tFOeS9mPYQTnGdaSGsgeefOsk2Jz\r\n \
        dA+L10TeYt9BgDfQNZtKdN1WO//KgIqXP7OdEFE4LjFYNcUxZQ4FADY+8=\r\n";
    const MESSAGE: &str = "From: Joe SixPack <joe@football.example.com>\r\n\
        To: Suzie Q <suzie@shopping.example.net>\r\n\
        Subject: Is dinner ready?\r\n\
        Date: Fri, 11 Jul 2003 21:00:37 -0700 (PDT)\r\n\
        Message-ID: <20030712040037.46341.5F8J@football.example.com>\r\n\
        \r\n\
        Hi.\r\n\
        \r\n\
        We lost the game.  Are you hungry yet?\r\n\
        \r\n\
        Joe.\r\n";
    const SENT: i64 = 1_057_982_437;

    /// A resolver with no server at all: nothing a test does can reach the network.
    fn offline() -> MessageAuthenticator {
        let config = ResolverConfig::from_parts(None, vec![], vec![]);
        MessageAuthenticator(TokioResolver::builder_with_config(config, TokioRuntimeProvider::default()).build().expect("resolver"))
    }

    fn keys() -> Keys {
        let keys = Keys::default();
        keys.publish("brisbane._domainkey.football.example.com", ED25519_KEY);
        keys.publish("test._domainkey.football.example.com", RSA_KEY);
        keys
    }

    async fn check(raw: &str, from: &str, now: i64) -> Option<DkimLocal> {
        verify_with(&offline(), &keys(), raw.as_bytes(), from, &["suzie@shopping.example.net".into()], SENT, now, true).await
    }

    #[tokio::test]
    async fn verifies_the_rfc_8463_example() {
        let pass = Some(DkimLocal { pass: true, domain: "football.example.com".into(), aligned: true });
        for signature in [ED25519_SIGNATURE, RSA_SIGNATURE] {
            assert_eq!(check(&format!("{signature}{MESSAGE}"), "football.example.com", SENT + 60).await, pass);
        }
        // Signed by the domain of another address than the one shown: valid, not aligned.
        let other = check(&format!("{ED25519_SIGNATURE}{MESSAGE}"), "bank.example", SENT + 60).await;
        assert_eq!(other, Some(DkimLocal { pass: true, domain: "football.example.com".into(), aligned: false }));
        // A subdomain of the From domain is aligned (same registrable domain).
        assert!(check(&format!("{ED25519_SIGNATURE}{MESSAGE}"), "news.example.com", SENT + 60).await.is_some_and(|l| l.aligned));

        // The body changed: invalid, whatever the date.
        let altered = format!("{ED25519_SIGNATURE}{}", MESSAGE.replace("We lost", "We won"));
        assert_eq!(check(&altered, "football.example.com", SENT + 365 * 86_400).await.map(|l| l.pass), Some(false));
        // Unless a mailing list or a forwarder passed it on: their footers break signatures.
        for relay in ["List-Id: <news.lists.example>\r\n", "list-post: <mailto:news@lists.example>\r\n", "ARC-Seal: i=1; a=rsa-sha256; cv=none; d=relay.example; s=k; b=x\r\n"] {
            let listed = format!("{ED25519_SIGNATURE}{relay}{}", MESSAGE.replace("We lost", "We won"));
            assert_eq!(check(&listed, "football.example.com", SENT + 60).await, None, "{relay}");
        }
        // A signed header changed: invalid if recent; years later the key may have been replaced.
        let subject = format!("{RSA_SIGNATURE}{}", MESSAGE.replace("Is dinner ready?", "Your account is locked"));
        assert_eq!(check(&subject, "football.example.com", SENT + 60).await.map(|l| l.pass), Some(false));
        assert_eq!(check(&subject, "football.example.com", SENT + 365 * 86_400).await, None);
        // One valid signature is enough.
        let broken = RSA_SIGNATURE.replace("b=F45dVWDf", "b=F45dVWDg");
        assert_eq!(check(&format!("{broken}{MESSAGE}"), "football.example.com", SENT + 60).await.map(|l| l.pass), Some(false));
        let both = format!("{broken}{ED25519_SIGNATURE}{MESSAGE}");
        assert_eq!(check(&both, "football.example.com", SENT + 60).await.map(|l| l.pass), Some(true));

        assert_eq!(check(MESSAGE, "football.example.com", SENT).await, None, "no signature");
        // No key known, and the resolver has no server: no verdict, not a failure.
        let unknown = ED25519_SIGNATURE.replace("s=brisbane", "s=sydney");
        assert_eq!(check(&format!("{unknown}{MESSAGE}"), "football.example.com", SENT).await, None);
    }

    #[tokio::test]
    async fn relaxed_canonicalisation_survives_whitespace_changes() {
        // Header names' case, folding and spaces, trailing spaces and empty lines at the end of
        // the body: what relaxed canonicalisation ignores (RFC 6376, 3.4.2 and 3.4.4).
        let reshaped = MESSAGE
            .replace("Subject: Is dinner ready?", "SUBJECT:   Is dinner\r\n\tready?  ")
            .replace("Joe.\r\n", "Joe.   \r\n\r\n\r\n")
            .replace("game.  Are", "game.    Are");
        assert_eq!(check(&format!("{ED25519_SIGNATURE}{reshaped}"), "football.example.com", SENT).await.map(|l| l.pass), Some(true));
        // A word added is no whitespace.
        let added = MESSAGE.replace("Joe.\r\n", "Joe.\r\nPS: click here\r\n");
        assert_eq!(check(&format!("{ED25519_SIGNATURE}{added}"), "football.example.com", SENT).await.map(|l| l.pass), Some(false));
    }

    #[tokio::test]
    async fn simple_canonicalisation_and_length_limits() {
        use base64::Engine as _;
        use mail_auth::common::crypto::Ed25519Key;
        use mail_auth::common::headers::HeaderWriter;
        use mail_auth::dkim::{Canonicalization, DkimSigner};
        let b64 = base64::engine::general_purpose::STANDARD;
        let public = b64.decode(ED25519_KEY.rsplit_once("p=").unwrap().1).unwrap();
        let seed = b64.decode(ED25519_SEED).unwrap();
        let sign = |canonicalization: Canonicalization, length: bool| {
            let key = Ed25519Key::from_seed_and_public_key(&seed, &public).unwrap();
            DkimSigner::from_key(key)
                .domain("football.example.com")
                .selector("brisbane")
                .headers(["From", "To", "Subject", "Date"])
                .header_canonicalization(canonicalization)
                .body_canonicalization(canonicalization)
                .body_length(length)
                .sign(MESSAGE.as_bytes())
                .unwrap()
                .to_header()
        };
        let now = crate::store::now();
        let simple = sign(Canonicalization::Simple, false);
        assert_eq!(check(&format!("{simple}{MESSAGE}"), "football.example.com", now).await.map(|l| l.pass), Some(true));
        // Simple canonicalisation keeps every space: one more is a change.
        let spaced = MESSAGE.replace("game.  Are", "game.   Are");
        assert_eq!(check(&format!("{simple}{spaced}"), "football.example.com", now).await.map(|l| l.pass), Some(false));
        // Only empty lines at the very end are ignored.
        let trailing = format!("{simple}{MESSAGE}\r\n\r\n");
        assert_eq!(check(&trailing, "football.example.com", now).await.map(|l| l.pass), Some(true));

        // `l=`: whatever follows the signed length would be unsigned. Not trusted at all.
        let limited = sign(Canonicalization::Relaxed, true);
        assert!(limited.contains("l="));
        assert_eq!(check(&format!("{limited}{MESSAGE}"), "football.example.com", now).await, None);
    }

    #[test]
    fn only_the_keys_are_looked_up() {
        let keys = keys();
        let gate = Gate { keys: &keys, allowed: HashSet::from(["brisbane._domainkey.football.example.com.".into()]), cached_only: false };
        assert!(matches!(gate.get("brisbane._domainkey.football.example.com."), Some(Txt::DomainKey(_))));
        // Allowed but not in the cache: looked up, unless the message is being opened.
        let unknown: HashSet<Box<str>> = HashSet::from(["r8f2k9q1x._domainkey.trk.example.".into()]);
        assert!(Gate { keys: &keys, allowed: unknown.clone(), cached_only: false }.get("r8f2k9q1x._domainkey.trk.example.").is_none());
        let opening = Gate { keys: &keys, allowed: unknown, cached_only: true };
        assert!(matches!(opening.get("r8f2k9q1x._domainkey.trk.example."), Some(Txt::Error(_))));
        let opening = Gate { keys: &keys, allowed: HashSet::from(["brisbane._domainkey.football.example.com.".into()]), cached_only: true };
        assert!(matches!(opening.get("brisbane._domainkey.football.example.com."), Some(Txt::DomainKey(_))));
        // Known to the cache, but not one of this message's keys.
        assert!(matches!(gate.get("test._domainkey.football.example.com."), Some(Txt::Error(_))));
        // Report addresses (RFC 6651) and third-party authorisations (RFC 6541): never asked.
        assert!(matches!(gate.get("_report._domainkey.football.example.com."), Some(Txt::Error(_))));
        assert!(matches!(gate.get("football.example.com._atps.example.net."), Some(Txt::Error(_))));

        let own = ["yann.dupont@example.net".to_string(), "me@example.net".to_string()];
        assert!(names_recipient("yanndupont-4821._domainkey.tracker.example.", &own));
        assert!(names_recipient("s.yann-dupont._domainkey.tracker.example.", &own));
        assert!(!names_recipient("me._domainkey.example.org.", &own), "too short to tell");
        assert!(!names_recipient("google._domainkey.example.org.", &own));
    }

    #[test]
    fn checked_only_when_the_provider_says_nothing_usable() {
        let ctx = Context::default();
        assert!(wanted(&Signals::default(), &ctx));
        let provider_pass = Signals { dkim: Some(true), ..Default::default() };
        assert!(!wanted(&provider_pass, &ctx));
        assert!(wanted(&provider_pass, &Context { foreign_results: Some("mx.forged.example".into()), ..Default::default() }));
        assert!(!wanted(&Signals { dkim: Some(false), ..Default::default() }, &ctx));
        let done = Signals { dkim_local: Some(DkimLocal { pass: true, domain: "x.example".into(), aligned: true }), ..Default::default() };
        assert!(!wanted(&done, &ctx));
        // The sender's DNS is not contacted for junk.
        assert!(!wanted(&Signals { spam_flag: true, ..Default::default() }, &ctx));
        assert!(!wanted(&Signals::default(), &Context { in_junk: true, ..Default::default() }));
        assert!(!wanted(&Signals::default(), &Context { blocked: true, ..Default::default() }));
        // Nor for a message already untrustworthy (an address in the display name).
        assert!(!wanted(&Signals { spoofed_name: true, ..Default::default() }, &ctx));
    }

    #[tokio::test]
    async fn an_opened_message_uses_the_known_keys_only() {
        let raw = format!("{ED25519_SIGNATURE}{MESSAGE}");
        let own = ["suzie@shopping.example.net".to_string()];
        let opened = verify_with(&offline(), &keys(), raw.as_bytes(), "football.example.com", &own, SENT, SENT + 60, false).await;
        assert_eq!(opened.map(|l| l.pass), Some(true));
        // Key not known yet: no verdict, and nothing was asked.
        let unknown = raw.replace("s=brisbane", "s=r8f2k9q1x");
        assert_eq!(verify_with(&offline(), &keys(), unknown.as_bytes(), "football.example.com", &own, SENT, SENT + 60, false).await, None);
        // One known key among the signatures is enough to check it.
        let both = format!("{}{RSA_SIGNATURE}{MESSAGE}", ED25519_SIGNATURE.replace("s=brisbane", "s=r8f2k9q1x"));
        let opened = verify_with(&offline(), &keys(), both.as_bytes(), "football.example.com", &own, SENT, SENT + 60, false).await;
        assert_eq!(opened.map(|l| l.pass), Some(true));
    }

    #[test]
    fn the_signatures_are_read_from_the_headers_alone() {
        let names = |raw: &str| header_signatures(raw.as_bytes()).into_iter().map(|s| key_name(&s.s, &s.d)).collect::<Vec<_>>();
        let both = format!("{ED25519_SIGNATURE}{}{MESSAGE}", RSA_SIGNATURE.replace("DKIM-Signature:", "dkim-signature :"));
        assert_eq!(names(&both), ["brisbane._domainkey.football.example.com.".into(), "test._domainkey.football.example.com.".into()] as [Box<str>; 2]);
        // The same ones `mail-auth` finds.
        let parsed = AuthenticatedMessage::parse(both.as_bytes()).unwrap();
        assert_eq!(names(&both), parsed.dkim_headers.iter().map(|h| key_name(&h.header.s, &h.header.d)).collect::<Vec<_>>());
        // A signature quoted in the body is not one, nor is a header with no colon.
        let quoted = format!("{MESSAGE}\r\n{RSA_SIGNATURE}");
        assert!(names(&quoted).is_empty());
        assert_eq!(names(&format!("X-Broken\r\n{ED25519_SIGNATURE}{MESSAGE}")).len(), 1);
        assert!(names(MESSAGE).is_empty());
        assert!(names("").is_empty());
    }
}
