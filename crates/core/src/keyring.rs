// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Querora Contributors

//! Credential storage. Secrets live in the macOS Keychain (service
//! `dev.querora.credentials`), never in SQLite, never in agent context.
//!
//! [`CredentialStore`] is the seam; [`MemoryStore`] backs tests and any
//! environment where no OS keychain is available (CI containers).

use std::collections::HashMap;
use std::sync::Mutex;
use thiserror::Error;

/// Errors from credential storage backends.
#[derive(Debug, Error)]
pub enum CoreError {
    /// The OS keychain rejected the operation.
    #[error("keychain error: {0}")]
    Keyring(String),
    /// SQLite app-db failure.
    #[error("storage error: {0}")]
    Storage(String),
}

/// Secret storage seam. Implementations must never log secret values.
pub trait CredentialStore: Send + Sync {
    /// Store (upsert) a secret under `account`.
    fn set(&self, account: &str, secret: &str) -> Result<(), CoreError>;
    /// Fetch a secret; `None` when absent.
    fn get(&self, account: &str) -> Result<Option<String>, CoreError>;
    /// Remove a secret; absent is not an error.
    fn delete(&self, account: &str) -> Result<(), CoreError>;
}

/// Keychain service name for all Querora secrets.
pub const SERVICE: &str = "dev.querora.credentials";

/// macOS Keychain-backed store (`security-framework` via the `keyring` crate).
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    /// Store bound to the Querora service.
    pub fn new() -> Self {
        Self {
            service: SERVICE.to_string(),
        }
    }

    /// Probe whether the OS keychain is usable (cheap write/read/delete).
    pub fn available(&self) -> bool {
        let probe = format!("probe.{}", uuid::Uuid::new_v4());
        self.set(&probe, "1").is_ok() && matches!(self.get(&probe), Ok(Some(ref s)) if s == "1")
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, CoreError> {
        keyring::Entry::new(&self.service, account).map_err(|e| CoreError::Keyring(e.to_string()))
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for KeychainStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CoreError> {
        let entry = self.entry(account)?;
        entry
            .set_password(secret)
            .map_err(|e| CoreError::Keyring(e.to_string()))
    }

    fn get(&self, account: &str) -> Result<Option<String>, CoreError> {
        let entry = self.entry(account)?;
        match entry.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CoreError::Keyring(e.to_string())),
        }
    }

    fn delete(&self, account: &str) -> Result<(), CoreError> {
        let entry = self.entry(account)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CoreError::Keyring(e.to_string())),
        }
    }
}

/// In-memory store: tests and keychain-less environments.
#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<String, String>>,
}

impl CredentialStore for MemoryStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CoreError> {
        self.map
            .lock()
            .expect("memory store poisoned")
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>, CoreError> {
        Ok(self
            .map
            .lock()
            .expect("memory store poisoned")
            .get(account)
            .cloned())
    }

    fn delete(&self, account: &str) -> Result<(), CoreError> {
        self.map
            .lock()
            .expect("memory store poisoned")
            .remove(account);
        Ok(())
    }
}

/// 0600 JSON file at `~/.querora/run/secrets.json` (dir 0700) — the dev
/// fallback for source secrets.
///
/// Rationale: unsigned dev builds get a fresh ad-hoc signature on every
/// rebuild, so keychain items created by a previous build require a GUI
/// authorization prompt to read — which blocks app commands (add/test
/// source) and startup. The file keeps the same user-only boundary.
/// Signed release builds keep the Keychain primary; this remains the
/// error fallback only.
pub struct FileStore {
    path: std::path::PathBuf,
}

impl Default for FileStore {
    fn default() -> Self {
        Self {
            path: crate::paths::run_dir().join("secrets.json"),
        }
    }
}

impl FileStore {
    /// Store bound to the default path.
    pub fn new() -> Self {
        Self::default()
    }

    fn load(&self) -> HashMap<String, String> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, map: &HashMap<String, String>) -> Result<(), CoreError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| CoreError::Keyring(e.to_string()))?;
        }
        std::fs::write(&self.path, serde_json::to_string(map).unwrap_or_default())
            .map_err(|e| CoreError::Keyring(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| CoreError::Keyring(e.to_string()))?;
        }
        Ok(())
    }
}

impl CredentialStore for FileStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CoreError> {
        let mut map = self.load();
        map.insert(account.to_string(), secret.to_string());
        self.save(&map)
    }

    fn get(&self, account: &str) -> Result<Option<String>, CoreError> {
        Ok(self.load().get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<(), CoreError> {
        let mut map = self.load();
        map.remove(account);
        self.save(&map)
    }
}

/// Primary store first, fallback on error/miss. `get` never blocks on the
/// primary twice for missing items (keychain miss = fast `NoEntry`).
pub struct ChainedStore {
    primary: Box<dyn CredentialStore>,
    fallback: Box<dyn CredentialStore>,
}

impl ChainedStore {
    /// Chain two stores.
    pub fn new(primary: Box<dyn CredentialStore>, fallback: Box<dyn CredentialStore>) -> Self {
        Self { primary, fallback }
    }
}

impl CredentialStore for ChainedStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), CoreError> {
        self.primary
            .set(account, secret)
            .or_else(|_| self.fallback.set(account, secret))
    }

    fn get(&self, account: &str) -> Result<Option<String>, CoreError> {
        match self.primary.get(account) {
            Ok(Some(s)) => Ok(Some(s)),
            Ok(None) => self.fallback.get(account),
            Err(_) => self.fallback.get(account),
        }
    }

    fn delete(&self, account: &str) -> Result<(), CoreError> {
        let a = self.primary.delete(account);
        let b = self.fallback.delete(account);
        a.and(b)
    }
}

/// Default store: dev builds prefer the file (no keychain prompt churn),
/// release builds prefer the OS keychain with the file as error fallback.
pub fn default_credential_store() -> Box<dyn CredentialStore> {
    let kc: Box<dyn CredentialStore> = Box::new(KeychainStore::new());
    let file: Box<dyn CredentialStore> = Box::new(FileStore::new());
    if cfg!(debug_assertions) {
        Box::new(ChainedStore::new(file, kc))
    } else {
        Box::new(ChainedStore::new(kc, file))
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;

    #[test]
    fn file_store_crud_and_permissions() {
        let s = FileStore {
            path: std::env::temp_dir().join(format!("qr-secrets-{}.json", uuid::Uuid::new_v4())),
        };
        assert_eq!(s.get("src").unwrap(), None);
        s.set("src", "pw").unwrap();
        assert_eq!(s.get("src").unwrap().as_deref(), Some("pw"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&s.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        s.delete("src").unwrap();
        assert_eq!(s.get("src").unwrap(), None);
        std::fs::remove_file(&s.path).ok();
    }

    #[test]
    fn chained_prefers_primary() {
        let s = ChainedStore::new(
            Box::new(MemoryStore::default()),
            Box::new(MemoryStore::default()),
        );
        s.set("a", "1").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("1"));
        s.delete("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_crud() {
        let s = MemoryStore::default();
        assert_eq!(s.get("src1").unwrap(), None);
        s.set("src1", "hunter2").unwrap();
        assert_eq!(s.get("src1").unwrap().as_deref(), Some("hunter2"));
        s.set("src1", "rotated").unwrap();
        assert_eq!(s.get("src1").unwrap().as_deref(), Some("rotated"));
        s.delete("src1").unwrap();
        assert_eq!(s.get("src1").unwrap(), None);
        // delete-missing is not an error
        s.delete("never-existed").unwrap();
    }
}

/// Keychain round-trip — opt-in (real keychain prompt-free on CI macs, but
/// keep it explicit): run with `QUERORA_IT_KEYCHAIN=1 cargo test -p querora-core keychain`.
#[test]
#[cfg(target_os = "macos")]
fn keychain_store_round_trip() {
    if std::env::var("QUERORA_IT_KEYCHAIN").ok().as_deref() != Some("1") {
        return;
    }
    let s = KeychainStore::new();
    let acct = format!("test.{}", uuid::Uuid::new_v4());
    s.set(&acct, "secret-value").unwrap();
    assert_eq!(s.get(&acct).unwrap().as_deref(), Some("secret-value"));
    s.delete(&acct).unwrap();
    assert_eq!(s.get(&acct).unwrap(), None);
}
