//! URLs come from messages, so from anyone: https only, public addresses only (never the
//! router, a local service or a company network), capped response sizes.

use std::sync::Arc;
use std::time::Duration;

use crate::avatars::{PublicOnly, allowed};
use crate::error::{Error, Result};

/// `follow`: follow redirects (still https only, still public addresses only).
fn client(follow: bool) -> Result<reqwest::Client> {
    // reqwest is built without a default crypto provider: install the engine's.
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let policy = if follow {
        reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 3 || !allowed(attempt.url()) { attempt.stop() } else { attempt.follow() }
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .dns_resolver(Arc::new(PublicOnly))
        // A proxy (system or HTTPS_PROXY) would resolve the name itself, past the public-only check.
        .no_proxy()
        .redirect(policy)
        .user_agent("facteur")
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

pub(crate) async fn read_capped(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| Error::Other(e.to_string()))? {
        if body.len() + chunk.len() > max {
            return Err(Error::Other(crate::tr!("réponse trop volumineuse", "response too large")));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// RFC 8058 one-click unsubscribe: a POST to the announced URL.
pub async fn post_one_click(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|e| Error::Other(e.to_string()))?;
    if !allowed(&parsed) {
        return Err(Error::Other(crate::tr!("lien de désabonnement refusé (https et adresse publique exigés)", "unsubscribe link rejected (https and a public address are required)")));
    }
    // No redirects: a POST sent elsewhere could hit something other than the list.
    client(false)?
        .post(parsed)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        // Without the URL: it carries the subscriber's token, and the error ends up in the log.
        .map_err(|e| {
            let e = e.without_url();
            Error::Other(crate::tr!("désabonnement refusé : {e}", "unsubscribe rejected: {e}"))
        })?;
    Ok(())
}

/// Creation date of a domain from RDAP (rdap.org redirects to the domain's registry). Only the
/// domain name is sent.
pub async fn domain_registered(domain: &str) -> Result<Option<i64>> {
    let response = client(true)?
        .get(format!("https://rdap.org/domain/{domain}"))
        .header("Accept", "application/rdap+json")
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .map_err(|e| Error::Other(e.to_string()))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body = read_capped(response, 512 * 1024).await?;
    Ok(serde_json::from_slice(&body).ok().and_then(|json| registration(&json)))
}

fn registration(json: &serde_json::Value) -> Option<i64> {
    json["events"]
        .as_array()?
        .iter()
        .find(|e| e["eventAction"] == "registration")
        .and_then(|e| e["eventDate"].as_str())
        .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
        .map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_rdap_registration() {
        let json = serde_json::json!({ "events": [
            { "eventAction": "last changed", "eventDate": "2024-01-01T00:00:00Z" },
            { "eventAction": "registration", "eventDate": "1997-09-15T04:00:00Z" }
        ]});
        assert_eq!(super::registration(&json), Some(874_296_000));
    }
}
