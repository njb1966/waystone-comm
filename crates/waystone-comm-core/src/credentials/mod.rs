//! Credential manager — secure local storage for passwords, tokens, and SSH keys.
//!
//! # Storage backends (tried in order)
//! 1. **OS keychain** — `libsecret` (Linux), `Keychain` (macOS), `Credential Manager` (Windows)
//!    via the `keyring` crate.  One entry per credential; account name is the UUID string.
//! 2. **Encrypted SQLite fallback** — `~/.config/waystone-comm/credentials.db`.
//!    The secret field is encrypted with AES-256-GCM using a per-installation key stored in
//!    `~/.config/waystone-comm/machine.key`.
//!
//! Credentials are **never** stored in plaintext in the dialing directory or config files.
//! `DirectoryEntry.credential_id` holds a UUID that is looked up here at connection time.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use thiserror::Error;
use uuid::Uuid;
use zeroize::ZeroizeOnDrop;

use crate::fsutil::atomic_write_private;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encryption error")]
    Encrypt,
    #[error("decryption error — wrong key or corrupt data")]
    Decrypt,
    #[error("credential not found")]
    NotFound,
}

// ── Secret string ─────────────────────────────────────────────────────────────

/// A string that is zeroed from memory when dropped.
#[derive(Clone, ZeroizeOnDrop)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Access the plaintext secret.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString(***)")
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// ── Credential types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Password,
    Token,
    SshKey,
}

impl fmt::Display for CredentialKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CredentialKind::Password => write!(f, "Password"),
            CredentialKind::Token => write!(f, "Token"),
            CredentialKind::SshKey => write!(f, "SSH Key"),
        }
    }
}

/// A full credential record including the plaintext secret.
#[derive(Debug, Clone)]
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub kind: CredentialKind,
    pub username: Option<String>,
    pub secret: SecretString,
}

impl Credential {
    pub fn new(
        name: impl Into<String>,
        kind: CredentialKind,
        username: Option<String>,
        secret: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            username,
            secret: SecretString::new(secret),
        }
    }
}

/// A credential record without the secret — safe for display in lists.
#[derive(Debug, Clone)]
pub struct CredentialSummary {
    pub id: Uuid,
    pub name: String,
    pub kind: CredentialKind,
    pub username: Option<String>,
}

// ── SSH key generation ────────────────────────────────────────────────────────

/// Generate a new Ed25519 SSH keypair.
///
/// Returns `(private_key_credential, public_key_openssh_string)`.
///
/// # Errors
/// Returns an error if key generation or encoding fails.
pub fn generate_ssh_keypair(
    name: impl Into<String>,
    comment: impl Into<String>,
) -> Result<(Credential, String), CredentialError> {
    use ssh_key::{Algorithm, PrivateKey};

    let name = name.into();
    let comment = comment.into();

    let key = PrivateKey::random(&mut rand::rngs::OsRng, Algorithm::Ed25519)
        .map_err(|_| CredentialError::Encrypt)?;

    let private_pem = key
        .to_openssh(ssh_key::LineEnding::LF)
        .map_err(|_| CredentialError::Encrypt)?;

    let public_openssh = key
        .public_key()
        .to_openssh()
        .map_err(|_| CredentialError::Encrypt)?;

    let public_with_comment = if comment.is_empty() {
        public_openssh
    } else {
        format!("{public_openssh} {comment}")
    };

    let cred = Credential::new(name, CredentialKind::SshKey, None, private_pem.as_str());

    Ok((cred, public_with_comment))
}

/// Derive the OpenSSH public key string from a stored OpenSSH private key.
///
/// # Errors
/// Returns an error if the private key cannot be parsed or encoded.
pub fn public_key_from_private_openssh(
    private_openssh: &str,
    comment: impl Into<String>,
) -> Result<String, CredentialError> {
    let key =
        ssh_key::PrivateKey::from_openssh(private_openssh).map_err(|_| CredentialError::Decrypt)?;
    let public_openssh = key
        .public_key()
        .to_openssh()
        .map_err(|_| CredentialError::Decrypt)?;
    let comment = comment.into();
    if comment.is_empty() {
        Ok(public_openssh)
    } else {
        Ok(format!("{public_openssh} {comment}"))
    }
}

// ── Credential manager ────────────────────────────────────────────────────────

/// Manages credentials using OS keychain (primary) or encrypted SQLite (fallback).
pub struct CredentialManager {
    db: SqlitePool,
    cipher: Aes256Gcm,
}

impl CredentialManager {
    /// Open the default credential store at `~/.config/waystone-comm/credentials.db`.
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened or migrated.
    pub async fn open_default() -> Result<Self, CredentialError> {
        let base = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("waystone-comm");
        std::fs::create_dir_all(&base)?;
        Self::open(&base.join("credentials.db"), &base.join("machine.key")).await
    }

    /// Open a credential store at the given paths (used in tests).
    pub async fn open(db_path: &Path, key_path: &Path) -> Result<Self, CredentialError> {
        let key = load_or_create_machine_key(key_path)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));

        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let db = SqlitePool::connect_with(opts).await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS credentials (
                id          TEXT PRIMARY KEY NOT NULL,
                name        TEXT NOT NULL,
                kind        TEXT NOT NULL,
                username    TEXT,
                nonce       BLOB NOT NULL,
                ciphertext  BLOB NOT NULL,
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&db)
        .await?;

        Ok(Self { db, cipher })
    }

    /// Store a credential. Returns the credential's UUID.
    ///
    /// Tries the OS keychain first; falls back to the encrypted SQLite database.
    ///
    /// # Errors
    /// Returns an error if both backends fail.
    pub async fn store(&self, cred: &Credential) -> Result<Uuid, CredentialError> {
        // Try OS keychain.
        let keychain_value = serde_json::json!({
            "kind": format!("{:?}", cred.kind),
            "username": cred.username,
            "secret": cred.secret.expose(),
        })
        .to_string();

        // Best-effort keychain store (ignored if keychain is unavailable).
        if let Ok(entry) = keyring::Entry::new("waystone-comm", &cred.id.to_string()) {
            let _ = entry.set_password(&keychain_value);
        }

        // Always store encrypted secret in SQLite as the canonical backend.
        // This ensures retrieve_sqlite always has valid data regardless of
        // keychain availability.
        self.store_sqlite(cred).await?;

        Ok(cred.id)
    }

    /// Retrieve a credential by UUID.
    ///
    /// # Errors
    /// Returns `CredentialError::NotFound` if the credential does not exist.
    pub async fn retrieve(&self, id: Uuid) -> Result<Credential, CredentialError> {
        // SQLite is the canonical store; keychain is an optional extra copy.
        self.retrieve_sqlite(id).await
    }

    /// Delete a credential by UUID from both backends.
    ///
    /// # Errors
    /// Returns an error if the database operation fails.
    pub async fn delete(&self, id: Uuid) -> Result<(), CredentialError> {
        // Best-effort keychain deletion.
        if let Ok(entry) = keyring::Entry::new("waystone-comm", &id.to_string()) {
            let _ = entry.delete_credential();
        }
        // Remove from SQLite.
        sqlx::query("DELETE FROM credentials WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// List all credentials (without secrets).
    ///
    /// # Errors
    /// Returns an error if the database cannot be read.
    pub async fn list(&self) -> Result<Vec<CredentialSummary>, CredentialError> {
        let rows: Vec<(String, String, String, Option<String>)> =
            sqlx::query_as("SELECT id, name, kind, username FROM credentials ORDER BY name")
                .fetch_all(&self.db)
                .await?;

        rows.into_iter()
            .filter_map(|(id_str, name, kind_str, username)| {
                let id = Uuid::parse_str(&id_str).ok()?;
                let kind: CredentialKind = serde_json::from_str(&format!("\"{kind_str}\"")).ok()?;
                Some(CredentialSummary {
                    id,
                    name,
                    kind,
                    username,
                })
            })
            .collect::<Vec<_>>()
            .pipe_ok()
    }

    // ── SQLite helpers ────────────────────────────────────────────────────────

    async fn store_sqlite(&self, cred: &Credential) -> Result<(), CredentialError> {
        let (nonce_bytes, ciphertext) = self.encrypt(cred.secret.expose())?;
        let kind = serde_json::to_string(&cred.kind)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();

        sqlx::query(
            "INSERT INTO credentials (id, name, kind, username, nonce, ciphertext)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, kind=excluded.kind,
               username=excluded.username,
               nonce=excluded.nonce, ciphertext=excluded.ciphertext",
        )
        .bind(cred.id.to_string())
        .bind(&cred.name)
        .bind(&kind)
        .bind(&cred.username)
        .bind(nonce_bytes.as_slice())
        .bind(ciphertext.as_slice())
        .execute(&self.db)
        .await?;

        Ok(())
    }

    async fn retrieve_sqlite(&self, id: Uuid) -> Result<Credential, CredentialError> {
        type SqliteRow = (String, String, String, Option<String>, Vec<u8>, Vec<u8>);
        let id_str = id.to_string();
        let row: Option<SqliteRow> = sqlx::query_as(
            "SELECT id, name, kind, username, nonce, ciphertext
                 FROM credentials WHERE id = ?",
        )
        .bind(&id_str)
        .fetch_optional(&self.db)
        .await?;

        let (id_s, name, kind_str, username, nonce, ciphertext) =
            row.ok_or(CredentialError::NotFound)?;

        let kind: CredentialKind =
            serde_json::from_str(&format!("\"{kind_str}\"")).unwrap_or(CredentialKind::Password);
        let secret = self.decrypt(&nonce, &ciphertext)?;
        let parsed_id = Uuid::parse_str(&id_s).unwrap_or(id);

        Ok(Credential {
            id: parsed_id,
            name,
            kind,
            username,
            secret: SecretString::new(secret),
        })
    }

    // ── Crypto helpers ────────────────────────────────────────────────────────

    fn encrypt(&self, plaintext: &str) -> Result<(Vec<u8>, Vec<u8>), CredentialError> {
        let mut nonce_bytes = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|_| CredentialError::Encrypt)?;
        Ok((nonce_bytes.to_vec(), ciphertext))
    }

    fn decrypt(&self, nonce: &[u8], ciphertext: &[u8]) -> Result<String, CredentialError> {
        if nonce.len() != 12 {
            return Err(CredentialError::Decrypt);
        }
        let nonce = Nonce::from_slice(nonce);
        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| CredentialError::Decrypt)?;
        String::from_utf8(plaintext).map_err(|_| CredentialError::Decrypt)
    }
}

// ── Machine key ───────────────────────────────────────────────────────────────

/// Load the 32-byte machine key from disk, or generate and persist a fresh one.
fn load_or_create_machine_key(path: &Path) -> Result<[u8; 32], CredentialError> {
    if path.exists() {
        let bytes = std::fs::read(path)?;
        if bytes.len() == 32 {
            restrict_machine_key_permissions(path)?;
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            return Ok(key);
        }
    }
    // Generate a new random key.
    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    atomic_write_private(path, &key)?;
    restrict_machine_key_permissions(path)?;

    Ok(key)
}

fn restrict_machine_key_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

// ── Helper trait for collecting ───────────────────────────────────────────────

trait PipeOk: Sized {
    fn pipe_ok(self) -> Result<Self, CredentialError>;
}

impl<T> PipeOk for Vec<T> {
    fn pipe_ok(self) -> Result<Self, CredentialError> {
        Ok(self)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // The tempdir must stay alive for the duration of the test so the SQLite
    // file isn't deleted.  Return it paired with the manager.
    async fn make_manager() -> (tempfile::TempDir, CredentialManager) {
        let tmp = tempdir().unwrap();
        let mgr = CredentialManager::open(
            &tmp.path().join("creds.db"),
            &tmp.path().join("machine.key"),
        )
        .await
        .expect("open credential manager");
        (tmp, mgr)
    }

    #[test]
    fn secret_string_exposes_value() {
        let s = SecretString::new("hunter2");
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn secret_string_debug_redacted() {
        let s = SecretString::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
    }

    #[tokio::test]
    async fn store_and_retrieve_roundtrip() {
        let (_tmp, mgr) = make_manager().await;
        let cred = Credential::new(
            "My Server",
            CredentialKind::Password,
            Some("alice".into()),
            "p@ss",
        );
        let id = mgr.store(&cred).await.expect("store");
        let got = mgr.retrieve(id).await.expect("retrieve");
        assert_eq!(got.name, "My Server");
        assert_eq!(got.secret.expose(), "p@ss");
        assert_eq!(got.username.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn list_returns_stored_credentials() {
        let (_tmp, mgr) = make_manager().await;
        let c1 = Credential::new("Alpha", CredentialKind::Password, None, "aaa");
        let c2 = Credential::new("Beta", CredentialKind::Token, None, "bbb");
        mgr.store(&c1).await.unwrap();
        mgr.store(&c2).await.unwrap();
        let list = mgr.list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|s| s.name == "Alpha"));
        assert!(list.iter().any(|s| s.name == "Beta"));
    }

    #[tokio::test]
    async fn delete_removes_credential() {
        let (_tmp, mgr) = make_manager().await;
        let cred = Credential::new("Temp", CredentialKind::Token, None, "tok");
        let id = mgr.store(&cred).await.unwrap();
        mgr.delete(id).await.unwrap();
        let list = mgr.list().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn retrieve_missing_returns_not_found() {
        let (_tmp, mgr) = make_manager().await;
        let err = mgr.retrieve(Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, CredentialError::NotFound));
    }

    #[tokio::test]
    async fn machine_key_stable_across_loads() {
        let tmp = tempdir().unwrap();
        let key_path = tmp.path().join("machine.key");
        let k1 = load_or_create_machine_key(&key_path).unwrap();
        let k2 = load_or_create_machine_key(&key_path).unwrap();
        assert_eq!(k1, k2);
    }

    #[tokio::test]
    async fn machine_key_creation_leaves_no_temp_files() {
        let tmp = tempdir().unwrap();
        let key_path = tmp.path().join("machine.key");

        load_or_create_machine_key(&key_path).unwrap();

        let temp_files: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();

        assert!(temp_files.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn machine_key_created_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let key_path = tmp.path().join("machine.key");

        load_or_create_machine_key(&key_path).unwrap();

        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn existing_machine_key_permissions_are_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let key_path = tmp.path().join("machine.key");
        let original = [7u8; 32];
        std::fs::write(&key_path, original).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let loaded = load_or_create_machine_key(&key_path).unwrap();

        assert_eq!(loaded, original);
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn generate_ssh_keypair_produces_valid_keys() {
        let (cred, pubkey) = generate_ssh_keypair("test-key", "user@host").unwrap();
        assert_eq!(cred.kind, CredentialKind::SshKey);
        assert!(cred.secret.expose().contains("OPENSSH PRIVATE KEY"));
        assert!(pubkey.starts_with("ssh-ed25519"));
        assert!(pubkey.contains("user@host"));
    }

    #[test]
    fn public_key_can_be_derived_from_stored_private_key() {
        let (cred, generated_pubkey) = generate_ssh_keypair("test-key", "user@host").unwrap();
        let derived_pubkey =
            public_key_from_private_openssh(cred.secret.expose(), "user@host").unwrap();

        assert_eq!(derived_pubkey, generated_pubkey);
    }
}
