//! Windows exe updates: a website manifest announces the latest version, the exe's address and
//! its signature, checked against the public key below before anything replaces the running exe.
//!
//! The manifest itself is not signed: the version is bound to the exe by the signature's
//! trusted comment (`version:X.Y.Z`, written by `tauri signer sign --app-version`), so a
//! tampered manifest cannot pair a new version number with an older, validly signed exe.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const MANIFEST_URL: &str = "https://shu-app.com/facteur/download/update.json";
pub const PLATFORM: &str = "windows-x86_64";
/// Minisign, as written by `tauri signer generate`.
const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDI3NDEwNjc0MEMyNjk5REQKUldUZG1TWU1kQVpCSndGMEVzMUVzS2FvTXQyYU1WUFYwMy91dURUOTBET1E0UTNBM2NhTVRIODIK";
/// An exe is about 45 MB: anything far larger is refused.
const MAX_DOWNLOAD: usize = 200 * 1024 * 1024;
/// The download fails when the server sends nothing for this long, however slow the link:
/// no total limit, so a large exe still arrives over a weak connection.
const STALL: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    platforms: HashMap<String, Platform>,
}

#[derive(Debug, Clone, Deserialize)]
struct Platform {
    url: String,
    signature: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Available {
    pub version: String,
    pub notes: String,
    #[serde(skip)]
    url: String,
    #[serde(skip)]
    signature: String,
    /// The running version: the signed version must be later.
    #[serde(skip)]
    current: String,
}

fn http() -> Result<reqwest::Client> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    // No client-wide timeout: it would cover the whole body. Each request bounds itself.
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

pub async fn check(current: &str) -> Result<Option<Available>> {
    let response = http()?
        .get(MANIFEST_URL)
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Other(crate::tr!("site des mises à jour injoignable : {e}", "update site unreachable: {e}")))?;
    let body = crate::http::read_capped(response, 256 * 1024).await?;
    let manifest: Manifest = serde_json::from_slice(&body).map_err(|e| Error::Other(e.to_string()))?;
    if !newer(&manifest.version, current) {
        return Ok(None);
    }
    let Some(platform) = manifest.platforms.get(PLATFORM).cloned() else { return Ok(None) };
    if !platform.url.starts_with("https://") {
        return Err(Error::Other(crate::tr!("adresse de mise à jour refusée", "update address refused")));
    }
    Ok(Some(Available {
        version: manifest.version,
        notes: manifest.notes,
        url: platform.url,
        signature: platform.signature,
        current: current.to_string(),
    }))
}

pub async fn download(update: &Available, mut progress: impl FnMut(u64, Option<u64>)) -> Result<Vec<u8>> {
    let failed = |e: String| Error::Other(crate::tr!("téléchargement de la mise à jour : {e}", "downloading the update: {e}"));
    let stalled = || failed(crate::tr!("le serveur ne répond plus", "the server stopped responding"));
    let mut response = tokio::time::timeout(STALL, http()?.get(&update.url).send())
        .await
        .map_err(|_| stalled())?
        .and_then(|r| r.error_for_status())
        .map_err(|e| failed(e.to_string()))?;
    let total = response.content_length();
    if total.is_some_and(|n| n > MAX_DOWNLOAD as u64) {
        return Err(Error::Other(crate::tr!("mise à jour trop lourde", "update too large")));
    }
    let mut body = Vec::with_capacity(total.unwrap_or(0) as usize);
    while let Some(chunk) = tokio::time::timeout(STALL, response.chunk()).await.map_err(|_| stalled())?.map_err(|e| failed(e.to_string()))? {
        if body.len() + chunk.len() > MAX_DOWNLOAD {
            return Err(Error::Other(crate::tr!("mise à jour trop lourde", "update too large")));
        }
        body.extend_from_slice(&chunk);
        progress(body.len() as u64, total);
    }
    let signed = verify(&body, &update.signature)?;
    check_signed_version(signed.as_deref(), &update.version, &update.current)?;
    Ok(body)
}

/// `signature` is what `tauri signer sign` writes (base64 of a minisign signature file).
/// Returns the version bound in its trusted comment, if any.
pub fn verify(bytes: &[u8], signature: &str) -> Result<Option<String>> {
    verify_with(PUBLIC_KEY, bytes, signature)
}

fn verify_with(public_key: &str, bytes: &[u8], signature: &str) -> Result<Option<String>> {
    let refused = || Error::Other(crate::tr!("signature de la mise à jour invalide", "invalid update signature"));
    let decode = |s: &str| B64.decode(s.trim()).ok().and_then(|b| String::from_utf8(b).ok());
    let key = decode(public_key).and_then(|k| minisign_verify::PublicKey::decode(&k).ok()).ok_or_else(refused)?;
    let signature = decode(signature).and_then(|s| minisign_verify::Signature::decode(&s).ok()).ok_or_else(refused)?;
    key.verify(bytes, &signature, true).map_err(|_| refused())?;
    // Tab-separated `key:value` fields, e.g. `timestamp:…\tfile:facteur.exe\tversion:0.4.0`.
    Ok(signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.trim().strip_prefix("version:"))
        .map(|v| v.trim().to_string()))
}

/// The signed version must be the one the manifest announces, and later than the running one:
/// no replay of an older (validly signed) exe under a new version number, no downgrade.
fn check_signed_version(signed: Option<&str>, announced: &str, current: &str) -> Result<()> {
    let refused = || {
        Error::Other(crate::tr!(
            "version signée de la mise à jour invalide",
            "the update's signed version does not match"
        ))
    };
    let signed = signed.ok_or_else(refused)?;
    if signed.trim_start_matches('v') != announced.trim().trim_start_matches('v') || !newer(signed, current) {
        return Err(refused());
    }
    Ok(())
}

fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u64>().ok());
        Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
    };
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

/// Replaces the exe at `exe` with `bytes`, then starts it (`launch`). The previous one stays
/// as `.exe.old` while it runs, and comes back when the new one cannot be put in place or
/// started (an antivirus quarantining it at launch): never left without an exe. `denied` words
/// a failure to write in its folder.
pub fn swap_and_launch(
    exe: &std::path::Path,
    bytes: &[u8],
    launch: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    denied: impl Fn(std::io::Error) -> Error,
) -> Result<()> {
    use std::io::Write as _;
    let new = exe.with_extension("exe.new");
    let old = exe.with_extension("exe.old");
    // Synced before the renames: a crash right after them must not leave a truncated exe.
    let written = std::fs::File::create(&new).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&new);
        return Err(denied(e));
    }
    let _ = std::fs::remove_file(&old);
    // The running exe can be renamed, not overwritten.
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(denied(e));
    }
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&new);
        return Err(denied(e));
    }
    if let Err(e) = launch(exe) {
        tracing::error!("update: restart failed: {e}");
        // The previous exe back: the next launch is the version that worked.
        let _ = std::fs::remove_file(exe);
        if std::fs::rename(&old, exe).is_ok() {
            return Err(Error::Other(crate::tr!(
                "mise à jour non installée : la nouvelle version n'a pas pu démarrer ({e})",
                "update not installed: the new version could not start ({e})"
            )));
        }
        return Err(Error::Other(crate::tr!(
            "mise à jour installée, mais le redémarrage a échoué ({e}) : relancez facteur",
            "update installed, but the restart failed ({e}): start facteur again"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_restart_puts_the_previous_exe_back() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("facteur.exe");
        std::fs::write(&exe, b"old").unwrap();
        let denied = |e: std::io::Error| Error::Other(e.to_string());
        // Quarantined by an antivirus as it starts.
        let quarantined = |p: &std::path::Path| {
            std::fs::remove_file(p)?;
            Err(std::io::Error::other("blocked"))
        };
        assert!(swap_and_launch(&exe, b"new", quarantined, denied).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
        assert!(!exe.with_extension("exe.old").exists());
        swap_and_launch(&exe, b"new", |_| Ok(()), denied).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    }

    #[test]
    fn signed_files_only() {
        let signature = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUZG1TWU1kQVpCSnk1RklZQU1kcVdjdjFEK213eVNNb1lZUGplTC9qTHd0RG1OR3ppSmUvUDUxdlg0bWdxTWdoRFZiUy82ck1SZnJpam8reEFCbjNLRDdRTEFHejhNbkFnPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyMjU1NzkyCWZpbGU6cHJvYmUuYmluCjl2VEtpdE0weTQwbUpvVUY1ZENXV3R5a04xS3hSOE1ZSCtsWGtqTFFzOGNWVExoS3hpT0RlY1NBUXhXbkR0VGlMbWdUdU9mK3U5c0ZVK1dHQUx5ZkJ3PT0K";
        // Signed before versions were bound to signatures: valid, but carries no version.
        assert_eq!(verify(b"facteur update test", signature).unwrap(), None);
        assert!(verify(b"facteur update test!", signature).is_err());
        assert!(verify(b"facteur update test", "bm90IGEgc2lnbmF0dXJl").is_err());
    }

    /// A throwaway key pair (not the updates' key), for signatures bound to a version.
    const TEST_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDU3M0FFNDVEOTk2Mzk2RTQKUldUa2xtT1pYZVE2VjZLR05UWElaZ05MeXNyQTRqRjVvd3dxQ1dONVlCMStEVTBkWFBqREhaN0gK";

    #[test]
    fn signed_version_is_bound() {
        // `tauri signer sign --app-version 0.5.0`, then without `--app-version`.
        let v050 = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUa2xtT1pYZVE2VjdlakNESXhDM0tHUk5RdmZIUlVaTjJNeU5zY3VMMVkzWHUvYmF3TGpQRVhxQ2FTQk0xaGZUMGZ5NmFiRmRvdStobi9NQ1hzWld2RjV1ekNKdm5hT1FJPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyMjY2ODUyCWZpbGU6cHJvYmUuYmluCXZlcnNpb246MC41LjAKNjd2MkR3aG91UUVTMjU1aU1NUndtQVQ1WkF1LzlXS0tKSG5OdzBWYVM0RXRHODBVVU5UZzdtQ1NnblE3S3dXMVlqR3pMeis1U2dYTTVEY3FidWI1QWc9PQo=";
        let unversioned = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUa2xtT1pYZVE2VjRIWVBUYTBtZ1lPdmZPOGNLbDRiS2p6bjRWN0FBSDlhdkx1eFlRUXZvdUxXYTA1ZGpoU1p2dFpsTEpTbVJYNkN5eFcydDk2NjlPN2ZiektWVW9MZEFVPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyMjY2ODUzCWZpbGU6cHJvYmUuYmluCkNsU1VIMWtqUitycVVsYzBWYlJNbmxQOU5HN21KY2tSNWw1eDZvVUtHcHB6TFJ4WFhYRzNxY01jNGdXN05hUThNMG5SQTdkcVpsQzhLMWw4TldaN0FRPT0K";
        let bytes = b"facteur update test";
        assert_eq!(verify_with(TEST_KEY, bytes, v050).unwrap().as_deref(), Some("0.5.0"));
        assert_eq!(verify_with(TEST_KEY, bytes, unversioned).unwrap(), None);
        assert!(verify_with(TEST_KEY, b"tampered", v050).is_err());
        // Signed with another key than the updates' one.
        assert!(verify(bytes, v050).is_err());

        assert!(check_signed_version(Some("0.5.0"), "0.5.0", "0.4.2").is_ok());
        // Replay: an older exe announced as a newer version.
        assert!(check_signed_version(Some("0.5.0"), "0.6.0", "0.5.1").is_err());
        assert!(check_signed_version(Some("0.5.0"), "0.5.0", "0.5.0").is_err());
        assert!(check_signed_version(Some("0.5.0"), "0.5.0", "0.6.0").is_err());
        assert!(check_signed_version(None, "0.5.0", "0.4.0").is_err());
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("0.10.0", "0.9.3"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
        assert!(!newer("garbage", "0.2.0"));
    }
}
