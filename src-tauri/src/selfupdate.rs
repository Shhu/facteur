//! Self-update without an installer: the signature-checked exe replaces the running one (Windows
//! allows renaming it), then restarts with `--after-update <pid>` so the new process waits for this
//! one to exit before taking the single-instance lock (see main.rs).

use std::sync::Mutex;

use facteur_core::{Error, Result, update};
use tauri::{AppHandle, Emitter};

/// Only the Windows exe updates itself: elsewhere nothing is offered.
pub async fn check(app: &AppHandle) -> Result<Option<update::Available>> {
    if !cfg!(windows) {
        return Ok(None);
    }
    update::check(&app.package_info().version.to_string()).await
}

#[derive(Clone, serde::Serialize)]
struct Progress {
    done: u64,
    total: Option<u64>,
}

/// Kept between download and install so the UI can save the open draft and drain the outbox right
/// before the restart, not before a download that can take minutes.
static DOWNLOADED: Mutex<Option<Vec<u8>>> = Mutex::new(None);

pub async fn download(app: &AppHandle) -> Result<()> {
    let Some(available) = check(app).await? else {
        return Err(Error::Other(facteur_core::tr!("facteur est déjà à jour", "facteur is already up to date")));
    };
    let bytes = update::download(&available, |done, total| {
        let _ = app.emit("update-progress", Progress { done, total });
    })
    .await?;
    tracing::info!(version = %available.version, size = bytes.len(), "update: downloaded and checked");
    *DOWNLOADED.lock().unwrap_or_else(|e| e.into_inner()) = Some(bytes);
    Ok(())
}

pub fn apply(app: &AppHandle) -> Result<()> {
    let Some(bytes) = DOWNLOADED.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return Err(Error::Other(facteur_core::tr!("aucune mise à jour téléchargée", "no update downloaded")));
    };
    replace_and_restart(app, &bytes)
}

pub fn discard() {
    DOWNLOADED.lock().unwrap_or_else(|e| e.into_inner()).take();
}

pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
        let _ = std::fs::remove_file(exe.with_extension("exe.new"));
    }
}

#[cfg(windows)]
fn replace_and_restart(app: &AppHandle, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;

    let exe = std::env::current_exe()?;
    let new = exe.with_extension("exe.new");
    let old = exe.with_extension("exe.old");
    let denied = |e: std::io::Error| {
        Error::Other(facteur_core::tr!(
            "impossible de remplacer {} ({e}) : placez facteur.exe dans un dossier où vous pouvez écrire",
            "cannot replace {} ({e}): put facteur.exe in a folder you can write to",
            exe.display()
        ))
    };
    tracing::info!(exe = %exe.display(), size = bytes.len(), "update: writing the new exe");
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
    if let Err(e) = std::fs::rename(&exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(denied(e));
    }
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        let _ = std::fs::remove_file(&new);
        return Err(denied(e));
    }
    tracing::info!("update: exe replaced, restarting");
    let pid = std::process::id();
    if let Err(e) = std::process::Command::new(&exe).arg("--after-update").arg(pid.to_string()).spawn() {
        // The new exe is in place: the next launch uses it.
        tracing::error!("update: restart failed: {e}");
        return Err(Error::Other(facteur_core::tr!(
            "mise à jour installée, mais le redémarrage a échoué ({e}) : relancez facteur",
            "update installed, but the restart failed ({e}): start facteur again"
        )));
    }
    tracing::info!(pid, "update: new instance started, quitting");
    app.exit(0);
    Ok(())
}

#[cfg(not(windows))]
fn replace_and_restart(_app: &AppHandle, _bytes: &[u8]) -> Result<()> {
    Err(Error::Other(facteur_core::tr!("mise à jour automatique : Windows seulement", "automatic update: Windows only")))
}
