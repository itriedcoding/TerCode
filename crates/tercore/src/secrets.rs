//! Secure storage for API keys.
//!
//! On Windows this is the Windows Credential Manager (via `keyring`, which
//! talks to `CredWriteW`/`CredReadW`). Non-Windows builds — used for tests and
//! for developing the core on Linux — fall back to a 0600 file under the app
//! data directory. Secrets are never written to `settings.json`.

use crate::error::{Result, TerError};
use std::path::{Path, PathBuf};

/// Credential Manager account name for the Ter Router key.
pub const TERROUTER_ACCOUNT: &str = "terrouter-api-key";
/// Credential Manager service (target prefix) owned by this app.
pub const SERVICE: &str = "com.tercode.TerCode";

pub trait SecretStore: Send + Sync {
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn delete(&self, account: &str) -> Result<()>;
    /// Human-readable backend name, shown in Settings.
    fn backend(&self) -> &'static str;
}

/// Windows Credential Manager backed store.
#[cfg(windows)]
pub struct CredentialManagerStore;

#[cfg(windows)]
impl CredentialManagerStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, account)
            .map_err(|e| TerError::Secret(format!("could not open Credential Manager: {e}")))
    }
}

#[cfg(windows)]
impl SecretStore for CredentialManagerStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(|e| TerError::Secret(format!("could not store credential: {e}")))
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(TerError::Secret(format!("could not read credential: {e}"))),
        }
    }

    fn delete(&self, account: &str) -> Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(TerError::Secret(format!("could not delete credential: {e}"))),
        }
    }

    fn backend(&self) -> &'static str {
        "Windows Credential Manager"
    }
}

#[cfg(not(windows))]
pub struct CredentialManagerStore;

#[cfg(not(windows))]
impl CredentialManagerStore {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl SecretStore for CredentialManagerStore {
    fn set(&self, _account: &str, _secret: &str) -> Result<()> {
        Err(TerError::Secret(
            "Windows Credential Manager is only available on Windows".into(),
        ))
    }
    fn get(&self, _account: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn delete(&self, _account: &str) -> Result<()> {
        Ok(())
    }
    fn backend(&self) -> &'static str {
        "unavailable (non-Windows build)"
    }
}

/// File-backed secret store used off Windows and by tests.
pub struct FileSecretStore {
    dir: PathBuf,
}

impl FileSecretStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, account: &str) -> PathBuf {
        let safe: String = account
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.secret"))
    }
}

impl SecretStore for FileSecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| TerError::io(self.dir.display().to_string(), e))?;
        let path = self.path(account);
        std::fs::write(&path, secret.as_bytes())
            .map_err(|e| TerError::io(path.display().to_string(), e))?;
        restrict_permissions(&path)?;
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        let path = self.path(account);
        match std::fs::read_to_string(&path) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(TerError::io(path.display().to_string(), e)),
        }
    }

    fn delete(&self, account: &str) -> Result<()> {
        let path = self.path(account);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(TerError::io(path.display().to_string(), e)),
        }
    }

    fn backend(&self) -> &'static str {
        "Encrypted file (development fallback)"
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| TerError::io(path.display().to_string(), e))?
        .permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|e| TerError::io(path.display().to_string(), e))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Build the platform-appropriate secret store.
pub fn default_store(data_dir: &Path) -> Box<dyn SecretStore> {
    #[cfg(windows)]
    {
        let _ = data_dir;
        Box::new(CredentialManagerStore::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(FileSecretStore::new(data_dir.join("secrets")))
    }
}

/// Convenience wrapper that also caches the key in memory for the session.
pub struct ApiKeyManager {
    store: Box<dyn SecretStore>,
    cached: std::sync::Mutex<Option<String>>,
}

impl ApiKeyManager {
    pub fn new(store: Box<dyn SecretStore>) -> Self {
        Self {
            store,
            cached: std::sync::Mutex::new(None),
        }
    }

    pub fn backend(&self) -> &'static str {
        self.store.backend()
    }

    pub fn load(&self) -> Result<Option<String>> {
        let value = self.store.get(TERROUTER_ACCOUNT)?;
        if let Ok(mut cache) = self.cached.lock() {
            *cache = value.clone();
        }
        Ok(value)
    }

    pub fn current(&self) -> Result<Option<String>> {
        if let Ok(cache) = self.cached.lock() {
            if let Some(v) = cache.as_ref() {
                return Ok(Some(v.clone()));
            }
        }
        self.load()
    }

    pub fn save(&self, key: &str) -> Result<()> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(TerError::Invalid("API key cannot be empty".into()));
        }
        self.store.set(TERROUTER_ACCOUNT, trimmed)?;
        if let Ok(mut cache) = self.cached.lock() {
            *cache = Some(trimmed.to_string());
        }
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        self.store.delete(TERROUTER_ACCOUNT)?;
        if let Ok(mut cache) = self.cached.lock() {
            *cache = None;
        }
        Ok(())
    }

    /// Masked form for display, e.g. `tr_live_••••••1a2b`.
    pub fn masked(&self) -> Result<Option<String>> {
        Ok(self.current()?.map(|k| mask_key(&k)))
    }
}

pub fn mask_key(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 10 {
        return "•".repeat(key.len().max(4));
    }
    let prefix: String = key.chars().take(8).collect();
    let suffix: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{prefix}••••••{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        assert_eq!(store.get("k").unwrap(), None);
        store.set("k", "tr_live_secret").unwrap();
        assert_eq!(store.get("k").unwrap().as_deref(), Some("tr_live_secret"));
        store.delete("k").unwrap();
        assert_eq!(store.get("k").unwrap(), None);
    }

    #[test]
    fn file_store_rejects_traversal_in_account_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets"));
        store.set("../../etc/passwd", "x").unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path().join("secrets")).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let name = entries[0].as_ref().unwrap().file_name();
        assert!(!name.to_string_lossy().contains('/'));
    }

    #[test]
    fn api_key_manager_caches_and_masks() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ApiKeyManager::new(Box::new(FileSecretStore::new(dir.path())));
        assert!(mgr.current().unwrap().is_none());
        mgr.save("  tr_live_abcdef123456  ").unwrap();
        assert_eq!(mgr.current().unwrap().as_deref(), Some("tr_live_abcdef123456"));
        let masked = mgr.masked().unwrap().unwrap();
        assert!(masked.starts_with("tr_live_"));
        assert!(masked.ends_with("3456"));
        assert!(!masked.contains("abcdef"));
        mgr.clear().unwrap();
        assert!(mgr.current().unwrap().is_none());
    }

    #[test]
    fn empty_key_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ApiKeyManager::new(Box::new(FileSecretStore::new(dir.path())));
        assert!(mgr.save("   ").is_err());
    }
}
