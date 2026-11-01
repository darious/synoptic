//! Credentials in the system keyring, shared with the reference.
//!
//! The reference (`surveillance/credentials.py`) uses python-keyring,
//! which on Linux is the freedesktop Secret Service, with service name
//! [`KEYRING_SERVICE`] and two entries per profile: user
//! `"<profile>:username"` holding the account name and
//! `"<profile>:password"` holding the password. [`KeyringStore`] reads
//! and writes exactly those, so a password saved by either client is
//! found by the other.
//!
//! The secret-service backend talks D-Bus and may block on an unlock
//! prompt; call it off the UI thread (and off a tokio worker: use
//! `spawn_blocking`).

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use keyring::Entry;

/// python-keyring service name the reference stores under.
pub const KEYRING_SERVICE: &str = "surveillance-station";

/// Which half of a profile's credentials a keyring entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialField {
    Username,
    Password,
}

impl CredentialField {
    fn suffix(self) -> &'static str {
        match self {
            CredentialField::Username => "username",
            CredentialField::Password => "password",
        }
    }
}

/// The keyring *user* name of one entry: `"<profile>:username"` or
/// `"<profile>:password"`, as the reference names them.
pub fn keyring_user(profile: &str, field: CredentialField) -> String {
    format!("{profile}:{}", field.suffix())
}

/// A profile's login. `Debug` never shows the password.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Why the keyring could not be used. Messages never contain a secret.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// No Secret Service on the bus, no bus, or the collection is locked
    /// and could not be unlocked: remembering credentials is a
    /// convenience, callers carry on without it.
    #[error("system keyring unavailable: {0}")]
    Unavailable(String),
    /// More than one item matched; the keyring needs cleaning up.
    #[error("more than one keyring item for profile {profile:?}")]
    Ambiguous { profile: String },
    /// Any other backend failure.
    #[error("keyring error: {0}")]
    Backend(String),
}

impl SecretError {
    fn from_keyring(profile: &str, e: keyring::Error) -> Self {
        match e {
            keyring::Error::NoStorageAccess(e) | keyring::Error::PlatformFailure(e) => {
                SecretError::Unavailable(e.to_string())
            }
            keyring::Error::Ambiguous(_) => SecretError::Ambiguous {
                profile: profile.to_string(),
            },
            other => SecretError::Backend(other.to_string()),
        }
    }
}

/// Where credentials live. [`KeyringStore`] for the real thing,
/// [`MemoryStore`] for tests and for running without a keyring.
pub trait SecretStore: Send + Sync {
    /// Both halves, or `None` when either is missing (as the reference).
    fn get(&self, profile: &str) -> Result<Option<Credentials>, SecretError>;
    fn store(&self, profile: &str, credentials: &Credentials) -> Result<(), SecretError>;
    /// Removing what is not there is not an error.
    fn delete(&self, profile: &str) -> Result<(), SecretError>;
}

/// The freedesktop Secret Service, through the `keyring` crate's
/// secret-service backend (chosen explicitly, whatever else the crate
/// was built with, since that is the store the reference uses).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    /// Linux/BSD: the Secret Service backend explicitly, whatever else the
    /// crate was built with.
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    fn entry(profile: &str, field: CredentialField) -> Result<Entry, SecretError> {
        let user = keyring_user(profile, field);
        let credential = keyring::secret_service::default_credential_builder()
            .build(None, KEYRING_SERVICE, &user)
            .map_err(|e| SecretError::from_keyring(profile, e))?;
        Ok(Entry::new_with_credential(credential))
    }

    /// macOS Keychain / Windows Credential Manager: the platform default.
    #[cfg(not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    fn entry(profile: &str, field: CredentialField) -> Result<Entry, SecretError> {
        let user = keyring_user(profile, field);
        Entry::new(KEYRING_SERVICE, &user).map_err(|e| SecretError::from_keyring(profile, e))
    }

    fn read(profile: &str, field: CredentialField) -> Result<Option<String>, SecretError> {
        match Self::entry(profile, field)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::from_keyring(profile, e)),
        }
    }

    fn remove(profile: &str, field: CredentialField) -> Result<(), SecretError> {
        match Self::entry(profile, field)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::from_keyring(profile, e)),
        }
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, profile: &str) -> Result<Option<Credentials>, SecretError> {
        let Some(username) = Self::read(profile, CredentialField::Username)? else {
            return Ok(None);
        };
        let Some(password) = Self::read(profile, CredentialField::Password)? else {
            return Ok(None);
        };
        Ok(Some(Credentials { username, password }))
    }

    fn store(&self, profile: &str, credentials: &Credentials) -> Result<(), SecretError> {
        Self::entry(profile, CredentialField::Username)?
            .set_password(&credentials.username)
            .map_err(|e| SecretError::from_keyring(profile, e))?;
        Self::entry(profile, CredentialField::Password)?
            .set_password(&credentials.password)
            .map_err(|e| SecretError::from_keyring(profile, e))
    }

    fn delete(&self, profile: &str) -> Result<(), SecretError> {
        // Try both even if the first fails, as the reference does.
        let first = Self::remove(profile, CredentialField::Username);
        let second = Self::remove(profile, CredentialField::Password);
        first.and(second)
    }
}

/// An in-memory store: nothing persists past the process.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, Credentials>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, profile: &str) -> Result<Option<Credentials>, SecretError> {
        Ok(self.entries.lock().expect("poisoned").get(profile).cloned())
    }

    fn store(&self, profile: &str, credentials: &Credentials) -> Result<(), SecretError> {
        self.entries
            .lock()
            .expect("poisoned")
            .insert(profile.to_string(), credentials.clone());
        Ok(())
    }

    fn delete(&self, profile: &str) -> Result<(), SecretError> {
        self.entries.lock().expect("poisoned").remove(profile);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_named_as_the_reference_names_them() {
        assert_eq!(KEYRING_SERVICE, "surveillance-station");
        assert_eq!(
            keyring_user("home-nas", CredentialField::Username),
            "home-nas:username"
        );
        assert_eq!(
            keyring_user("home-nas", CredentialField::Password),
            "home-nas:password"
        );
    }

    #[test]
    fn debug_never_shows_the_password() {
        let c = Credentials {
            username: "admin".into(),
            password: "hunter2".into(),
        };
        let shown = format!("{c:?}");
        assert!(shown.contains("admin"));
        assert!(!shown.contains("hunter2"));
    }

    #[test]
    fn memory_store_round_trips_and_needs_both_halves() {
        let store = MemoryStore::new();
        assert_eq!(store.get("nas").unwrap(), None);
        let c = Credentials {
            username: "u".into(),
            password: "p".into(),
        };
        store.store("nas", &c).unwrap();
        assert_eq!(store.get("nas").unwrap(), Some(c));
        store.delete("nas").unwrap();
        store.delete("nas").unwrap();
        assert_eq!(store.get("nas").unwrap(), None);
    }

    /// Reading a profile nobody has stored is the one keyring call that is
    /// safe to make in a test: it either finds nothing or reports that
    /// there is no keyring (headless, no Secret Service on the bus). It
    /// must never panic or hang, and never be anything but those two.
    #[test]
    fn keyring_store_reports_absence_or_unavailability() {
        let profile = format!("ss-config-test-{}", std::process::id());
        match KeyringStore.get(&profile) {
            Ok(None) | Err(SecretError::Unavailable(_)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
