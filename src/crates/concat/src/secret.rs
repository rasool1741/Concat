// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The Gemini API key, kept in the operating system's credential store.
//!
//! Never in `settings.json`, a project file or a log: the key is read from
//! here when a call is made and held only for that call. Errors from the
//! store are plain words and never carry the key.

/// The name the entry is filed under in the store.
const SERVICE: &str = "Concat";
/// The entry's account name.
const ACCOUNT: &str = "gemini-api-key";

/// Somewhere a secret can be kept.
pub trait KeyStore {
    /// The saved key, or `None` when there is none.
    fn load(&self) -> Result<Option<String>, String>;
    /// Saves `key`, replacing any earlier one.
    fn save(&self, key: &str) -> Result<(), String>;
    /// Forgets the key; nothing saved is not an error.
    fn clear(&self) -> Result<(), String>;
}

/// The operating system's store: Credential Manager, the keychain, the
/// kernel keyring. Phones have none this app can reach, and say so.
pub struct OsKeyStore;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod os {
    use super::{ACCOUNT, KeyStore, OsKeyStore, SERVICE};

    fn entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| e.to_string())
    }

    impl KeyStore for OsKeyStore {
        fn load(&self) -> Result<Option<String>, String> {
            match entry()?.get_password() {
                Ok(key) if key.trim().is_empty() => Ok(None),
                Ok(key) => Ok(Some(key)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(error.to_string()),
            }
        }

        fn save(&self, key: &str) -> Result<(), String> {
            entry()?.set_password(key).map_err(|e| e.to_string())
        }

        fn clear(&self) -> Result<(), String> {
            match entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error.to_string()),
            }
        }
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
mod os {
    use super::{KeyStore, OsKeyStore};

    const UNAVAILABLE: &str = "no secure key store on this platform";

    impl KeyStore for OsKeyStore {
        fn load(&self) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn save(&self, _key: &str) -> Result<(), String> {
            Err(UNAVAILABLE.to_owned())
        }

        fn clear(&self) -> Result<(), String> {
            Ok(())
        }
    }
}

/// A store in memory, for tests.
#[cfg(test)]
#[derive(Default)]
pub struct MemoryKeyStore(std::sync::Mutex<Option<String>>);

#[cfg(test)]
impl KeyStore for MemoryKeyStore {
    fn load(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn save(&self, key: &str) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(key.to_owned());
        Ok(())
    }

    fn clear(&self) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_clear() {
        let store = MemoryKeyStore::default();
        assert_eq!(store.load(), Ok(None));
        store.save("abc").unwrap();
        assert_eq!(store.load(), Ok(Some("abc".to_owned())));
        store.clear().unwrap();
        assert_eq!(store.load(), Ok(None));
        store.clear().unwrap();
    }
}
