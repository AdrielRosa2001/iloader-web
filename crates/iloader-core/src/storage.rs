//! File-based storage replacing the OS keyring on a headless server.
//!
//! * [`FileStorage`]: one file per key (anisette state, pairing records,
//!   certificates), mode 0600 inside a 0700 directory.
//! * [`CredentialStore`]: list of saved Apple IDs, with passwords encrypted
//!   using ChaCha20-Poly1305 when the server has a key configured.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use base64::prelude::*;
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload},
};
use isideload::util::storage::SideloadingStorage;
use rootcause::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::AppError;

/// Creates `dir` (and parents) and restricts it to the current user.
pub fn create_private_dir(dir: &Path) -> Result<(), AppError> {
    fs::create_dir_all(dir)
        .map_err(|e| AppError::Filesystem("Failed to create data dir".into(), e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| {
            AppError::Filesystem("Failed to restrict data dir".into(), e.to_string())
        })?;
    }
    Ok(())
}

/// Writes `data` to `path` atomically with mode 0600.
pub fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Rejects keys that could escape the storage directory. Keys may be nested
/// with `/` (isideload stores certificate keys as `<email hash>/key`), but
/// every segment must be a plain name: no empty, `.`/`..` or hidden segments,
/// no absolute paths or backslashes.
pub fn is_safe_key(key: &str) -> bool {
    fn safe_segment(seg: &str) -> bool {
        !seg.is_empty()
            && !seg.starts_with('.')
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '@' | '+'))
    }
    !key.is_empty()
        && key.len() <= 200
        && key.split('/').count() <= 4
        && key.split('/').all(safe_segment)
}

#[derive(Clone)]
pub struct FileStorage {
    dir: Arc<PathBuf>,
}

impl FileStorage {
    pub fn new(dir: PathBuf) -> Result<Self, AppError> {
        create_private_dir(&dir)?;
        Ok(Self { dir: Arc::new(dir) })
    }

    fn path_for(&self, key: &str) -> Result<PathBuf, Report> {
        if !is_safe_key(key) {
            return Err(report!("Invalid storage key: {key:?}"));
        }
        Ok(self.dir.join(key))
    }

    pub fn exists(&self, key: &str) -> bool {
        matches!(self.retrieve_data(key), Ok(Some(_)))
    }
}

impl SideloadingStorage for FileStorage {
    fn store(&self, key: &str, value: &str) -> Result<(), Report> {
        self.store_data(key, value.as_bytes())
    }

    fn retrieve(&self, key: &str) -> Result<Option<String>, Report> {
        Ok(self
            .retrieve_data(key)?
            .map(|d| String::from_utf8_lossy(&d).into_owned()))
    }

    fn store_data(&self, key: &str, value: &[u8]) -> Result<(), Report> {
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent()
            && parent != self.dir.as_path()
        {
            create_private_dir(parent).map_err(|e| report!("{e}"))?;
        }
        write_private_file(&path, value).context("Failed to write data to file")?;
        Ok(())
    }

    fn retrieve_data(&self, key: &str) -> Result<Option<Vec<u8>>, Report> {
        let path = self.path_for(key)?;
        match fs::read(&path) {
            // an empty file is how isideload's FsStorage "deletes"; treat it as absent
            Ok(data) if data.is_empty() => Ok(None),
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(report!(e).context("Failed to read data from file").into()),
        }
    }

    fn delete(&self, key: &str) -> Result<(), Report> {
        let path = self.path_for(key)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(report!(e).context("Failed to delete file").into()),
        }
    }
}

/// Derives a 32-byte key from arbitrary key material (e.g. a key file).
pub fn derive_key(material: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"iloader-server credentials v1\0");
    h.update(material);
    h.finalize().into()
}

#[derive(Serialize, Deserialize, Default)]
struct AccountsFile {
    accounts: Vec<SavedAccount>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SavedAccount {
    email: String,
    nonce: String,
    ciphertext: String,
}

pub struct CredentialStore {
    path: PathBuf,
    key: Option<[u8; 32]>,
    lock: Mutex<()>,
}

impl CredentialStore {
    pub fn new(path: PathBuf, key: Option<[u8; 32]>) -> Self {
        Self {
            path,
            key,
            lock: Mutex::new(()),
        }
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    fn load(&self) -> Result<AccountsFile, AppError> {
        match fs::read(&self.path) {
            Ok(d) => serde_json::from_slice(&d).map_err(|e| {
                AppError::Storage("Saved accounts file is corrupt".into(), e.to_string())
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AccountsFile::default()),
            Err(e) => Err(AppError::Storage(
                "Failed to read saved accounts".into(),
                e.to_string(),
            )),
        }
    }

    fn save(&self, file: &AccountsFile) -> Result<(), AppError> {
        let data = serde_json::to_vec_pretty(file)
            .map_err(|e| AppError::Storage("Failed to encode accounts".into(), e.to_string()))?;
        write_private_file(&self.path, &data)
            .map_err(|e| AppError::Storage("Failed to write saved accounts".into(), e.to_string()))
    }

    fn cipher(&self) -> Result<ChaCha20Poly1305, AppError> {
        let key = self.key.ok_or_else(|| {
            AppError::KeyringWithMessage(
                "Saving credentials is disabled".into(),
                "no encryption key configured on the server (ILOADER_SECRET_KEY_FILE)".into(),
            )
        })?;
        Ok(ChaCha20Poly1305::new(&key.into()))
    }

    /// Emails with a saved password.
    pub fn list(&self) -> Result<Vec<String>, AppError> {
        let _g = self.lock.lock().unwrap();
        Ok(self.load()?.accounts.into_iter().map(|a| a.email).collect())
    }

    pub fn save_password(&self, email: &str, password: &str) -> Result<(), AppError> {
        let cipher = self.cipher()?;
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: password.as_bytes(),
                    aad: email.as_bytes(),
                },
            )
            .map_err(|_| AppError::Keyring("Failed to encrypt credentials".into()))?;
        let _g = self.lock.lock().unwrap();
        let mut file = self.load()?;
        file.accounts.retain(|a| a.email != email);
        file.accounts.push(SavedAccount {
            email: email.to_string(),
            nonce: BASE64_STANDARD.encode(nonce),
            ciphertext: BASE64_STANDARD.encode(ciphertext),
        });
        self.save(&file)
    }

    pub fn get_password(&self, email: &str) -> Result<String, AppError> {
        let cipher = self.cipher()?;
        let account = {
            let _g = self.lock.lock().unwrap();
            self.load()?
                .accounts
                .into_iter()
                .find(|a| a.email == email)
                .ok_or_else(|| {
                    AppError::KeyringWithMessage(
                        "Failed to get credentials".into(),
                        "No saved password for this account".into(),
                    )
                })?
        };
        let bad = |_| AppError::Keyring("Saved credentials are corrupt".into());
        let nonce = BASE64_STANDARD.decode(&account.nonce).map_err(bad)?;
        let ct = BASE64_STANDARD.decode(&account.ciphertext).map_err(bad)?;
        if nonce.len() != 12 {
            return Err(AppError::Keyring("Saved credentials are corrupt".into()));
        }
        let plain = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: email.as_bytes(),
                },
            )
            .map_err(|_| {
                AppError::Keyring(
                    "Failed to decrypt saved credentials (was the server key changed?)".into(),
                )
            })?;
        String::from_utf8(plain).map_err(|_| AppError::Keyring("Saved credentials are corrupt".into()))
    }

    pub fn delete(&self, email: &str) -> Result<(), AppError> {
        let _g = self.lock.lock().unwrap();
        let mut file = self.load()?;
        file.accounts.retain(|a| a.email != email);
        self.save(&file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_storage_roundtrip_delete_and_perms() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStorage::new(dir.path().join("st")).unwrap();
        assert_eq!(s.retrieve_data("anisette_state").unwrap(), None);
        s.store_data("anisette_state", b"abc").unwrap();
        assert_eq!(s.retrieve_data("anisette_state").unwrap(), Some(b"abc".to_vec()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = fs::metadata(dir.path().join("st/anisette_state")).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o600);
            let m = fs::metadata(dir.path().join("st")).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o700);
        }
        s.delete("anisette_state").unwrap();
        assert!(!s.exists("anisette_state"));
        s.delete("anisette_state").unwrap();
    }

    #[test]
    fn file_storage_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStorage::new(dir.path().to_path_buf()).unwrap();
        for k in [
            "../x", "", ".hidden", "..", "a\\b", "/abs", "a//b", "a/../b", "a/./b", "a/", "a/.k",
            "a/b/c/d/e",
        ] {
            assert!(s.store_data(k, b"x").is_err(), "{k:?} should be rejected");
        }
        assert!(is_safe_key("rppairing1.1_file_00008110-001A2B3C"));

        // isideload's certificate identity key: "<sha256 hex of email>/key"
        let k = "f0e4c04f981312f32df6e12ee2bdf53eadd1c91beb3d2211ac35c8be643c88f5/key";
        s.store_data(k, b"pem").unwrap();
        assert_eq!(s.retrieve_data(k).unwrap(), Some(b"pem".to_vec()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let sub = dir.path().join(k).parent().unwrap().to_path_buf();
            assert_eq!(fs::metadata(&sub).unwrap().permissions().mode() & 0o777, 0o700);
        }
        s.delete(k).unwrap();
        assert_eq!(s.retrieve_data(k).unwrap(), None);
    }

    #[test]
    fn credentials_encrypt_and_require_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        let store = CredentialStore::new(path.clone(), Some(derive_key(b"k1")));
        store.save_password("a@b.c", "hunter2").unwrap();
        assert_eq!(store.list().unwrap(), vec!["a@b.c".to_string()]);
        assert_eq!(store.get_password("a@b.c").unwrap(), "hunter2");
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("hunter2"));

        let wrong = CredentialStore::new(path.clone(), Some(derive_key(b"k2")));
        assert!(wrong.get_password("a@b.c").is_err());
        let none = CredentialStore::new(path.clone(), None);
        assert!(none.save_password("x@y.z", "p").is_err());

        store.delete("a@b.c").unwrap();
        assert!(store.list().unwrap().is_empty());
    }
}
