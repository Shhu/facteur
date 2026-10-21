use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::Message;

use crate::error::{Error, Result};
use crate::imap::Credential;
use crate::model::{Account, Draft, Security};

fn smtp_err(e: impl std::fmt::Display) -> Error {
    Error::Smtp(e.to_string())
}

/// The parser stops at the first address: make sure no other one hides after it
/// (« Patron <pirate@x.fr> <patron@y.fr> »).
fn mailbox(s: &str) -> Result<Mailbox> {
    let s = s.trim();
    let m: Mailbox = s.parse().map_err(|e| Error::Smtp(crate::tr!("adresse invalide « {s} » : {e}", "invalid address “{s}”: {e}")))?;
    let email = m.email.to_string();
    let exact = s.eq_ignore_ascii_case(&email) || s.to_ascii_lowercase().ends_with(&format!("<{}>", email.to_ascii_lowercase()));
    if !exact {
        return Err(Error::Smtp(crate::tr!("adresse ambiguë « {s} » : mettez le nom entre guillemets", "ambiguous address “{s}”: put the name in quotes")));
    }
    Ok(m)
}

/// Also returns the raw bytes, for the Sent folder. `as_draft`: keeps Bcc recipients. A draft is
/// built even when an address is invalid or missing (kept as typed); sending validates them all.
pub fn build(
    account: &Account,
    draft: &Draft,
    in_reply_to: Option<&str>,
    references: &[String],
    as_draft: bool,
) -> Result<Message> {
    build_inner(account, draft, in_reply_to, references, as_draft, as_draft)
}

/// The message for the SMTP server (without its Bcc header) and the raw copy for the Sent
/// folder, which keeps it. Both come from one build: same Message-ID, date and body.
pub fn build_outgoing(account: &Account, draft: &Draft, in_reply_to: Option<&str>, references: &[String]) -> Result<(Message, Vec<u8>)> {
    let mut message = build_inner(account, draft, in_reply_to, references, true, false)?;
    let copy = message.formatted();
    // The envelope was computed from the headers, Bcc included: they still get the message.
    message.headers_mut().remove::<lettre::message::header::Bcc>();
    Ok((message, copy))
}

fn build_inner(
    account: &Account,
    draft: &Draft,
    in_reply_to: Option<&str>,
    references: &[String],
    keep_bcc: bool,
    lenient: bool,
) -> Result<Message> {
    let (name, email) = match draft.from.as_deref().filter(|f| !f.eq_ignore_ascii_case(&account.email)) {
        Some(alias) => match account.identities.iter().find(|i| i.email.eq_ignore_ascii_case(alias)) {
            Some(identity) => (if identity.name.is_empty() { account.name.clone() } else { identity.name.clone() }, identity.email.clone()),
            // A draft written from an alias removed since then is still saved, from the account.
            None if lenient => (account.name.clone(), account.email.clone()),
            None => return Err(Error::Smtp(crate::tr!("{alias} n'est pas une adresse d'envoi de ce compte", "{alias} is not a sending address of this account"))),
        },
        None => (account.name.clone(), account.email.clone()),
    };
    let from = Mailbox::new(Some(name).filter(|n| !n.is_empty()), email.parse().map_err(smtp_err)?);
    let mut b = Message::builder().from(from.clone()).subject(&draft.subject).date_now();
    if let Some(id) = &draft.draft_id {
        b = b.message_id(Some(format!("<{id}>")));
    }
    if keep_bcc {
        b = b.keep_bcc();
    }
    // Draft fields holding an address that does not parse: written as typed once built.
    let mut raw_fields: Vec<(&'static str, String)> = Vec::new();
    let mut recipients = 0;
    for (field, list) in [("To", &draft.to), ("Cc", &draft.cc), ("Bcc", &draft.bcc)] {
        let entries: Vec<&str> = list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let parsed: Vec<Result<Mailbox>> = entries.iter().map(|s| mailbox(s)).collect();
        if lenient && parsed.iter().any(Result::is_err) {
            raw_fields.push((field, entries.join(", ")));
            continue;
        }
        for m in parsed {
            let m = m?;
            recipients += 1;
            b = match field {
                "To" => b.to(m),
                "Cc" => b.cc(m),
                _ => b.bcc(m),
            };
        }
    }
    // A draft may have no valid recipient yet; its envelope is never used (it is not sent).
    if lenient && recipients == 0 {
        b = b.envelope(lettre::address::Envelope::new(Some(from.email.clone()), vec![from.email.clone()]).map_err(smtp_err)?);
    }
    if let Some(irt) = in_reply_to {
        b = b.in_reply_to(format!("<{irt}>"));
        let refs: Vec<String> = thread_references(references, irt).iter().map(|r| format!("<{r}>")).collect();
        b = b.references(refs.join(" "));
    }

    let body = match draft.html.as_deref().filter(|h| !h.trim().is_empty()) {
        None => Body::Single(SinglePart::plain(draft.body.clone())),
        Some(html) => {
            // Always derived from the HTML: the plain `body` sent along by the editor can lag
            // behind it (text deleted or changed since).
            let text = crate::mime::html_to_text(html);
            let (html, images) = inline_images(html)?;
            let document = format!(
                "<!doctype html><html><head><meta charset=\"utf-8\"></head>\
                 <body style=\"font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; font-size: 14px\">{html}</body></html>"
            );
            // No image: alternative(text, html). With: alternative(text, related(html, images)).
            let alternative = MultiPart::alternative().singlepart(SinglePart::plain(text));
            Body::Multi(if images.is_empty() {
                alternative.singlepart(SinglePart::html(document))
            } else {
                let mut related = MultiPart::related().singlepart(SinglePart::html(document));
                for (cid, mime, data) in images {
                    let ct = ContentType::parse(&mime).unwrap_or(ContentType::parse("application/octet-stream").unwrap());
                    related = related.singlepart(Attachment::new_inline(cid).body(data, ct));
                }
                alternative.multipart(related)
            })
        }
    };

    // An invitation, its answer or its cancellation: the calendar part goes along with the text,
    // its iTIP method named as the object says.
    let body = match &draft.calendar {
        Some(ics) => {
            let method = ics
                .lines()
                .find_map(|l| l.trim().strip_prefix("METHOD:"))
                .map(str::trim)
                .filter(|m| matches!(*m, "REQUEST" | "REPLY" | "CANCEL"))
                .unwrap_or("REPLY");
            let part = SinglePart::builder()
                .header(ContentType::parse(&format!("text/calendar; charset=utf-8; method={method}")).map_err(smtp_err)?)
                .body(ics.clone());
            Body::Multi(match body {
                Body::Single(p) => MultiPart::alternative().singlepart(p).singlepart(part),
                Body::Multi(m) => m.singlepart(part),
            })
        }
        None => body,
    };

    let msg = if draft.attachments.is_empty() {
        match body {
            Body::Single(p) => b.singlepart(p),
            Body::Multi(m) => b.multipart(m),
        }
    } else {
        let mut mp = match body {
            Body::Single(p) => MultiPart::mixed().singlepart(p),
            Body::Multi(m) => MultiPart::mixed().multipart(m),
        };
        for a in &draft.attachments {
            let data = B64.decode(&a.data).map_err(smtp_err)?;
            let ct = ContentType::parse(&a.mime).unwrap_or(ContentType::parse("application/octet-stream").unwrap());
            mp = mp.singlepart(Attachment::new(a.filename.clone()).body(data, ct));
        }
        b.multipart(mp)
    };
    let mut msg = msg.map_err(smtp_err)?;
    for (field, value) in raw_fields {
        use lettre::message::header::{HeaderName, HeaderValue};
        msg.headers_mut().insert_raw(HeaderValue::new(HeaderName::new_from_ascii_str(field), value));
    }
    Ok(msg)
}

enum Body {
    Single(SinglePart),
    Multi(MultiPart),
}

/// At most this many Message-IDs in References besides the thread's root (RFC 5322 §3.6.4
/// allows trimming the middle; a thread of hundreds of replies would grow the header forever).
const MAX_REFERENCES: usize = 20;

/// References of a reply: the parent's, then the parent itself, each once, the root kept and
/// only the last [`MAX_REFERENCES`] of the rest.
fn thread_references(parent_refs: &[String], parent: &str) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for r in parent_refs.iter().map(String::as_str).chain([parent]) {
        let r = r.trim().trim_matches(|c| c == '<' || c == '>');
        if !r.is_empty() && !refs.iter().any(|x| x == r) {
            refs.push(r.to_string());
        }
    }
    if refs.len() > MAX_REFERENCES + 1 {
        refs.drain(1..refs.len() - MAX_REFERENCES);
    }
    refs
}

/// Above this size, once encoded, a message is refused before it is queued: most providers stop
/// at 20 to 35 MB (Gmail 25, Outlook 20 to 35), and the server would bounce it.
pub const MAX_MESSAGE_SIZE: usize = 25 * 1024 * 1024;

/// Rough: base64 attachments and images get their line breaks, text parts count as written.
pub fn estimated_size(draft: &Draft) -> usize {
    let attachments: usize = draft.attachments.iter().map(|a| a.data.len()).sum();
    let html = draft.html.as_deref().map_or(0, str::len);
    // The text part derived from the HTML weighs less than the HTML; the plain body is counted
    // when there is no HTML.
    let text = if html > 0 { html / 4 } else { draft.body.len() };
    (attachments + html) / 76 * 78 + text + draft.calendar.as_deref().map_or(0, str::len)
}

pub fn check_size(size: usize) -> Result<()> {
    if size > MAX_MESSAGE_SIZE {
        let mb = size.div_ceil(1024 * 1024);
        let max = MAX_MESSAGE_SIZE / (1024 * 1024);
        return Err(Error::Other(crate::tr!(
            "message trop lourd ({mb} Mo, {max} Mo au plus) : retirez des pièces jointes ou partagez-les par un lien",
            "message too large ({mb} MB, {max} MB at most): remove attachments or share them as a link"
        )));
    }
    Ok(())
}

type InlineImage = (String, String, Vec<u8>);

fn inline_images(html: &str) -> Result<(String, Vec<InlineImage>)> {
    let mut out = String::with_capacity(html.len());
    let mut images = Vec::new();
    let mut rest = html;
    while let Some(pos) = rest.find("data:image/") {
        let (before, from) = rest.split_at(pos);
        // Only the value of a `src="…"` attribute: the same text in the body (a quote, say) stays
        // text.
        let quote = before.chars().last().filter(|c| *c == '"' || *c == '\'');
        let in_src = quote.is_some() && before[..before.len() - 1].trim_end().to_ascii_lowercase().ends_with("src=");
        let end = match quote {
            Some(q) => from.find(q),
            None => from.find(|c: char| c.is_whitespace() || c == '>' || c == '<'),
        }
        .unwrap_or(from.len());
        let url = &from[..end];
        out.push_str(before);
        // Unreadable content: left as is rather than blocking the send.
        let decoded = url
            .split_once(";base64,")
            .filter(|_| in_src)
            .and_then(|(mime, data)| B64.decode(data.trim()).ok().map(|bytes| (mime, bytes)));
        match decoded {
            Some((mime, bytes)) => {
                let cid = format!("img{}@facteur", images.len() + 1);
                out.push_str(&format!("cid:{cid}"));
                images.push((cid, mime.trim_start_matches("data:").to_string(), bytes));
            }
            None => out.push_str(url),
        }
        rest = &from[end..];
    }
    out.push_str(rest);
    Ok((out, images))
}

/// Seconds allowed to connect, say hello and sign in (`test_connection`).
const CONNECT_SECS: u64 = 60;

/// Seconds allowed to hand the message over: two minutes, plus one second per 50 KB (a large
/// attachment on a slow uplink). lettre only bounds the TCP connection itself; a server that
/// stops answering mid-transfer would otherwise hold the send forever.
fn send_secs(size: usize) -> u64 {
    120 + size as u64 / 50_000
}

pub async fn send(account: &Account, credential: &Credential, message: &Message) -> Result<()> {
    send_with(account, credential, message, CONNECT_SECS, send_secs(message.formatted().len())).await
}

async fn send_with(account: &Account, credential: &Credential, message: &Message, connect_secs: u64, send_secs: u64) -> Result<()> {
    use lettre::transport::smtp::client::{AsyncSmtpConnection, TlsParameters};
    use lettre::transport::smtp::commands::{Data, Ehlo, Mail, Rcpt};
    use lettre::transport::smtp::extension::{ClientId, Extension, MailBodyParameter, MailParameter};
    use std::time::Duration;
    let s = &account.smtp;
    let stopped = || crate::tr!("{} ne répond plus pendant la connexion", "{} stopped responding while connecting", s.host);
    // One connection for the whole exchange (lettre's transport without its pool would open a
    // second one after a connection test, and a failure in between looked like an uncertain send).
    // Everything up to the end of the DATA command is certain: the server cannot have accepted a
    // message it has not received, so a failure there is retried.
    let connect = async {
        let tls = |host: &str| TlsParameters::new(host.to_string()).map_err(smtp_err);
        let hello = ClientId::default();
        let timeout = Some(Duration::from_secs(30));
        let mut conn = match s.security {
            Security::Tls => AsyncSmtpConnection::connect_tokio1((s.host.as_str(), s.port), timeout, &hello, Some(tls(&s.host)?), None).await,
            _ => AsyncSmtpConnection::connect_tokio1((s.host.as_str(), s.port), timeout, &hello, None, None).await,
        }
        .map_err(|e| connection_error(&e))?;
        if matches!(s.security, Security::StartTls) {
            conn.starttls(tls(&s.host)?, &hello).await.map_err(|e| connection_error(&e))?;
        }
        // The largest message the server takes (SIZE, RFC 1870): lettre keeps no note of it, a
        // second EHLO gives it. Before signing in, where a second EHLO is common.
        let limit = conn.command(Ehlo::new(hello.clone())).await.ok().and_then(|r| size_limit(r.message()));
        let auth = match credential {
            Credential::Password(password) if !password.is_empty() => {
                Some((lettre::transport::smtp::authentication::DEFAULT_MECHANISMS.to_vec(), Credentials::new(account.username.clone(), password.clone())))
            }
            Credential::Token(token) => Some((vec![Mechanism::Xoauth2], Credentials::new(account.username.clone(), token.clone()))),
            Credential::Password(_) => None,
        };
        if let Some((mechanisms, credentials)) = auth {
            conn.auth(&mechanisms, &credentials).await.map_err(|e| connection_error(&e))?;
        }
        Ok::<_, Error>((conn, limit))
    };
    let (mut conn, limit) = match tokio::time::timeout(Duration::from_secs(connect_secs), connect).await {
        Err(_) => return Err(Error::Smtp(stopped())),
        Ok(res) => res?,
    };
    let envelope = message.envelope();
    let raw = message.formatted();
    let prepare = async {
        let mut options = Vec::new();
        if let Some(max) = limit {
            if raw.len() > max {
                let (mb, max) = (raw.len().div_ceil(1024 * 1024), max / (1024 * 1024));
                return Err(Error::Rejected(crate::tr!(
                    "message trop lourd pour ce serveur ({mb} Mo, {max} Mo au plus) : retirez des pièces jointes ou partagez-les par un lien",
                    "message too large for this server ({mb} MB, {max} MB at most): remove attachments or share them as a link"
                )));
            }
            options.push(MailParameter::Size(raw.len()));
        }
        let non_ascii = envelope.from().into_iter().chain(envelope.to()).any(|a: &lettre::Address| !a.to_string().is_ascii());
        if non_ascii {
            if !conn.server_info().supports_feature(Extension::SmtpUtfEight) {
                return Err(Error::Rejected(crate::tr!(
                    "le serveur n'accepte pas les adresses non ASCII (SMTPUTF8)",
                    "the server does not accept non-ASCII addresses (SMTPUTF8)"
                )));
            }
            options.push(MailParameter::SmtpUtfEight);
        }
        if !raw.is_ascii() {
            if !conn.server_info().supports_feature(Extension::EightBitMime) {
                return Err(Error::Rejected(crate::tr!(
                    "le serveur n'accepte pas les messages 8 bits (8BITMIME)",
                    "the server does not accept 8-bit messages (8BITMIME)"
                )));
            }
            options.push(MailParameter::Body(MailBodyParameter::EightBitMime));
        }
        conn.command(Mail::new(envelope.from().cloned(), options)).await.map_err(|e| connection_error(&e))?;
        for to in envelope.to() {
            conn.command(Rcpt::new(to.clone(), vec![])).await.map_err(|e| connection_error(&e))?;
        }
        conn.command(Data).await.map_err(|e| connection_error(&e))?;
        Ok(())
    };
    let prepared = match tokio::time::timeout(Duration::from_secs(connect_secs), prepare).await {
        Err(_) => Err(Error::Smtp(stopped())),
        Ok(res) => res,
    };
    if let Err(e) = prepared {
        conn.abort().await;
        return Err(e);
    }
    let res = match tokio::time::timeout(Duration::from_secs(send_secs), conn.message(&raw)).await {
        Ok(Ok(_)) => Ok(()),
        // Timeout mid-send: the server may have accepted the message. Resending it on its own
        // could duplicate it for the recipient.
        Err(_) => Err(Error::Uncertain(crate::tr!("le serveur ne répond plus", "the server stopped responding"))),
        Ok(Err(e)) => Err(classify(&e, e.to_string())),
    };
    match &res {
        Ok(()) => {
            let _ = tokio::time::timeout(Duration::from_secs(5), conn.quit()).await;
        }
        Err(_) => conn.abort().await,
    }
    res
}

/// The SIZE an EHLO answer announces; none, or 0 (no limit): `None`.
fn size_limit<'a>(lines: impl Iterator<Item = &'a str>) -> Option<usize> {
    lines
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            words.next().filter(|w| w.eq_ignore_ascii_case("SIZE"))?;
            words.next()?.parse::<usize>().ok()
        })
        .find(|n| *n > 0)
}

/// A failure before the message data was handed over: certainly not sent. Refused credentials
/// (535), an unknown recipient (550) and the like are final: retrying on its own would fail the
/// same way, or get the address banned.
fn connection_error(e: &lettre::transport::smtp::Error) -> Error {
    if e.is_permanent() || e.is_client() { Error::Rejected(e.to_string()) } else { smtp_err(e) }
}

fn classify(e: &lettre::transport::smtp::Error, text: String) -> Error {
    if e.is_transient() {
        // 4xx (greylisting, mailbox busy, rate limit): not accepted, worth another try.
        Error::Smtp(text)
    } else if e.is_permanent() || e.is_client() {
        // 5xx: the same message would be refused again.
        Error::Rejected(text)
    } else {
        // Connection cut, unreadable reply, timeout: the message may have gone.
        Error::Uncertain(text)
    }
}

/// These providers copy sent messages to Sent themselves: an APPEND would duplicate them.
pub fn server_saves_sent(account: &Account) -> bool {
    host_saves_sent(&account.smtp.host)
}

fn host_saves_sent(smtp_host: &str) -> bool {
    let h = smtp_host.to_lowercase();
    ["gmail.com", "googlemail.com", "office365.com", "outlook.com"].iter().any(|d| h.ends_with(d))
}

/// What to check before sending again a message that may have gone. Sent only says so where the
/// provider files what it accepted; elsewhere facteur files the copy itself (whatever the
/// server did with the message), or none when the send was cut short by facteur closing.
pub fn maybe_sent_advice(smtp_host: &str) -> String {
    if host_saves_sent(smtp_host) {
        crate::tr!("vérifiez dans Envoyés avant de réessayer", "check Sent before trying again")
    } else {
        crate::tr!(
            "il est peut-être parti, vérifiez auprès d'un destinataire avant de réessayer",
            "it may have gone, check with a recipient before trying again"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthMethod, Security, ServerConfig};

    #[test]
    fn reads_the_size_the_server_takes() {
        let lines = |l: &[&'static str]| l.to_vec().into_iter();
        assert_eq!(size_limit(lines(&["smtp.example.com", "8BITMIME", "SIZE 35882577", "AUTH PLAIN"])), Some(35_882_577));
        assert_eq!(size_limit(lines(&["mx", "size 1000"])), Some(1000));
        assert_eq!(size_limit(lines(&["mx", "SIZE 0"])), None);
        assert_eq!(size_limit(lines(&["mx", "SIZE"])), None);
        assert_eq!(size_limit(lines(&["mx", "PIPELINING"])), None);
    }

    #[test]
    fn sent_is_named_as_proof_only_where_the_provider_files_it() {
        assert!(maybe_sent_advice("smtp.gmail.com").contains("Envoyés"));
        assert!(maybe_sent_advice("smtp.office365.com").contains("Envoyés"));
        let elsewhere = maybe_sent_advice("ssl0.ovh.net");
        assert!(!elsewhere.contains("Envoyés") && elsewhere.contains("destinataire"));
    }

    #[test]
    fn hidden_address_in_display_name_is_refused() {
        assert!(mailbox("Patron <pirate@x.fr> <patron@y.fr>").is_err());
        let quoted = crate::store::format_address(&crate::model::Address { name: Some("Patron <pirate@x.fr>".into()), email: "patron@y.fr".into() });
        assert_eq!(mailbox(&quoted).unwrap().email.to_string(), "patron@y.fr");
        assert_eq!(mailbox("Marie Dupont <marie@y.fr>").unwrap().email.to_string(), "marie@y.fr");
        assert_eq!(mailbox("marie@y.fr").unwrap().email.to_string(), "marie@y.fr");
        let comma = crate::store::format_address(&crate::model::Address { name: Some("Dupont, Marie".into()), email: "marie@y.fr".into() });
        assert_eq!(mailbox(&comma).unwrap().name.as_deref(), Some("Dupont, Marie"));
    }

    #[test]
    fn data_url_in_text_does_not_block_sending() {
        let (html, images) = inline_images(r#"<p>data:image/png;base64,!!</p><img src="data:image/png;base64,AAAA">"#).unwrap();
        assert_eq!(images.len(), 1);
        assert!(html.contains("<p>data:image/png;base64,!!</p>") && html.contains("cid:img1@facteur"));
    }

    fn account() -> Account {
        let srv = ServerConfig { host: "localhost".into(), port: 25, security: Security::None };
        Account {
            id: 1, name: "Yann".into(), email: "y@example.org".into(), username: "y".into(), imap: srv.clone(), smtp: srv,
            auth: AuthMethod::Password, last_sync: None, signature: String::new(), avatar: None, label: None, identities: Vec::new(), color: None, avatar_custom: false, screener_since: None, quota: None,
        }
    }

    #[test]
    fn rich_draft_roundtrip_with_inline_image() {
        let pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let draft = Draft {
            account_id: 1,
            to: vec!["bob@example.org".into()],
            subject: "Riche".into(),
            html: Some(format!("<p>Bonjour <strong>Bob</strong></p><p><img src=\"data:image/png;base64,{pixel}\"></p>")),
            ..Default::default()
        };
        let raw = build(&account(), &draft, None, &[], true).unwrap().formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("multipart/alternative") && text.contains("multipart/related"));
        assert!(text.contains("cid:img1@facteur") && !text.contains("data:image"), "inline image, not a data URL");

        let parts = crate::mime::parse_draft(&raw).unwrap();
        assert_eq!(parts.body.trim(), "Bonjour Bob", "text version derived from the HTML");
        let html = parts.html.unwrap();
        assert!(html.contains("<strong>Bob</strong>"));
        assert!(html.contains("data:image/png;base64,"), "image turned back into a data URL for the editor");
        assert!(parts.attachments.is_empty(), "the image is not an attachment");
    }

    #[test]
    fn plain_part_follows_the_html_and_sent_copy_keeps_bcc() {
        let draft = Draft {
            account_id: 1,
            to: vec!["bob@example.org".into()],
            bcc: vec!["boss@example.org".into()],
            subject: "Bcc".into(),
            // Stale plain text from the editor: the HTML is what the user sees and sends.
            body: "texte effacé".into(),
            html: Some("<p>Nouveau texte</p>".into()),
            draft_id: Some("abc@example.org".into()),
            ..Default::default()
        };
        let (message, copy) = build_outgoing(&account(), &draft, None, &[]).unwrap();
        let sent = String::from_utf8_lossy(&message.formatted()).to_string();
        let copy = String::from_utf8_lossy(&copy).to_string();
        assert!(!sent.contains("Bcc:") && !sent.contains("boss@example.org"), "hidden from the recipients");
        assert!(copy.contains("Bcc: boss@example.org"), "kept in the Sent copy");
        assert!(message.envelope().to().iter().any(|a| a.to_string() == "boss@example.org"), "still delivered to Bcc");
        assert!(sent.contains("Message-ID: <abc@example.org>") && copy.contains("Message-ID: <abc@example.org>"));
        assert!(!sent.contains("texte effac") && sent.contains("Nouveau texte"));
    }

    #[test]
    fn drafts_keep_invalid_or_missing_addresses_but_sending_refuses_them() {
        let draft = Draft { account_id: 1, to: vec!["bob@".into(), "alice@example.org".into()], subject: "Brouillon".into(), body: "x".into(), from: Some("ancien@alias.org".into()), ..Default::default() };
        assert!(build(&account(), &draft, None, &[], false).is_err(), "sending validates");
        let raw = build(&account(), &draft, None, &[], true).expect("a draft is always saved").formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("To: bob@, alice@example.org"), "{text}");
        assert!(text.contains("From: Yann <y@example.org>"), "unknown alias: from the account");
        assert_eq!(crate::mime::parse_draft(&raw).unwrap().to, ["bob@", "alice@example.org"], "reopened as typed");
        let empty = Draft { account_id: 1, subject: "Idée".into(), body: "à compléter".into(), ..Default::default() };
        assert!(build(&account(), &empty, None, &[], true).is_ok(), "no recipient yet");
    }

    /// A tiny SMTP server: `mail_reply` answers MAIL FROM (or `None` to accept it), `hang`
    /// makes it go silent after the message data.
    async fn fake_server(mail_reply: Option<&'static str>, rcpt_reply: Option<&'static str>, hang: bool) -> u16 {
        fake_server_counted(mail_reply, rcpt_reply, hang).await.0
    }

    /// [`fake_server`], with the number of connections it accepted. A reply of `DROP` closes the
    /// connection instead of answering.
    async fn fake_server_counted(
        mail_reply: Option<&'static str>,
        rcpt_reply: Option<&'static str>,
        hang: bool,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = connections.clone();
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else { return };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let (read, mut write) = socket.into_split();
                    let mut lines = BufReader::new(read).lines();
                    write.write_all(b"220 localhost ESMTP\r\n").await.unwrap();
                    let mut in_data = false;
                    while let Ok(Some(line)) = lines.next_line().await {
                        if in_data {
                            if line == "." {
                                in_data = false;
                                if hang {
                                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                                    return;
                                }
                                write.write_all(b"250 queued\r\n").await.unwrap();
                            }
                            continue;
                        }
                        let verb = line.split([' ', ':']).next().unwrap_or("").to_ascii_uppercase();
                        let reply: String = match verb.as_str() {
                            "EHLO" => "250-localhost\r\n250 8BITMIME\r\n".into(),
                            "MAIL" => format!("{}\r\n", mail_reply.unwrap_or("250 ok")),
                            "RCPT" => format!("{}\r\n", rcpt_reply.unwrap_or("250 ok")),
                            "DATA" => {
                                in_data = true;
                                "354 go\r\n".into()
                            }
                            "QUIT" => {
                                let _ = write.write_all(b"221 bye\r\n").await;
                                return;
                            }
                            _ => "250 ok\r\n".into(),
                        };
                        if reply.starts_with("DROP") {
                            return;
                        }
                        write.write_all(reply.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        (port, connections)
    }

    async fn try_send(port: u16) -> Result<()> {
        let mut account = account();
        account.smtp = ServerConfig { host: "127.0.0.1".into(), port, security: Security::None };
        let draft = Draft { account_id: 1, to: vec!["bob@example.org".into()], subject: "Test".into(), body: "Salut".into(), ..Default::default() };
        let (message, _) = build_outgoing(&account, &draft, None, &[]).unwrap();
        send_with(&account, &Credential::Password(String::new()), &message, 10, 2).await
    }

    #[tokio::test]
    async fn smtp_replies_decide_whether_to_retry() {
        assert!(try_send(fake_server(None, None, false).await).await.is_ok());
        // Greylisting (4xx): not sent, tried again later.
        assert!(matches!(try_send(fake_server(Some("451 4.7.1 greylisted"), None, false).await).await, Err(Error::Smtp(_))));
        // Unknown recipient (5xx): refused for good, not retried on its own.
        assert!(matches!(try_send(fake_server(None, Some("550 5.1.1 no such user"), false).await).await, Err(Error::Rejected(_))));
        // Silence after the data: it may have gone.
        assert!(matches!(try_send(fake_server(None, None, true).await).await, Err(Error::Uncertain(_))));
        // Nobody listening: certainly not sent.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        assert!(matches!(try_send(closed).await, Err(Error::Smtp(_))));
        // Connection cut before the message data: certainly not sent, so retried (it used to
        // look like an uncertain send, never retried).
        assert!(matches!(try_send(fake_server(None, Some("DROP"), false).await).await, Err(Error::Smtp(_))));
        assert!(matches!(try_send(fake_server(Some("DROP"), None, false).await).await, Err(Error::Smtp(_))));
        // One connection for the whole exchange.
        let (port, connections) = fake_server_counted(None, None, false).await;
        assert!(try_send(port).await.is_ok());
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn references_are_deduplicated_and_bounded() {
        let parent: Vec<String> = (0..40).map(|i| format!("m{i}@x")).chain(["m3@x".to_string()]).collect();
        let refs = thread_references(&parent, "p@x");
        assert_eq!(refs.len(), MAX_REFERENCES + 1);
        assert_eq!(refs[0], "m0@x", "the root is kept");
        assert_eq!(refs.last().map(String::as_str), Some("p@x"));
        assert_eq!(refs[1], "m21@x");
        assert_eq!(thread_references(&["<a@x>".into(), "a@x".into()], "a@x"), ["a@x"]);

        let draft = Draft { account_id: 1, to: vec!["b@x.org".into()], subject: "Re".into(), body: "x".into(), ..Default::default() };
        let raw = build(&account(), &draft, Some("p@x"), &["r@x".into(), "p@x".into()], false).unwrap().formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("References: <r@x> <p@x>\r\n"), "{text}");
        assert!(text.contains("In-Reply-To: <p@x>"));
    }

    #[test]
    fn oversized_messages_are_refused() {
        let small = Draft { account_id: 1, body: "x".into(), ..Default::default() };
        assert!(check_size(estimated_size(&small)).is_ok());
        let attachment = crate::model::OutgoingAttachment { filename: "a.bin".into(), mime: "application/octet-stream".into(), data: "A".repeat(26 * 1024 * 1024) };
        let big = Draft { attachments: vec![attachment], ..small };
        assert!(check_size(estimated_size(&big)).is_err());
    }

    #[test]
    fn plain_draft_stays_plain() {
        let draft = Draft { account_id: 1, to: vec!["b@x.org".into()], subject: "Texte".into(), body: "Salut".into(), ..Default::default() };
        let raw = build(&account(), &draft, None, &[], false).unwrap().formatted();
        assert!(!String::from_utf8_lossy(&raw).contains("text/html"));
        assert!(crate::mime::parse_draft(&raw).unwrap().html.is_none());
    }
}
