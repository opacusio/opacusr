//! OS keychain storage for the unattended access password.
//!
//! Backends (via the `keyring` crate): Windows Credential Manager,
//! macOS Keychain, Linux Secret Service (GNOME Keyring / KWallet).
//! On Linux the Secret Service needs a running provider; when none is
//! available every function returns an error and the agent transparently
//! falls back to the plaintext `agent.toml` field (mode 0600).
//! Android/iOS have no `keyring` backend — the same error path applies
//! (mobile viewers never store unattended passwords anyway).
//!
//! The password must be *recoverable* (the host re-runs Argon2id for every
//! challenge), so hashed storage is not an option — keychain storage keeps
//! it out of world-readable config files and out of backups.

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use keyring::Entry;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
const SERVICE: &str = "opacusr";
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const ACCOUNT: &str = "unattended";

/// Android/iOS build: no OS keychain crate is linked — every call
/// returns the same «unavailable» error so callers fall back to the
/// config file, exactly like a headless Linux box without Secret
/// Service.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn unavailable() -> String {
    "keychain unavailable on this platform".into()
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn store_password(_password: &str) -> Result<(), String> {
    Err(unavailable())
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn load_password() -> Result<Option<String>, String> {
    Err(unavailable())
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn delete_password() -> Result<(), String> {
    Err(unavailable())
}

/// Desktop build: real `keyring` backends.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn entry() -> Result<Entry, String> {
    Entry::new(SERVICE, ACCOUNT).map_err(|e| format!("keychain unavailable: {e}"))
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn store_password(password: &str) -> Result<(), String> {
    entry()?
        .set_password(password)
        .map_err(|e| format!("keychain store: {e}"))
}

/// Read the password back. `Ok(None)` when the entry does not exist.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn load_password() -> Result<Option<String>, String> {
    match entry()?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain read: {e}")),
    }
}

/// Remove the stored password. Missing entry counts as success.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn delete_password() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain delete: {e}")),
    }
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use super::*;
    use keyring::credential::{Credential, CredentialApi, CredentialBuilderApi};
    use keyring::Error as KeyringError;
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Mutex, Once};

    /// The stock `keyring::mock` store is per-`Entry` (each `Entry::new`
    /// yields a fresh instance), which cannot test a store/load round trip
    /// across separate entries. This builder keeps a process-wide map keyed
    /// by `(service, user)` — good enough for our module's contract.
    #[derive(Debug)]
    struct SharedStore {
        key: (String, String),
    }

    type SharedMap = HashMap<(String, String), Vec<u8>>;
    static STORE: Mutex<Option<SharedMap>> = Mutex::new(None);

    fn with_map<T>(f: impl FnOnce(&mut SharedMap) -> T) -> T {
        let mut guard = STORE.lock().expect("store mutex");
        f(guard.get_or_insert_with(HashMap::new))
    }

    impl CredentialApi for SharedStore {
        fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
            with_map(|m| m.insert(self.key.clone(), secret.to_vec()));
            Ok(())
        }
        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            match with_map(|m| m.get(&self.key).cloned()) {
                Some(bytes) => Ok(bytes),
                None => Err(KeyringError::NoEntry),
            }
        }
        fn delete_credential(&self) -> keyring::Result<()> {
            with_map(|m| m.remove(&self.key));
            Ok(())
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[derive(Debug, Default)]
    struct SharedStoreBuilder;

    impl CredentialBuilderApi for SharedStoreBuilder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<Credential>> {
            Ok(Box::new(SharedStore {
                key: (service.to_string(), user.to_string()),
            }))
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Force the shared mock store for the whole test binary.
    pub(crate) fn install_mock() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            keyring::set_default_credential_builder(Box::new(SharedStoreBuilder));
        });
    }

    #[test]
    fn mock_round_trip_and_delete() {
        install_mock();
        // Fresh slate for this service/user pair.
        let _ = delete_password();
        assert_eq!(load_password().unwrap(), None);

        store_password("s3cret-pass").unwrap();
        assert_eq!(load_password().unwrap().as_deref(), Some("s3cret-pass"));

        // Overwrite works.
        store_password("second").unwrap();
        assert_eq!(load_password().unwrap().as_deref(), Some("second"));

        delete_password().unwrap();
        assert_eq!(load_password().unwrap(), None);
        // Deleting again is fine (idempotent).
        delete_password().unwrap();
    }
}
