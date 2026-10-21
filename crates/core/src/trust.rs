//! Message trust score, 0 to 100. Combines what the headers say (authentication checked by the
//! user's provider on receipt, spam markers, spoofing) with what facteur knows of the sender.
//! On low trust, features that contact the sender or confirm the user's address are cut off.

use serde::{Deserialize, Serialize};

use crate::tr;

/// `None`: the headers say nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    pub dkim: Option<bool>,
    pub spf: Option<bool>,
    pub dmarc: Option<bool>,
    pub spam_flag: bool,
    /// The display name holds another address than the real one (« service@banque.fr <x@arnaque.ru> »).
    pub spoofed_name: bool,
    /// Sender domain written with non-ASCII characters (UTF-8 or punycode `xn--`), which may
    /// imitate a known domain with other characters.
    pub lookalike_domain: bool,
    pub reply_to_elsewhere: bool,
    /// Server that gave the authentication verdict (authserv-id of the topmost
    /// `Authentication-Results`), in lowercase.
    #[serde(default)]
    pub authserv: Option<String>,
    /// DKIM signature checked by facteur itself, when the provider's verdict could not be used
    /// (see [`crate::dkim`]). Not read from the headers: kept from the stored signals.
    #[serde(default)]
    pub dkim_local: Option<DkimLocal>,
}

/// What facteur found checking a message's DKIM signatures itself: the best one (a valid
/// signature of the From domain, else of another domain, else an invalid one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DkimLocal {
    pub pass: bool,
    pub domain: String,
    /// Same registrable domain as the From address.
    pub aligned: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Context {
    pub known_correspondent: bool,
    /// Earlier messages from this address, junk aside.
    pub seen_before: i64,
    pub in_junk: bool,
    pub blocked: bool,
    /// The verdict comes from another server than the one that usually signs this account's
    /// mail: the sender may have written it.
    pub foreign_results: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize)]
pub struct Reason {
    pub positive: bool,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Trust {
    pub score: u8,
    pub level: Level,
    pub reasons: Vec<Reason>,
}

impl Trust {
    pub fn is_low(&self) -> bool {
        self.level == Level::Low
    }
}

pub fn evaluate(s: &Signals, c: &Context) -> Trust {
    let mut score: i32 = 50;
    let mut reasons = Vec::new();
    let mut note = |delta: i32, text: &str| {
        score += delta;
        reasons.push(Reason { positive: delta > 0, text: text.to_string() });
    };

    if c.blocked {
        note(-100, &tr!("Expéditeur bloqué", "Blocked sender"));
    }
    // Verdict of unknown origin: its passes do not count, its failures do (a fraudster would
    // not write one against himself).
    let mut s = s.clone();
    if let Some(server) = &c.foreign_results {
        let ignored = [&mut s.dkim, &mut s.spf, &mut s.dmarc].into_iter().filter(|v| **v == Some(true)).fold(0, |n, v| {
            *v = None;
            n + 1
        });
        if ignored > 0 {
            let server = if server == NO_AUTHSERV && !crate::i18n::english() { "(sans identifiant)" } else { server.as_str() };
            note(-10, &tr!("Authentification annoncée par un serveur inconnu ({server}) : ignorée", "Authentication reported by an unknown server ({server}): ignored"));
        }
    }
    let s = &s;
    match s.dmarc {
        Some(true) => note(25, &tr!("Domaine authentifié (DMARC)", "Authenticated domain (DMARC)")),
        Some(false) => note(-35, &tr!("Échec de l'authentification du domaine (DMARC) : expéditeur possiblement usurpé", "Domain authentication failed (DMARC): the sender may be spoofed")),
        None => {}
    }
    match s.dkim {
        Some(true) => note(10, &tr!("Signature numérique valide (DKIM)", "Valid digital signature (DKIM)")),
        Some(false) => note(-15, &tr!("Signature numérique invalide (DKIM)", "Invalid digital signature (DKIM)")),
        None => {}
    }
    // facteur's own check: only where the provider's DKIM verdict is missing or was set aside,
    // so a pass is never counted twice.
    if let (None, Some(local)) = (s.dkim, &s.dkim_local) {
        let domain = &local.domain;
        match (local.pass, local.aligned) {
            (true, true) => note(10, &tr!("Signature DKIM vérifiée par facteur pour {domain}", "DKIM signature verified by facteur for {domain}")),
            (true, false) => note(4, &tr!(
                "Signature DKIM vérifiée par facteur, mais pour un autre domaine que l'expéditeur ({domain})",
                "DKIM signature verified by facteur, but for a domain other than the sender's ({domain})"
            )),
            // Lists and forwarders that change a message break its signature (RFC 6376, 6.1: a
            // failed signature is like none): a hint, not enough alone to make the message Low.
            (false, _) => note(-10, &tr!(
                "Signature DKIM de {domain} invalide : message modifié en route ou usurpé",
                "Invalid DKIM signature from {domain}: the message was altered on the way or forged"
            )),
        }
    }
    match s.spf {
        Some(true) => note(5, &tr!("Serveur d'envoi autorisé (SPF)", "Authorized sending server (SPF)")),
        Some(false) => note(-15, &tr!("Serveur d'envoi non autorisé par le domaine (SPF)", "Sending server not authorized by the domain (SPF)")),
        None => {}
    }
    if s.dkim.is_none() && s.spf.is_none() && s.dmarc.is_none() {
        note(0, &tr!("Aucune authentification indiquée par le serveur", "No authentication reported by the server"));
    }
    if s.spam_flag {
        note(-40, &tr!("Marqué comme spam par le filtre du serveur", "Marked as spam by the server's filter"));
    }
    if s.spoofed_name {
        note(-30, &tr!("Le nom affiché contient une autre adresse que celle de l'expéditeur", "The display name contains an address other than the sender's"));
    }
    if s.lookalike_domain {
        note(-15, &tr!("Domaine écrit avec des caractères spéciaux pouvant en imiter un autre", "Domain written with special characters that may imitate another one"));
    }
    if s.reply_to_elsewhere {
        note(-10, &tr!("Les réponses partiraient vers un autre domaine que celui de l'expéditeur", "Replies would go to a different domain than the sender's"));
    }
    if c.known_correspondent {
        note(30, &tr!("Vous lui avez déjà écrit", "You have written to them before"));
    } else if c.seen_before >= 2 {
        note(10, &tr!("Expéditeur déjà connu", "Known sender"));
    }
    if c.in_junk {
        note(-40, &tr!("Rangé dans les indésirables", "Filed in junk"));
    }

    let score = score.clamp(0, 100) as u8;
    let level = match score {
        70.. => Level::High,
        40..=69 => Level::Medium,
        _ => Level::Low,
    };
    // Unfavourable reasons first: they are what explains a warning.
    reasons.sort_by_key(|r| r.positive);
    Trust { score, level, reasons }
}

/// Stored for a header without authserv-id; shown translated.
const NO_AUTHSERV: &str = "(no identifier)";

/// Some servers (Microsoft) put no authserv-id.
pub fn authserv_id(value: &str) -> String {
    let first = value.split(';').next().unwrap_or("").trim();
    if first.contains('=') || first.is_empty() {
        return NO_AUTHSERV.into();
    }
    first.split_whitespace().next().unwrap_or("").to_ascii_lowercase()
}

/// The `resinfo`s of an `Authentication-Results` value (RFC 8601), each as its tokens: split on
/// `;` and whitespace outside comments and quoted strings, comments left out. The provider's own
/// header echoes text the sender chose (the envelope sender, in a comment or a quoted
/// `smtp.mailfrom`), which must never read as a verdict. `None`: a comment or a quoted string
/// is left open, so where the sender's text ends cannot be told.
fn resinfos(value: &str) -> Option<Vec<Vec<String>>> {
    let mut out = vec![Vec::new()];
    let mut token = String::new();
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    let end = |token: &mut String, out: &mut Vec<Vec<String>>| {
        if !token.is_empty() {
            out.last_mut().unwrap().push(std::mem::take(token));
        }
    };
    for c in value.chars() {
        if escaped {
            if depth == 0 {
                token.push(c);
            }
            escaped = false;
        } else if c == '\\' && (quoted || depth > 0) {
            escaped = true;
        } else if quoted {
            token.push(c);
            quoted = c != '"';
        } else if c == '(' {
            if depth == 0 {
                end(&mut token, &mut out);
            }
            depth += 1;
        } else if depth > 0 {
            if c == ')' {
                depth -= 1;
            }
        } else if c == '"' {
            token.push(c);
            quoted = true;
        } else if c == ';' {
            end(&mut token, &mut out);
            out.push(Vec::new());
        } else if c.is_whitespace() {
            end(&mut token, &mut out);
        } else {
            token.push(c);
        }
    }
    end(&mut token, &mut out);
    (depth == 0 && !quoted && !escaped).then_some(out)
}

/// `method=result` of a resinfo (its first token), in lowercase, the method without its version.
fn method_result(tokens: &[String]) -> Option<(String, String)> {
    let (method, result) = tokens.first()?.split_once('=')?;
    let method = method.split('/').next().unwrap_or(method);
    Some((method.trim().to_ascii_lowercase(), result.trim().to_ascii_lowercase()))
}

/// Pass the topmost `Authentication-Results`: the one the user's own provider added. Those below
/// may come from the sender, so be forged.
pub fn parse_authentication_results(value: &str) -> (Option<bool>, Option<bool>, Option<bool>) {
    let mut dkim: Option<bool> = None;
    let mut spf = None;
    let mut dmarc = None;
    let parsed = resinfos(value);
    // Malformed: only the failures are kept (nobody plants one against themselves), read
    // anywhere, so an unclosed comment cannot hide the provider's `dmarc=fail`.
    let methods: Vec<(String, String)> = match &parsed {
        Some(resinfos) => resinfos.iter().filter_map(|tokens| method_result(tokens)).collect(),
        None => value
            .split(|c: char| c == ';' || c.is_whitespace())
            .filter_map(|token| token.split_once('='))
            .map(|(m, r)| (m.trim().to_ascii_lowercase(), r.trim().to_ascii_lowercase()))
            .filter(|(_, r)| r != "pass")
            .collect(),
    };
    for (method, result) in methods {
        let verdict = match result.as_str() {
            "pass" => Some(true),
            "fail" | "softfail" | "permerror" => Some(false),
            _ => None,
        };
        match method.as_str() {
            // Several DKIM signatures: one valid is enough.
            "dkim" => dkim = match (dkim, verdict) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (None, v) => v,
                (current, None) => current,
                (Some(false), Some(false)) => Some(false),
            },
            "spf" if spf.is_none() => spf = verdict,
            "dmarc" if dmarc.is_none() => dmarc = verdict,
            _ => {}
        }
    }
    (dkim, spf, dmarc)
}

/// A DKIM signature the receiving server verified, as its `Authentication-Results` names it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DkimPass {
    pub domain: Option<String>,
    /// Domain of `header.i` (`@example.com`).
    pub identity: Option<String>,
    pub selector: Option<String>,
    /// `header.b`: first characters of the signature (RFC 6008).
    pub b: Option<String>,
}

pub fn dkim_passes(value: &str) -> Vec<DkimPass> {
    // Comments ("(1024-bit key)") and quoted strings may hold anything, `;` and `=` included.
    let Some(resinfos) = resinfos(value) else { return Vec::new() };
    resinfos
        .iter()
        .filter_map(|tokens| {
            if method_result(tokens)? != ("dkim".into(), "pass".into()) {
                return None;
            }
            let mut pass = DkimPass::default();
            for token in &tokens[1..] {
                let Some((name, value)) = token.split_once('=') else { continue };
                let value = value.trim_matches('"').to_string();
                match name.to_ascii_lowercase().as_str() {
                    "header.d" => pass.domain = Some(value.to_ascii_lowercase()),
                    "header.i" => pass.identity = value.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()),
                    "header.s" => pass.selector = Some(value),
                    "header.b" => pass.b = Some(value),
                    _ => {}
                }
            }
            Some(pass)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_from_an_unknown_server_do_not_count() {
        assert_eq!(authserv_id("mx.google.com; dkim=pass"), "mx.google.com");
        assert_eq!(authserv_id("MX.Fournisseur.fr 1; spf=pass"), "mx.fournisseur.fr");
        assert_eq!(authserv_id("spf=pass (sender IP is 1.2.3.4) smtp.mailfrom=x.fr; dkim=pass"), "(no identifier)");

        let forged = Signals { dkim: Some(true), spf: Some(true), dmarc: Some(true), authserv: Some("faux".into()), ..Default::default() };
        let trusted = evaluate(&forged, &Context::default());
        assert_eq!(trusted.level, Level::High);
        let checked = evaluate(&forged, &Context { foreign_results: Some("faux".into()), ..Default::default() });
        assert!(checked.level < Level::High);
        assert!(checked.reasons.iter().any(|r| r.text.contains("serveur inconnu (faux)")));
        // An announced failure always counts: nobody denounces himself.
        let failed = Signals { dmarc: Some(false), ..forged };
        assert_eq!(evaluate(&failed, &Context { foreign_results: Some("faux".into()), ..Default::default() }).level, Level::Low);
    }

    #[test]
    fn reads_the_providers_verdict() {
        let gmail = "mx.google.com;\r\n dkim=pass header.i=@airbnb.com header.s=s1;\r\n \
                     spf=pass (google.com: domain of x@airbnb.com) smtp.mailfrom=x@airbnb.com;\r\n \
                     dmarc=pass (p=REJECT sp=REJECT dis=NONE) header.from=airbnb.com";
        assert_eq!(parse_authentication_results(gmail), (Some(true), Some(true), Some(true)));
        let forged = "mx.example.net; dkim=fail; dkim=pass; spf=softfail; dmarc=fail";
        assert_eq!(parse_authentication_results(forged), (Some(true), Some(false), Some(false)));
        assert_eq!(parse_authentication_results("mx.example.net; none"), (None, None, None));

        let passes = dkim_passes(gmail);
        assert_eq!(passes, [DkimPass { identity: Some("airbnb.com".into()), selector: Some("s1".into()), ..Default::default() }]);
        let outlook = "spf=pass (sender IP is 1.2.3.4; x=y) smtp.mailfrom=n.example; dkim=pass (signature was verified)\r\n header.d=N.example;dkim=fail header.d=other.example";
        assert_eq!(dkim_passes(outlook), [DkimPass { domain: Some("n.example".into()), ..Default::default() }]);
        assert!(dkim_passes("mx.example.net; dkim=fail header.d=x.example").is_empty());
    }

    #[test]
    fn verdicts_planted_by_the_sender_are_not_read() {
        // The envelope sender, chosen by the sender, echoed in a comment and a quoted value.
        let planted = "mx.google.com; dkim=fail header.i=@bank.example; spf=softfail (google.com: domain of \
                       transitioning \"a dkim=pass dmarc=pass b\"@evil.example does not designate 1.2.3.4) \
                       smtp.mailfrom=\"a dkim=pass dmarc=pass b\"@evil.example; dmarc=fail (p=REJECT) header.from=bank.example";
        assert_eq!(parse_authentication_results(planted), (Some(false), Some(false), Some(false)));
        let semicolon = "mx.example.net; spf=softfail smtp.mailfrom=\"x; dkim=pass header.d=bank.example\"@evil.example; dkim=fail header.d=bank.example";
        assert!(dkim_passes(semicolon).is_empty());
        assert_eq!(parse_authentication_results(semicolon), (Some(false), Some(false), None));
        let nested = "mx.example.net; spf=pass (a (b; dkim=pass) \\) dmarc=pass) smtp.mailfrom=x.example; dmarc=fail";
        assert_eq!(parse_authentication_results(nested), (None, Some(true), Some(false)));
        // A comment left open: nothing passes, the failures still count.
        let open = "mx.example.net; spf=softfail (domain of a(b@evil.example; dkim=pass; dmarc=fail";
        assert_eq!(parse_authentication_results(open), (None, Some(false), Some(false)));
        assert!(dkim_passes(open).is_empty());
        let quoted = "mx.example.net; dkim=pass header.d=\"n.example\" header.s=\"s1\"";
        assert_eq!(dkim_passes(quoted), [DkimPass { domain: Some("n.example".into()), selector: Some("s1".into()), ..Default::default() }]);
    }

    #[test]
    fn facteurs_own_dkim_check() {
        let local = |pass, aligned| Some(DkimLocal { pass, domain: "wesend-sante.fr".into(), aligned });
        let reasons = |s: &Signals, c: &Context| evaluate(s, c).reasons.into_iter().map(|r| (r.positive, r.text)).collect::<Vec<_>>();
        let unknown = Context { foreign_results: Some("mx.forged.example".into()), ..Default::default() };

        let verified = Signals { dkim: Some(true), authserv: Some("mx.forged.example".into()), dkim_local: local(true, true), ..Default::default() };
        let r = reasons(&verified, &unknown);
        assert!(r.contains(&(true, "Signature DKIM vérifiée par facteur pour wesend-sante.fr".into())), "{r:?}");
        assert!(evaluate(&verified, &unknown).score > evaluate(&Signals { dkim_local: None, ..verified.clone() }, &unknown).score);
        // The provider's own pass already counts: not twice.
        let trusted = evaluate(&verified, &Context::default());
        assert!(!trusted.reasons.iter().any(|r| r.text.contains("par facteur")));
        assert_eq!(trusted.score, evaluate(&Signals { dkim_local: None, ..verified.clone() }, &Context::default()).score);

        let other = Signals { dkim_local: Some(DkimLocal { pass: true, domain: "sendgrid.net".into(), aligned: false }), ..Default::default() };
        let t = evaluate(&other, &Context::default());
        assert!(t.score > 50 && t.score < evaluate(&Signals { dkim_local: local(true, true), ..Default::default() }, &Context::default()).score);
        assert!(t.reasons.iter().any(|r| r.positive && r.text.contains("sendgrid.net")));

        let forged = evaluate(&Signals { dkim_local: local(false, true), ..Default::default() }, &Context::default());
        assert!(forged.score < 50);
        assert!(forged.reasons.iter().any(|r| !r.positive && r.text.contains("message modifié en route ou usurpé")));
        assert_eq!(forged.level, Level::Medium, "a broken signature alone is not Low");

        // Signals stored before this field existed still read.
        let old: Signals = serde_json::from_str(r#"{"dkim":true,"spf":null,"dmarc":null,"spam_flag":false,"spoofed_name":false,"lookalike_domain":false,"reply_to_elsewhere":false}"#).unwrap();
        assert_eq!(old.dkim_local, None);
    }

    #[test]
    fn levels() {
        let authenticated = Signals { dkim: Some(true), spf: Some(true), dmarc: Some(true), ..Default::default() };
        assert_eq!(evaluate(&authenticated, &Context::default()).level, Level::High);

        let phishing = Signals { dmarc: Some(false), spoofed_name: true, ..Default::default() };
        let t = evaluate(&phishing, &Context::default());
        assert_eq!(t.level, Level::Low);
        assert!(!t.reasons[0].positive, "unfavourable reasons first");

        // No authentication (small server): medium, and a known correspondent moves it up.
        assert_eq!(evaluate(&Signals::default(), &Context::default()).level, Level::Medium);
        let friend = Context { known_correspondent: true, ..Default::default() };
        assert_eq!(evaluate(&Signals::default(), &friend).level, Level::High);

        let junk = Context { in_junk: true, ..Default::default() };
        assert_eq!(evaluate(&authenticated, &junk).level, Level::Medium);
        let blocked = Context { blocked: true, ..Default::default() };
        assert_eq!(evaluate(&authenticated, &blocked).level, Level::Low);
    }
}
