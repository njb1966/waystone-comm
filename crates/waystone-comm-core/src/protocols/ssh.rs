use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{key::PrivateKeyWithHashAlg, load_secret_key};
use russh::{cipher, kex, mac, ChannelMsg, Disconnect, Preferred};
use ssh_key::{Algorithm, EcdsaCurve, HashAlg, PrivateKey};
use tokio::sync::Mutex;

use crate::connection::{Connection, ConnectionError, ConnectionStatus, Protocol, Result};
use crate::credentials::{CredentialKind, CredentialManager};
use crate::directory::DirectoryEntry;

// ── Host-key verification ─────────────────────────────────────────────────────

/// Simple TOFU (trust-on-first-use) host key verifier backed by a known_hosts
/// file at `~/.config/waystone-comm/known_hosts`.
///
/// SSH host key verification is MANDATORY — there is no option to skip it.
struct WaystoneCommHostKeyVerifier {
    known_hosts_path: PathBuf,
    host: String,
    port: u16,
}

impl WaystoneCommHostKeyVerifier {
    fn new(host: impl Into<String>, port: u16) -> Self {
        let path = dirs_path();
        Self {
            known_hosts_path: path,
            host: host.into(),
            port,
        }
    }

    #[cfg(test)]
    fn new_for_path(host: impl Into<String>, port: u16, path: PathBuf) -> Self {
        Self {
            known_hosts_path: path,
            host: host.into(),
            port,
        }
    }

    fn host_key_line(&self, key_type: &str, fingerprint: &str) -> String {
        format!(
            "{}\t{}\t{}\t{}",
            self.host, self.port, key_type, fingerprint
        )
    }

    fn parse_host_key_line(line: &str) -> Option<(&str, u16, &str, &str)> {
        let mut parts = line.split_whitespace();
        let host = parts.next()?;
        let port = parts.next()?.parse().ok()?;
        let key_type = parts.next()?;
        let fingerprint = parts.next()?;
        Some((host, port, key_type, fingerprint))
    }
}

fn dirs_path() -> PathBuf {
    let base = std::env::var("WAYSTONE_COMM_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("~/.config"))
                .join("waystone-comm")
        });
    base.join("known_hosts")
}

#[async_trait]
impl client::Handler for WaystoneCommHostKeyVerifier {
    type Error = ConnectionError;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        // On first connection: trust and record (TOFU). On subsequent
        // connections verify against the stored fingerprint.
        // A changed key for the same host/port is rejected.
        let fingerprint = server_public_key
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string();
        let algorithm = server_public_key.algorithm();
        let key_type = algorithm.as_str();

        if let Some(parent) = self.known_hosts_path.parent() {
            std::fs::create_dir_all(parent).map_err(ConnectionError::Io)?;
        }

        if self.known_hosts_path.exists() {
            let known =
                std::fs::read_to_string(&self.known_hosts_path).map_err(ConnectionError::Io)?;
            for line in known.lines() {
                let Some((host, port, _known_type, known_fingerprint)) =
                    Self::parse_host_key_line(line)
                else {
                    continue;
                };
                if host == self.host && port == self.port {
                    if known_fingerprint == fingerprint {
                        return Ok(true);
                    }
                    return Err(ConnectionError::HostKeyMismatch(format!(
                        "{}:{} expected {known_fingerprint}, got {fingerprint}",
                        self.host, self.port
                    )));
                }
            }
        }

        // Record fingerprint
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.known_hosts_path)
            .map_err(ConnectionError::Io)?;
        writeln!(f, "{}", self.host_key_line(key_type, &fingerprint))
            .map_err(ConnectionError::Io)?;

        Ok(true)
    }
}

// ── Channel wrapper ───────────────────────────────────────────────────────────

/// Wraps a russh client handle + channel into the `Connection` trait.
pub struct SshConnection {
    status: ConnectionStatus,
    handle: Option<Handle<WaystoneCommHostKeyVerifier>>,
    channel: Option<Arc<Mutex<russh::Channel<client::Msg>>>>,
    // Buffered data received from the channel that hasn't been consumed yet
    read_buf: Vec<u8>,
}

impl SshConnection {
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            handle: None,
            channel: None,
            read_buf: Vec::new(),
        }
    }
}

impl Default for SshConnection {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connection for SshConnection {
    async fn connect(&mut self, entry: &DirectoryEntry) -> Result<()> {
        self.status = ConnectionStatus::Connecting;

        let port = entry.connection.port.unwrap_or(22);
        let host = entry.connection.host.clone();
        let username = entry.connection.username.clone().unwrap_or_else(whoami);

        // SSH client configuration
        let mut config = ssh_config_from_entry(entry);
        if legacy_ssh_enabled(entry) {
            config.preferred = legacy_ssh_preferred();
        }
        let config = Arc::new(config);

        let handler = WaystoneCommHostKeyVerifier::new(host.clone(), port);
        let mut handle = client::connect(config, (host.as_str(), port), handler).await?;

        // Authenticate
        let authenticated = self.authenticate(&mut handle, entry, &username).await?;

        if !authenticated {
            return Err(ConnectionError::AuthFailed(
                "all authentication methods exhausted".into(),
            ));
        }

        // Open a PTY channel
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| ConnectionError::Protocol(e.to_string()))?;

        let (cols, rows) = (entry.terminal.cols, entry.terminal.rows);
        channel
            .request_pty(
                false,
                &entry.terminal.emulation,
                cols as u32,
                rows as u32,
                0,
                0,
                &[],
            )
            .await
            .map_err(|e| ConnectionError::Protocol(e.to_string()))?;

        channel
            .request_shell(false)
            .await
            .map_err(|e| ConnectionError::Protocol(e.to_string()))?;

        self.channel = Some(Arc::new(Mutex::new(channel)));
        self.handle = Some(handle);
        self.status = ConnectionStatus::Connected;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(handle) = self.handle.take() {
            let _ = handle
                .disconnect(Disconnect::ByApplication, "user disconnect", "en")
                .await;
        }
        self.channel = None;
        self.read_buf.clear();
        self.status = ConnectionStatus::Disconnected;
        Ok(())
    }

    async fn read(&mut self) -> Result<Vec<u8>> {
        // Return any buffered data first
        if !self.read_buf.is_empty() {
            return Ok(std::mem::take(&mut self.read_buf));
        }

        let channel = self
            .channel
            .as_ref()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;
        let mut ch = channel.lock().await;

        loop {
            match ch.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    return Ok(data.to_vec());
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    // stderr — surface to the caller the same as stdout
                    return Ok(data.to_vec());
                }
                Some(ChannelMsg::Eof)
                | Some(ChannelMsg::Close)
                | Some(ChannelMsg::ExitStatus { exit_status: _ })
                | Some(ChannelMsg::ExitSignal { .. })
                | None => {
                    self.status = ConnectionStatus::Disconnected;
                    return Err(ConnectionError::Disconnected("session closed".into()));
                }
                _ => {
                    // Other messages (window adjust, etc.) — keep looping
                }
            }
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let channel = self
            .channel
            .as_ref()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;
        let ch = channel.lock().await;
        ch.data(data)
            .await
            .map_err(|e| ConnectionError::Protocol(e.to_string()))
    }

    async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let channel = self
            .channel
            .as_ref()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;
        let ch = channel.lock().await;
        ch.window_change(cols as u32, rows as u32, 0, 0)
            .await
            .map_err(|e| ConnectionError::Protocol(e.to_string()))
    }

    fn protocol(&self) -> Protocol {
        Protocol::Ssh
    }

    fn status(&self) -> ConnectionStatus {
        self.status.clone()
    }

    fn supports_file_transfer(&self) -> bool {
        true // Zmodem + SFTP both run over SSH
    }
}

// ── Auth helpers ──────────────────────────────────────────────────────────────

impl SshConnection {
    async fn authenticate(
        &self,
        handle: &mut Handle<WaystoneCommHostKeyVerifier>,
        entry: &DirectoryEntry,
        username: &str,
    ) -> Result<bool> {
        // Some BBS SSH daemons intentionally accept "none" auth.
        if handle
            .authenticate_none(username)
            .await
            .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?
        {
            return Ok(true);
        }

        // If a password was explicitly supplied, try it before keys. Some BBS
        // SSH daemons close the auth channel after unsupported key attempts.
        if let Some(password) = entry.connection.extra.get("password") {
            if Self::authenticate_password_or_keyboard_interactive(handle, username, password)
                .await?
            {
                return Ok(true);
            }
        }

        // 1. Try public key auth (Ed25519 preferred, RSA fallback)
        if let Some(key_path) = entry.connection.extra.get("key_path") {
            let expanded = shellexpand::tilde(key_path).into_owned();
            let keypair = load_secret_key(&expanded, None)
                .map_err(|e| ConnectionError::AuthFailed(format!("{expanded}: {e}")))?;
            if Self::authenticate_keypair(handle, username, keypair).await? {
                return Ok(true);
            }
        }

        // 2. Try a credential-manager secret referenced by the entry.
        if let Some(credential_id) = entry.credential_id {
            let manager = CredentialManager::open_default()
                .await
                .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?;
            let credential = manager
                .retrieve(credential_id)
                .await
                .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?;
            match credential.kind {
                CredentialKind::Password | CredentialKind::Token => {
                    if Self::authenticate_password_or_keyboard_interactive(
                        handle,
                        username,
                        credential.secret.expose(),
                    )
                    .await?
                    {
                        return Ok(true);
                    }
                }
                CredentialKind::SshKey => {
                    let keypair = PrivateKey::from_openssh(credential.secret.expose())
                        .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?;
                    if Self::authenticate_keypair(handle, username, keypair).await? {
                        return Ok(true);
                    }
                }
            }
        }

        // 3. Try default key locations: ~/.ssh/id_ed25519, ~/.ssh/id_rsa
        let default_keys = [
            dirs::home_dir().unwrap_or_default().join(".ssh/id_ed25519"),
            dirs::home_dir().unwrap_or_default().join(".ssh/id_rsa"),
        ];
        for key_path in &default_keys {
            if key_path.exists() {
                if let Ok(keypair) = load_secret_key(key_path, None) {
                    if Self::authenticate_keypair(handle, username, keypair).await? {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    async fn authenticate_keypair(
        handle: &mut Handle<WaystoneCommHostKeyVerifier>,
        username: &str,
        keypair: PrivateKey,
    ) -> Result<bool> {
        let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(keypair), None)
            .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?;
        handle
            .authenticate_publickey(username, key_with_hash)
            .await
            .map_err(|e| ConnectionError::AuthFailed(e.to_string()))
    }

    async fn authenticate_password_or_keyboard_interactive(
        handle: &mut Handle<WaystoneCommHostKeyVerifier>,
        username: &str,
        password: &str,
    ) -> Result<bool> {
        if handle
            .authenticate_password(username, password)
            .await
            .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?
        {
            return Ok(true);
        }

        Self::authenticate_keyboard_interactive(handle, username, password).await
    }

    async fn authenticate_keyboard_interactive(
        handle: &mut Handle<WaystoneCommHostKeyVerifier>,
        username: &str,
        password: &str,
    ) -> Result<bool> {
        let mut response = handle
            .authenticate_keyboard_interactive_start(username, None)
            .await
            .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?;

        loop {
            match response {
                KeyboardInteractiveAuthResponse::Success => return Ok(true),
                KeyboardInteractiveAuthResponse::Failure => return Ok(false),
                KeyboardInteractiveAuthResponse::InfoRequest {
                    name: _,
                    instructions: _,
                    prompts,
                } => {
                    let responses = prompts
                        .iter()
                        .map(|prompt| {
                            if prompt.echo {
                                String::new()
                            } else {
                                password.to_string()
                            }
                        })
                        .collect();
                    response = handle
                        .authenticate_keyboard_interactive_respond(responses)
                        .await
                        .map_err(|e| ConnectionError::AuthFailed(e.to_string()))?;
                }
            }
        }
    }
}

fn legacy_ssh_enabled(entry: &DirectoryEntry) -> bool {
    entry
        .connection
        .extra
        .get("legacy_ssh")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn ssh_config_from_entry(entry: &DirectoryEntry) -> russh::client::Config {
    let keepalive_interval = entry
        .connection
        .extra
        .get("keepalive_interval")
        .and_then(|value| value.parse().ok())
        .unwrap_or(30);
    let keepalive_max = entry
        .connection
        .extra
        .get("keepalive_max")
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let inactivity_timeout = entry
        .connection
        .extra
        .get("inactivity_timeout")
        .and_then(|value| value.parse().ok())
        .map(std::time::Duration::from_secs);

    russh::client::Config {
        inactivity_timeout,
        keepalive_interval: Some(std::time::Duration::from_secs(keepalive_interval)),
        keepalive_max,
        ..<_>::default()
    }
}

fn legacy_ssh_preferred() -> Preferred {
    Preferred {
        kex: Cow::Owned(vec![
            kex::CURVE25519,
            kex::CURVE25519_PRE_RFC_8731,
            kex::ECDH_SHA2_NISTP256,
            kex::ECDH_SHA2_NISTP384,
            kex::ECDH_SHA2_NISTP521,
            kex::DH_G16_SHA512,
            kex::DH_G14_SHA256,
            kex::DH_G14_SHA1,
            kex::DH_G1_SHA1,
            kex::EXTENSION_SUPPORT_AS_CLIENT,
            kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
        ]),
        key: Cow::Owned(vec![
            Algorithm::Ed25519,
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384,
            },
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP521,
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            },
            Algorithm::Rsa { hash: None },
        ]),
        cipher: Cow::Owned(vec![
            cipher::CHACHA20_POLY1305,
            cipher::AES_256_GCM,
            cipher::AES_256_CTR,
            cipher::AES_192_CTR,
            cipher::AES_128_CTR,
            cipher::AES_256_CBC,
            cipher::AES_192_CBC,
            cipher::AES_128_CBC,
            cipher::TRIPLE_DES_CBC,
        ]),
        mac: Cow::Owned(vec![
            mac::HMAC_SHA512_ETM,
            mac::HMAC_SHA256_ETM,
            mac::HMAC_SHA512,
            mac::HMAC_SHA256,
            mac::HMAC_SHA1_ETM,
            mac::HMAC_SHA1,
        ]),
        compression: Preferred::DEFAULT.compression,
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client::Handler;

    fn test_public_key() -> ssh_key::PublicKey {
        ssh_key::PrivateKey::random(&mut rand::rngs::OsRng, ssh_key::Algorithm::Ed25519)
            .unwrap()
            .public_key()
            .clone()
    }

    #[test]
    fn default_status() {
        let conn = SshConnection::new();
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
        assert_eq!(conn.protocol(), Protocol::Ssh);
        assert!(conn.supports_file_transfer());
    }

    #[tokio::test]
    async fn disconnect_when_idle_is_ok() {
        let mut conn = SshConnection::new();
        assert!(conn.disconnect().await.is_ok());
    }

    #[tokio::test]
    async fn read_without_connect_errors() {
        let mut conn = SshConnection::new();
        assert!(matches!(
            conn.read().await,
            Err(ConnectionError::Disconnected(_))
        ));
    }

    #[tokio::test]
    async fn write_without_connect_errors() {
        let mut conn = SshConnection::new();
        assert!(matches!(
            conn.write(b"hello").await,
            Err(ConnectionError::Disconnected(_))
        ));
    }

    #[tokio::test]
    async fn host_key_first_use_records_host_bound_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = test_public_key();
        let mut verifier =
            WaystoneCommHostKeyVerifier::new_for_path("example.com", 22, path.clone());

        assert!(verifier.check_server_key(&key).await.unwrap());

        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("example.com\t22\tssh-ed25519\t"));
    }

    #[tokio::test]
    async fn host_key_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = test_public_key();
        let other_key = test_public_key();
        let mut verifier =
            WaystoneCommHostKeyVerifier::new_for_path("example.com", 22, path.clone());
        verifier.check_server_key(&key).await.unwrap();

        let mut verifier = WaystoneCommHostKeyVerifier::new_for_path("example.com", 22, path);
        let err = verifier.check_server_key(&other_key).await.unwrap_err();

        assert!(matches!(err, ConnectionError::HostKeyMismatch(_)));
    }

    #[tokio::test]
    async fn same_fingerprint_for_different_host_is_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = test_public_key();
        let mut first = WaystoneCommHostKeyVerifier::new_for_path("one.example", 22, path.clone());
        first.check_server_key(&key).await.unwrap();

        let mut second = WaystoneCommHostKeyVerifier::new_for_path("two.example", 22, path.clone());
        assert!(second.check_server_key(&key).await.unwrap());

        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("one.example\t22\tssh-ed25519\t"));
        assert!(content.contains("two.example\t22\tssh-ed25519\t"));
    }

    #[test]
    fn legacy_ssh_profile_includes_bbs_compatibility_algorithms() {
        let preferred = legacy_ssh_preferred();

        assert!(preferred
            .kex
            .iter()
            .any(|name| name.as_ref() == "diffie-hellman-group1-sha1"));
        assert!(preferred
            .cipher
            .iter()
            .any(|name| name.as_ref() == "3des-cbc"));
        assert!(preferred
            .mac
            .iter()
            .any(|name| name.as_ref() == "hmac-sha1"));
    }

    #[test]
    fn ssh_config_uses_keepalive_not_inactivity_timeout_by_default() {
        let entry = DirectoryEntry::new("bbs", Protocol::Ssh, "bbs.example.com");
        let config = ssh_config_from_entry(&entry);

        assert_eq!(
            config.keepalive_interval,
            Some(std::time::Duration::from_secs(30))
        );
        assert_eq!(config.keepalive_max, 3);
        assert_eq!(config.inactivity_timeout, None);
    }

    #[test]
    fn ssh_config_honors_explicit_idle_settings() {
        let mut entry = DirectoryEntry::new("bbs", Protocol::Ssh, "bbs.example.com");
        entry
            .connection
            .extra
            .insert("keepalive_interval".into(), "45".into());
        entry
            .connection
            .extra
            .insert("keepalive_max".into(), "5".into());
        entry
            .connection
            .extra
            .insert("inactivity_timeout".into(), "600".into());

        let config = ssh_config_from_entry(&entry);

        assert_eq!(
            config.keepalive_interval,
            Some(std::time::Duration::from_secs(45))
        );
        assert_eq!(config.keepalive_max, 5);
        assert_eq!(
            config.inactivity_timeout,
            Some(std::time::Duration::from_secs(600))
        );
    }
}
