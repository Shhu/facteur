//! Account secrets live only in the system keychain: nothing is written to the data folder.

use crate::error::{Error, Result};

const SERVICE: &str = "facteur";

#[derive(Default)]
pub struct Secrets;

impl Secrets {
    pub fn new() -> Self {
        Secrets
    }

    fn key(account_id: i64) -> String {
        format!("account-{account_id}")
    }

    fn entry(account_id: i64) -> Result<platform::Entry> {
        platform::entry(SERVICE, &Self::key(account_id))
    }

    pub fn set(&self, account_id: i64, secret: &str) -> Result<()> {
        Self::entry(account_id)?
            .set_password(secret)
            .map_err(|e| Error::Secret(crate::tr!("trousseau du système indisponible : {e}", "system keychain unavailable: {e}")))
    }

    pub fn get(&self, account_id: i64) -> Result<String> {
        Self::entry(account_id)?.get_password().map_err(|e| Error::Secret(e.to_string()))
    }

    pub fn delete(&self, account_id: i64) -> Result<()> {
        if let Ok(e) = Self::entry(account_id) {
            let _ = e.delete_credential();
        }
        Ok(())
    }
}

#[cfg(not(target_os = "android"))]
mod platform {
    use crate::error::{Error, Result};
    pub use keyring::Entry;

    pub fn entry(service: &str, user: &str) -> Result<Entry> {
        Entry::new(service, user).map_err(|e| Error::Secret(e.to_string()))
    }
}

#[cfg(target_os = "android")]
mod platform {
    use std::sync::Once;

    use crate::error::{Error, Result};
    pub use keyring_core::Entry;

    static STORE: Once = Once::new();

    pub fn entry(service: &str, user: &str) -> Result<Entry> {
        // The Android context is only ready once the app has started: create the store lazily.
        STORE.call_once(|| match android_native_keyring_store::Store::new() {
            Ok(store) => keyring_core::set_default_store(store),
            Err(e) => tracing::error!("Android Keystore unavailable: {e}"),
        });
        Entry::new(service, user).map_err(|e| Error::Secret(e.to_string()))
    }
}
