use chrono::{Datelike, NaiveDate, TimeZone};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Query {
    pub words: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    /// From them, or addressed to them (To or Cc).
    pub with: Vec<String>,
    pub subject: Vec<String>,
    /// A role ("inbox", "sent"...) or part of a folder name.
    pub folder: Option<String>,
    pub has_attachment: bool,
    pub unread: Option<bool>,
    pub flagged: bool,
    /// Filed in one of the user's own folders (Gmail: a user label), or in none. The inbox and
    /// system folders don't count.
    pub labelled: Option<bool>,
    /// Start of day, local time.
    pub after: Option<i64>,
    pub before: Option<i64>,
}

impl Query {
    pub fn is_empty(&self) -> bool {
        *self == Query::default()
    }
}

fn tokens(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn day(value: &str) -> Option<i64> {
    // `%y` before `%Y`: chrono's `%Y` takes any number of digits and would read "26" as year 26.
    // A four-digit year fails `%y` (too long) and falls through to `%Y`.
    let d = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(value, "%d/%m/%y"))
        .or_else(|_| NaiveDate::parse_from_str(value, "%d/%m/%Y"))
        .ok()
        .filter(|d| d.year() >= 1900)?;
    Some(chrono::Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest()?.timestamp())
}

const OPERATORS: &[&str] = &[
    "de", "from", "à", "a", "to", "pour", "avec", "with", "objet", "sujet", "subject", "dans", "in", "label", "libellé", "libelle", "avant",
    "before", "après", "apres", "after", "depuis", "since",
];

pub fn parse(input: &str) -> Query {
    let mut q = Query::default();
    for token in tokens(input) {
        let lower = token.to_lowercase();
        match lower.as_str() {
            "pj" | "has:attachment" | "avec:pj" | "has:pj" => {
                q.has_attachment = true;
                continue;
            }
            "non-lu" | "nonlu" | "is:unread" => {
                q.unread = Some(true);
                continue;
            }
            "lu" | "is:read" => {
                q.unread = Some(false);
                continue;
            }
            "épinglé" | "epingle" | "épinglés" | "is:pinned" | "is:starred" | "is:flagged" => {
                q.flagged = true;
                continue;
            }
            "étiqueté" | "etiquete" | "has:userlabels" => {
                q.labelled = Some(true);
                continue;
            }
            "sans-étiquette" | "sans-etiquette" | "has:nouserlabels" => {
                q.labelled = Some(false);
                continue;
            }
            _ => {}
        }
        // `is:` / `has:` with a value not (yet) known (`is:p` typed on the way to `is:pinned`),
        // or a known operator still without its value (`de:`): nothing to search for. As words,
        // they would be looked up as prefixes in the whole index.
        if let Some((key, value)) = token.split_once(':') {
            let key = key.to_lowercase();
            if matches!(key.as_str(), "is" | "has") || (value.is_empty() && OPERATORS.contains(&key.as_str())) {
                continue;
            }
        }
        let Some((key, value)) = token.split_once(':').filter(|(_, v)| !v.is_empty()) else {
            q.words.push(token);
            continue;
        };
        let value = value.to_string();
        match key.to_lowercase().as_str() {
            "de" | "from" => q.from.push(value),
            "à" | "a" | "to" | "pour" => q.to.push(value),
            "avec" | "with" => q.with.push(value),
            "objet" | "sujet" | "subject" => q.subject.push(value),
            // A Gmail label is a folder here: `label:` is `dans:`, as in Gmail.
            "dans" | "in" | "label" | "libellé" | "libelle" => q.folder = Some(value),
            "avant" | "before" => q.before = day(&value).or(q.before),
            "après" | "apres" | "after" | "depuis" | "since" => q.after = day(&value).or(q.after),
            // Not an operator (a time like 10:30, a URL): plain text.
            _ => q.words.push(token),
        }
    }
    q
}

pub fn folder_role(value: &str) -> Option<crate::model::Role> {
    use crate::model::Role;
    Some(match value.to_lowercase().as_str() {
        "inbox" | "boîte" | "boite" | "réception" | "reception" => Role::Inbox,
        "sent" | "envoyés" | "envoyes" => Role::Sent,
        "drafts" | "brouillons" => Role::Drafts,
        "trash" | "corbeille" => Role::Trash,
        "junk" | "spam" | "indésirables" | "indesirables" => Role::Junk,
        "archive" | "archives" => Role::Archive,
        "all" | "tous" => Role::All,
        "plus-tard" | "snoozed" => Role::Snoozed,
        _ => return None,
    })
}

/// Values are stripped of quotes, backslashes and control characters, which would escape the
/// IMAP string. Quoted strings are 7-bit only (RFC 3501): with `literals` (the server takes
/// LITERAL+ or LITERAL-), non-ASCII values go as non-synchronizing literals instead.
pub fn imap_criteria(q: &Query, literals: bool) -> String {
    let string = |v: &str| {
        let v = v.chars().filter(|c| !matches!(c, '"' | '\\') && !c.is_control()).collect::<String>();
        // LITERAL- takes literals of up to 4096 bytes.
        if literals && !v.is_ascii() && v.len() <= 4096 { format!("{{{}+}}\r\n{v}", v.len()) } else { format!("\"{v}\"") }
    };
    let date = |t: i64| chrono::Local.timestamp_opt(t, 0).single().map(|d| d.format("%d-%b-%Y").to_string());
    let mut parts = Vec::new();
    parts.extend(q.from.iter().map(|v| format!("FROM {}", string(v))));
    // Like the local search: addressed to him, in To or Cc.
    parts.extend(q.to.iter().map(|v| {
        let v = string(v);
        format!("OR TO {v} CC {v}")
    }));
    // OR takes exactly two criteria, hence the nesting.
    parts.extend(q.with.iter().map(|v| {
        let v = string(v);
        format!("OR FROM {v} OR TO {v} CC {v}")
    }));
    parts.extend(q.subject.iter().map(|v| format!("SUBJECT {}", string(v))));
    parts.extend(q.words.iter().map(|v| format!("TEXT {}", string(v))));
    match q.unread {
        Some(true) => parts.push("UNSEEN".into()),
        Some(false) => parts.push("SEEN".into()),
        None => {}
    }
    if q.flagged {
        parts.push("FLAGGED".into());
    }
    // IMAP has no attachment criterion: an approximate narrowing (the usual structure of a
    // message with attachments), made exact by the caller once the content is known.
    if q.has_attachment {
        parts.push("HEADER Content-Type \"multipart/mixed\"".into());
    }
    parts.extend(q.after.and_then(date).map(|d| format!("SINCE {d}")));
    parts.extend(q.before.and_then(date).map(|d| format!("BEFORE {d}")));
    if parts.is_empty() { "ALL".into() } else { parts.join(" ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_labels_filter() {
        assert_eq!(parse("étiqueté").labelled, Some(true));
        assert_eq!(parse("has:userlabels devis").labelled, Some(true));
        assert_eq!(parse("sans-étiquette").labelled, Some(false));
        assert_eq!(parse("HAS:NOUSERLABELS").labelled, Some(false));
        assert!(parse("has:nouserlabels").words.is_empty());
    }

    #[test]
    fn parses_operators() {
        let q = parse(r#"de:marie objet:"facture mars" pj non-lu avant:2026-03-01 dans:envoyés devis 10:30"#);
        assert_eq!(q.from, ["marie"]);
        assert_eq!(q.subject, ["facture mars"]);
        assert!(q.has_attachment && q.unread == Some(true));
        assert!(q.before.is_some() && q.after.is_none());
        assert_eq!(q.folder.as_deref(), Some("envoyés"));
        assert_eq!(q.words, ["devis", "10:30"]);
        assert_eq!(folder_role("Envoyés"), Some(crate::model::Role::Sent));

        let with = parse("avec:paul@x.fr");
        assert_eq!(with.with, ["paul@x.fr"]);
        assert_eq!(imap_criteria(&with, true), "OR FROM \"paul@x.fr\" OR TO \"paul@x.fr\" CC \"paul@x.fr\"");
        assert_eq!(imap_criteria(&parse("à:paul"), true), "OR TO \"paul\" CC \"paul\"");
        assert_eq!(imap_criteria(&parse("pj"), true), "HEADER Content-Type \"multipart/mixed\"");
        assert_eq!(imap_criteria(&parse("dans:archives"), true), "ALL");
        assert_eq!(parse("label:Mairie").folder.as_deref(), Some("Mairie"));
        assert_eq!(parse("libellé:\"Mes factures\" devis").folder.as_deref(), Some("Mes factures"));
        let imap = imap_criteria(&parse("from:a\\b épinglé"), true);
        assert_eq!(imap, "FROM \"ab\" FLAGGED");
        // Accented text: a literal (byte length) when the server takes them, quoted otherwise.
        assert_eq!(imap_criteria(&parse("de:françoise réunion"), true), "FROM {10+}\r\nfrançoise TEXT {8+}\r\nréunion");
        assert_eq!(imap_criteria(&parse("réunion"), false), "TEXT \"réunion\"");
        assert!(parse("   ").is_empty());
    }

    #[test]
    fn unfinished_operators_are_not_words() {
        for partial in ["is:", "is:p", "is:un", "has:", "has:atta", "de:", "objet:"] {
            assert!(parse(partial).is_empty(), "{partial}");
        }
        assert_eq!(parse("is:pinned devis").words, ["devis"]);
        // Not an operator: still text.
        assert_eq!(parse("note: 10:30").words, ["note:", "10:30"]);
    }

    #[test]
    fn two_digit_years_are_this_century() {
        let march = day("2026-03-01");
        assert!(march.is_some());
        assert_eq!(parse("après:1/3/26").after, march);
        assert_eq!(parse("avant:01/03/26").before, march);
        assert_eq!(parse("avant:01/03/2026").before, march);
        assert_eq!(parse("after:1/3/2026").after, march);
        // Not a meaningful year: ignored rather than read as year 26.
        assert_eq!(parse("après:26-03-01").after, None);
    }
}
