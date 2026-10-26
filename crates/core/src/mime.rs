use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use mail_parser::{HeaderValue, MessageParser, MimeHeaders, PartType};

use crate::error::{Error, Result};
use crate::model::{Address, Attachment};
use crate::store::BodyInfo;

#[derive(Debug, Clone, Default)]
pub struct HeaderInfo {
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub date: i64,
    /// INTERNALDATE: reliable, unlike the Date header.
    pub received_at: Option<i64>,
    /// Bulk or automated mail (list, newsletter, notification, no-reply): not a person.
    pub bulk: bool,
    pub category: crate::model::Category,
    pub signals: crate::trust::Signals,
    /// Without angle brackets.
    pub list_id: Option<String>,
}

pub struct ParsedBody {
    pub headers: HeaderInfo,
    pub html: String,
    /// The HTML was produced from the plain text (no original layout).
    pub is_plain: bool,
    pub text: Option<String>,
    pub has_remote_content: bool,
    /// Where the remote content would be loaded from.
    pub remote: Remote,
    pub attachments: Vec<Attachment>,
    pub unsubscribe: Option<Unsubscribe>,
    pub links: Vec<(String, usize)>,
    pub calendar: Option<String>,
}

/// The hosts remote content would be fetched from (lowercase); `unlisted`: some is written so its
/// host cannot be told (`@import`, `image-set`, a relative URL), which no approval covers.
#[derive(Debug, Clone, Default)]
pub struct Remote {
    pub any: bool,
    pub hosts: std::collections::BTreeSet<String>,
    pub unlisted: bool,
}

impl Remote {
    fn add(&mut self, other: Remote) {
        self.any |= other.any;
        self.hosts.extend(other.hosts);
        self.unlisted |= other.unlisted;
    }
}

/// Host of a URL as a browser would fetch it (`//host/…` taken as https).
fn fetched_host(url: &str) -> Option<String> {
    let url = url.trim_matches(|c: char| c <= ' ');
    let absolute = if url.starts_with("//") { format!("https:{url}") } else { url.to_string() };
    let parsed = url::Url::parse(&absolute).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.host_str().map(str::to_ascii_lowercase)).flatten()
}

/// A `text/calendar` part, or an `.ics` attachment.
fn calendar_part(msg: &mail_parser::Message) -> Option<String> {
    msg.parts.iter().find_map(|p| {
        let is_calendar = p.content_type().is_some_and(|c| {
            let sub = c.subtype().unwrap_or("").to_ascii_lowercase();
            (c.ctype().eq_ignore_ascii_case("text") && sub == "calendar") || sub == "ics"
        }) || p.attachment_name().is_some_and(|n| n.to_ascii_lowercase().ends_with(".ics"));
        if !is_calendar || matches!(p.body, PartType::Multipart(_)) {
            return None;
        }
        let text = String::from_utf8_lossy(p.contents()).into_owned();
        text.contains("BEGIN:VCALENDAR").then_some(text)
    })
}

fn addresses(a: Option<&mail_parser::Address>) -> Vec<Address> {
    a.map(|a| {
        a.iter()
            .filter_map(|addr| {
                Some(Address {
                    name: addr.name().map(str::to_string).filter(|n| !n.is_empty()),
                    email: addr.address()?.to_string(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn id_list(v: &HeaderValue) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => vec![t.to_string()],
        HeaderValue::TextList(l) => l.iter().map(|t| t.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn header_info(msg: &mail_parser::Message, fallback_date: i64) -> HeaderInfo {
    let category = classify(msg);
    HeaderInfo {
        message_id: msg.message_id().map(str::to_string),
        in_reply_to: id_list(msg.in_reply_to()).into_iter().next(),
        references: id_list(msg.references()),
        subject: msg.subject().unwrap_or_default().to_string(),
        from: addresses(msg.from()).into_iter().next(),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        reply_to: addresses(msg.reply_to()),
        date: msg.date().map(|d| d.to_timestamp()).unwrap_or(fallback_date),
        received_at: None,
        bulk: category != crate::model::Category::People,
        category,
        signals: signals(msg),
        list_id: msg.header_raw("List-Id").and_then(|v| {
            let id = v.rsplit_once('<').and_then(|(_, r)| r.split_once('>')).map_or(v, |(id, _)| id).trim();
            (!id.is_empty()).then(|| id.to_ascii_lowercase())
        }),
    }
}

/// At most this many links are counted per message (a hostile message can hold millions).
const MAX_LINKS: usize = 5_000;
/// At most this many distinct domains are kept per message (also bounds `message_links` rows).
const MAX_LINK_DOMAINS: usize = 200;

/// Registrable domains (`click.mail.airbnb.com` -> `airbnb.com`), most frequent first.
pub fn link_domains(content: &str) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut links = 0;
    let lower = content.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while links < MAX_LINKS
        && let Some(pos) = rest.find("http")
    {
        rest = &rest[pos + 4..];
        let after = rest.strip_prefix("s://").or_else(|| rest.strip_prefix("://"));
        let Some(after) = after else { continue };
        let end = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')).unwrap_or(after.len());
        // Hostnames are at most 253 characters: a longer run is not a host.
        let host = &after[..end.min(253)];
        let domain = crate::autoconfig::registrable(host.trim_end_matches('.'));
        if domain.contains('.') && !domain.starts_with('.') {
            links += 1;
            let known = counts.len();
            match counts.get_mut(&domain) {
                Some(n) => *n += 1,
                None if known < MAX_LINK_DOMAINS => {
                    counts.insert(domain, 1);
                }
                None => {}
            }
        }
    }
    let mut counts: Vec<(String, usize)> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// The original HTML if there is one, else the text.
fn link_source(msg: &mail_parser::Message) -> String {
    html_body(msg).or_else(|| joined_text(msg.raw_message(), msg.text_bodies())).unwrap_or_default()
}

/// The HTML body, from every part of `html_bodies` in order: Apple Mail and iOS put an
/// attachment at the cursor and the text goes on after it, in a part of its own that
/// `body_html(0)` would drop. None when no part is real HTML (mail-parser would make one up from
/// the text; ours, from `text_to_html`, is better).
fn html_body(msg: &mail_parser::Message) -> Option<String> {
    if !msg.html_bodies().any(|p| p.is_text_html()) {
        return None;
    }
    let cid_of = |p: &mail_parser::MessagePart| p.content_id().map(|c| c.trim_matches(|c| c == '<' || c == '>').to_string()).filter(|c| !c.is_empty());
    let html_parts: Vec<&str> = msg.html_bodies().filter_map(|p| if let PartType::Html(h) = &p.body { Some(h.as_ref()) } else { None }).collect();
    // The `cid:` references of every HTML part, collected once and only if an image part needs them.
    let referenced = std::cell::OnceCell::new();
    let referenced = || referenced.get_or_init(|| html_parts.iter().flat_map(|h| cid_refs(h)).collect::<HashSet<String>>());
    let mut out = String::new();
    for part in msg.html_bodies() {
        let piece = match &part.body {
            PartType::Html(h) => redecoded(msg.raw_message(), part, h, true).into_owned(),
            PartType::Text(t) => text_to_html(&redecoded(msg.raw_message(), part, t, false)),
            // An inline image between the parts, shown where it was through the usual `cid:`
            // inlining, unless an HTML part already shows it. Anything else stays an attachment.
            _ => match cid_of(part) {
                Some(cid)
                    if part.content_type().is_some_and(|c| c.ctype().eq_ignore_ascii_case("image"))
                        && !referenced().contains(&cid) =>
                {
                    format!("<img src=\"cid:{}\" alt=\"\">", html_escape::encode_double_quoted_attribute(&cid))
                }
                _ => continue,
            },
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&piece);
    }
    Some(out)
}

/// Every text part of `parts` in order (see `html_body`), the HTML ones through our
/// `html_to_text` (mail-parser's puts no line breaks between blocks). None without any.
fn joined_text<'a>(raw: &[u8], parts: impl Iterator<Item = &'a mail_parser::MessagePart<'a>>) -> Option<String> {
    let texts: Vec<String> = parts
        .filter_map(|p| match &p.body {
            PartType::Text(t) => {
                let t = redecoded(raw, p, t, false);
                let ct = p.content_type();
                let param = |name: &str, value: &str| ct.and_then(|c| c.attribute(name)).is_some_and(|v| v.trim().eq_ignore_ascii_case(value));
                Some(if param("format", "flowed") { unflow(&t, param("delsp", "yes")) } else { t.into_owned() })
            }
            PartType::Html(h) => Some(html_to_text(&redecoded(raw, p, h, true))),
            _ => None,
        })
        .collect();
    // A blank text part (some mailers add one beside the real HTML) is no text: the HTML is read.
    if texts.iter().all(|t| t.trim().is_empty()) {
        return None;
    }
    // The same budget as text drawn from HTML: what goes to the search index, the preview and the
    // reader stays bounded for a huge plain-text body too.
    let mut text = texts.join("\n");
    if text.len() > TEXT_BUDGET {
        let mut cut = TEXT_BUDGET;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Some(text)
}

/// RFC 3676 `format=flowed` text as plain lines: a soft line break (a line ending with a space)
/// is joined to the next line of the same quote depth, and space-stuffing is undone. `delsp`: the
/// space before each soft break was added by the sender and goes too. The signature separator
/// `-- ` always stands on its own line. Quoted lines come out as `>> text`.
fn unflow(text: &str, delsp: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    // Quote depth of the paragraph the last line left open (it ended with a soft break).
    let mut open: Option<usize> = None;
    for line in text.lines() {
        let depth = line.bytes().take_while(|b| *b == b'>').count();
        let content = &line[depth..];
        let content = content.strip_prefix(' ').unwrap_or(content);
        let signature = content == "-- ";
        let flowed = !signature && content.ends_with(' ');
        if signature || open != Some(depth) {
            let mut start = String::new();
            if depth > 0 {
                start.push_str(&">".repeat(depth));
                start.push(' ');
            } else if content.starts_with('>') {
                // Stuffed so as not to read as a quote: it stays so.
                start.push(' ');
            }
            lines.push(start);
        }
        let kept = if flowed && delsp { &content[..content.len() - 1] } else { content };
        if let Some(last) = lines.last_mut() {
            last.push_str(kept);
        }
        open = flowed.then_some(depth);
    }
    lines.join("\n")
}

/// The topmost header of this name: the last one added, by our own provider on receipt.
/// (mail-parser's `header_raw` returns the lowest one, which the sender may have written.)
fn top_header<'a>(msg: &'a mail_parser::Message, name: &str) -> Option<&'a str> {
    let h = msg.headers().iter().find(|h| h.name.as_str().eq_ignore_ascii_case(name))?;
    std::str::from_utf8(msg.raw_message().get(h.offset_start as usize..h.offset_end as usize)?).ok()
}

fn signals(msg: &mail_parser::Message) -> crate::trust::Signals {
    let results = top_header(msg, "Authentication-Results");
    let (dkim, spf, dmarc) = results.map(crate::trust::parse_authentication_results).unwrap_or_default();
    let authserv = results.map(crate::trust::authserv_id);
    let spam_flag = top_header(msg, "X-Spam-Flag").is_some_and(|v| v.trim().eq_ignore_ascii_case("yes"))
        || top_header(msg, "X-Spam-Status").is_some_and(|v| v.trim().to_ascii_lowercase().starts_with("yes"))
        || top_header(msg, "X-Spam").is_some_and(|v| v.trim().eq_ignore_ascii_case("yes"));
    let from = msg.from().and_then(|a| a.first());
    let address = from.and_then(|a| a.address()).unwrap_or_default().to_ascii_lowercase();
    let domain = address.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_default();
    // « service@banque.fr » in the name while the real address is elsewhere.
    let spoofed_name = from.and_then(|a| a.name()).is_some_and(|name| {
        name.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '(' | ')' | '"' | '\'' | ','))
            .any(|w| w.contains('@') && w.contains('.') && !w.eq_ignore_ascii_case(&address))
    });
    let lookalike_domain = lookalike(&domain);
    let reply_to_elsewhere = addresses(msg.reply_to()).iter().any(|r| {
        let other = r.email.rsplit_once('@').map(|(_, d)| d.to_ascii_lowercase()).unwrap_or_default();
        !domain.is_empty() && crate::autoconfig::registrable(&other) != crate::autoconfig::registrable(&domain)
    });
    crate::trust::Signals { dkim, spf, dmarc, spam_flag, spoofed_name, lookalike_domain, reply_to_elsewhere, authserv, dkim_local: None }
}

/// Sender domain that may imitate another one: written with non-ASCII characters (UTF-8 in the
/// header, which IDNA could even map onto a real ASCII domain: fullwidth letters), encoded in
/// punycode (`xn--`), or not a valid IDNA name at all.
fn lookalike(domain: &str) -> bool {
    if domain.is_empty() {
        return false;
    }
    if !domain.is_ascii() {
        return true;
    }
    match idna::domain_to_ascii(domain) {
        Ok(ascii) => ascii.split('.').any(|label| label.starts_with("xn--")),
        Err(_) => true,
    }
}

/// A newsletter announces its list (List-Unsubscribe, List-Id, RFC 2369); a notification is sent
/// by a robot (Auto-Submitted, RFC 3834, no-reply address); the rest comes from a person. Rules
/// call anything that is not a person "bulk".
fn classify(msg: &mail_parser::Message) -> crate::model::Category {
    use crate::model::Category;
    let has = |name: &str| msg.header_raw(name).is_some_and(|v| !v.trim().is_empty());
    let local = msg
        .from()
        .and_then(|a| a.first())
        .and_then(|a| a.address())
        .and_then(|a| a.split('@').next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    // By whole words of the address (« alerts-noreply », « news.letter »): « newsome@ » or
    // « accountant@ » are people.
    let words: Vec<&str> = local.split(['.', '_', '+', '-']).filter(|w| !w.is_empty()).collect();
    let any = |list: &[&str]| list.contains(&local.as_str()) || words.iter().any(|w| list.contains(w));
    if any(&["newsletter", "newsletters", "news", "marketing", "promo", "promos", "promotions", "offres", "deals"]) {
        return Category::Newsletter;
    }
    // "Do not reply" in every spelling (no-reply, ne_pas_repondre, DoNotReply…).
    let squashed: String = local.chars().filter(char::is_ascii_alphanumeric).collect();
    let robot = msg.header_raw("Auto-Submitted").is_some_and(|v| !v.trim().eq_ignore_ascii_case("no"))
        || ["noreply", "donotreply", "nepasrepondre", "mailerdaemon"].iter().any(|p| squashed.contains(p))
        || any(&[
            "noreply", "no-reply", "no_reply", "donotreply", "do-not-reply", "notification", "notifications", "notify", "alert",
            "alerts", "mailer-daemon", "postmaster", "bounce", "bounces", "automated", "security", "account", "accounts",
            "verify", "verification", "support", "billing", "invoice", "invoices", "receipt", "receipts",
        ]);
    if robot {
        return Category::Notification;
    }
    let precedence = msg.header_raw("Precedence").unwrap_or_default().trim().to_ascii_lowercase();
    if has("List-Id") || has("List-Unsubscribe") || has("List-Post") || has("Feedback-ID") || matches!(precedence.as_str(), "bulk" | "list") {
        return Category::Newsletter;
    }
    if matches!(precedence.as_str(), "junk" | "auto_reply") {
        return Category::Notification;
    }
    Category::People
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Unsubscribe {
    /// One-click (RFC 8058): an automatic POST, no page to visit.
    pub one_click: Option<String>,
    pub web: Option<String>,
    /// Full `mailto:`.
    pub mailto: Option<String>,
}

pub fn unsubscribe(msg: &mail_parser::Message) -> Option<Unsubscribe> {
    let raw = msg.header_raw("List-Unsubscribe")?;
    let links: Vec<String> = raw
        .split('<')
        .skip(1)
        .filter_map(|part| part.split_once('>').map(|(link, _)| link.split_whitespace().collect::<String>()))
        .collect();
    let web = links.iter().find(|l| l.starts_with("https://") || l.starts_with("http://")).cloned();
    let mailto = links.iter().find(|l| l.to_ascii_lowercase().starts_with("mailto:")).cloned();
    let one_click = msg
        .header_raw("List-Unsubscribe-Post")
        .filter(|v| v.to_ascii_lowercase().contains("one-click"))
        .and(web.clone())
        .filter(|l| l.starts_with("https://"))
        .filter(|_| dkim_covers_unsubscribe(msg));
    (web.is_some() || mailto.is_some()).then_some(Unsubscribe { one_click, web, mailto })
}

/// RFC 8058: the one-click POST is only for a message whose `List-Unsubscribe` and
/// `List-Unsubscribe-Post` are covered by a DKIM signature that verified, so nobody can graft
/// them onto a message (a forward, a replay) and have facteur POST to a URL of their choice.
/// The verdict is the topmost `Authentication-Results` (our provider's), which names the
/// signature that passed; that signature's `h=` tag must list both headers.
fn dkim_covers_unsubscribe(msg: &mail_parser::Message) -> bool {
    let Some(passes) = top_header(msg, "Authentication-Results").map(crate::trust::dkim_passes) else { return false };
    let signatures: Vec<std::collections::HashMap<String, String>> = msg
        .headers()
        .iter()
        .filter(|h| h.name.as_str().eq_ignore_ascii_case("DKIM-Signature"))
        .filter_map(|h| std::str::from_utf8(msg.raw_message().get(h.offset_start as usize..h.offset_end as usize)?).ok())
        .map(|value| {
            value
                .split(';')
                .filter_map(|tag| tag.split_once('='))
                // Folding whitespace may appear anywhere in a tag value.
                .map(|(name, v)| (name.trim().to_string(), v.split_whitespace().collect::<String>()))
                .collect()
        })
        .collect();
    let covers = |sig: &std::collections::HashMap<String, String>| {
        let signed: Vec<String> = sig.get("h").map(|h| h.split(':').map(|n| n.trim().to_ascii_lowercase()).collect()).unwrap_or_default();
        signed.iter().any(|n| n == "list-unsubscribe") && signed.iter().any(|n| n == "list-unsubscribe-post")
    };
    let names = |pass: &crate::trust::DkimPass, sig: &std::collections::HashMap<String, String>| {
        let d = sig.get("d").map(|d| d.to_ascii_lowercase()).unwrap_or_default();
        let identified = pass.domain.is_some() || pass.identity.is_some() || pass.selector.is_some() || pass.b.is_some();
        // A bare "dkim=pass" can only be tied to a message signed once.
        (identified || signatures.len() == 1)
            && !d.is_empty()
            && pass.domain.as_ref().is_none_or(|p| *p == d)
            && pass.identity.as_ref().is_none_or(|i| *i == d || i.ends_with(&format!(".{d}")))
            && pass.selector.as_ref().is_none_or(|s| sig.get("s").is_some_and(|own| own.eq_ignore_ascii_case(s)))
            && pass.b.as_ref().is_none_or(|b| b.len() >= 8 && sig.get("b").is_some_and(|own| own.starts_with(b.as_str())))
    };
    signatures.iter().any(|sig| covers(sig) && passes.iter().any(|pass| names(pass, sig)))
}

/// A line holding only a mailing tool's placeholder it forgot to fill (`%opentrack%`,
/// `{{first_name}}`, `*|FNAME|*`, `[[unsubscribe]]`): left out of the preview, where it would
/// stand first.
fn template_token(line: &str) -> bool {
    let l = line.trim();
    let wrapped = |open: &str, close: &str| {
        l.len() > open.len() + close.len()
            && l.starts_with(open)
            && l.ends_with(close)
            && l[open.len()..l.len() - close.len()].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '))
    };
    wrapped("%", "%") || wrapped("{{", "}}") || wrapped("*|", "|*") || wrapped("[[", "]]")
}

/// A header block alone (`BODY.PEEK[HEADER]`).
pub fn parse_headers(raw: &[u8], fallback_date: i64) -> HeaderInfo {
    match MessageParser::default().parse_headers(raw) {
        Some(msg) => header_info(&msg, fallback_date),
        None => HeaderInfo { date: fallback_date, ..Default::default() },
    }
}

pub fn body_info(raw: &[u8]) -> BodyInfo {
    let Some(msg) = MessageParser::default().parse(raw) else {
        return BodyInfo { preview: String::new(), text: String::new(), has_attachments: false, link_domains: Vec::new(), list_id: None };
    };
    // The text parts, else ours from the HTML (mail-parser would make one up from the HTML,
    // with no line breaks between blocks).
    let text = joined_text(msg.raw_message(), msg.text_bodies()).or_else(|| joined_text(msg.raw_message(), msg.html_bodies())).unwrap_or_default();
    // Its first words only: the rest of the body is never walked for 200 characters.
    let preview: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('>') && !template_token(l))
        .flat_map(str::split_whitespace)
        .take(200)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect();
    // The images parse_body shows in the HTML, by the same rule (an `<img src>` within the
    // budgets, not a mention in CSS or a stripped attribute), without encoding them: this runs
    // at sync. The sanitizer only runs for a message with such images.
    let html = html_body(&msg);
    let candidates: HashMap<String, (usize, String)> = inline_parts(&msg, html.as_deref())
        .into_iter()
        .map(|(cid, part)| (cid, (data_url_len(&part_mime(part), part.contents().len()), String::new())))
        .collect();
    let shown = match &html {
        Some(h) if !candidates.is_empty() => sanitize_html_budgeted(h, candidates).2,
        _ => HashSet::new(),
    };
    BodyInfo {
        preview,
        text,
        has_attachments: real_attachments(&msg, &shown).next().is_some(),
        link_domains: link_domains(&link_source(&msg)).into_iter().map(|(d, _)| d).collect(),
        list_id: header_info(&msg, 0).list_id,
    }
}

/// Above this an inline image is not shown in the message (it stays downloadable).
const MAX_INLINE_IMAGE: usize = 10 * 1024 * 1024;
/// Inline images (data URLs, so base64) written into one message's HTML, all occurrences
/// together: `<img src=cid:a>` repeated 50,000 times must not inline the image 50,000 times.
const MAX_INLINE_TOTAL: usize = 20 * 1024 * 1024;
/// At most this many `cid:` images are inlined per message; the others are left without source.
const MAX_INLINE_COUNT: usize = 500;

/// Images shown in the HTML (`cid:`) are left out; an inline part the HTML does not show stays an
/// attachment, or it would be lost. `shown`: the Content-IDs (without `<>`) really substituted
/// into an `<img>`, so an image dropped by the inline budgets stays downloadable.
fn real_attachments<'a>(msg: &'a mail_parser::Message<'a>, shown: &'a HashSet<String>) -> impl Iterator<Item = (usize, &'a mail_parser::MessagePart<'a>)> {
    msg.attachments().enumerate().filter(move |(_, p)| {
        // No Content-Disposition (Python's add_related, .NET's LinkedResource): inline too.
        let inline = p.content_disposition().is_none_or(|d| d.ctype().eq_ignore_ascii_case("inline"));
        let shown = p.content_id().is_some_and(|cid| shown.contains(cid.trim_matches(|c| c == '<' || c == '>')));
        !(inline && shown)
    })
}

fn part_mime(part: &mail_parser::MessagePart) -> String {
    part.content_type()
        .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")))
        .unwrap_or_else(|| "application/octet-stream".into())
}

/// Length of the data URL `inline_cids` writes for a part (base64, as `sanitize_html` counts it).
fn data_url_len(mime: &str, size: usize) -> usize {
    "data:;base64,".len() + mime.len() + size.div_ceil(3) * 4
}

/// Embedded images the HTML references (`cid:`) and that fit the inline limits, with their
/// Content-ID (without `<>`). Anything else (a PDF with a Content-ID, an image too large) stays
/// an attachment.
fn inline_parts<'a>(msg: &'a mail_parser::Message<'a>, html: Option<&str>) -> Vec<(String, &'a mail_parser::MessagePart<'a>)> {
    let mut parts = Vec::new();
    let Some(html) = html else { return parts };
    // One pass over the HTML, then a lookup per part: a `contains` per part would cost
    // parts × HTML size on a message with tens of thousands of tiny images.
    let referenced = cid_refs(html);
    let mut encoded = 0;
    for part in msg.attachments() {
        if parts.len() >= MAX_INLINE_COUNT {
            break;
        }
        if let Some(cid) = part.content_id()
            && part.contents().len() <= MAX_INLINE_IMAGE
            && part.content_type().is_some_and(|c| c.ctype().eq_ignore_ascii_case("image"))
            && encoded + data_url_len(&part_mime(part), part.contents().len()) <= MAX_INLINE_TOTAL
        {
            let cid = cid.trim_matches(|c| c == '<' || c == '>');
            if referenced.contains(cid) {
                encoded += data_url_len(&part_mime(part), part.contents().len());
                parts.push((cid.to_string(), part));
            }
        }
    }
    parts
}

/// Longest Content-ID looked for in a `cid:` reference; longer ones are never inlined.
const MAX_CID_LEN: usize = 998;

/// The ids of the `cid:` references in `html` (case-insensitive scheme), as written and
/// percent-decoded. An id ends at a quote, a bracket, a parenthesis (CSS `url(cid:…)`) or a
/// space. One linear pass: references never overlap and an id is cut at `MAX_CID_LEN`.
fn cid_refs(html: &str) -> HashSet<String> {
    let bytes = html.as_bytes();
    let mut refs = HashSet::new();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if !bytes[i..i + 4].eq_ignore_ascii_case(b"cid:") {
            i += 1;
            continue;
        }
        let start = i + 4;
        let rest = &bytes[start..bytes.len().min(start + MAX_CID_LEN)];
        let mut end = start + rest.iter().position(|&b| matches!(b, b'"' | b'\'' | b'<' | b'>' | b'(' | b')') || b <= b' ').unwrap_or(rest.len());
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        let id = &html[start..end];
        if !id.is_empty() {
            if id.contains('%') {
                refs.insert(percent_decode(id));
            }
            refs.insert(id.to_string());
        }
        i = end.max(start);
    }
    refs
}

/// Embedded images the HTML shows (`cid:`), as data URLs by Content-ID (without `<>`).
fn inline_cids(msg: &mail_parser::Message, html: Option<&str>) -> HashMap<String, String> {
    inline_parts(msg, html)
        .into_iter()
        .map(|(cid, part)| (cid, format!("data:{};base64,{}", part_mime(part), B64.encode(part.contents()))))
        .collect()
}

pub fn parse_body(raw: &[u8]) -> Result<ParsedBody> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Mime(crate::tr!("message illisible", "unreadable message")))?;
    let headers = header_info(&msg, 0);

    // Inline images, cid -> data URL: only those the HTML really shows (a 20 MB attachment with
    // a Content-ID need not be encoded at each opening).
    let html_body = html_body(&msg);
    let cids = inline_cids(&msg, html_body.as_deref());

    let text = joined_text(msg.raw_message(), msg.text_bodies());
    let (html, remote, is_plain, shown) = match html_body {
        // mail-parser makes up HTML from the text when there is none: ours is better.
        Some(h) => {
            let (html, remote, shown) = sanitize_html_inlining(&h, cids);
            (html, remote, false, shown)
        }
        _ => (text_to_html(text.as_deref().unwrap_or_default()), Remote::default(), true, HashSet::new()),
    };

    // A message made of thousands of tiny parts would become thousands of chips: the first ones
    // are listed (a real message is far below), the message itself stays readable.
    let attachments = real_attachments(&msg, &shown)
        .take(MAX_LISTED_ATTACHMENTS)
        .map(|(index, p)| Attachment {
            index,
            filename: p.attachment_name().unwrap_or("sans-nom").to_string(),
            mime: part_mime(p),
            size: p.contents().len(),
        })
        .collect();

    let unsubscribe = unsubscribe(&msg);
    let links = link_domains(&link_source(&msg));
    let calendar = calendar_part(&msg);
    Ok(ParsedBody { headers, html, is_plain, text, has_remote_content: remote.any, remote, attachments, unsubscribe, links, calendar })
}

pub struct DraftParts {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Inline images turned back into data URLs for the editor.
    pub html: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    /// References of the draft, without `in_reply_to`.
    pub references: Vec<String>,
    pub attachments: Vec<crate::model::OutgoingAttachment>,
    /// May be an alias.
    pub from: Option<String>,
}

pub fn parse_draft(raw: &[u8]) -> Result<DraftParts> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Mime(crate::tr!("brouillon illisible", "unreadable draft")))?;
    let list = |a: Option<&mail_parser::Address>| addresses(a).iter().map(crate::store::format_address).collect();
    // Every body part, as the reader joins them: Apple Mail puts text after an inline photo.
    let html_body = html_body(&msg);
    let cids = inline_cids(&msg, html_body.as_deref());
    let (rich, shown) = match html_body {
        Some(h) => {
            let (html, _, shown) = sanitize_html_inlining(&h, cids);
            (Some(html), shown)
        }
        None => (None, HashSet::new()),
    };
    // Only the images now shown in the HTML (as data URLs) belong to it. Any other part, even with
    // a Content-ID (a PDF from Apple Mail or Outlook, an image too large to inline), stays an
    // attachment of the draft, or it would be lost on the next save.
    let inline = |p: &&mail_parser::MessagePart| p.content_id().is_some_and(|cid| shown.contains(cid.trim_matches(|c| c == '<' || c == '>')));
    let in_reply_to = id_list(msg.in_reply_to()).into_iter().next();
    // References without the message replied to (it is added back when the draft is built).
    let mut references: Vec<String> = Vec::new();
    for r in id_list(msg.references()) {
        if Some(&r) != in_reply_to.as_ref() && !references.contains(&r) {
            references.push(r);
        }
    }
    Ok(DraftParts {
        to: list(msg.to()),
        cc: list(msg.cc()),
        bcc: list(msg.bcc()),
        subject: msg.subject().unwrap_or_default().to_string(),
        body: joined_text(msg.raw_message(), msg.text_bodies()).unwrap_or_default(),
        html: rich.clone(),
        message_id: msg.message_id().map(str::to_string),
        in_reply_to,
        references,
        from: addresses(msg.from()).into_iter().next().map(|a| a.email),
        attachments: msg
            .attachments()
            .filter(|p| !inline(p))
            .map(|p| {
                let (mime, data) = attachment_file(raw, p);
                crate::model::OutgoingAttachment { filename: p.attachment_name().unwrap_or("sans-nom").to_string(), mime, data: B64.encode(data) }
            })
            .collect(),
    })
}

pub fn attachment(raw: &[u8], index: usize) -> Result<(String, String, Vec<u8>)> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Mime(crate::tr!("message illisible", "unreadable message")))?;
    let part = msg
        .attachments()
        .nth(index)
        .ok_or_else(|| Error::NotFound(crate::tr!("pièce jointe {index}", "attachment {index}")))?;
    let (mime, data) = attachment_file(raw, part);
    Ok((part.attachment_name().unwrap_or("sans-nom").to_string(), mime, data))
}

/// An attachment as a file: its type and its bytes as sent. mail-parser hands text parts over as
/// UTF-8 (transcoded from their charset, or with U+FFFD for the bytes that are not UTF-8), which
/// would change the file: a reopened draft would save a CSV in Windows-1252 damaged.
fn attachment_file(raw: &[u8], part: &mail_parser::MessagePart) -> (String, Vec<u8>) {
    let mut mime = part_mime(part);
    let data = match &part.body {
        PartType::Message(m) => m.raw_message().to_vec(),
        PartType::Text(_) | PartType::Html(_) => match original_bytes(raw, part) {
            Some(bytes) => {
                // Labelled with their charset, for the viewer and a forwarded copy. Only a plain
                // token: anything else would make the whole type unreadable to lettre.
                let charset = part.content_type().and_then(|c| c.attribute("charset")).map(str::trim);
                if let Some(charset) = charset.filter(|c| !c.is_empty() && c.len() <= 40 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:+".contains(&b))) {
                    mime = format!("{mime}; charset={charset}");
                }
                bytes
            }
            None => part.contents().to_vec(),
        },
        _ => part.contents().to_vec(),
    };
    (mime, data)
}

pub fn mime_charset(mime: &str) -> Option<&str> {
    mime.split(';').skip(1).find_map(|p| {
        let (name, value) = p.split_once('=')?;
        name.trim().eq_ignore_ascii_case("charset").then(|| value.trim().trim_matches('"')).filter(|v| !v.is_empty())
    })
}

/// Body of a part as sent, with only its Content-Transfer-Encoding undone.
/// A text part's text, decoded again from its bytes when mail-parser had no charset to go by and
/// the bytes were not UTF-8 (its lossy decoding leaves U+FFFD for every accent): the charset of an
/// HTML `<meta>`, else Windows-1252 (see [`text_as_utf8`]).
fn redecoded<'t>(raw: &[u8], part: &mail_parser::MessagePart, text: &'t str, html: bool) -> std::borrow::Cow<'t, str> {
    use std::borrow::Cow;
    // Named and known to mail-parser: it decoded the text. Otherwise (none, `unknown-8bit`, an
    // empty or made-up name) it fell back to lossy UTF-8.
    let decoded = part
        .content_type()
        .and_then(|c| c.attribute("charset"))
        .is_some_and(|c| mail_parser::decoders::charsets::map::charset_decoder(c.trim().as_bytes()).is_some());
    if !text.contains('\u{FFFD}') || decoded {
        return Cow::Borrowed(text);
    }
    let Some(bytes) = original_bytes(raw, part) else { return Cow::Borrowed(text) };
    let declared = if html { meta_charset(&bytes) } else { None };
    match String::from_utf8(text_as_utf8(&bytes, declared.as_deref()).into_owned()) {
        Ok(again) => Cow::Owned(again),
        Err(_) => Cow::Borrowed(text),
    }
}

/// The charset an HTML document declares in its head (`<meta charset=…>` or `http-equiv`).
fn meta_charset(html: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&html[..html.len().min(4096)]).to_ascii_lowercase();
    let at = head.find("<meta")?;
    let from = head[at..].find("charset=")? + at + "charset=".len();
    let value: String = head[from..].trim_start_matches(['"', '\'', ' ']).chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')).collect();
    (!value.is_empty()).then_some(value)
}

fn original_bytes(raw: &[u8], part: &mail_parser::MessagePart) -> Option<Vec<u8>> {
    use mail_parser::Encoding;
    use mail_parser::decoders::{base64::base64_decode, quoted_printable::quoted_printable_decode};
    let body = raw.get(part.raw_body_offset() as usize..part.raw_end_offset() as usize)?;
    match part.encoding {
        Encoding::Base64 => base64_decode(body),
        Encoding::QuotedPrintable => quoted_printable_decode(body),
        Encoding::None => Some(body.to_vec()),
    }
}

/// The result is shown in a sandboxed iframe without scripts anyway: this is a second barrier.
/// Also returns whether the message has remote resources (tracking pixels, hosted images…).
pub fn sanitize_html(html: &str, cids: HashMap<String, String>) -> (String, bool) {
    let (clean, remote, _) = sanitize_html_inlining(html, cids);
    (clean, remote.any)
}

/// `sanitize_html`, plus the Content-IDs of `cids` really written into an `<img>` (the others
/// went over the inline budgets, or are not shown).
fn sanitize_html_inlining(html: &str, cids: HashMap<String, String>) -> (String, Remote, HashSet<String>) {
    sanitize_html_budgeted(html, cids.into_iter().map(|(cid, data)| (cid, (data.len(), data))).collect())
}

/// `sanitize_html_inlining`, each image given as (length counted against the inline budget,
/// what is written): [`body_info`] gives empty sources of the real lengths, to learn which
/// images the reader shows without encoding them.
fn sanitize_html_budgeted(html: &str, cids: HashMap<String, (usize, String)>) -> (String, Remote, HashSet<String>) {
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let remote = Arc::new(Mutex::new(Remote::default()));
    let flag = remote.clone();
    let cids = Arc::new(cids);
    // Budget of inlined image bytes and substitutions, shared by every `<img src="cid:…">`.
    let inlined = Arc::new(AtomicUsize::new(0));
    let substitutions = Arc::new(AtomicUsize::new(0));
    let shown = Arc::new(Mutex::new(HashSet::new()));
    let shown_by_filter = shown.clone();
    let generic_attrs: HashSet<&str> = [
        "style", "class", "id", "align", "valign", "width", "height", "bgcolor", "color", "border",
        "cellpadding", "cellspacing", "dir", "lang", "title", "colspan", "rowspan", "face", "size",
    ]
    .into();
    let clean = ammonia::Builder::default()
        .add_tags(["style", "center", "font"])
        .rm_clean_content_tags(["style"])
        .generic_attributes(generic_attrs)
        .add_tag_attributes("img", ["src", "alt"])
        .add_tag_attributes("a", ["href"])
        .url_schemes(["http", "https", "mailto", "data", "cid", "tel"].into())
        .link_rel(Some("noopener noreferrer"))
        // Called with the attribute as written by the sender (`HTTPS://`, leading spaces, tabs
        // inside the scheme…): decisions are made on the URL as a browser would parse it.
        .attribute_filter(move |element, attribute, value| {
            if attribute == "style" {
                flag.lock().unwrap_or_else(|e| e.into_inner()).add(css_remote(value));
                return Some(value.into());
            }
            let is_url = matches!((element, attribute), ("img", "src") | ("a", "href"));
            if !is_url {
                return Some(value.into());
            }
            let image = element == "img";
            match url::Url::parse(value) {
                Ok(url) => match url.scheme() {
                    "cid" if image => {
                        let budget_left = substitutions.fetch_add(1, Ordering::Relaxed) < MAX_INLINE_COUNT;
                        // The id as written (a `?` or `#` in a Content-ID is not a URL query).
                        let raw = value.trim_matches(|c: char| c <= ' ');
                        let id = raw.get(..4).filter(|p| p.eq_ignore_ascii_case("cid:")).map_or(url.path(), |_| &raw[4..]);
                        let (key, (len, data)) = cids.get_key_value(id).or_else(|| cids.get_key_value(percent_decode(id).as_str()))?;
                        // Over budget: no source at all (the alt text stays), never a remote fallback.
                        let total = inlined.fetch_add(*len, Ordering::Relaxed) + len;
                        if !(budget_left && total <= MAX_INLINE_TOTAL) {
                            return None;
                        }
                        shown_by_filter.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone());
                        Some(data.clone().into())
                    }
                    "cid" => None,
                    "http" | "https" => {
                        if image {
                            let mut r = flag.lock().unwrap_or_else(|e| e.into_inner());
                            r.any = true;
                            match url.host_str() {
                                Some(host) => r.hosts.insert(host.to_ascii_lowercase()),
                                None => std::mem::replace(&mut r.unlisted, true),
                            };
                        }
                        Some(url.to_string().into())
                    }
                    // `data:` is only for images: a `data:` link would open a forged page.
                    "data" if image => Some(value.trim().to_string().into()),
                    "data" => None,
                    "mailto" | "tel" if !image => Some(value.into()),
                    _ => None,
                },
                // Protocol-relative (`//tracker.example/p.gif`): remote, resolved against the
                // app's own scheme otherwise. Rewritten to https so the CSP sees it as remote.
                Err(url::ParseError::RelativeUrlWithoutBase) => {
                    let trimmed = value.trim_matches(|c: char| c <= ' ');
                    if trimmed.starts_with("//") || trimmed.starts_with("\\\\") || trimmed.starts_with("/\\") || trimmed.starts_with("\\/") {
                        let absolute = url::Url::parse(&format!("https:{}", trimmed.replace('\\', "/"))).ok()?;
                        if image {
                            let mut r = flag.lock().unwrap_or_else(|e| e.into_inner());
                            r.any = true;
                            match absolute.host_str() {
                                Some(host) => r.hosts.insert(host.to_ascii_lowercase()),
                                None => std::mem::replace(&mut r.unlisted, true),
                            };
                        }
                        Some(absolute.to_string().into())
                    } else if image {
                        // A relative image has no base in a mail: it could only hit the app.
                        None
                    } else {
                        Some(value.into())
                    }
                }
                Err(_) => None,
            }
        })
        .clean(&defused(html))
        .to_string();
    // <style> sheets can load resources too.
    let mut remote = std::mem::take(&mut *remote.lock().unwrap_or_else(|e| e.into_inner()));
    for block in style_blocks(&clean) {
        remote.add(css_remote(block));
    }
    let shown = std::mem::take(&mut *shown.lock().unwrap_or_else(|e| e.into_inner()));
    (clean, remote, shown)
}

/// `%XX` sequences decoded (RFC 2392 `cid:` URLs are URL-encoded Content-IDs).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).copied().and_then(hex), bytes.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Contents of the `<style>` elements of sanitized HTML (ammonia writes lowercase tags).
fn style_blocks(html: &str) -> impl Iterator<Item = &str> {
    let mut rest = html;
    std::iter::from_fn(move || {
        let start = rest.find("<style")?;
        let open_end = start + rest[start..].find('>')? + 1;
        let close = rest[open_end..].find("</style").map_or(rest.len(), |c| open_end + c);
        let block = &rest[open_end..close];
        rest = &rest[close..];
        if rest.starts_with("</style") {
            rest = &rest[7..];
        }
        Some(block)
    })
}

/// What CSS would load from the network, by host (see [`css_loads_remote`]).
fn css_remote(css: &str) -> Remote {
    let mut remote = Remote { any: css_loads_remote(css), ..Default::default() };
    if !remote.any {
        return remote;
    }
    let css = css_normalize(css);
    if css.contains("@import") || css.contains("image-set(") || css.contains("image(") {
        remote.unlisted = true;
    }
    let mut rest = css.as_str();
    while let Some(pos) = rest.find("url(") {
        rest = &rest[pos + 4..];
        let arg = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'');
        let end = arg.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')')).unwrap_or(arg.len());
        let url = &arg[..end];
        if url.starts_with("data:") {
            continue;
        }
        match fetched_host(url) {
            Some(host) => {
                remote.hosts.insert(host);
            }
            None => remote.unlisted = true,
        }
    }
    remote
}

/// CSS that would load something from the network: `url(…)` other than `data:`, `@import`,
/// `image-set(…)`. Case, CSS escapes (`\75 rl(`) and comments are undone first: a browser reads
/// `URL(` and `u\rl(` as `url(` (comments only ever err on the side of flagging).
fn css_loads_remote(css: &str) -> bool {
    let css = css_normalize(css);
    if css.contains("@import") || css.contains("image-set(") || css.contains("image(") {
        return true;
    }
    let mut rest = css.as_str();
    while let Some(pos) = rest.find("url(") {
        rest = &rest[pos + 4..];
        let arg = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'');
        if !arg.starts_with("data:") {
            return true;
        }
    }
    false
}

fn css_normalize(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut hex = String::new();
                while hex.len() < 6 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
                    hex.push(chars.next().unwrap_or_default());
                }
                if hex.is_empty() {
                    // `\x` stands for `x` (a backslash before a newline is a line continuation).
                    if let Some(next) = chars.next()
                        && next != '\n'
                    {
                        out.extend(next.to_lowercase());
                    }
                } else {
                    // One whitespace after a hex escape belongs to it.
                    if chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                    let decoded = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}');
                    out.extend(decoded.to_lowercase());
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

pub fn text_to_html(text: &str) -> String {
    let mut out = String::from("<div style=\"white-space: pre-wrap; font-family: inherit\">");
    for line in text.lines() {
        let quoted = line.starts_with('>');
        if quoted {
            out.push_str("<span style=\"color: #6b7280\">");
        }
        for (i, word) in line.split(' ').enumerate() {
            if i > 0 {
                out.push(' ');
            }
            if word.starts_with("https://") || word.starts_with("http://") {
                let w = html_escape::encode_double_quoted_attribute(word);
                out.push_str(&format!("<a href=\"{w}\">{}</a>", html_escape::encode_text(word)));
            } else {
                out.push_str(&html_escape::encode_text(word));
            }
        }
        if quoted {
            out.push_str("</span>");
        }
        out.push('\n');
    }
    out.push_str("</div>");
    out
}

/// Formatting elements left open at once beyond which they are dropped, whatever follows.
const MAX_OPEN_FORMATTING: i64 = 1000;
/// Elements the parser would rebuild (open formatting elements times the tags after them).
const MAX_REBUILT: i64 = 2_000_000;
/// Elements open inside one another beyond which more are dropped: the parser walks the open ones
/// at every tag, so each level costs as much as all those above it. Mail never nests this deep.
const MAX_NESTING: usize = 1000;
/// End tags matching nothing open beyond which they are dropped: each walks the open elements
/// for nothing.
const MAX_STRAY_ENDS: usize = 1000;

/// A tag as the HTML parser reads it: `start..end` in the text, its name as written.
struct Tag<'a> {
    start: usize,
    end: usize,
    closing: bool,
    name: &'a str,
}

/// The tags of `html`, in one pass, read as the parser does: a `<` inside a quoted attribute
/// value, a comment, or the text of a script, style or title starts no tag.
fn html_tags(html: &str) -> Vec<Tag<'_>> {
    const RAW_TEXT: [&str; 9] = ["script", "style", "xmp", "iframe", "noembed", "noframes", "noscript", "title", "textarea"];
    let b = html.as_bytes();
    let find = |from: usize, needle: &[u8]| b.get(from..).and_then(|rest| rest.windows(needle.len()).position(|w| w == needle)).map(|p| from + p);
    let mut tags = Vec::new();
    let mut i = 0;
    while let Some(at) = b.get(i..).and_then(|rest| rest.iter().position(|&c| c == b'<')).map(|p| i + p) {
        let next = b.get(at + 1).copied();
        // Comments, `<!…>` and `<?…>`: no tag, up to their end.
        if b[at..].starts_with(b"<!--") {
            i = match b.get(at + 4..) {
                Some([b'>', ..]) => at + 5,
                Some([b'-', b'>', ..]) => at + 6,
                _ => find(at + 4, b"-->").map_or(b.len(), |e| e + 3),
            };
            continue;
        }
        let closing = next == Some(b'/');
        let name_at = at + 1 + usize::from(closing);
        if !b.get(name_at).is_some_and(u8::is_ascii_alphabetic) {
            i = match (next, closing) {
                (Some(b'!' | b'?'), _) | (_, true) => find(at, b">").map_or(b.len(), |e| e + 1),
                _ => at + 1,
            };
            continue;
        }
        let stop = |c: &u8| c.is_ascii_whitespace() || matches!(c, b'/' | b'>');
        let name_end = b[name_at..].iter().position(stop).map_or(b.len(), |p| name_at + p);
        // Attributes, a quoted value read whole.
        let mut j = name_end;
        let end = loop {
            while b.get(j).is_some_and(|c| c.is_ascii_whitespace() || *c == b'/') {
                j += 1;
            }
            match b.get(j) {
                None => break None,
                Some(b'>') => break Some(j + 1),
                _ => {}
            }
            j += 1;
            while b.get(j).is_some_and(|c| !stop(c) && *c != b'=') {
                j += 1;
            }
            while b.get(j).is_some_and(u8::is_ascii_whitespace) {
                j += 1;
            }
            if b.get(j) != Some(&b'=') {
                continue;
            }
            j += 1;
            while b.get(j).is_some_and(u8::is_ascii_whitespace) {
                j += 1;
            }
            match b.get(j) {
                Some(&q @ (b'"' | b'\'')) => j = b[j + 1..].iter().position(|&c| c == q).map_or(b.len(), |p| j + 2 + p),
                _ => {
                    while b.get(j).is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>') {
                        j += 1;
                    }
                }
            }
        };
        // Cut by the end of the text: the parser drops it.
        let Some(end) = end else { break };
        let name = &html[name_at..name_end];
        tags.push(Tag { start: at, end, closing, name });
        i = end;
        // Their text runs to their own end tag.
        if !closing && let Some(raw) = RAW_TEXT.iter().find(|r| r.eq_ignore_ascii_case(name)) {
            let mut k = end;
            i = loop {
                let Some(e) = find(k, b"</") else { break b.len() };
                let after = e + 2 + raw.len();
                if b.get(e + 2..after).is_some_and(|n| n.eq_ignore_ascii_case(raw.as_bytes())) && b.get(after).is_none_or(stop) {
                    break e;
                }
                k = e + 2;
            };
        } else if !closing && name.eq_ignore_ascii_case("plaintext") {
            break;
        }
    }
    tags
}

/// `html` without the tags crafted to blow up the parser, which only carry presentation:
/// - formatting tags (b, i, font…), when too many are left open: the parser re-opens every
///   formatting element still open at each piece of text after a block closed them, so a few
///   hundred left open before thousands of paragraphs build hundreds of millions of elements
///   (gigabytes) from 100 KB. The cost is those left open times what follows, not their number:
///   ordinary mail closes them.
/// - elements nested past [`MAX_NESTING`], and end tags matching nothing open past
///   [`MAX_STRAY_ENDS`].
fn defused(html: &str) -> std::borrow::Cow<'_, str> {
    const FORMATTING: [&str; 14] = ["a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u"];
    const VOID: [&str; 15] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "keygen", "link", "meta", "param", "source", "track", "wbr"];
    // Closed by the next of their kind or by their parent's end: never nested deep.
    const IMPLIED: [&str; 18] =
        ["p", "li", "dt", "dd", "option", "optgroup", "tr", "td", "th", "thead", "tbody", "tfoot", "caption", "colgroup", "rb", "rt", "rp", "rtc"];
    // An end tag that does something even with nothing open.
    const ALWAYS: [&str; 4] = ["p", "br", "body", "html"];
    let is = |list: &[&str], name: &str| list.iter().any(|l| l.eq_ignore_ascii_case(name));
    let tags = html_tags(html);
    let mut dropped = vec![false; tags.len()];
    // Formatting elements open at once at most, as the parser keeps them: three of a kind (same
    // name and attributes) at most, the others forgotten (its "Noah's Ark" rule). Told by the tag
    // as written, which can only count more.
    let mut formatting: HashMap<String, Vec<String>> = HashMap::new();
    let mut alike: HashMap<String, i64> = HashMap::new();
    let (mut now, mut peak) = (0i64, 0i64);
    // The other elements open, as the parser nests them, and those left out past the limit.
    let mut stack: Vec<String> = Vec::new();
    let mut open: HashMap<String, usize> = HashMap::new();
    let mut skipped: HashMap<String, usize> = HashMap::new();
    let mut stray = Vec::new();
    let mut deep = false;
    for (n, tag) in tags.iter().enumerate() {
        let name = tag.name.to_ascii_lowercase();
        if is(&FORMATTING, &name) {
            let open = formatting.entry(name).or_default();
            if tag.closing {
                if let Some(key) = open.pop() {
                    let count = alike.entry(key).or_default();
                    *count -= 1;
                    if *count < 3 {
                        now -= 1;
                    }
                }
            } else {
                let key = html[tag.start..tag.end].to_ascii_lowercase();
                let count = alike.entry(key.clone()).or_default();
                if *count < 3 {
                    now += 1;
                    peak = peak.max(now);
                }
                *count += 1;
                open.push(key);
            }
            continue;
        }
        if is(&VOID, &name) || is(&IMPLIED, &name) {
            continue;
        }
        if !tag.closing {
            if stack.len() >= MAX_NESTING {
                dropped[n] = true;
                deep = true;
                *skipped.entry(name).or_default() += 1;
            } else {
                *open.entry(name.clone()).or_default() += 1;
                stack.push(name);
            }
        } else if let Some(count) = skipped.get_mut(&name).filter(|c| **c > 0) {
            *count -= 1;
            dropped[n] = true;
        } else if open.get(&name).is_some_and(|c| *c > 0) {
            // Closes it, and whatever is still open inside it.
            while let Some(top) = stack.pop() {
                let done = top == name;
                if let Some(c) = open.get_mut(&top) {
                    *c -= 1;
                }
                if done {
                    break;
                }
            }
        } else if !is(&ALWAYS, &name) {
            stray.push(n);
        }
    }
    let crafted = peak > MAX_OPEN_FORMATTING || peak.saturating_mul(tags.len() as i64) > MAX_REBUILT;
    if crafted {
        for (n, tag) in tags.iter().enumerate() {
            dropped[n] |= is(&FORMATTING, tag.name);
        }
    }
    let strays = stray.len() > MAX_STRAY_ENDS;
    if strays {
        for n in stray {
            dropped[n] = true;
        }
    }
    if !crafted && !deep && !strays {
        return std::borrow::Cow::Borrowed(html);
    }
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    for (tag, _) in tags.iter().zip(&dropped).filter(|(_, d)| **d) {
        out.push_str(&html[at..tag.start]);
        at = tag.end;
    }
    out.push_str(&html[at..]);
    std::borrow::Cow::Owned(out)
}

/// Plain-text version of HTML: the text/plain part of outgoing mail, the search index and the
/// preview. Blocks and `<br>` become line breaks (a blank line between paragraphs), quotes get
/// `> ` prefixes, list items keep a marker, and a link whose text is not its address gets the
/// address after it.
pub fn html_to_text(html: &str) -> String {
    // Normalised first: lowercase tags, only the structural ones, text and attributes escaped.
    let cleaned = ammonia::Builder::empty()
        .add_tags([
            "p", "div", "br", "hr", "blockquote", "ul", "ol", "li", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "table",
            "tr", "td", "th", "dt", "dd", "dl", "a", "section", "article", "header", "footer", "center",
        ])
        .add_tag_attributes("a", ["href"])
        .url_schemes(["http", "https", "mailto"].into())
        .link_rel(None)
        .clean_content_tags(["style", "script", "head", "title"].into())
        .clean(&defused(html))
        .to_string();
    let mut w = TextWriter::default();
    // Open lists: `None` for a bulleted one, the next number for a numbered one.
    let mut lists: Vec<Option<usize>> = Vec::new();
    // Open links: their address, and where their text starts in the output.
    let mut links: Vec<Option<(String, usize)>> = Vec::new();
    let mut first_cell = true;
    let mut rest = cleaned.as_str();
    while !rest.is_empty() {
        // Enough for the preview, the search index and any real message: hostile HTML cannot
        // make it grow without bound.
        if w.out.len() > TEXT_BUDGET {
            break;
        }
        let Some(lt) = rest.find('<') else {
            w.text(rest);
            break;
        };
        w.text(&rest[..lt]);
        rest = &rest[lt..];
        // The end of the tag, outside quoted attribute values (`>` is not escaped in them).
        let mut quoted = false;
        let end = rest.char_indices().find(|&(_, c)| {
            if c == '"' {
                quoted = !quoted;
            }
            c == '>' && !quoted
        });
        let Some((end, _)) = end else { break };
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        match (name.as_str(), closing) {
            ("br", _) => w.line_break(),
            ("hr", _) => {
                w.block(1);
                w.text("----------");
                w.block(1);
            }
            ("p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" | "table" | "dl", _) => {
                w.block(2);
                if name == "pre" {
                    w.pre = !closing;
                }
            }
            ("blockquote", false) => {
                w.block(2);
                w.quote += 1;
            }
            ("blockquote", true) => {
                w.block(2);
                w.quote = w.quote.saturating_sub(1);
            }
            ("ul" | "ol", false) => {
                w.block(if lists.is_empty() { 2 } else { 1 });
                lists.push((name == "ol").then_some(1));
            }
            ("ul" | "ol", true) => {
                lists.pop();
                w.block(if lists.is_empty() { 2 } else { 1 });
            }
            ("li", false) => {
                w.block(1);
                let indent = "  ".repeat(lists.len().saturating_sub(1).min(MAX_DEPTH));
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{indent}{}. ", *n - 1)
                    }
                    _ => format!("{indent}- "),
                };
                w.raw(&marker);
            }
            ("tr", _) => {
                w.block(1);
                first_cell = true;
            }
            ("td" | "th", false) => {
                if !first_cell {
                    w.text(" ");
                }
                first_cell = false;
            }
            ("a", false) => {
                let href = tag
                    .split_once("href=\"")
                    .and_then(|(_, v)| v.split_once('"'))
                    .map(|(v, _)| html_escape::decode_html_entities(v).into_owned());
                w.flush();
                // Nested links are no list: past a few levels, crafted, they are not looked at
                // (each one's end compares its whole text so far).
                links.push(href.filter(|_| links.len() < MAX_LINK_DEPTH).map(|h| (h, w.out.len())));
            }
            ("a", true) => {
                if let Some(Some((href, start))) = links.pop()
                    && w.out.len().saturating_sub(start) <= MAX_LINK_TEXT
                {
                    let shown = w.out.get(start..).unwrap_or_default().trim().to_string();
                    let address = href.strip_prefix("mailto:").unwrap_or(&href);
                    if !shown.is_empty() && !shown.contains(address) && !address.contains(&shown) {
                        w.raw(&format!(" <{href}>"));
                    }
                }
            }
            ("div" | "li" | "dt" | "dd" | "section" | "article" | "header" | "footer" | "center", _) => w.block(1),
            _ => {}
        }
    }
    w.out.truncate(w.out.trim_end().len());
    w.out
}

/// A text attachment as UTF-8, for the viewer (which reads it as UTF-8): kept when it already
/// is, read through its byte order mark, else as Windows-1252 (Notepad's legacy encoding), unless
/// it starts as real UTF-8 (one broken byte then costs one character, not every accent).
/// `charset`: the one the sender declared (see [`mime_charset`]), followed when it is known and
/// not UTF-8, except for text that reads as valid UTF-8 in an ASCII-compatible charset (a
/// mislabelled file: real ISO-8859 text is almost never valid UTF-8).
pub fn text_as_utf8<'a>(data: &'a [u8], charset: Option<&str>) -> std::borrow::Cow<'a, [u8]> {
    use std::borrow::Cow;
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(data) {
        return Cow::Owned(encoding.decode_without_bom_handling(&data[bom..]).0.into_owned().into_bytes());
    }
    if let Some(encoding) = charset
        .and_then(|c| encoding_rs::Encoding::for_label(c.trim().as_bytes()))
        .filter(|e| *e != encoding_rs::UTF_8 && *e != encoding_rs::REPLACEMENT)
        && !(encoding.is_ascii_compatible() && std::str::from_utf8(data).is_ok())
    {
        return Cow::Owned(encoding.decode_without_bom_handling(data).0.into_owned().into_bytes());
    }
    match std::str::from_utf8(data) {
        Ok(_) => Cow::Borrowed(data),
        Err(e) if !data[..e.valid_up_to()].is_ascii() => Cow::Owned(String::from_utf8_lossy(data).into_owned().into_bytes()),
        Err(_) => Cow::Owned(encoding_rs::WINDOWS_1252.decode_without_bom_handling(data).0.into_owned().into_bytes()),
    }
}

const TEXT_BUDGET: usize = 4 << 20;
/// Links inside links whose address is still compared with their text (see `html_to_text`).
const MAX_LINK_DEPTH: usize = 32;
/// A link's text longer than this is no address hidden behind other words: not compared.
const MAX_LINK_TEXT: usize = 2000;
/// Attachments listed for a message.
const MAX_LISTED_ATTACHMENTS: usize = 300;
/// Deepest quote or list level shown by [`html_to_text`]: HTML nesting has no limit, and a
/// prefix per level on every line made the text grow with depth times lines.
const MAX_DEPTH: usize = 8;
/// Most line breaks in a row: more blank lines say nothing more.
const MAX_BREAKS: usize = 4;

/// Output of [`html_to_text`]: line breaks are asked for, then written when text follows (none
/// at the start or end, the larger of two requests between blocks).
#[derive(Default)]
struct TextWriter {
    out: String,
    /// Line breaks owed before the next text.
    pending: usize,
    /// Quote depth of the blank lines among them.
    blank_quote: usize,
    /// Current quote depth.
    quote: usize,
    at_line_start: bool,
    /// Inside `<pre>`: spaces and line breaks of the text are kept.
    pre: bool,
}

impl TextWriter {
    fn ask(&mut self, n: usize, add: bool) {
        if self.out.is_empty() {
            return;
        }
        self.blank_quote = if self.pending == 0 { self.quote } else { self.blank_quote.min(self.quote) };
        self.pending = if add { self.pending + n } else { self.pending.max(n) }.min(MAX_BREAKS);
    }

    /// End of a block: `n` line breaks (2: a blank line).
    fn block(&mut self, n: usize) {
        self.ask(n, false);
    }

    /// `<br>`: each one is a line break of its own.
    fn line_break(&mut self) {
        if self.out.is_empty() {
            return;
        }
        self.ask(1, true);
    }

    fn flush(&mut self) {
        if self.pending == 0 {
            return;
        }
        let trimmed = self.out.trim_end_matches(' ').len();
        self.out.truncate(trimmed);
        // A blank line inside a quote is still part of it (not the one leaving or entering it).
        let blank_quote = self.blank_quote.min(self.quote).min(MAX_DEPTH);
        for i in 0..self.pending {
            self.out.push('\n');
            if i + 1 < self.pending && blank_quote > 0 {
                self.out.push_str(&">".repeat(blank_quote));
            }
        }
        self.pending = 0;
        self.at_line_start = true;
    }

    fn start_line(&mut self) {
        if self.at_line_start || self.out.is_empty() {
            self.out.push_str(&"> ".repeat(self.quote.min(MAX_DEPTH)));
            self.at_line_start = false;
        }
    }

    /// Text as written by the HTML serialiser (entities escaped).
    fn text(&mut self, escaped: &str) {
        let decoded = html_escape::decode_html_entities(escaped);
        if self.pre {
            for (i, line) in decoded.split('\n').enumerate() {
                if i > 0 {
                    self.ask(1, true);
                }
                if !line.is_empty() {
                    self.flush();
                    self.start_line();
                    self.out.push_str(&line.replace('\u{a0}', " "));
                }
            }
            return;
        }
        // HTML whitespace collapses; a non-breaking space is a real one.
        let mut collapsed = String::with_capacity(decoded.len());
        let mut space = false;
        for c in decoded.chars() {
            if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c') {
                space = true;
            } else {
                if space {
                    collapsed.push(' ');
                }
                space = false;
                collapsed.push(if c == '\u{a0}' { ' ' } else { c });
            }
        }
        if space {
            collapsed.push(' ');
        }
        let line_start = self.out.is_empty() || self.pending > 0 || self.at_line_start;
        let collapsed = if line_start { collapsed.trim_start_matches(' ') } else { collapsed.as_str() };
        if collapsed.is_empty() {
            return;
        }
        self.flush();
        self.start_line();
        if collapsed.starts_with(' ') && self.out.ends_with(' ') {
            self.out.push_str(&collapsed[1..]);
        } else {
            self.out.push_str(collapsed);
        }
    }

    fn raw(&mut self, s: &str) {
        self.flush();
        self.start_line();
        self.out.push_str(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfilled_template_tokens_stay_out_of_the_preview() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\n\r\n%opentrack%\r\n\r\nAcc\xc3\xa9dez aux informations d'arriv\xc3\xa9e\r\n{{ unsubscribe }}\r\n";
        assert_eq!(body_info(raw).preview, "Accédez aux informations d'arrivée");
        // A line saying more than a token stays.
        assert!(!template_token("Remise de 20% sur tout %"));
        assert!(!template_token("50%"));
        assert!(template_token(" *|FNAME|* "));
    }

    #[test]
    fn trust_reads_the_topmost_authentication_header() {
        // The provider adds its verdict on top; the one the sender wrote, lower down, is ignored.
        let raw = b"Authentication-Results: mx.fournisseur.fr; dkim=fail; spf=fail; dmarc=fail\r\n\
X-Spam-Flag: YES\r\n\
From: Patron <patron@banque.fr>\r\n\
Authentication-Results: faux; dkim=pass; spf=pass; dmarc=pass\r\n\
X-Spam-Flag: NO\r\n\
Subject: virement\r\n\r\ncorps";
        let s = parse_headers(raw, 0).signals;
        assert_eq!((s.dkim, s.spf, s.dmarc), (Some(false), Some(false), Some(false)));
        assert!(s.spam_flag);
    }

    #[test]
    fn smart_inbox_categories() {
        use crate::model::Category;
        let cat = |raw: &[u8]| parse_headers(raw, 0).category;
        assert_eq!(cat(b"From: Marie <marie@exemple.fr>\r\nSubject: Diner\r\n\r\nx"), Category::People);
        assert_eq!(cat(b"From: Boutique <infos@boutique.fr>\r\nList-Unsubscribe: <https://b.fr/u>\r\nSubject: Soldes\r\n\r\nx"), Category::Newsletter);
        assert_eq!(cat(b"From: Banque <no-reply@banque.fr>\r\nList-Unsubscribe: <https://b.fr/u>\r\nSubject: Code\r\n\r\nx"), Category::Notification);
        assert_eq!(cat(b"From: Robot <robot@site.fr>\r\nAuto-Submitted: auto-generated\r\nSubject: Alerte\r\n\r\nx"), Category::Notification);
        assert_eq!(cat(b"From: Journal <newsletter@journal.fr>\r\nSubject: Une\r\n\r\nx"), Category::Newsletter);
        // Whole words: people whose address contains « news » or « account ».
        assert_eq!(cat(b"From: Paul <paul.newsome@exemple.fr>\r\nSubject: Salut\r\n\r\nx"), Category::People);
        assert_eq!(cat(b"From: Cabinet <accountant@exemple.fr>\r\nSubject: Bilan\r\n\r\nx"), Category::People);
        assert_eq!(cat(b"From: DGFiP <ne-pas-repondre@dgfip.fr>\r\nSubject: Avis\r\n\r\nx"), Category::Notification);
        assert_eq!(cat(b"From: Site <alerts-noreply@site.fr>\r\nSubject: Alerte\r\n\r\nx"), Category::Notification);
        assert!(parse_headers(b"From: DGFiP <ne-pas-repondre@dgfip.fr>\r\nSubject: Avis\r\n\r\nx", 0).bulk, "not a person");
    }

    #[test]
    fn data_links_are_dropped_but_data_images_kept() {
        let (html, _) = sanitize_html(r#"<a href="data:text/html,<script>x</script>">x</a><img src="data:image/png;base64,AAAA">"#, HashMap::new());
        assert!(!html.contains("data:text"));
        assert!(html.contains("data:image/png"));
    }

    const MAIL: &[u8] = b"From: =?utf-8?q?Fran=C3=A7oise?= <f@example.org>\r\n\
To: yann@example.org\r\n\
Subject: =?utf-8?q?R=C3=A9union?=\r\n\
Message-ID: <b@example.org>\r\n\
In-Reply-To: <a@example.org>\r\n\
References: <root@example.org> <a@example.org>\r\n\
Date: Thu, 15 Oct 2026 10:00:00 +0200\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p onclick=\"x()\">Bonjour <script>alert(1)</script><img src=\"https://track.example/p.gif\"></p>\r\n";

    #[test]
    fn headers() {
        let h = parse_headers(MAIL, 0);
        assert_eq!(h.subject, "Réunion");
        assert_eq!(h.from.unwrap().name.as_deref(), Some("Françoise"));
        assert_eq!(h.message_id.as_deref(), Some("b@example.org"));
        assert_eq!(h.in_reply_to.as_deref(), Some("a@example.org"));
        assert_eq!(h.references, ["root@example.org", "a@example.org"]);
        assert!(h.date > 0);
    }

    #[test]
    fn newsletters_are_recognised_and_offer_unsubscribe() {
        let headers = "From: Promo <news@shop.example>\r\nSubject: Soldes\r\n\
List-Unsubscribe: <mailto:unsub@shop.example?subject=stop>,\r\n <https://shop.example/u/42>\r\n\
List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n\r\nBonjour\r\n";
        let news = headers.as_bytes();
        assert!(parse_headers(news, 0).bulk);
        // No verified DKIM signature over the unsubscribe headers: no one-click POST (RFC 8058).
        let u = parse_body(news).unwrap().unsubscribe.unwrap();
        assert_eq!(u.one_click, None);
        assert_eq!(u.web.as_deref(), Some("https://shop.example/u/42"));
        assert_eq!(u.mailto.as_deref(), Some("mailto:unsub@shop.example?subject=stop"));

        let signed = |results: &str, h: &str| {
            let raw = format!(
                "Authentication-Results: {results}\r\nDKIM-Signature: v=1; a=rsa-sha256; d=shop.example; s=k1;\r\n\th=from:subject:{h};\r\n\tbh=x; b=ABCDEFGH\r\n IJKL\r\n{headers}"
            );
            parse_body(raw.as_bytes()).unwrap().unsubscribe.unwrap().one_click
        };
        let both = "list-unsubscribe:List-Unsubscribe-Post";
        assert_eq!(signed("mx.p.fr; dkim=pass header.d=shop.example header.s=k1 header.b=ABCDEFGH", both).as_deref(), Some("https://shop.example/u/42"));
        assert_eq!(signed("mx.p.fr; dkim=pass", both).as_deref(), Some("https://shop.example/u/42"), "single signature");
        assert_eq!(signed("mx.p.fr; dkim=pass header.d=shop.example", "list-unsubscribe"), None, "post header not signed");
        assert_eq!(signed("mx.p.fr; dkim=fail header.d=shop.example", both), None);
        assert_eq!(signed("mx.p.fr; dkim=pass header.d=other.example", both), None, "another signature passed");
        assert_eq!(signed("mx.p.fr; dkim=pass header.d=shop.example header.b=ZZZZZZZZ", both), None);

        let robot = b"From: GitHub <noreply@github.com>\r\nSubject: x\r\n\r\n.\r\n";
        assert!(parse_headers(robot, 0).bulk);
        assert!(!parse_headers(MAIL, 0).bulk, "a person");
        assert!(parse_body(MAIL).unwrap().unsubscribe.is_none());
    }

        #[test]
    fn trust_signals_from_headers() {
        let phishing = b"Authentication-Results: mx.provider.fr; spf=fail smtp.mailfrom=x@arnaque.example; dmarc=fail\r\n\
From: \"service@banque.fr\" <x@arnaque.example>\r\nReply-To: <collecte@ailleurs.example>\r\nX-Spam-Flag: YES\r\n\
Subject: Votre compte\r\n\r\n.\r\n";
        let s = parse_headers(phishing, 0).signals;
        assert_eq!((s.spf, s.dmarc), (Some(false), Some(false)));
        assert!(s.spoofed_name && s.reply_to_elsewhere && s.spam_flag);
        assert!(!parse_headers(MAIL, 0).signals.spoofed_name);
        let idn = b"From: <x@xn--pypal-4ve.com>\r\n\r\n.\r\n";
        assert!(parse_headers(idn, 0).signals.lookalike_domain);
    }

    #[test]
    fn link_domains_are_counted_per_registrable_domain() {
        let html = r#"<a href="https://click.mail.airbnb.com/x?u=1">A</a> <a href="HTTPS://www.airbnb.com/">B</a>
            voir http://exemple.co.uk/page et <img src="https://cdn.tracker.example/p.gif">"#;
        assert_eq!(
            link_domains(html),
            [("airbnb.com".to_string(), 2), ("exemple.co.uk".to_string(), 1), ("tracker.example".to_string(), 1)]
        );
        let list = b"List-Id: Drizzle <drizzle-team.github.com>\r\nFrom: a@b.c\r\n\r\n.\r\n";
        assert_eq!(parse_headers(list, 0).list_id.as_deref(), Some("drizzle-team.github.com"));
    }

    #[test]
    fn repeated_inline_images_are_capped() {
        let data = format!("data:image/png;base64,{}", "A".repeat(100 * 1024));
        let cids = HashMap::from([("a@x".to_string(), data)]);
        let html = r#"<img src="cid:a@x" alt="logo">"#.repeat(50_000);
        let (clean, _) = sanitize_html(&html, cids);
        assert!(clean.len() < MAX_INLINE_TOTAL + 2 * 1024 * 1024, "{} bytes", clean.len());
        let inlined = clean.matches("data:image/png").count();
        assert!(inlined > 0 && inlined <= MAX_INLINE_COUNT, "{inlined}");
        // Beyond the budget: an image without source, never a remote fallback.
        assert!(clean.ends_with(r#"<img alt="logo">"#));
        // URL-encoded and upper-case `cid:` still resolve.
        let cids = HashMap::from([("b c@x".to_string(), "data:image/gif;base64,R0".to_string())]);
        let (clean, _) = sanitize_html(r#"<img src="CID:b%20c@x">"#, cids);
        assert!(clean.contains("data:image/gif"), "{clean}");
    }

    #[test]
    fn cid_references_are_collected_in_one_pass() {
        let refs = cid_refs("<img src=\"cid:a@x\"><img src='CID:b%20c@x'><img src=cid:d@x>\n<td style=\"background:url(cid:e@x)\">cid:f@x");
        for id in ["a@x", "b%20c@x", "b c@x", "d@x", "e@x", "f@x"] {
            assert!(refs.contains(id), "{id}: {refs:?}");
        }
        assert!(!refs.contains("a@x\""));
        // Back-to-back schemes or an unterminated id stay linear and never split a character.
        let schemes = "cid:".repeat(100_000);
        assert!(cid_refs(&schemes).iter().map(String::len).sum::<usize>() <= schemes.len());
        let long = format!("cid:{}", "\u{e9}".repeat(MAX_CID_LEN));
        assert!(cid_refs(&long).iter().all(|id| id.len() <= MAX_CID_LEN));
        // Thousands of tiny images the HTML does not show: no part is inlined, and the ones it
        // shows stop at the count cap.
        let mut raw = String::from("From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"r\"\r\n\r\n--r\r\nContent-Type: text/html\r\n\r\n");
        for i in 0..MAX_INLINE_COUNT + 10 {
            raw += &format!("<img src=\"cid:s{i}@x\">");
        }
        raw += "\r\n";
        for i in 0..MAX_INLINE_COUNT + 10 {
            raw += &format!("--r\r\nContent-Type: image/gif\r\nContent-ID: <s{i}@x>\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGOA==\r\n");
            raw += &format!("--r\r\nContent-Type: image/gif\r\nContent-ID: <h{i}@x>\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGOA==\r\n");
        }
        raw += "--r--\r\n";
        let msg = MessageParser::default().parse(raw.as_bytes()).unwrap();
        let html = html_body(&msg);
        let parts = inline_parts(&msg, html.as_deref());
        assert_eq!(parts.len(), MAX_INLINE_COUNT);
        assert!(parts.iter().all(|(cid, _)| cid.starts_with('s')));
    }

    #[test]
    fn remote_content_is_detected_however_it_is_written() {
        let remote = |html: &str| sanitize_html(html, HashMap::new()).1;
        assert!(remote(r#"<img src="  HTTPS://Tracker.Example/p.gif">"#));
        assert!(remote(r#"<img src="//tracker.example/p.gif">"#));
        assert!(remote(r#"<div style="background: URL( 'https://t.example/p' )">x</div>"#));
        assert!(remote(r#"<div style="background: u\72l(//t.example/p)">x</div>"#));
        assert!(remote("<style>DIV { background: Url(https://t.example/p) }</style><p>x</p>"));
        assert!(remote("<style>@IMPORT 'https://t.example/a.css';</style><p>x</p>"));
        assert!(remote(r#"<div style="background-image: image-set('https://t.example/a.png' 1x)">x</div>"#));
        assert!(!remote(r#"<div style="background:url(data:image/png;base64,AA)">x</div><img src="data:image/png;base64,AA">"#));
        assert!(!remote(r#"<a href="HTTPS://example.com/">lien</a><p>url(https://dans.le/texte)</p>"#));

        // Rewritten to what the browser would load, so the CSP and the UI see the real URL.
        let (html, _) = sanitize_html(r#"<img src=" HTTPS://Tracker.Example/p.gif"><img src="//t.example/q"><img src="local.png">"#, HashMap::new());
        assert!(html.contains(r#"src="https://tracker.example/p.gif""#), "{html}");
        assert!(html.contains(r#"src="https://t.example/q""#), "{html}");
        assert!(!html.contains("local.png"), "{html}");
        // A scheme broken up by a tab is still `data:` to the browser.
        let (html, _) = sanitize_html("<a href=\"da\tta:text/html,<script>x</script>\">x</a>", HashMap::new());
        assert!(!html.contains("href"), "{html}");
    }

    #[test]
    fn remote_content_is_listed_by_host() {
        let hosts = |html: &str| {
            let (_, remote, _) = sanitize_html_inlining(html, HashMap::new());
            (remote.hosts.into_iter().collect::<Vec<_>>(), remote.unlisted)
        };
        assert_eq!(
            hosts(r#"<img src="https://windu.my.Salesforce.com/p.png"><img src="//cdn.example/a.gif"><div style="background: url('https://img.example/b.jpg')">x</div>"#),
            (vec!["cdn.example".to_string(), "img.example".to_string(), "windu.my.salesforce.com".to_string()], false)
        );
        assert_eq!(hosts("<style>p { background: URL(https://s.example/c.png) }</style><p>x</p>"), (vec!["s.example".to_string()], false));
        assert_eq!(hosts(r#"<a href="https://link.example/">lien</a><img src="data:image/png;base64,AA">"#), (vec![], false));
        // What cannot be read to a host stays blocked: never counted as approved.
        assert!(hosts("<style>@import 'https://t.example/a.css';</style><p>x</p>").1);
        assert!(hosts(r#"<div style="background: url(relative.png)">x</div>"#).1);
    }

    #[test]
    fn link_counting_is_bounded() {
        let many: String = (0..300).map(|i| format!("https://d{i}.example/ ")).collect::<String>() + &"https://a.example/ ".repeat(20_000);
        let domains = link_domains(&many);
        assert_eq!(domains.len(), MAX_LINK_DOMAINS);
        assert!(domains.iter().map(|(_, n)| n).sum::<usize>() <= MAX_LINKS);
        // Private suffixes: every customer is a domain of its own.
        assert_eq!(link_domains("https://a.evil.pages.dev/ https://b.pages.dev/").len(), 2);
    }

    #[test]
    fn utf8_sender_domains_are_lookalikes() {
        assert!(lookalike("pаypal.com"), "Cyrillic а");
        assert!(lookalike("ｐaypal.com"), "fullwidth p, mapped onto paypal.com by IDNA");
        assert!(lookalike("xn--pypal-4ve.com"));
        assert!(!lookalike("paypal.com"));
        assert!(!lookalike(""));
        let raw = "From: <x@pаypal.com>\r\nSubject: compte\r\n\r\n.\r\n";
        assert!(parse_headers(raw.as_bytes(), 0).signals.lookalike_domain);
    }

    #[test]
    fn sanitizes() {
        let b = parse_body(MAIL).unwrap();
        assert!(!b.html.contains("script"));
        assert!(!b.html.contains("onclick"));
        assert!(b.html.contains("Bonjour"));
        assert!(b.has_remote_content);
        assert_eq!(body_info(MAIL).preview, "Bonjour");
    }

    #[test]
    fn html_to_text_keeps_the_layout() {
        let html = "<p>Bonjour Marie,</p><p>Voici <b>le</b> point&nbsp;:<br>ligne deux</p>\
            <ul><li>un</li><li>deux<ul><li>sous</li></ul></li></ul><ol><li>premier</li><li>second</li></ol>\
            <div>Gmail</div><div><br></div><div>style</div>\
            <p>Le 3 mars, Paul a écrit :</p><blockquote><p>Salut</p><p>Ça va ?</p><blockquote>plus ancien</blockquote></blockquote>\
            <p>Voir <a href=\"https://exemple.fr/doc?a=1&amp;b=2\">le document</a> ou https://exemple.fr</p>\
            <p><a href=\"https://exemple.fr\">https://exemple.fr</a></p><style>p { color: red }</style>";
        let text = html_to_text(html);
        assert_eq!(
            text,
            "Bonjour Marie,\n\nVoici le point :\nligne deux\n\n- un\n- deux\n  - sous\n\n1. premier\n2. second\n\n\
             Gmail\n\nstyle\n\nLe 3 mars, Paul a écrit :\n\n> Salut\n>\n> Ça va ?\n>\n> > plus ancien\n\n\
             Voir le document <https://exemple.fr/doc?a=1&b=2> ou https://exemple.fr\n\nhttps://exemple.fr"
        );
        // Plain text stays as it is; entities are decoded once.
        assert_eq!(html_to_text("a &amp;lt; b"), "a &lt; b");
        assert_eq!(html_to_text("<pre>  x\n    y</pre>"), "  x\n    y");
        // The preview and the search index see words apart, and no quoted text in the preview.
        let raw = format!("From: a@b.c\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n");
        let info = body_info(raw.as_bytes());
        assert!(info.preview.starts_with("Bonjour Marie, Voici le point : ligne deux - un"), "{}", info.preview);
        assert!(!info.preview.contains("plus ancien"));
        assert!(info.text.contains("ligne deux"));
    }

    #[test]
    fn a_charset_given_only_in_the_html_or_not_at_all_is_read() {
        let latin = |body: &[u8], ctype: &str| {
            let mut raw = format!("From: a@example.org\r\nSubject: x\r\nMIME-Version: 1.0\r\nContent-Type: {ctype}\r\nContent-Transfer-Encoding: 8bit\r\n\r\n").into_bytes();
            raw.extend_from_slice(body);
            raw
        };
        let html = latin(b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=iso-8859-1\"></head><body><p>\xc9t\xe9 \xe0 Paris</p></body></html>", "text/html");
        let info = body_info(&html);
        assert!(info.text.contains("Été à Paris"), "{:?}", info.text);
        let plain = latin(b"D\xe9j\xe0 vu", "text/plain");
        assert!(body_info(&plain).text.contains("Déjà vu"));
        // Declared, it is left to mail-parser.
        let utf8 = latin("Déjà vu".as_bytes(), "text/plain; charset=utf-8");
        assert!(body_info(&utf8).text.contains("Déjà vu"));
        // Named but unknown to it: read as if none were named.
        let unknown = latin(b"D\xe9j\xe0 vu", "text/plain; charset=unknown-8bit");
        assert!(body_info(&unknown).text.contains("Déjà vu"));
        // A blank text part beside the HTML: the HTML is the text.
        let raw = b"From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\n \r\n\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>Votre commande est partie</p>\r\n--b--\r\n";
        assert!(body_info(raw).text.contains("commande est partie"));
    }

    #[test]
    fn crafted_formatting_tags_do_not_blow_up_the_parser() {
        let html = format!("{}{}", (0..5000).map(|i| format!("<b id={i}>")).collect::<String>(), "<p>x".repeat(20_000));
        let started = std::time::Instant::now();
        let text = html_to_text(&html);
        let (shown, _) = sanitize_html(&html, HashMap::new());
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(text.contains('x') && shown.len() < 4 << 20, "{}", shown.len());
        // An ordinary message keeps its formatting.
        assert_eq!(defused("<b>gras</b> <a href=x>lien</a> <br>"), "<b>gras</b> <a href=x>lien</a> <br>");
        assert!(!defused(&html).contains("<b"));
        // Fewer left open, inside the first paragraph, before many more: still the same blowup.
        let inside = format!("<p>{}{}", (0..999).map(|i| format!("<b id={i}>")).collect::<String>(), "<p>x".repeat(22_000));
        let started = std::time::Instant::now();
        let text = html_to_text(&inside);
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
        assert!(text.contains('x'));
        // A long newsletter that closes its tags keeps them.
        let closed = "<p><b>Titre</b> <a href=\"https://x.example\">lien</a></p>".repeat(5000);
        assert!(matches!(defused(&closed), std::borrow::Cow::Borrowed(_)));
        // Formatting tags never closed by `>`: one pass, not one per tag.
        let open = format!("{}{}", "<b>".repeat(1001), "<b ".repeat(1_000_000));
        let started = std::time::Instant::now();
        defused(&open);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn closing_tags_hidden_in_attributes_or_comments_are_not_counted() {
        for open in ["<b id={i} title=\"</b>\">", "<b id={i}><!--</b>-->", "<b id={i} title=</b>"] {
            let html = format!("{}{}", (0..5000).map(|i| open.replace("{i}", &i.to_string())).collect::<String>(), "<p>x".repeat(20_000));
            assert!(!defused(&html).contains("<b "), "{open}");
            let started = std::time::Instant::now();
            assert!(html_to_text(&html).contains('x'));
            assert!(started.elapsed() < std::time::Duration::from_secs(10), "{open}: {:?}", started.elapsed());
        }
        // A closing tag inside a style or a title is text too.
        let styled = format!("{}{}", (0..5000).map(|i| format!("<b id={i}><style></b></style>")).collect::<String>(), "<p>x".repeat(20_000));
        assert!(!defused(&styled).contains("<b "));
    }

    #[test]
    fn unclosed_identical_fonts_keep_their_formatting() {
        let report = "<p><font face=\"Arial\" size=2>ligne <a href=\"https://x.example\">lien</a></p>".repeat(1000);
        assert!(matches!(defused(&report), std::borrow::Cow::Borrowed(_)), "the parser keeps three of a kind at most");
        assert!(html_to_text(&report).contains("https://x.example"));
    }

    #[test]
    fn tag_names_are_read_once() {
        let html = "<x".repeat(1_000_000);
        let started = std::time::Instant::now();
        defused(&html);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    }

    #[test]
    fn deep_nesting_of_ordinary_tags_stays_quick() {
        let divs = format!("{}x", "<div>".repeat(200_000));
        let spans = format!("{}x{}", "<span>".repeat(100_000), "</q>".repeat(100_000));
        for html in [divs, spans] {
            let started = std::time::Instant::now();
            assert!(html_to_text(&html).contains('x'));
            let (shown, _) = sanitize_html(&html, HashMap::new());
            assert!(shown.contains('x'));
            assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
        }
        // Ordinary mail is left as it is.
        let mail = "<div><table><tr><td><p>a<p>b</td></tr></table><span>c</span></div><br></p>".repeat(2000);
        assert!(matches!(defused(&mail), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn html_to_text_stays_bounded_on_deep_nesting() {
        // A prefix per quote level on every line, and a `>` per pending blank line, used to give
        // gigabytes from half a megabyte of HTML.
        let html = format!("x{}{}{}", "<blockquote>".repeat(10_000), "<br>y".repeat(100_000), "<br>".repeat(100_000));
        let text = html_to_text(&html);
        assert!(text.len() < 8 << 20, "{}", text.len());
        assert!(text.contains("> > > > > > > > y\n"));
        assert!(!text.contains("> > > > > > > > > "));
        let lists = format!("{}z", "<ul><li>".repeat(10_000));
        let text = html_to_text(&lists);
        assert!(text.len() < 1 << 20, "{}", text.len());
        assert!(text.ends_with(&format!("{}- z", "  ".repeat(MAX_DEPTH))));
        // Runs of line breaks are capped too.
        assert_eq!(html_to_text(&format!("a{}b", "<br>".repeat(1000))), "a\n\n\n\nb");
    }

    #[test]
    fn nested_links_stay_quick() {
        let html = format!("{}fin", r#"<a href="https://x.example/00001"><object>"#.repeat(20_000));
        let started = std::time::Instant::now();
        let text = html_to_text(&html);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(text.contains("fin"));
        // A plain link keeps its address after its words.
        assert_eq!(html_to_text(r#"<a href="https://x.example/a">ici</a>"#), "ici <https://x.example/a>");
    }

    #[test]
    fn a_link_after_trailing_spaces_in_pre_does_not_underflow() {
        // The line break inside the link trims the spaces written before it opened.
        let text = html_to_text("<pre>a          <a href=\"https://x.example\">\n </a></pre>");
        assert!(text.starts_with('a'), "{text:?}");
    }

    #[test]
    fn an_image_shown_without_a_disposition_is_no_attachment() {
        let pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let raw = format!(
            "From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"r\"\r\n\r\n\
--r\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Logo <img src=\"cid:logo@x\"></p>\r\n\
--r\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\nContent-Transfer-Encoding: base64\r\n\r\n{pixel}\r\n--r--\r\n"
        );
        assert!(!body_info(raw.as_bytes()).has_attachments, "Python's add_related writes no disposition");
        assert!(parse_body(raw.as_bytes()).unwrap().attachments.is_empty());
    }

    #[test]
    fn text_attachments_are_shown_as_utf8() {
        assert_eq!(&*text_as_utf8("déjà".as_bytes(), None), "déjà".as_bytes());
        // Notepad's Windows-1252, and a UTF-16 file with its byte order mark.
        assert_eq!(&*text_as_utf8(b"caf\xe9 \x80", None), "café €".as_bytes());
        assert_eq!(&*text_as_utf8(b"\xff\xfeh\x00\xe9\x00", None), "hé".as_bytes());
        assert_eq!(&*text_as_utf8(b"\xef\xbb\xbfok", None), b"ok");
        // UTF-8 with one broken byte stays UTF-8.
        assert_eq!(&*text_as_utf8(b"d\xc3\xa9j\xe0", None), "déj\u{fffd}".as_bytes());
    }

    #[test]
    fn reopened_draft_keeps_attachments_with_a_content_id() {
        let pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        // An image shown in the HTML, a PDF with a Content-ID (Apple Mail), and an image the HTML
        // does not show.
        let raw = format!(
            "From: Y <y@example.org>\r\nTo: b@example.org\r\nSubject: Pièces\r\nMessage-ID: <d@example.org>\r\n\
In-Reply-To: <parent@x>\r\nReferences: <root@x> <mid@x> <parent@x>\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"m\"\r\n\r\n\
--m\r\nContent-Type: multipart/related; boundary=\"r\"\r\n\r\n\
--r\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Voir <img src=\"cid:img1@facteur\"> et <img src=\"cid:doc@apple\"></p>\r\n\
--r\r\nContent-Type: image/png\r\nContent-ID: <img1@facteur>\r\nContent-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\n{pixel}\r\n\
--r--\r\n\
--m\r\nContent-Type: application/pdf; name=\"devis.pdf\"\r\nContent-ID: <doc@apple>\r\nContent-Disposition: inline; filename=\"devis.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQ=\r\n\
--m\r\nContent-Type: image/png; name=\"photo.png\"\r\nContent-ID: <unused@x>\r\nContent-Disposition: attachment; filename=\"photo.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{pixel}\r\n\
--m--\r\n"
        );
        let parts = parse_draft(raw.as_bytes()).unwrap();
        let names: Vec<&str> = parts.attachments.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["devis.pdf", "photo.png"]);
        assert!(parts.html.unwrap().contains("data:image/png;base64,"), "the shown image stays in the HTML");
        assert_eq!(parts.in_reply_to.as_deref(), Some("parent@x"));
        assert_eq!(parts.references, ["root@x", "mid@x"]);
    }

    #[test]
    fn inline_images_dropped_by_the_budgets_stay_attachments() {
        // Three 6 MB photos shown in the HTML (Apple Mail): only two fit the 20 MB of data URLs.
        let photo = B64.encode(vec![0u8; 6 * 1024 * 1024]);
        let mut raw = String::from(
            "From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"r\"\r\n\r\n\
--r\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p><img src=\"cid:p1@x\"><img src=\"cid:p2@x\"><img src=\"cid:p3@x\"><img src=\"cid:doc@x\"></p>\r\n",
        );
        for i in 1..=3 {
            raw += &format!(
                "--r\r\nContent-Type: image/jpeg; name=\"p{i}.jpg\"\r\nContent-ID: <p{i}@x>\r\nContent-Disposition: inline; filename=\"p{i}.jpg\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{photo}\r\n"
            );
        }
        // A PDF referenced by `cid:` is no image: it stays an attachment.
        raw += "--r\r\nContent-Type: application/pdf; name=\"doc.pdf\"\r\nContent-ID: <doc@x>\r\nContent-Disposition: inline; filename=\"doc.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--r--\r\n";
        let body = parse_body(raw.as_bytes()).unwrap();
        let names: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["p3.jpg", "doc.pdf"]);
        assert_eq!(body.html.matches("data:image/jpeg;base64,").count(), 2);
        assert!(body_info(raw.as_bytes()).has_attachments);
    }

    #[test]
    fn flowed_text_is_unwrapped() {
        // Thunderbird: soft breaks joined, the quote kept apart, the signature on its own line.
        let flowed = "Bonjour,\r\n\r\nVoici un paragraphe assez long pour \r\n\u{20}passer sur deux lignes.\r\n\r\n> Le message \r\n> cit\u{e9}.\r\n>> Plus \r\n>> ancien.\r\n\r\n--\u{20}\r\nYann\r\n";
        assert_eq!(
            unflow(flowed, false),
            "Bonjour,\n\nVoici un paragraphe assez long pour passer sur deux lignes.\n\n> Le message cit\u{e9}.\n>> Plus ancien.\n\n-- \nYann"
        );
        // delsp=yes: the space at the break was added, the word is whole again.
        assert_eq!(unflow("Merci pour l'informa \r\ntion.\r\n", true), "Merci pour l'information.");
        // Read through the part's Content-Type, by the reader and the index alike.
        let raw = "From: a@example.org\r\nContent-Type: text/plain; charset=utf-8; format=flowed; delsp=yes\r\n\r\nl'informa \r\ntion du jour\r\n";
        let body = parse_body(raw.as_bytes()).unwrap();
        assert_eq!(body.text.as_deref(), Some("l'information du jour"));
        assert!(body_info(raw.as_bytes()).text.contains("l'information du jour"));
        // Not flowed: the line breaks are the sender's.
        let fixed = raw.replace("; format=flowed; delsp=yes", "");
        assert!(parse_body(fixed.as_bytes()).unwrap().text.is_some_and(|t| t.contains("l'informa \ntion") || t.contains("l'informa \r\ntion")));
    }

    #[test]
    fn list_flag_and_reader_agree_on_inline_images() {
        // An Outlook background (`background=` is stripped, a CSS `url()` is not substituted): an
        // attachment in the reader, so in the list too. The `<img>` one is shown in both.
        let raw = "From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"r\"\r\n\r\n\
--r\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<table><tr><td background=\"cid:bg@x\" style=\"background:url(cid:bg@x)\"><img src=\"cid:logo@x\"></td></tr></table>\r\n\
--r\r\nContent-Type: image/png; name=\"bg.png\"\r\nContent-ID: <bg@x>\r\nContent-Disposition: inline; filename=\"bg.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n\
--r\r\nContent-Type: image/png; name=\"logo.png\"\r\nContent-ID: <logo@x>\r\nContent-Disposition: inline; filename=\"logo.png\"\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--r--\r\n";
        let body = parse_body(raw.as_bytes()).unwrap();
        let names: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["bg.png"]);
        assert!(body_info(raw.as_bytes()).has_attachments);
        let shown_only = raw.replace("background=\"cid:bg@x\" style=\"background:url(cid:bg@x)\"", "");
        assert!(parse_body(shown_only.as_bytes()).unwrap().attachments.iter().all(|a| a.filename != "logo.png"));
    }

    #[test]
    fn a_reopened_draft_keeps_its_text_attachments_as_they_were() {
        // A CSV in Windows-1252 with no charset said: 0xE9 is "é", not UTF-8.
        let csv: &[u8] = b"nom;ville\r\nAndr\xe9;Lyon\r\n";
        let mut raw = b"From: a@example.org\r\nSubject: Fichier\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n\
--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCi-joint.\r\n\
--m\r\nContent-Type: text/csv; name=\"liste.csv\"\r\nContent-Disposition: attachment; filename=\"liste.csv\"\r\n\r\n"
            .to_vec();
        raw.extend_from_slice(csv);
        raw.extend_from_slice(b"\r\n--m--\r\n");
        let draft = parse_draft(&raw).unwrap();
        let file = B64.decode(&draft.attachments[0].data).unwrap();
        assert_eq!(file.strip_suffix(b"\r\n").unwrap_or(&file), csv.strip_suffix(b"\r\n").unwrap());
        assert!(draft.attachments[0].mime.starts_with("text/csv"));
    }

    #[test]
    fn text_after_an_inline_attachment_is_kept() {
        // Apple Mail: a PDF in the middle of an HTML message, the text going on below it.
        let apple = "From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"a\"\r\n\r\n\
--a\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee the quote.\r\nThe price is 40 euros.\r\n\
--a\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n\
--m\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>See the quote.</p>\r\n\
--m\r\nContent-Type: application/pdf; name=\"quote.pdf\"\r\nContent-Disposition: inline; filename=\"quote.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n\
--m\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>The price is 40 euros.</p></body></html>\r\n\
--m--\r\n--a--\r\n";
        let body = parse_body(apple.as_bytes()).unwrap();
        assert!(!body.is_plain);
        assert!(body.html.contains("See the quote.") && body.html.contains("The price is 40 euros."), "{}", body.html);
        let names: Vec<&str> = body.attachments.iter().map(|a| a.filename.as_str()).collect();
        assert_eq!(names, ["quote.pdf"]);
        assert!(body_info(apple.as_bytes()).text.contains("The price is 40 euros."));

        // iOS: text, an inline photo, more text.
        let ios = "From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n\
--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHere is the photo:\r\n\
--m\r\nContent-Type: image/jpeg; name=\"photo.jpg\"\r\nContent-ID: <photo@x>\r\nContent-Disposition: inline; filename=\"photo.jpg\"\r\nContent-Transfer-Encoding: base64\r\n\r\n/9j/4AAQ\r\n\
--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe code is 1234.\r\n--m--\r\n";
        let body = parse_body(ios.as_bytes()).unwrap();
        assert!(body.is_plain);
        assert!(body.html.contains("Here is the photo:") && body.html.contains("The code is 1234."), "{}", body.html);
        assert!(body.text.as_deref().is_some_and(|t| t.contains("The code is 1234.")));
        assert_eq!(body.attachments.len(), 1);
        let info = body_info(ios.as_bytes());
        assert!(info.text.contains("Here is the photo:") && info.text.contains("The code is 1234."), "{}", info.text);
        assert!(info.preview.contains("The code is 1234."));
        // The same, reopened as a draft: nothing after the photo is lost.
        let draft = parse_draft(ios.as_bytes()).unwrap();
        assert!(draft.body.contains("Here is the photo:") && draft.body.contains("The code is 1234."), "{}", draft.body);
        let draft = parse_draft(apple.as_bytes()).unwrap();
        assert!(draft.html.as_deref().is_some_and(|h| h.contains("The price is 40 euros.")), "{:?}", draft.html);

        // The same photo between two HTML parts is shown where it was, not as an attachment.
        let html = ios.replace("text/plain", "text/html");
        let body = parse_body(html.as_bytes()).unwrap();
        assert!(body.html.contains("data:image/jpeg;base64,") && body.html.contains("The code is 1234."), "{}", body.html);
        assert!(body.attachments.is_empty());
        assert!(!body_info(html.as_bytes()).has_attachments);
    }

    #[test]
    fn text_attachments_are_saved_as_sent() {
        // A CSV from Excel on a French Windows (cp1252), with and without its charset, base64,
        // quoted-printable and 8bit, and a UTF-8 text attachment.
        let csv: &[u8] = b"nom;ville\r\nC\xe9line;Besan\xe7on\r\n";
        let raw = [
            b"From: a@example.org\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nCi-joint.\r\n".to_vec(),
            format!("--m\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=\"a.csv\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", B64.encode(csv)).into_bytes(),
            b"--m\r\nContent-Type: text/csv; charset=windows-1252\r\nContent-Disposition: attachment; filename=\"b.csv\"\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nnom;ville\r\nC=E9line;Besan=E7on\r\n\r\n".to_vec(),
            b"--m\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=\"c.csv\"\r\nContent-Transfer-Encoding: 8bit\r\n\r\n".to_vec(),
            csv.to_vec(),
            "\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Disposition: attachment; filename=\"d.txt\"\r\n\r\nÇa va ?\r\n--m--\r\n".as_bytes().to_vec(),
        ]
        .concat();
        for (index, declared) in ["text/csv", "text/csv; charset=windows-1252", "text/csv"].into_iter().enumerate() {
            let (_, mime, data) = attachment(&raw, index).unwrap();
            assert_eq!(mime, declared);
            assert_eq!(data, csv, "attachment {index}");
        }
        let (name, mime, data) = attachment(&raw, 3).unwrap();
        assert_eq!((name.as_str(), mime.as_str(), data.as_slice()), ("d.txt", "text/plain; charset=utf-8", "Ça va ?".as_bytes()));

        // Polish notes in ISO-8859-2: shown through their declared charset, not as cp1252.
        let notes: &[u8] = b"Za\xbf\xf3\xb3\xe6";
        let raw = [
            b"From: a@example.org\r\nContent-Type: multipart/mixed; boundary=\"m\"\r\n\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nNotatki.\r\n".to_vec(),
            b"--m\r\nContent-Type: text/plain; charset=\"ISO-8859-2\"\r\nContent-Disposition: attachment; filename=\"notes.txt\"\r\nContent-Transfer-Encoding: 8bit\r\n\r\n".to_vec(),
            notes.to_vec(),
            b"\r\n--m--\r\n".to_vec(),
        ]
        .concat();
        let (_, mime, data) = attachment(&raw, 0).unwrap();
        assert_eq!((mime.as_str(), data.as_slice()), ("text/plain; charset=ISO-8859-2", notes));
        assert_eq!(mime_charset(&mime), Some("ISO-8859-2"));
        // A forwarded copy keeps its label (smtp::build falls back to octet-stream otherwise).
        assert!(lettre::message::header::ContentType::parse(&mime).is_ok());
        assert_eq!(&*text_as_utf8(&data, mime_charset(&mime)), "Zażółć".as_bytes());
        // A mislabelled file that is valid UTF-8 stays as it is.
        assert_eq!(&*text_as_utf8("Zażółć".as_bytes(), Some("iso-8859-2")), "Zażółć".as_bytes());
    }
}
