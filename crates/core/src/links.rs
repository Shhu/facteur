use serde::Serialize;

use crate::autoconfig::registrable;
use crate::tr;
use crate::trust::{Level, Reason, Trust};

/// Link shorteners: the real destination does not appear in the link.
const SHORTENERS: &[&str] = &[
    "bit.ly", "t.co", "tinyurl.com", "goo.gl", "ow.ly", "buff.ly", "is.gd", "rebrand.ly", "cutt.ly", "lnkd.in",
    "t.ly", "shorturl.at", "rb.gy", "s.id", "tiny.cc", "bl.ink", "short.io", "dub.sh", "urlz.fr", "v.gd", "x.co",
];

/// Click trackers of mailing services (registrable domain, service name): every link points at
/// the service, which forwards to the address shown. Where it forwards cannot be read without
/// following the link (which counts a click).
const CLICK_TRACKERS: &[(&str, &str)] = &[
    ("sendgrid.net", "SendGrid"), ("list-manage.com", "Mailchimp"), ("mandrillapp.com", "Mandrill"),
    ("sparkpostmail.com", "SparkPost"), ("hubspotlinks.com", "HubSpot"), ("sendibt2.com", "Brevo"),
    ("sendibt3.com", "Brevo"), ("sendibm1.com", "Brevo"), ("mjt.lu", "Mailjet"), ("awstrack.me", "Amazon SES"),
    ("exct.net", "Salesforce"), ("klclick.com", "Klaviyo"), ("klclick1.com", "Klaviyo"), ("klclick2.com", "Klaviyo"),
    ("klclick3.com", "Klaviyo"), ("rs6.net", "Constant Contact"), ("createsend1.com", "Campaign Monitor"),
    ("cmail19.com", "Campaign Monitor"), ("cmail20.com", "Campaign Monitor"), ("pstmrk.it", "Postmark"),
    ("mlsend.com", "MailerLite"), ("convertkit-mail.com", "Kit"), ("customeriomail.com", "Customer.io"),
];

/// Known tracking parameters, besides every `utm_*`.
const TRACKING: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "yclid", "twclid", "igshid", "mc_cid", "mc_eid",
    "_hsenc", "_hsmi", "mkt_tok", "oly_anon_id", "oly_enc_id", "vero_id", "vero_conv", "wickedid", "trk",
    "trkcampaign", "sc_campaign", "sc_channel", "ml_subscriber", "ml_subscriber_hash", "s_cid", "ncid", "spm",
];

#[derive(Debug, Clone, Serialize)]
pub struct LinkFacts {
    pub url: String,
    /// Accents restored (`xn--` decoded).
    pub host: String,
    /// Registrable domain (`mail.client.banque.fr` -> `banque.fr`), accents restored.
    pub domain: String,
    /// As spelled in DNS, for lookups.
    #[serde(skip)]
    pub domain_ascii: String,
    pub secure: bool,
    pub ip: bool,
    /// Non-Latin or special characters in the domain (possible homograph).
    pub punycode: bool,
    /// Domain the link text announces, if any.
    pub shown_host: Option<String>,
    /// The text announces another domain than the real destination.
    pub deceptive: bool,
    pub shortener: bool,
    /// Click tracker of a mailing service (its name): the address shown cannot be checked.
    pub tracker: Option<&'static str>,
    /// Destination embedded in the link (click-tracking redirectors: `?url=https://…`).
    pub redirect_to: Option<String>,
    pub redirect_domain: Option<String>,
    pub tracking: Vec<String>,
    pub clean_url: Option<String>,
}

fn bare(host: &str) -> String {
    host.trim_end_matches('.').to_lowercase().trim_start_matches("www.").to_string()
}

fn unicode(host: &str) -> String {
    idna::domain_to_unicode(host).0
}

/// Domain a link text shows: its leading domain-like token once invisible characters are gone
/// (a zero-width space must not split « pay\u{200B}pal.com » out of reach). Normalised like a URL
/// host (IDNA mapping, fullwidth dots, case) so both sides of the comparison match.
fn shown_host(text: &str) -> Option<String> {
    let t: String = text.chars().filter(|c| !crate::attachments::invisible(*c)).collect();
    let t = t.trim_start();
    let t = ["https://", "http://", "//"]
        .iter()
        .find_map(|p| t.get(..p.len()).filter(|h| h.eq_ignore_ascii_case(p)).map(|_| &t[p.len()..]))
        .unwrap_or(t);
    // Dots as IDNA reads them: ideographic and fullwidth full stops are dots too.
    let token: String = t
        .chars()
        .take_while(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '\u{3002}' | '\u{FF0E}' | '\u{FF61}'))
        .collect();
    // A sentence may end right after the domain: « Rendez-vous sur paypal.com. »
    let token = token.trim_end_matches(['.', '\u{3002}', '\u{FF0E}', '\u{FF61}']);
    let ascii = url::Url::parse(&format!("http://{token}/")).ok()?.host_str()?.to_string();
    let labels: Vec<&str> = ascii.split('.').collect();
    let valid = labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty())
        // « voir.plus » or « rapport.pdf » are no domains: the end must be a real public suffix.
        && labels.last().is_some_and(|tld| tld.len() >= 2 && (tld.starts_with("xn--") || tld.chars().all(|c| c.is_ascii_alphabetic())))
        && psl::suffix(ascii.as_bytes()).is_some_and(|s| s.is_known());
    valid.then(|| bare(&unicode(&ascii)))
}

pub fn analyze(href: &str, text: &str) -> Option<LinkFacts> {
    let url = url::Url::parse(href.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let ascii = bare(url.host_str()?);
    let ip = matches!(url.host(), Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)));
    let host = if ip { ascii.clone() } else { unicode(&ascii) };
    let domain_ascii = if ip { ascii.clone() } else { registrable(&ascii) };
    let domain = if ip { ascii.clone() } else { unicode(&domain_ascii) };

    let shown = shown_host(text);
    let deceptive = shown.as_ref().is_some_and(|s| *s != host && !host.ends_with(&format!(".{s}")) && !s.ends_with(&format!(".{host}")));

    let redirect_to = url
        .query_pairs()
        .map(|(_, v)| v.into_owned())
        .find(|v| v.starts_with("https://") || v.starts_with("http://"));
    let redirect_domain = redirect_to
        .as_deref()
        .and_then(|r| url::Url::parse(r).ok())
        .and_then(|u| u.host_str().map(|h| unicode(&registrable(&bare(h)))))
        .filter(|d| *d != domain);

    let is_tracking = |k: &str| {
        let k = k.to_ascii_lowercase();
        k.starts_with("utm_") || TRACKING.contains(&k.as_str())
    };
    let tracking: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).filter(|k| is_tracking(k)).collect();
    let clean_url = (!tracking.is_empty()).then(|| {
        let mut clean = url.clone();
        let kept: Vec<(String, String)> =
            url.query_pairs().filter(|(k, _)| !is_tracking(k)).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        if kept.is_empty() {
            clean.set_query(None);
        } else {
            clean.query_pairs_mut().clear().extend_pairs(kept);
        }
        clean.to_string()
    });

    Some(LinkFacts {
        url: url.to_string(),
        punycode: !ip && ascii.split('.').any(|l| l.starts_with("xn--")),
        secure: url.scheme() == "https",
        shortener: SHORTENERS.contains(&ascii.as_str()),
        tracker: CLICK_TRACKERS.iter().find(|(d, _)| *d == domain_ascii).map(|(_, name)| *name),
        host,
        domain,
        domain_ascii,
        ip,
        shown_host: shown,
        deceptive,
        redirect_to,
        redirect_domain,
        tracking,
        clean_url,
    })
}

#[derive(Debug, Clone, Default)]
pub struct LinkContext {
    pub message_trust: Option<Level>,
    pub sender_domain: bool,
    /// Other messages (junk aside) linking to this domain, and their senders.
    pub seen_messages: i64,
    pub seen_senders: i64,
    pub wrote_to: bool,
    /// RDAP creation date, if known.
    pub registered: Option<i64>,
    /// What facteur knows of the domain the text announces, for a deceptive link.
    pub announced: Option<AnnouncedDomain>,
    /// The link is on the From address's domain, but that address is not proven (no DMARC
    /// pass, no DKIM signature of its own): it does not vouch for the link.
    pub sender_domain_unproven: Option<String>,
    pub now: i64,
}

/// The domain a deceptive link's text announces (« app.wesend-sante.fr » shown, a click tracker
/// behind), as facteur knows it: where the user expects to land.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AnnouncedDomain {
    /// Accents restored.
    pub domain: String,
    /// RDAP creation date, if known.
    pub registered: Option<i64>,
    /// Other messages (junk and bin aside) linking to it or sent from it.
    pub seen_messages: i64,
    pub sender_domain: bool,
}

fn month_year(at: i64) -> String {
    use chrono::Datelike;
    const FR: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];
    const EN: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let Some(date) = chrono::DateTime::from_timestamp(at, 0) else { return String::new() };
    let month = date.month0() as usize;
    format!("{} {}", if crate::i18n::english() { EN[month] } else { FR[month] }, date.year())
}

/// Weighed only behind a mailing service's click tracker, where the announced domain is most
/// likely the real destination; otherwise the link goes elsewhere anyway and they only inform.
fn announced_reasons(a: &AnnouncedDomain, tracker: bool, now: i64) -> Vec<(i32, String)> {
    let d = &a.domain;
    let n = a.seen_messages;
    let seen = match (n, crate::i18n::english()) {
        (0, true) => "never seen in your messages".to_string(),
        (0, false) => "jamais vu dans vos messages".to_string(),
        (n, true) => format!("found in {n} other message{}", if n > 1 { "s" } else { "" }),
        (n, false) => format!("présent dans {n} autre{s} message{s}", s = if n > 1 { "s" } else { "" }),
    };
    let days = a.registered.map(|at| (now - at) / 86_400);
    let (delta, age) = match days {
        Some(days) if days < 30 => {
            let days = days.max(0);
            let (fr, en) = (if days > 1 { "s" } else { "" }, if days != 1 { "s" } else { "" });
            (-20, Some(tr!("domaine créé il y a {days} jour{fr}", "domain created {days} day{en} ago")))
        }
        Some(days) if days < 180 => {
            let months = days / 30;
            let en = if months != 1 { "s" } else { "" };
            (-10, Some(tr!("domaine récent, créé il y a {months} mois", "recent domain, created {months} month{en} ago")))
        }
        Some(_) => (if n >= 2 { 10 } else { 0 }, Some(tr!("domaine créé en {}", "domain created in {}", month_year(a.registered.unwrap_or_default())))),
        None => (if n >= 2 { 5 } else { 0 }, None),
    };
    let what = match age {
        Some(age) => format!("{age}, {seen}"),
        None => seen,
    };
    let weigh = |delta: i32| if tracker { delta } else { 0 };
    let mut out = vec![(weigh(delta), tr!("Domaine annoncé « {d} » : {what}", "Announced domain “{d}”: {what}"))];
    if a.sender_domain {
        out.push((weigh(10), tr!("Domaine annoncé « {d} » : c'est celui de l'expéditeur", "Announced domain “{d}”: the sender's own")));
    }
    out
}

pub fn evaluate(f: &LinkFacts, c: &LinkContext) -> Trust {
    let mut score: i32 = 50;
    let mut reasons = Vec::new();
    let mut note = |delta: i32, text: String| {
        score += delta;
        reasons.push(Reason { positive: delta > 0, text });
    };

    if let (true, Some(announced)) = (f.deceptive, &c.announced) {
        for (delta, text) in announced_reasons(announced, f.tracker.is_some(), c.now) {
            note(delta, text);
        }
    }
    if let (true, Some(service)) = (f.deceptive, f.tracker) {
        // Usual for newsletters and notifications, but mailing services carry phishing too.
        note(-15, tr!(
            "Lien de suivi des clics de {service} : il devrait mener à « {} », ce qui ne se vérifie qu'en cliquant",
            "{service} click-tracking link: it should lead to “{}”, which can only be checked by clicking",
            f.shown_host.as_deref().unwrap_or("")
        ));
    } else if f.deceptive {
        note(-50, tr!("Le texte affiche « {} » mais le lien mène vers « {} »", "The text shows “{}” but the link goes to “{}”", f.shown_host.as_deref().unwrap_or(""), f.host));
    }
    if f.ip {
        note(-40, tr!("Le lien vise une adresse IP au lieu d'un nom de domaine", "The link points to an IP address instead of a domain name"));
    }
    if f.punycode {
        note(-30, tr!("Domaine écrit avec des caractères spéciaux pouvant en imiter un autre", "Domain written with special characters that may imitate another one"));
    }
    if !f.secure {
        note(-10, tr!("Connexion non chiffrée (http)", "Unencrypted connection (http)"));
    }
    if f.shortener {
        note(-10, tr!("Lien raccourci : la destination finale est masquée", "Shortened link: the final destination is hidden"));
    }
    if let Some(d) = &f.redirect_domain {
        note(-5, tr!("Passe par un redirecteur vers {d}", "Goes through a redirector to {d}"));
    }
    match c.message_trust {
        Some(Level::Low) => note(-40, tr!("Le message lui-même est peu fiable", "The message itself is untrustworthy")),
        Some(Level::High) => note(10, tr!("Message d'un expéditeur vérifié", "Message from a verified sender")),
        _ => {}
    }
    if c.sender_domain {
        note(25, tr!("Même domaine que l'expéditeur", "Same domain as the sender"));
    } else if let Some(d) = &c.sender_domain_unproven {
        note(0, tr!(
            "Même domaine que l'expéditeur, mais rien ne prouve que le message vient bien de {d} : il ne compte pas",
            "Same domain as the sender, but nothing proves the message really comes from {d}: it does not count"
        ));
    }
    if c.wrote_to {
        note(20, tr!("Vous avez déjà écrit à ce domaine", "You have written to this domain before"));
    }
    if c.seen_senders >= 2 || c.seen_messages >= 3 {
        let n = c.seen_messages;
        note(15, if crate::i18n::english() {
            format!("Found in {n} other message{}", if n > 1 { "s" } else { "" })
        } else {
            format!("Présent dans {n} autre{} message{}", if n > 1 { "s" } else { "" }, if n > 1 { "s" } else { "" })
        });
    } else if c.seen_messages == 0 && !c.sender_domain {
        note(0, tr!("Domaine jamais vu dans vos messages", "Domain never seen in your messages"));
    }
    if let Some(at) = c.registered {
        let days = (c.now - at) / 86_400;
        if days < 30 {
            let days = days.max(0);
            let (fr, en) = (if days > 1 { "s" } else { "" }, if days != 1 { "s" } else { "" });
            note(-40, tr!("Domaine créé il y a {days} jour{fr}", "Domain created {days} day{en} ago"));
        } else if days < 180 {
            let months = days / 30;
            let en = if months != 1 { "s" } else { "" };
            note(-10, tr!("Domaine récent (créé il y a {months} mois)", "Recent domain (created {months} month{en} ago)"));
        } else if days > 2 * 365 {
            note(5, tr!("Domaine existant depuis {} ans", "Domain in use for {} years", days / 365));
        }
    }

    let score = score.clamp(0, 100) as u8;
    // A deceptive link or an IP stays suspect whatever the other signs say. Except a click
    // tracker announcing the proven sender's own domain: a newsletter's ordinary link.
    let own_tracker = f.tracker.is_some() && c.announced.as_ref().is_some_and(|a| a.sender_domain);
    let level = match score {
        _ if f.ip || (f.deceptive && !own_tracker) => Level::Low,
        70.. => Level::High,
        40..=69 => Level::Medium,
        _ => Level::Low,
    };
    reasons.sort_by_key(|r| r.positive);
    Trust { score, level, reasons }
}

/// How the sender's domain proves its mail, for a link whose trust leans on the sender: what is
/// missing there is also worth telling the sender.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SenderAuth {
    /// The From address's domain, accents restored.
    pub domain: String,
    /// A DMARC policy published (`_dmarc.` of the domain or of its registrable domain); `None`
    /// when the lookup failed.
    pub dmarc: Option<bool>,
    /// An SPF record published for the domain; `None` when the lookup failed.
    pub spf: Option<bool>,
    /// This message's DKIM signature.
    pub dkim: Dkim,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(tag = "state", content = "domain", rename_all = "lowercase")]
pub enum Dkim {
    /// Valid, by the sender's own domain.
    Own,
    /// Valid, by another domain (a mailing service's).
    Other(String),
    /// Valid, the signing domain not known.
    Signed,
    Invalid,
    /// The provider reported no signature.
    Missing,
    /// No verdict to go by.
    #[default]
    Unknown,
}

impl SenderAuth {
    pub fn incomplete(&self) -> bool {
        self.dmarc == Some(false) || self.spf == Some(false) || matches!(self.dkim, Dkim::Invalid | Dkim::Missing | Dkim::Other(_))
    }
}

/// The message's DKIM signature, as the provider and facteur's own check saw it.
pub fn dkim_state(signals: &crate::trust::Signals) -> Dkim {
    match (&signals.dkim_local, signals.dkim) {
        (Some(d), _) if d.pass && d.aligned => Dkim::Own,
        (Some(d), _) if d.pass => Dkim::Other(idna::domain_to_unicode(&d.domain).0),
        (Some(_), _) | (None, Some(false)) => Dkim::Invalid,
        (None, Some(true)) => Dkim::Signed,
        // The provider wrote its verdict and no DKIM in it: none was there.
        (None, None) if signals.authserv.is_some() && (signals.spf.is_some() || signals.dmarc.is_some()) => Dkim::Missing,
        (None, None) => Dkim::Unknown,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkReport {
    #[serde(flatten)]
    pub facts: LinkFacts,
    pub registered: Option<i64>,
    pub seen_messages: i64,
    pub announced: Option<AnnouncedDomain>,
    /// Only when something is missing.
    pub sender_auth: Option<SenderAuth>,
    pub trust: Trust,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_the_link_hides() {
        let f = analyze("https://secure.paypal.com.verify-account.ru/login?utm_source=mail&id=4", "www.paypal.com").unwrap();
        assert_eq!((f.host.as_str(), f.domain.as_str()), ("secure.paypal.com.verify-account.ru", "verify-account.ru"));
        assert!(f.deceptive);
        assert_eq!(f.tracking, ["utm_source"]);
        assert_eq!(f.clean_url.as_deref(), Some("https://secure.paypal.com.verify-account.ru/login?id=4"));

        let f = analyze("http://click.news.example.com/r?u=https%3A%2F%2Fshop.example.org%2Fpromo", "Voir l'offre").unwrap();
        assert!(!f.deceptive && !f.secure && f.shown_host.is_none());
        assert_eq!(f.redirect_to.as_deref(), Some("https://shop.example.org/promo"));
        assert_eq!(f.redirect_domain.as_deref(), Some("example.org"));

        let f = analyze("https://xn--pypal-4ve.com/", "Mon compte").unwrap();
        assert!(f.punycode);
        assert_eq!(f.domain, "pаypal.com");

        assert!(!analyze("https://www.impots.gouv.fr/accueil", "impots.gouv.fr").unwrap().deceptive);
        assert!(!analyze("https://mail.google.com/", "google.com").unwrap().deceptive);
        assert!(analyze("https://bit.ly/3xyz", "https://bit.ly/3xyz").unwrap().shortener);
        assert!(analyze("http://192.168.1.10/a", "ici").unwrap().ip);
        assert!(analyze("mailto:a@b.fr", "a@b.fr").is_none());
        assert_eq!(shown_host("Cliquez ici."), None);

        // The text need not be only the domain, and invisible or lookalike spellings do not hide it.
        for text in ["paypal.com!", "pay\u{200B}pal.com", "\u{FEFF}PayPal.COM", "ｐａｙｐａｌ．ｃｏｍ", "https://www.paypal.com/x", "paypal.com. Merci"] {
            assert_eq!(shown_host(text).as_deref(), Some("paypal.com"), "{text}");
            assert!(analyze("https://evil.example/", text).unwrap().deceptive, "{text}");
        }
        assert_eq!(shown_host("rapport.pdf"), None);
        assert_eq!(shown_host("Voir paypal.com"), None);

        // A mailing service's click tracker: still told apart, but not taken for a forgery.
        let f = analyze("https://u7642744.ct.sendgrid.net/ls/click?upn=abc", "https://app.wesend-sante.fr/redirect").unwrap();
        assert!(f.deceptive);
        assert_eq!(f.tracker, Some("SendGrid"));
        let trust = evaluate(&f, &LinkContext::default());
        assert!(trust.reasons.iter().any(|r| r.text.contains("SendGrid")) && trust.score > 20, "{trust:?}");
        assert_eq!(analyze("https://evil.example/", "paypal.com").unwrap().tracker, None);
        assert_eq!(shown_host("xn--pypal-4ve.com").as_deref(), Some("pаypal.com"));

        // Public Suffix List, private section: a free hosting customer is not the platform.
        let f = analyze("https://login.paypal-secure.pages.dev/", "Connexion").unwrap();
        assert_eq!(f.domain, "paypal-secure.pages.dev");
        let f = analyze("https://a.github.io/r?u=https%3A%2F%2Fb.github.io%2F", "x").unwrap();
        assert_eq!(f.redirect_domain.as_deref(), Some("b.github.io"));
    }

    #[test]
    fn tells_what_the_sender_domain_lacks() {
        use crate::trust::{DkimLocal, Signals};
        let provider = |dkim, spf, dmarc| Signals { dkim, spf, dmarc, authserv: Some("mx.example.net".into()), ..Default::default() };
        assert_eq!(dkim_state(&provider(None, Some(true), None)), Dkim::Missing);
        assert_eq!(dkim_state(&Signals::default()), Dkim::Unknown, "no verdict at all: nothing to say");
        assert_eq!(dkim_state(&provider(Some(false), Some(true), None)), Dkim::Invalid);
        let local = |pass, aligned| Signals { dkim_local: Some(DkimLocal { pass, domain: "sendgrid.net".into(), aligned }), ..Default::default() };
        assert_eq!(dkim_state(&local(true, true)), Dkim::Own);
        assert_eq!(dkim_state(&local(true, false)), Dkim::Other("sendgrid.net".into()));

        let auth = |dmarc, spf, dkim| SenderAuth { domain: "ecollectivites.fr".into(), dmarc, spf, dkim };
        assert!(auth(Some(false), Some(true), Dkim::Own).incomplete());
        assert!(auth(Some(true), Some(true), Dkim::Missing).incomplete());
        assert!(!auth(Some(true), Some(true), Dkim::Own).incomplete());
        assert!(!auth(None, None, Dkim::Unknown).incomplete(), "lookups failed: nothing claimed");

        // The link on the sender's domain, the sender not proven: said, and worth nothing.
        let f = analyze("https://pastell-mailsec.ecollectivites.fr/mail/1/captcha", "https://pastell-mailsec.ecollectivites.fr/mail/1/captcha").unwrap();
        let plain = evaluate(&f, &LinkContext::default());
        let unproven = evaluate(&f, &LinkContext { sender_domain_unproven: Some("ecollectivites.fr".into()), ..Default::default() });
        assert_eq!(plain.score, unproven.score);
        assert!(unproven.reasons.iter().any(|r| r.text.contains("ecollectivites.fr")), "{unproven:?}");
    }

    #[test]
    fn scores_links() {
        let now = 1_800_000_000;
        let known = analyze("https://www.banque.fr/espace", "Mon espace").unwrap();
        let ctx = LinkContext { sender_domain: true, seen_messages: 5, seen_senders: 1, now, ..Default::default() };
        assert_eq!(evaluate(&known, &ctx).level, Level::High);

        let young = LinkContext { registered: Some(now - 5 * 86_400), now, ..Default::default() };
        let t = evaluate(&known, &young);
        assert_eq!(t.level, Level::Low);
        assert!(t.reasons.iter().any(|r| !r.positive && r.text.contains("créé il y a 5 jours")));

        let trap = analyze("https://evil.example/", "www.banque.fr").unwrap();
        assert_eq!(evaluate(&trap, &ctx).level, Level::Low, "deceptive even with a good context");
    }

    #[test]
    fn weighs_the_announced_domain() {
        let now = 1_800_000_000;
        let tracked = analyze("https://u7642744.ct.sendgrid.net/ls/click?upn=abc", "https://app.wesend-sante.fr/redirect").unwrap();
        let known = AnnouncedDomain { domain: "wesend-sante.fr".into(), registered: Some(1_552_000_000), seen_messages: 12, sender_domain: true };
        let score = |f: &LinkFacts, a: Option<AnnouncedDomain>| evaluate(f, &LinkContext { announced: a, now, ..Default::default() });
        let t = score(&tracked, Some(known.clone()));
        let texts: Vec<(bool, &str)> = t.reasons.iter().map(|r| (r.positive, r.text.as_str())).collect();
        assert!(texts.contains(&(true, "Domaine annoncé « wesend-sante.fr » : domaine créé en mars 2019, présent dans 12 autres messages")), "{texts:?}");
        assert!(texts.contains(&(true, "Domaine annoncé « wesend-sante.fr » : c'est celui de l'expéditeur")), "{texts:?}");
        assert!(t.score > score(&tracked, None).score);
        // The proven sender's own domain behind a tracker: no forced Low; anything else keeps it.
        assert_ne!(t.level, Level::Low, "{t:?}");
        assert_eq!(score(&tracked, None).level, Level::Low);
        let other = AnnouncedDomain { sender_domain: false, ..known.clone() };
        assert_eq!(score(&tracked, Some(other)).level, Level::Low);
        let low_message = LinkContext { announced: Some(known.clone()), message_trust: Some(Level::Low), now, ..Default::default() };
        assert_eq!(evaluate(&tracked, &low_message).level, Level::Low);

        let young = AnnouncedDomain { registered: Some(now - 10 * 86_400), seen_messages: 0, sender_domain: false, ..known.clone() };
        let t = score(&tracked, Some(young));
        assert!(t.reasons.iter().any(|r| !r.positive && r.text == "Domaine annoncé « wesend-sante.fr » : domaine créé il y a 10 jours, jamais vu dans vos messages"));
        assert!(t.score < score(&tracked, None).score);
        let recent = AnnouncedDomain { registered: Some(now - 90 * 86_400), seen_messages: 1, sender_domain: false, ..known.clone() };
        assert!(score(&tracked, Some(recent)).reasons.iter().any(|r| r.text.ends_with("domaine récent, créé il y a 3 mois, présent dans 1 autre message")));
        let unknown = AnnouncedDomain { registered: None, seen_messages: 0, sender_domain: false, ..known.clone() };
        let t = score(&tracked, Some(unknown));
        assert_eq!(t.score, score(&tracked, None).score, "never seen: neutral");
        assert!(t.reasons.iter().any(|r| r.text == "Domaine annoncé « wesend-sante.fr » : jamais vu dans vos messages"));

        // A link to elsewhere: a well-known announced domain does not make it any better.
        let trap = analyze("https://evil.example/", "www.banque.fr").unwrap();
        let bank = AnnouncedDomain { domain: "banque.fr".into(), ..known };
        let t = score(&trap, Some(bank));
        assert_eq!(t.score, score(&trap, None).score);
        assert!(t.reasons.iter().any(|r| r.text.starts_with("Domaine annoncé « banque.fr »")));
        assert_eq!(t.level, Level::Low, "not a tracker: the sender's announced domain does not lift it");
    }
}
