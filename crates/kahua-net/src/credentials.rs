//! Storage for the Open Cloud API key and the Roblox cookie.
//!
//! Secrets go to the system keyring (service `kahua`). When the keyring is not
//! available, they go to `~/.config/kahua/credentials.toml`, which is created
//! with mode 0600 on Unix.

use std::fmt;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

const SERVICE: &str = "kahua";
const API_KEY: &str = "api_key";
const COOKIE: &str = "cookie";

/// The stored secrets. `Debug` never prints their values.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("cookie", &self.cookie.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Credentials {
    /// Loads from the default store. See [`CredentialStore::default_location`].
    pub fn load() -> Result<Self> {
        CredentialStore::default_location()?.load()
    }

    /// Saves to the default store.
    pub fn save(&self) -> Result<()> {
        CredentialStore::default_location()?.save(self)
    }

    /// Removes all stored secrets from the default store.
    pub fn clear() -> Result<()> {
        CredentialStore::default_location()?.clear()
    }
}

/// Where credentials are kept.
pub struct CredentialStore {
    file: PathBuf,
    backend: Box<dyn SecretBackend>,
}

impl fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialStore").field("file", &self.file).finish_non_exhaustive()
    }
}

impl CredentialStore {
    /// The system keyring, with `~/.config/kahua/credentials.toml` as fallback.
    pub fn default_location() -> Result<Self> {
        Ok(Self { file: default_file_path()?, backend: Box::new(KeyringBackend) })
    }

    /// Keeps all secrets in the given file and never uses the keyring.
    pub fn file_only(file: impl Into<PathBuf>) -> Self {
        Self { file: file.into(), backend: Box::new(UnavailableBackend) }
    }

    /// The fallback file used by this store.
    pub fn file_path(&self) -> &Path {
        &self.file
    }

    /// Reads the secrets. Keyring values take priority over the file.
    pub fn load(&self) -> Result<Credentials> {
        let stored = self.read_file()?;
        Ok(Credentials { api_key: self.lookup(API_KEY, stored.api_key), cookie: self.lookup(COOKIE, stored.cookie) })
    }

    /// Writes the secrets to the keyring. Any value the keyring cannot hold
    /// goes to the fallback file instead.
    pub fn save(&self, credentials: &Credentials) -> Result<()> {
        let in_file = Credentials {
            api_key: self.store(API_KEY, credentials.api_key.as_deref()),
            cookie: self.store(COOKIE, credentials.cookie.as_deref()),
        };
        if in_file.api_key.is_none() && in_file.cookie.is_none() {
            remove_file_if_exists(&self.file)
        } else {
            self.write_file(&in_file)
        }
    }

    /// Removes the secrets from the keyring and the fallback file.
    pub fn clear(&self) -> Result<()> {
        for name in [API_KEY, COOKIE] {
            if let Err(err) = self.backend.delete(name) {
                log::warn!("could not remove {name} from the system keyring: {err}");
            }
        }
        remove_file_if_exists(&self.file)
    }

    fn lookup(&self, name: &str, from_file: Option<String>) -> Option<String> {
        match self.backend.get(name) {
            Ok(Some(value)) => Some(value),
            Ok(None) => from_file,
            Err(err) => {
                log::debug!("system keyring lookup for {name} failed: {err}");
                from_file
            }
        }
    }

    /// Stores one secret and returns it if it must go to the file.
    fn store(&self, name: &str, value: Option<&str>) -> Option<String> {
        let Some(value) = value else {
            if let Err(err) = self.backend.delete(name) {
                log::debug!("removing {name} from the system keyring failed: {err}");
            }
            return None;
        };
        match self.backend.set(name, value) {
            Ok(()) => None,
            Err(err) => {
                log::warn!(
                    "system keyring unavailable ({err}); storing {name} in {} with 0600 permissions",
                    self.file.display()
                );
                // Remove a stale keyring value so the file copy is the one read back.
                let _ = self.backend.delete(name);
                Some(value.to_owned())
            }
        }
    }

    fn read_file(&self) -> Result<Credentials> {
        match fs::read_to_string(&self.file) {
            Ok(text) => toml::from_str(&text).map_err(|_| {
                // The parser error can quote file contents, so it is not included.
                anyhow!("credentials file {} is not valid; delete it and sign in again", self.file.display())
            }),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Credentials::default()),
            Err(err) => Err(err).with_context(|| format!("reading credentials file {} failed", self.file.display())),
        }
    }

    fn write_file(&self, credentials: &Credentials) -> Result<()> {
        let text = toml::to_string(credentials).context("encoding credentials failed")?;
        if let Some(parent) = self.file.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {} failed", parent.display()))?;
        }

        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.file)
            .with_context(|| format!("writing credentials file {} failed", self.file.display()))?;
        // `mode` only applies when the file is created, so fix an existing file too.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .context("restricting credentials file permissions failed")?;
        }
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| format!("writing credentials file {} failed", self.file.display()))
    }
}

/// Backend for secret storage. Tests substitute an in-memory one.
trait SecretBackend: Send + Sync {
    fn get(&self, name: &str) -> keyring::Result<Option<String>>;
    fn set(&self, name: &str, value: &str) -> keyring::Result<()>;
    fn delete(&self, name: &str) -> keyring::Result<()>;
}

struct KeyringBackend;

impl SecretBackend for KeyringBackend {
    fn get(&self, name: &str) -> keyring::Result<Option<String>> {
        match keyring::Entry::new(SERVICE, name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn set(&self, name: &str, value: &str) -> keyring::Result<()> {
        keyring::Entry::new(SERVICE, name)?.set_password(value)
    }

    fn delete(&self, name: &str) -> keyring::Result<()> {
        match keyring::Entry::new(SERVICE, name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

/// Stands in for a keyring that is absent or disabled.
struct UnavailableBackend;

impl SecretBackend for UnavailableBackend {
    fn get(&self, _name: &str) -> keyring::Result<Option<String>> {
        Err(unavailable())
    }

    fn set(&self, _name: &str, _value: &str) -> keyring::Result<()> {
        Err(unavailable())
    }

    fn delete(&self, _name: &str) -> keyring::Result<()> {
        Ok(())
    }
}

fn unavailable() -> keyring::Error {
    keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("keyring is not in use")))
}

fn default_file_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not find the home directory")?;
    Ok(home.join(".config").join("kahua").join("credentials.toml"))
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("removing {} failed", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// An in-memory keyring.
    #[derive(Default)]
    struct MemoryBackend(Mutex<HashMap<String, String>>);

    impl SecretBackend for MemoryBackend {
        fn get(&self, name: &str) -> keyring::Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(name).cloned())
        }

        fn set(&self, name: &str, value: &str) -> keyring::Result<()> {
            self.0.lock().unwrap().insert(name.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete(&self, name: &str) -> keyring::Result<()> {
            self.0.lock().unwrap().remove(name);
            Ok(())
        }
    }

    fn sample() -> Credentials {
        Credentials { api_key: Some("key-123".to_owned()), cookie: Some("cookie-456".to_owned()) }
    }

    #[test]
    fn file_fallback_round_trips_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("credentials.toml");
        let store = CredentialStore::file_only(&path);

        store.save(&sample()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, sample());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        store.clear().unwrap();
        assert!(!path.exists());
        assert_eq!(store.load().unwrap(), Credentials::default());
    }

    #[test]
    fn existing_file_permissions_are_tightened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        CredentialStore::file_only(&path).save(&sample()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn keyring_is_preferred_and_file_stays_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let store = CredentialStore { file: path.clone(), backend: Box::new(MemoryBackend::default()) };

        store.save(&sample()).unwrap();
        assert!(!path.exists());
        assert_eq!(store.load().unwrap(), sample());
    }

    #[test]
    fn keyring_failure_falls_back_to_file_and_drops_stale_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let backend = MemoryBackend::default();
        backend.set(API_KEY, "stale-key").unwrap();

        // Keyring writes fail, so every secret must go to the file.
        struct Failing(MemoryBackend);
        impl SecretBackend for Failing {
            fn get(&self, name: &str) -> keyring::Result<Option<String>> {
                self.0.get(name)
            }
            fn set(&self, _: &str, _: &str) -> keyring::Result<()> {
                Err(unavailable())
            }
            fn delete(&self, name: &str) -> keyring::Result<()> {
                self.0.delete(name)
            }
        }

        let store = CredentialStore { file: path.clone(), backend: Box::new(Failing(backend)) };
        store.save(&sample()).unwrap();
        assert!(path.exists());
        assert_eq!(store.load().unwrap(), sample());
    }

    #[test]
    fn invalid_file_error_does_not_echo_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        fs::write(&path, "api_key = \"SECRET-VALUE-SHOULD-NOT-LEAK\"\ncookie = [").unwrap();
        let err = CredentialStore::file_only(&path).load().unwrap_err();
        assert!(!format!("{err:#}").contains("SECRET-VALUE"));
    }

    #[test]
    fn debug_redacts_secrets() {
        let text = format!("{:?}", sample());
        assert!(!text.contains("key-123"));
        assert!(!text.contains("cookie-456"));
        assert!(text.contains("<redacted>"));
        assert!(format!("{:?}", Credentials::default()).contains("None"));
    }
}
