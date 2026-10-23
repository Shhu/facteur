//! OAuth2 sign-in (Google, Microsoft): installed-app flow with PKCE and a loopback redirect. The
//! refresh token goes to the system keychain; the access token only lives in memory.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use base64::engine::general_purpose::{URL_SAFE_NO_PAD, URL_SAFE_NO_PAD_INDIFFERENT};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::autoconfig::urlencode;
use crate::error::{Error, Result};
use crate::model::OAuthProvider;

// facteur's client ids, published as Thunderbird does. For an installed app they are not
// secret: every distributed binary contains them. Security rests on PKCE, the loopback redirect
// and the user's consent. A fork replaces them at build time with the same-named env variables.
const GOOGLE_CLIENT_ID: &str = or(
    option_env!("FACTEUR_GOOGLE_CLIENT_ID"),
    "471734057504-14vlk7k18b96cjfb5piminpk62hgi4ti.apps.googleusercontent.com",
);
const GOOGLE_CLIENT_SECRET: &str = or(option_env!("FACTEUR_GOOGLE_CLIENT_SECRET"), "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");
const MICROSOFT_CLIENT_ID: &str = or(option_env!("FACTEUR_MICROSOFT_CLIENT_ID"), "4a5c4064-ce7c-48a8-aee8-39a303f941fa");

/// Microsoft: the mail token's scopes (one token per service).
const MICROSOFT_MAIL_SCOPES: &str = "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access openid email profile";
/// Microsoft: the calendar's (Graph). Accounts signed in before facteur wrote to calendars only
/// consented to reading them.
const MICROSOFT_CALENDAR_SCOPES: &str = "https://graph.microsoft.com/Calendars.ReadWrite offline_access";
const MICROSOFT_CALENDAR_READ_SCOPES: &str = "https://graph.microsoft.com/Calendars.Read offline_access";
const MICROSOFT_CONTACTS_SCOPES: &str = "https://graph.microsoft.com/Contacts.Read offline_access";

const fn or(value: Option<&'static str>, default: &'static str) -> &'static str {
    match value {
        Some(v) => v,
        None => default,
    }
}

fn non_empty(v: &'static str) -> Option<&'static str> {
    (!v.is_empty()).then_some(v)
}

struct App {
    auth_url: &'static str,
    token_url: &'static str,
    /// Asked for at sign-in (consent).
    scopes: &'static str,
    /// The mail token's, when it differs: Microsoft issues one token per service, and the
    /// sign-in asks for the calendar's consent too (see [`refresh_calendar`]).
    mail_scopes: Option<&'static str>,
    /// Asked for by a sign-in that also needs what `scopes` leaves out.
    extended_scopes: Option<&'static str>,
    client_id: &'static str,
    client_secret: Option<&'static str>,
    /// Redirect host the provider accepts for an installed app.
    redirect_host: &'static str,
    extra_auth_params: &'static [(&'static str, &'static str)],
}

fn app(provider: OAuthProvider) -> Result<App> {
    let missing = |var: &str| {
        Error::Other(crate::tr!(
            "connexion {} non configurée : {var} vide (voir crates/core/src/oauth.rs)",
            "{} sign-in not configured: {var} is empty (see crates/core/src/oauth.rs)",
            match provider {
                OAuthProvider::Google => "Google",
                OAuthProvider::Microsoft => "Microsoft",
            }
        ))
    };
    Ok(match provider {
        OAuthProvider::Google => App {
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            // `profile`: the account's name and photo, shown in facteur. Calendars: full access,
            // to create and edit events. Contacts: read only, for the composer's suggestions.
            scopes: "https://mail.google.com/ openid email profile https://www.googleapis.com/auth/calendar \
                     https://www.googleapis.com/auth/contacts.readonly",
            extended_scopes: None,
            mail_scopes: None,
            client_id: non_empty(GOOGLE_CLIENT_ID).ok_or_else(|| missing("GOOGLE_CLIENT_ID"))?,
            client_secret: non_empty(GOOGLE_CLIENT_SECRET),
            redirect_host: "127.0.0.1",
            // Google only returns a refresh token with `offline` + `consent`.
            extra_auth_params: &[("access_type", "offline"), ("prompt", "consent")],
        },
        OAuthProvider::Microsoft => App {
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            // Reading the calendars only: many work tenants let users consent to that but not to
            // writing. Writing and the contacts are asked for when used (`extended`).
            scopes: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send \
                     offline_access openid email profile https://graph.microsoft.com/Calendars.Read",
            extended_scopes: Some(
                "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send \
                 offline_access openid email profile https://graph.microsoft.com/Calendars.ReadWrite \
                 https://graph.microsoft.com/Contacts.Read",
            ),
            mail_scopes: Some(MICROSOFT_MAIL_SCOPES),
            client_id: non_empty(MICROSOFT_CLIENT_ID).ok_or_else(|| missing("MICROSOFT_CLIENT_ID"))?,
            client_secret: None,
            redirect_host: "localhost",
            extra_auth_params: &[],
        },
    })
}

/// No derived `Debug`: tokens must never reach a log.
#[derive(Clone)]
pub struct Tokens {
    pub access_token: String,
    /// Wall-clock time: a monotonic `Instant` does not advance while the machine sleeps, so after
    /// a night asleep an expired token would still look valid.
    pub expires_at: SystemTime,
    pub refresh_token: Option<String>,
    /// Read from the `id_token`.
    pub email: Option<String>,
    /// Google, with the `profile` scope.
    pub picture: Option<String>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens").field("email", &self.email).finish_non_exhaustive()
    }
}

pub struct Login {
    pub url: String,
    provider: OAuthProvider,
    listener: TcpListener,
    redirect_uri: String,
    state: String,
    verifier: String,
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

pub fn ticket() -> String {
    random_token()
}

/// `extended`: also the permissions asked for only when used (Microsoft: writing to the calendars,
/// reading the contacts).
pub async fn begin(provider: OAuthProvider, login_hint: Option<&str>, extended: bool) -> Result<Login> {
    let app = app(provider)?;
    let scopes = if extended { app.extended_scopes.unwrap_or(app.scopes) } else { app.scopes };
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://{}:{port}", app.redirect_host);
    let state = random_token();
    let verifier = random_token();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let mut params = vec![
        ("client_id", app.client_id),
        ("redirect_uri", redirect_uri.as_str()),
        ("response_type", "code"),
        ("scope", scopes),
        ("state", state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    params.extend_from_slice(app.extra_auth_params);
    if let Some(hint) = login_hint.filter(|h| !h.is_empty()) {
        params.push(("login_hint", hint));
    }
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect();
    let url = format!("{}?{}", app.auth_url, query.join("&"));
    Ok(Login { url, provider, listener, redirect_uri, state, verifier })
}

impl Login {
    pub async fn finish(self) -> Result<Tokens> {
        let code = tokio::time::timeout(Duration::from_secs(300), self.wait_for_code())
            .await
            .map_err(|_| Error::Other(crate::tr!("connexion abandonnée : aucun retour du navigateur en 5 minutes", "sign-in abandoned: no response from the browser within 5 minutes")))??;
        let app = app(self.provider)?;
        let mut body = vec![
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("client_id", app.client_id),
            ("code_verifier", self.verifier.as_str()),
        ];
        if let Some(secret) = app.client_secret {
            body.push(("client_secret", secret));
        }
        // The token of the mail, not of the calendar the consent covers too.
        if let Some(scopes) = app.mail_scopes {
            body.push(("scope", scopes));
        }
        token_request(&app, &body).await
    }

    async fn wait_for_code(&self) -> Result<String> {
        loop {
            let (mut stream, _) = self.listener.accept().await?;
            // One connection at a time, each brief: the browser sometimes opens a connection in
            // advance and never uses it, or drops it; move on to the next one.
            let mut buf = Vec::new();
            let read = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let mut chunk = [0u8; 2048];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Ok::<_, std::io::Error>(())
            })
            .await;
            if !matches!(read, Ok(Ok(()))) {
                continue;
            }
            let request = String::from_utf8_lossy(&buf);
            let query = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|path| path.split_once('?'))
                .map(|(_, q)| parse_query(q))
                .unwrap_or_default();

            // The browser also asks for /favicon.ico: only the real redirect is answered.
            let outcome = if query.get("state").map(String::as_str) != Some(self.state.as_str()) {
                None
            } else if let Some(code) = query.get("code") {
                Some(Ok(code.clone()))
            } else {
                let reason = query.get("error_description").or(query.get("error")).cloned().unwrap_or_default();
                Some(Err(Error::Other(crate::tr!("connexion refusée par le fournisseur : {reason}", "sign-in refused by the provider: {reason}"))))
            };

            let (title, text) = match &outcome {
                Some(Ok(_)) => (
                    crate::tr!("Connexion réussie", "Signed in"),
                    crate::tr!("Vous pouvez fermer cet onglet et revenir dans facteur.", "You can close this tab and return to facteur."),
                ),
                Some(Err(_)) => (crate::tr!("Connexion refusée", "Sign-in refused"), crate::tr!("Revenez dans facteur pour réessayer.", "Return to facteur to try again.")),
                None => ("facteur".to_string(), crate::tr!("Rien à faire ici.", "Nothing to do here.")),
            };
            let lang = if crate::i18n::english() { "en" } else { "fr" };
            let html = format!(
                "<!doctype html><html lang=\"{lang}\"><meta charset=\"utf-8\"><title>{title}</title>\
                 <body style=\"font-family:system-ui;margin:4rem auto;max-width:32rem;text-align:center\">\
                 <h1>{title}</h1><p>{text}</p></body></html>"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                html.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
            if let Some(result) = outcome {
                return result;
            }
        }
    }
}

/// Microsoft sometimes returns a new refresh token: the caller must then replace the keychain's.
pub async fn refresh(provider: OAuthProvider, refresh_token: &str) -> Result<Tokens> {
    let app = app(provider)?;
    let mut body = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", app.client_id),
    ];
    if let Some(secret) = app.client_secret {
        body.push(("client_secret", secret));
    }
    if provider == OAuthProvider::Microsoft {
        body.push(("scope", app.mail_scopes.unwrap_or(app.scopes)));
    }
    token_request(&app, &body).await
}

/// Microsoft: a Graph token for the calendars, `write` or read only. A sign-in from before
/// facteur asked for them lacks that consent: the account must sign in again (an auth error only
/// for the calendars).
pub async fn refresh_calendar(refresh_token: &str, write: bool) -> Result<Tokens> {
    refresh_graph(refresh_token, if write { MICROSOFT_CALENDAR_SCOPES } else { MICROSOFT_CALENDAR_READ_SCOPES }).await
}

/// Microsoft: a Graph token for the contacts, as [`refresh_calendar`].
pub async fn refresh_contacts(refresh_token: &str) -> Result<Tokens> {
    refresh_graph(refresh_token, MICROSOFT_CONTACTS_SCOPES).await
}

async fn refresh_graph(refresh_token: &str, scopes: &str) -> Result<Tokens> {
    let app = app(OAuthProvider::Microsoft)?;
    let body = [("grant_type", "refresh_token"), ("refresh_token", refresh_token), ("client_id", app.client_id), ("scope", scopes)];
    token_request(&app, &body).await
}

async fn token_request(app: &App, body: &[(&str, &str)]) -> Result<Tokens> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let form: Vec<String> = body.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect();
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| Error::Other(e.to_string()))?
        .post(app.token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.join("&"))
        .send()
        .await
        .map_err(|e| Error::Other(crate::tr!("fournisseur OAuth injoignable : {e}", "OAuth provider unreachable: {e}")))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| Error::Other(e.to_string()))?;
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let reason = json["error_description"].as_str().or(json["error"].as_str()).unwrap_or(&text);
        // Refresh token revoked, expired, or the app's access removed: only signing in again
        // helps, so it is an authentication error (the account then waits for the user).
        if revoked(status.as_u16(), json["error"].as_str()) {
            return Err(Error::Auth(crate::tr!(
                "accès refusé par le fournisseur ({reason}) : reconnectez le compte",
                "access refused by the provider ({reason}): reconnect the account"
            )));
        }
        return Err(Error::Other(crate::tr!("jeton refusé par le fournisseur : {reason}", "token refused by the provider: {reason}")));
    }
    let access_token = json["access_token"]
        .as_str()
        .ok_or_else(|| Error::Other(crate::tr!("réponse du fournisseur sans jeton d'accès", "provider response without an access token")))?
        .to_string();
    let expires_in = json["expires_in"].as_u64().unwrap_or(3600);
    let claims = json["id_token"].as_str().and_then(id_token_claims);
    let email = claims.as_ref().and_then(email_from_claims);
    let picture = claims.as_ref().and_then(|c| c["picture"].as_str()).map(str::to_string);
    Ok(Tokens {
        access_token,
        expires_at: SystemTime::now() + Duration::from_secs(expires_in),
        refresh_token: json["refresh_token"].as_str().map(str::to_string),
        email,
        picture,
    })
}

/// A token endpoint answer that means the grant is gone for good (RFC 6749 §5.2), or needs the
/// user again (Microsoft: a new sign-in policy, consent to renew), not a passing failure.
fn revoked(status: u16, error: Option<&str>) -> bool {
    matches!(status, 400 | 401)
        && matches!(error, Some("invalid_grant" | "unauthorized_client" | "invalid_client" | "interaction_required" | "consent_required"))
}

/// Returned as a data URL for the local cache: shown offline, without calling the provider on
/// each display.
pub async fn fetch_picture(url: &str) -> Result<String> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    // Google serves the size asked for by the `=sNN-c` suffix: 128 px is enough.
    let url = match url.rsplit_once("=s") {
        Some((base, _)) if url.contains("googleusercontent.com") => format!("{base}=s128-c"),
        _ => url.to_string(),
    };
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Other(e.to_string()))?
        .get(&url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Other(crate::tr!("photo de profil indisponible : {e}", "profile picture unavailable: {e}")))?;
    let mime = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .filter(|m| m.starts_with("image/"))
        .unwrap_or("image/jpeg")
        .to_string();
    // Streamed with a cap: a server may announce no size, or lie about it.
    const MAX_PICTURE: usize = 512 * 1024;
    let too_large = || Error::Other(crate::tr!("photo de profil trop lourde", "profile picture too large"));
    if resp.content_length().is_some_and(|n| n > MAX_PICTURE as u64) {
        return Err(too_large());
    }
    let bytes = crate::http::read_capped(resp, MAX_PICTURE).await?;
    Ok(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)))
}

pub async fn google_picture(access_token: &str) -> Result<Option<String>> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Other(e.to_string()))?
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .bearer_auth(access_token)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Other(crate::tr!("profil Google indisponible : {e}", "Google profile unavailable: {e}")))?;
    let body = crate::http::read_capped(resp, 64 * 1024).await?;
    let profile: serde_json::Value = serde_json::from_slice(&body).map_err(|e| Error::Other(e.to_string()))?;
    Ok(profile["picture"].as_str().filter(|u| u.starts_with("https://")).map(str::to_string))
}

/// The `id_token` comes straight from the provider over TLS: its claims are read without
/// checking the signature, only for the account's address and photo.
fn id_token_claims(id_token: &str) -> Option<serde_json::Value> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD_INDIFFERENT.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
fn email_from_id_token(id_token: &str) -> Option<String> {
    email_from_claims(&id_token_claims(id_token)?)
}

fn email_from_claims(claims: &serde_json::Value) -> Option<String> {
    ["email", "preferred_username", "upn"]
        .iter()
        .find_map(|k| claims[k].as_str())
        .filter(|s| s.contains('@'))
        .map(|s| s.to_ascii_lowercase())
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (urldecode(k), urldecode(v))
        })
        .collect()
}

pub(crate) fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            // On bytes: slicing the string inside a character would panic (« %é »).
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revoked_grant_is_told_apart_from_a_passing_failure() {
        assert!(revoked(400, Some("invalid_grant")));
        assert!(revoked(401, Some("unauthorized_client")));
        assert!(!revoked(400, Some("invalid_request")));
        assert!(revoked(400, Some("interaction_required")), "a new MFA policy: the user signs in again");
        assert!(revoked(400, Some("consent_required")));
        assert!(!revoked(503, Some("invalid_grant")));
        assert!(!revoked(400, None));
    }

    /// Local server answering once with `size` bytes and no Content-Length.
    async fn serve(size: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nConnection: close\r\n\r\n").await;
            let chunk = vec![b'x'; 64 * 1024];
            let mut sent = 0;
            while sent < size {
                let n = chunk.len().min(size - sent);
                if sock.write_all(&chunk[..n]).await.is_err() {
                    return;
                }
                sent += n;
            }
        });
        format!("http://{addr}/photo")
    }

    #[tokio::test]
    async fn profile_picture_download_is_capped() {
        assert!(fetch_picture(&serve(1000).await).await.unwrap().starts_with("data:image/png;base64,"));
        assert!(fetch_picture(&serve(8 * 1024 * 1024).await).await.is_err());
    }

    #[test]
    fn urldecode_survives_non_ascii() {
        assert_eq!(urldecode("%é%41"), "%éA");
        assert_eq!(urldecode("a%2"), "a%2");
    }

    #[test]
    fn decodes_the_redirect_request() {
        let q = parse_query("code=4%2F0AX4&state=abc&scope=email%20openid+https%3A%2F%2Fmail.google.com%2F");
        assert_eq!(q["code"], "4/0AX4");
        assert_eq!(q["state"], "abc");
        assert_eq!(q["scope"], "email openid https://mail.google.com/");
        assert_eq!(urldecode("a%2"), "a%2");
    }

    #[test]
    fn address_in_the_id_token() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"1","email":"Jean@Example.com","email_verified":true}"#);
        assert_eq!(email_from_id_token(&format!("h.{payload}.sig")).as_deref(), Some("jean@example.com"));
        let payload = URL_SAFE_NO_PAD.encode(br#"{"preferred_username":"jean@outlook.com"}"#);
        assert_eq!(email_from_id_token(&format!("h.{payload}.sig")).as_deref(), Some("jean@outlook.com"));
        assert_eq!(email_from_id_token("pas.un.jwt"), None);
    }
}
