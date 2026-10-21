//! Windows does not let an app make itself the default `mailto:` handler: we register as able to
//! open it (Capabilities + RegisteredApplications) and the user picks facteur in Settings.

use serde::Serialize;

#[derive(Serialize)]
pub struct MailClientStatus {
    pub supported: bool,
    pub registered: bool,
    pub default: bool,
}

#[cfg(windows)]
mod imp {
    use std::io;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    const PROG_ID: &str = "facteur.mailto";
    const CAPABILITIES: &str = r"Software\facteur\Capabilities";

    fn hkcu() -> RegKey {
        RegKey::predef(HKEY_CURRENT_USER)
    }

    pub fn register() -> io::Result<()> {
        let exe = std::env::current_exe()?.to_string_lossy().into_owned();
        let (prog, _) = hkcu().create_subkey(format!(r"Software\Classes\{PROG_ID}"))?;
        prog.set_value("", &"facteur email link")?;
        prog.set_value("URL Protocol", &"")?;
        prog.create_subkey("DefaultIcon")?.0.set_value("", &format!("\"{exe}\",0"))?;
        prog.create_subkey(r"shell\open\command")?.0.set_value("", &format!("\"{exe}\" \"%1\""))?;

        let (caps, _) = hkcu().create_subkey(CAPABILITIES)?;
        caps.set_value("ApplicationName", &"facteur")?;
        caps.set_value("ApplicationDescription", &"facteur email client")?;
        caps.create_subkey("URLAssociations")?.0.set_value("mailto", &PROG_ID)?;
        hkcu().create_subkey(r"Software\RegisteredApplications")?.0.set_value("facteur", &CAPABILITIES)?;
        Ok(())
    }

    pub fn registered() -> bool {
        hkcu().open_subkey_with_flags(format!(r"Software\Classes\{PROG_ID}"), KEY_READ).is_ok()
    }

    pub fn is_default() -> bool {
        hkcu()
            .open_subkey_with_flags(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\mailto\UserChoice", KEY_READ)
            .and_then(|k| k.get_value::<String, _>("ProgId"))
            .is_ok_and(|p| p == PROG_ID)
    }
}

/// Replayed at startup: the exe may have moved since.
pub fn register() -> Result<(), String> {
    #[cfg(windows)]
    return imp::register().map_err(|e| e.to_string());
    #[cfg(not(windows))]
    Err(facteur_core::tr!("réglage disponible seulement sous Windows pour l'instant", "this setting is only available on Windows for now"))
}

pub fn refresh_if_registered() {
    #[cfg(windows)]
    if imp::registered()
        && let Err(e) = imp::register()
    {
        tracing::warn!("mailto registration: {e}");
    }
}

pub fn status() -> MailClientStatus {
    #[cfg(windows)]
    return MailClientStatus { supported: true, registered: imp::registered(), default: imp::is_default() };
    #[cfg(not(windows))]
    MailClientStatus { supported: false, registered: false, default: false }
}

pub fn mailto_arg<S: AsRef<str>>(args: &[S]) -> Option<String> {
    args.iter().map(AsRef::as_ref).find(|a| a.to_ascii_lowercase().starts_with("mailto:")).map(str::to_string)
}
