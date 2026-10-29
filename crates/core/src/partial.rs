//! A large message opened without its attachments: its structure first (BODYSTRUCTURE), then
//! only the parts it shows (text, HTML, embedded images, an invitation). The attachments keep
//! their own MIME headers with an empty body, so they are listed in the same order, at the same
//! indexes, as in the whole message downloaded afterwards.

use async_imap::imap_proto::{BodyStructure, ContentEncoding};
use std::collections::HashMap;

/// Below this, the whole message is downloaded at once.
pub const PARTIAL_FROM: u32 = 1 << 20;
/// A part this small is fetched with the text, whatever it is.
const SMALL_PART: u32 = 256 << 10;
/// Marks an attachment left on the server; its value is the decoded size.
pub const DEFERRED_HEADER: &str = "X-Facteur-Deferred";

#[derive(Debug, PartialEq)]
enum Node {
    Leaf { section: String, defer: bool, size: usize },
    Multi { subtype: String, children: Vec<Node> },
}

#[derive(Debug)]
pub struct Plan {
    root: Node,
}

impl Plan {
    /// The FETCH items: every part's MIME header, and the bodies of the parts shown.
    pub fn query(&self) -> String {
        let mut items = vec!["UID".to_string(), "BODY.PEEK[HEADER]".to_string()];
        leaves(&self.root, &mut |section, defer| {
            items.push(format!("BODY.PEEK[{section}.MIME]"));
            if !defer {
                items.push(format!("BODY.PEEK[{section}]"));
            }
        });
        format!("({})", items.join(" "))
    }

    pub fn fetched(&self) -> Vec<String> {
        let mut out = Vec::new();
        leaves(&self.root, &mut |section, defer| {
            if !defer {
                out.push(section.to_string());
            }
        });
        out
    }

    pub fn deferred(&self) -> Vec<String> {
        let mut out = Vec::new();
        leaves(&self.root, &mut |section, defer| {
            if defer {
                out.push(section.to_string());
            }
        });
        out
    }

    /// The message rebuilt from what was fetched, for [`crate::mime::parse_body`]. `mime` and
    /// `bodies` by section; None when the server left one out (the whole message is downloaded).
    pub fn assemble(&self, header: &[u8], mime: &HashMap<String, Vec<u8>>, bodies: &HashMap<String, Vec<u8>>) -> Option<Vec<u8>> {
        let Node::Multi { subtype, children } = &self.root else { return None };
        let mut out = Vec::new();
        let boundary = boundary(0);
        out.extend_from_slice(&strip_headers(header, &["content-type", "content-transfer-encoding", DEFERRED_HEADER]));
        out.extend_from_slice(format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"\r\n\r\n").as_bytes());
        write_children(&mut out, children, &boundary, 1, mime, bodies)?;
        Some(out)
    }
}

/// None when nothing is worth leaving on the server: a single part, or no large attachment.
pub fn plan(structure: &BodyStructure) -> Option<Plan> {
    let BodyStructure::Multipart { .. } = structure else { return None };
    let root = node(structure, "")?;
    let plan = Plan { root };
    (!plan.deferred().is_empty()).then_some(plan)
}

fn node(structure: &BodyStructure, section: &str) -> Option<Node> {
    let child = |i: usize| if section.is_empty() { (i + 1).to_string() } else { format!("{section}.{}", i + 1) };
    match structure {
        BodyStructure::Multipart { common, bodies, .. } => Some(Node::Multi {
            subtype: plain_token(&common.ty.subtype)?,
            children: bodies.iter().enumerate().map(|(i, b)| node(b, &child(i))).collect::<Option<_>>()?,
        }),
        BodyStructure::Basic { common, other, .. } | BodyStructure::Text { common, other, .. } | BodyStructure::Message { common, other, .. } => {
            let ty = common.ty.ty.to_ascii_lowercase();
            let subtype = common.ty.subtype.to_ascii_lowercase();
            let attachment = common.disposition.as_ref().is_some_and(|d| d.ty.eq_ignore_ascii_case("attachment"));
            let shown = match (ty.as_str(), subtype.as_str()) {
                ("text", "plain" | "html") => !attachment,
                ("text", "calendar") => true,
                // Embedded in the HTML (`cid:`): wanted to show it.
                ("image", _) => other.id.is_some(),
                _ => false,
            };
            let defer = !shown && other.octets > SMALL_PART;
            let size = match other.transfer_encoding {
                ContentEncoding::Base64 => other.octets as usize / 78 * 57,
                _ => other.octets as usize,
            };
            Some(Node::Leaf { section: section.to_string(), defer, size })
        }
    }
}

fn leaves(node: &Node, f: &mut impl FnMut(&str, bool)) {
    match node {
        Node::Leaf { section, defer, .. } => f(section, *defer),
        Node::Multi { children, .. } => children.iter().for_each(|c| leaves(c, f)),
    }
}

fn write_children(out: &mut Vec<u8>, children: &[Node], boundary: &str, depth: usize, mime: &HashMap<String, Vec<u8>>, bodies: &HashMap<String, Vec<u8>>) -> Option<()> {
    for child in children {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match child {
            Node::Multi { subtype, children } => {
                let inner = self::boundary(depth);
                out.extend_from_slice(format!("Content-Type: multipart/{subtype}; boundary=\"{inner}\"\r\n\r\n").as_bytes());
                write_children(out, children, &inner, depth + 1, mime, bodies)?;
            }
            Node::Leaf { section, defer, size } => {
                out.extend_from_slice(&strip_headers(mime.get(section)?, &[DEFERRED_HEADER]));
                if *defer {
                    out.extend_from_slice(format!("{DEFERRED_HEADER}: {size}\r\n\r\n").as_bytes());
                } else {
                    out.extend_from_slice(b"\r\n");
                    out.extend_from_slice(bodies.get(section)?);
                }
                out.extend_from_slice(b"\r\n");
            }
        }
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Some(())
}

fn boundary(depth: usize) -> String {
    format!("=_facteur-partial-{depth}-7f3c9a1e")
}

fn plain_token(s: &str) -> Option<String> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.+".contains(&b))).then(|| s.to_ascii_lowercase())
}

/// The header lines (with their continuations) except `names`, each ending in CRLF, without the
/// blank line that closes a header.
fn strip_headers(header: &[u8], names: &[&str]) -> Vec<u8> {
    let text = String::from_utf8_lossy(header);
    let mut out = String::new();
    let mut keep = true;
    for line in text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        if line.is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            let name = line.split(':').next().unwrap_or_default().trim();
            keep = !names.iter().any(|n| n.eq_ignore_ascii_case(name));
        }
        if keep {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::imap_proto::{AttributeValue, Response};

    fn structure(line: &str) -> BodyStructure<'static> {
        let raw = format!("* 1 FETCH (UID 7 BODYSTRUCTURE {line})\r\n");
        let (_, response) = Response::parse(raw.as_bytes()).unwrap();
        let Response::Fetch(_, attrs) = response.into_owned() else { panic!("not a FETCH") };
        attrs
            .into_iter()
            .find_map(|a| if let AttributeValue::BodyStructure(b) = a { Some(b) } else { None })
            .unwrap()
    }

    // Text and HTML, then a 3.8 MB PDF (5.2 MB in base64): Gmail's answer for a quote with its PDF.
    const QUOTE: &str = r#"((("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL "QUOTED-PRINTABLE" 420 12 NIL NIL NIL)("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "QUOTED-PRINTABLE" 2100 40 NIL NIL NIL) "ALTERNATIVE" ("BOUNDARY" "alt") NIL NIL)("APPLICATION" "PDF" ("NAME" "Devis 9597.pdf") NIL NIL "BASE64" 5200000 NIL ("ATTACHMENT" ("FILENAME" "Devis 9597.pdf")) NIL) "MIXED" ("BOUNDARY" "mix") NIL NIL)"#;

    #[test]
    fn the_text_is_fetched_and_a_large_attachment_left_on_the_server() {
        let plan = plan(&structure(QUOTE)).unwrap();
        assert_eq!(plan.fetched(), ["1.1", "1.2"]);
        assert_eq!(plan.deferred(), ["2"]);
        assert_eq!(
            plan.query(),
            "(UID BODY.PEEK[HEADER] BODY.PEEK[1.1.MIME] BODY.PEEK[1.1] BODY.PEEK[1.2.MIME] BODY.PEEK[1.2] BODY.PEEK[2.MIME])"
        );
    }

    #[test]
    fn embedded_images_small_parts_and_invitations_come_with_the_text() {
        let line = r#"(("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "7BIT" 900 20 NIL NIL NIL)("IMAGE" "PNG" ("NAME" "logo.png") "<logo@x>" NIL "BASE64" 900000 NIL ("INLINE" NIL) NIL)("TEXT" "CALENDAR" ("METHOD" "REQUEST") NIL NIL "BASE64" 300000 10 NIL NIL NIL)("TEXT" "CSV" NIL NIL NIL "BASE64" 20000 10 NIL ("ATTACHMENT" ("FILENAME" "a.csv")) NIL)("VIDEO" "MP4" NIL NIL NIL "BASE64" 9000000 NIL ("ATTACHMENT" ("FILENAME" "v.mp4")) NIL) "MIXED" ("BOUNDARY" "b") NIL NIL)"#;
        let plan = plan(&structure(line)).unwrap();
        assert_eq!(plan.fetched(), ["1", "2", "3", "4"]);
        assert_eq!(plan.deferred(), ["5"]);
    }

    #[test]
    fn nothing_large_to_leave_means_the_whole_message() {
        let small = r#"(("TEXT" "PLAIN" NIL NIL NIL "7BIT" 400 10 NIL NIL NIL)("APPLICATION" "PDF" NIL NIL NIL "BASE64" 90000 NIL ("ATTACHMENT" ("FILENAME" "a.pdf")) NIL) "MIXED" ("BOUNDARY" "b") NIL NIL)"#;
        assert!(plan(&structure(small)).is_none());
        // A single part, however large, has nothing to leave out.
        let single = r#"("TEXT" "HTML" ("CHARSET" "UTF-8") NIL NIL "BASE64" 3000000 400 NIL NIL NIL)"#;
        assert!(plan(&structure(single)).is_none());
    }

    const HEADER: &str = "From: SARL <contact@example.fr>\r\nTo: yann@example.com\r\nSubject: Devis 9597\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed;\r\n boundary=\"mix\"\r\n\r\n";

    fn full_quote() -> Vec<u8> {
        let parts = "--mix\r\nContent-Type: multipart/alternative; boundary=\"alt\"\r\n\r\n--alt\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\nBonjour, voici le devis\r\n--alt\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n<p>Bonjour, voici le <b>devis</b></p>\r\n--alt--\r\n--mix\r\nContent-Type: application/pdf; name=\"Devis 9597.pdf\"\r\nContent-Disposition: attachment; filename=\"Devis 9597.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--mix--\r\n";
        format!("{HEADER}{parts}").into_bytes()
    }

    fn fetched_quote() -> (HashMap<String, Vec<u8>>, HashMap<String, Vec<u8>>) {
        let mime: HashMap<String, Vec<u8>> = [
            ("1.1", "Content-Type: text/plain; charset=UTF-8\r\n\r\n"),
            ("1.2", "Content-Type: text/html; charset=UTF-8\r\n\r\n"),
            // A forged marker is not taken for ours.
            ("2", "Content-Type: application/pdf; name=\"Devis 9597.pdf\"\r\nContent-Disposition: attachment; filename=\"Devis 9597.pdf\"\r\nContent-Transfer-Encoding: base64\r\nX-Facteur-Deferred: 1\r\n\r\n"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect();
        let bodies = [("1.1", "Bonjour, voici le devis"), ("1.2", "<p>Bonjour, voici le <b>devis</b></p>")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        (mime, bodies)
    }

    #[test]
    fn the_rebuilt_message_shows_the_text_and_lists_the_attachment_where_the_whole_one_does() {
        let plan = plan(&structure(QUOTE)).unwrap();
        let (mime, bodies) = fetched_quote();
        let raw = plan.assemble(HEADER.as_bytes(), &mime, &bodies).unwrap();
        let parsed = crate::mime::parse_body(&raw).unwrap();
        assert!(parsed.html.contains("devis"), "{}", parsed.html);
        assert_eq!(parsed.headers.subject, "Devis 9597");
        let whole = crate::mime::parse_body(&full_quote()).unwrap();
        let listed = |p: &crate::mime::ParsedBody| p.attachments.iter().map(|a| (a.index, a.filename.clone(), a.mime.clone())).collect::<Vec<_>>();
        assert_eq!(listed(&parsed), listed(&whole));
        assert_eq!(parsed.attachments[0].size, 5200000 / 78 * 57);
    }

    #[test]
    fn a_part_the_server_left_out_means_the_whole_message() {
        let plan = plan(&structure(QUOTE)).unwrap();
        let (mut mime, bodies) = fetched_quote();
        mime.remove("2");
        assert!(plan.assemble(HEADER.as_bytes(), &mime, &bodies).is_none());
    }
}
