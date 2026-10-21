//! Address books of the accounts, read only: the names and addresses of their contacts, for the
//! composer's suggestions. Google contacts (People API), Outlook contacts (Graph) and CardDAV
//! (see `caldav` for the server side).

use std::time::Duration;

use crate::error::{Error, Result};

const MAX_PAGES: usize = 50;
const MAX_PAGE: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Contact {
    /// The contact's id on the provider (a resource name, a CardDAV address).
    pub remote_id: String,
    pub name: Option<String>,
    /// Lowercase.
    pub emails: Vec<String>,
}

fn client() -> Result<reqwest::Client> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent("facteur")
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

/// No access (a sign-in from before facteur asked for the contacts): an auth error for the
/// address book only, never for the mail.
pub fn needs_consent() -> Error {
    Error::Auth(crate::tr!(
        "facteur n'a pas accès aux contacts de ce compte : reconnectez-le en cochant l'accès aux contacts",
        "facteur has no access to this account's contacts: reconnect it and tick the access to contacts"
    ))
}

fn emails<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in values.map(|e| e.trim().to_lowercase()) {
        if e.contains('@') && !e.contains(char::is_whitespace) && !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

fn name(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string)
}

async fn get(http: &reqwest::Client, token: &str, url: reqwest::Url, service: &str) -> Result<serde_json::Value> {
    let resp = http
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| Error::Other(crate::tr!("{service} injoignable : {e}", "{service} unreachable: {e}")))?;
    let status = resp.status();
    let body = crate::http::read_capped(resp, MAX_PAGE).await?;
    if status.is_success() {
        return Ok(serde_json::from_slice(&body)?);
    }
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let error = &json["error"];
    let text = serde_json::to_string(error).unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED
        || (status == reqwest::StatusCode::FORBIDDEN && !text.contains("SERVICE_DISABLED") && !text.contains("accessNotConfigured"))
    {
        tracing::info!("{service}: access refused ({status}): {}", error["message"].as_str().unwrap_or_default());
        return Err(needs_consent());
    }
    if text.contains("SERVICE_DISABLED") || text.contains("accessNotConfigured") {
        return Err(Error::Other(crate::tr!(
            "l'API Google People n'est pas activée pour l'application",
            "the Google People API is not enabled for the application"
        )));
    }
    let message = error["message"].as_str().map(str::to_string).unwrap_or_else(|| status.to_string());
    Err(Error::Other(format!("{service}: {message}")))
}

pub async fn google(token: &str) -> Result<Vec<Contact>> {
    let http = client()?;
    let mut out = Vec::new();
    let mut page: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = reqwest::Url::parse("https://people.googleapis.com/v1/people/me/connections").map_err(|e| Error::Other(e.to_string()))?;
        url.query_pairs_mut().append_pair("personFields", "names,emailAddresses").append_pair("pageSize", "1000");
        if let Some(p) = &page {
            url.query_pairs_mut().append_pair("pageToken", p);
        }
        let json = get(&http, token, url, "Google Contacts").await?;
        out.extend(json["connections"].as_array().into_iter().flatten().filter_map(map_google));
        page = json["nextPageToken"].as_str().map(str::to_string);
        if page.is_none() {
            break;
        }
    }
    Ok(out)
}

fn map_google(person: &serde_json::Value) -> Option<Contact> {
    let emails = emails(person["emailAddresses"].as_array().into_iter().flatten().filter_map(|e| e["value"].as_str()));
    if emails.is_empty() {
        return None;
    }
    let names = person["names"].as_array();
    let primary = names.and_then(|n| n.iter().find(|n| n["metadata"]["primary"].as_bool() == Some(true)).or(n.first()));
    Some(Contact { remote_id: person["resourceName"].as_str()?.to_string(), name: name(primary.and_then(|n| n["displayName"].as_str())), emails })
}

pub async fn outlook(token: &str) -> Result<Vec<Contact>> {
    let http = client()?;
    let mut out = Vec::new();
    let mut url = reqwest::Url::parse("https://graph.microsoft.com/v1.0/me/contacts").map_err(|e| Error::Other(e.to_string()))?;
    url.query_pairs_mut().append_pair("$select", "id,displayName,emailAddresses").append_pair("$top", "500");
    let mut next = Some(url);
    for _ in 0..MAX_PAGES {
        let Some(url) = next.take() else { break };
        // Only Graph's own pages are followed: the token goes nowhere else.
        if url.scheme() != "https" || url.host_str() != Some("graph.microsoft.com") {
            break;
        }
        let json = get(&http, token, url, "Microsoft Graph").await?;
        out.extend(json["value"].as_array().into_iter().flatten().filter_map(map_outlook));
        next = json["@odata.nextLink"].as_str().and_then(|u| reqwest::Url::parse(u).ok());
    }
    Ok(out)
}

fn map_outlook(item: &serde_json::Value) -> Option<Contact> {
    let emails = emails(item["emailAddresses"].as_array().into_iter().flatten().filter_map(|e| e["address"].as_str()));
    if emails.is_empty() {
        return None;
    }
    Some(Contact { remote_id: item["id"].as_str()?.to_string(), name: name(item["displayName"].as_str()), emails })
}

/// The contacts of vCards (RFC 6350, and the 3.0 most servers still send): FN, or N when FN is
/// missing, and every EMAIL.
pub fn parse_vcards(href: &str, text: &str) -> Vec<Contact> {
    let mut out = Vec::new();
    let mut current: Option<(Option<String>, Option<String>, Vec<String>)> = None;
    let mut n = 0;
    for line in unfold(text) {
        let Some((head, value)) = line.split_once(':') else { continue };
        let property = head.split(';').next().unwrap_or_default();
        // Grouped properties: `item1.EMAIL`.
        let property = property.rsplit('.').next().unwrap_or_default().to_ascii_uppercase();
        match (property.as_str(), value.trim()) {
            ("BEGIN", v) if v.eq_ignore_ascii_case("VCARD") => current = Some((None, None, Vec::new())),
            ("END", v) if v.eq_ignore_ascii_case("VCARD") => {
                if let Some((full, structured, mails)) = current.take() {
                    let emails = emails(mails.iter().map(String::as_str));
                    if !emails.is_empty() {
                        n += 1;
                        let remote_id = if n == 1 { href.to_string() } else { format!("{href}#{n}") };
                        out.push(Contact { remote_id, name: full.or(structured), emails });
                    }
                }
            }
            ("FN", v) => {
                if let Some(c) = current.as_mut() {
                    c.0 = name(Some(&unescape(v)));
                }
            }
            ("N", v) => {
                if let Some(c) = current.as_mut() {
                    // Family;Given;Additional;Prefix;Suffix.
                    let parts: Vec<String> = v.split(';').map(unescape).collect();
                    let given = parts.get(1).map(|s| s.trim()).unwrap_or_default();
                    let family = parts.first().map(|s| s.trim()).unwrap_or_default();
                    c.1 = name(Some(&format!("{given} {family}")));
                }
            }
            ("EMAIL", v) => {
                if let Some(c) = current.as_mut() {
                    c.2.push(v.strip_prefix("mailto:").unwrap_or(v).to_string());
                }
            }
            _ => {}
        }
    }
    out
}

fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        match (line.strip_prefix([' ', '\t']), out.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if line.is_empty() => {}
            _ => out.push(line.to_string()),
        }
    }
    out
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push(' '),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_google_contacts() {
        let json = serde_json::json!({ "connections": [
            { "resourceName": "people/c1", "names": [{ "displayName": "Marie Curie", "metadata": { "primary": true } }],
              "emailAddresses": [{ "value": "Marie@Example.org" }, { "value": "marie@example.org" }, { "value": "m.curie@lab.fr" }] },
            { "resourceName": "people/c2", "names": [{ "displayName": "Sans adresse" }] },
            { "resourceName": "people/c3", "emailAddresses": [{ "value": "anon@example.org" }] }
        ]});
        let list: Vec<_> = json["connections"].as_array().unwrap().iter().filter_map(map_google).collect();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].name.as_deref(), list[0].emails.clone()), (Some("Marie Curie"), vec!["marie@example.org".to_string(), "m.curie@lab.fr".into()]));
        assert_eq!((list[1].remote_id.as_str(), list[1].name.as_deref()), ("people/c3", None));
    }

    #[test]
    fn maps_outlook_contacts() {
        let item = serde_json::json!({ "id": "AAA", "displayName": " Paul ", "emailAddresses": [{ "name": "Paul", "address": "paul@example.org" }] });
        let c = map_outlook(&item).unwrap();
        assert_eq!((c.remote_id.as_str(), c.name.as_deref(), c.emails.as_slice()), ("AAA", Some("Paul"), ["paul@example.org".to_string()].as_slice()));
        assert!(map_outlook(&serde_json::json!({ "id": "B", "displayName": "x", "emailAddresses": [] })).is_none());
    }

    #[test]
    fn reads_vcards() {
        let text = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:1\r\nFN:Dupont\\, Jean\r\nN:Dupont;Jean;;;\r\nitem1.EMAIL;TYPE=INTERNET:jean@example.org\r\n\
EMAIL;TYPE=WORK:jean.dupont@work.\r\n example\r\nPHOTO;ENCODING=b;TYPE=JPEG:AAAA\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nN:Martin;Léa;;;\r\nEMAIL:mailto:lea@example.org\r\nEND:VCARD\r\nBEGIN:VCARD\r\nFN:Sans adresse\r\nEND:VCARD\r\n";
        let list = parse_vcards("/card/a.vcf", text);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name.as_deref(), Some("Dupont, Jean"));
        assert_eq!(list[0].emails, ["jean@example.org", "jean.dupont@work.example"]);
        assert_eq!((list[1].remote_id.as_str(), list[1].name.as_deref(), list[1].emails[0].as_str()), ("/card/a.vcf#2", Some("Léa Martin"), "lea@example.org"));
    }
}
