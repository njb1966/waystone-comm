use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{connection::Protocol, fsutil::atomic_write, logging::LogSettings};

// ── DirectoryEntry (MASTERPLAN §10.1) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    /// Unique identifier for this entry.
    pub id: Uuid,

    /// Human-readable name shown in the dialing directory.
    pub name: String,

    /// Optional group/folder this entry belongs to.
    pub group: Option<String>,

    /// Connection protocol.
    pub protocol: Protocol,

    /// Connection-specific settings.
    pub connection: ConnectionSettings,

    /// Terminal emulation override. New entries default per protocol
    /// (see `default_emulation_for_protocol`).
    pub terminal: TerminalSettings,

    /// Optional credential manager UUID reference. Never store the secret here.
    pub credential_id: Option<Uuid>,

    /// Free-form tags for filtering/searching.
    pub tags: Vec<String>,

    /// Markdown notes shown in the directory sidebar.
    pub notes: Option<String>,

    /// Timestamp of the last successful connection.
    pub last_connected: Option<DateTime<Utc>>,

    /// Per-entry script hooks (paths into ~/.config/waystone-comm/scripts/).
    pub scripts: ScriptHooks,

    /// Arbitrary session variables accessible from scripts.
    pub session_vars: HashMap<String, String>,

    /// Key profile name for this entry. Falls back to "Default" if `None`.
    #[serde(default)]
    pub key_profile: Option<String>,

    /// Session logging settings for this entry.
    #[serde(default)]
    pub log: LogSettings,
}

/// Default terminal emulation for a freshly created entry of this protocol.
///
/// Telnet and raw TCP are almost always classic BBSes that send CP437 art,
/// so they default to `ansi-bbs`. Everything else defaults to `xterm-256color`.
#[must_use]
pub fn default_emulation_for_protocol(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::Telnet | Protocol::Raw => "ansi-bbs",
        _ => "xterm-256color",
    }
}

impl DirectoryEntry {
    /// Create a new entry with required fields and sensible defaults.
    #[must_use]
    pub fn new(name: impl Into<String>, protocol: Protocol, host: impl Into<String>) -> Self {
        let emulation = default_emulation_for_protocol(&protocol).into();
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            group: None,
            protocol,
            connection: ConnectionSettings {
                host: host.into(),
                port: None,
                username: None,
                extra: HashMap::new(),
            },
            terminal: TerminalSettings {
                emulation,
                ..TerminalSettings::default()
            },
            credential_id: None,
            tags: Vec::new(),
            notes: None,
            last_connected: None,
            scripts: ScriptHooks::default(),
            session_vars: HashMap::new(),
            key_profile: None,
            log: LogSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionSettings {
    /// Hostname or IP address (empty string for serial ports).
    pub host: String,

    /// Port number — None means use the protocol default.
    pub port: Option<u16>,

    /// Username for authenticated protocols.
    pub username: Option<String>,

    /// Protocol-specific key/value overrides (e.g. baud_rate, key_path).
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSettings {
    /// Terminal emulation type string (e.g. "xterm-256color", "vt100", "ansi").
    pub emulation: String,

    /// Terminal columns.
    pub cols: u16,

    /// Terminal rows.
    pub rows: u16,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            emulation: "xterm-256color".into(),
            cols: 80,
            rows: 24,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScriptHooks {
    pub on_connect: Option<String>,
    pub on_disconnect: Option<String>,
    pub on_data: Option<String>,
}

// ── Directory store ───────────────────────────────────────────────────────────

use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
struct DirectoryFile {
    #[serde(default)]
    entries: Vec<DirectoryEntry>,
}

/// Manages the collection of saved directory entries backed by a TOML file.
pub struct Directory {
    path: PathBuf,
    entries: Vec<DirectoryEntry>,
}

impl Directory {
    /// Load (or create) the directory from the default config path
    /// (`~/.config/waystone-comm/directory.toml`).
    pub fn load_default() -> std::io::Result<Self> {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("waystone-comm")
            .join("directory.toml");
        Self::load(path)
    }

    /// Load (or create) the directory from the given TOML path.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let entries = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            let file: DirectoryFile =
                toml::from_str(&text).map_err(|e| std::io::Error::other(e.to_string()))?;
            file.entries
        } else {
            Vec::new()
        };
        Ok(Self { path, entries })
    }

    /// Persist entries to the TOML file.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = DirectoryFile {
            entries: self.entries.clone(),
        };
        let text =
            toml::to_string_pretty(&file).map_err(|e| std::io::Error::other(e.to_string()))?;
        atomic_write(&self.path, text.as_bytes())
    }

    pub fn add_entry(&mut self, entry: DirectoryEntry) {
        self.entries.push(entry);
    }

    #[must_use]
    pub fn get_entry(&self, id: Uuid) -> Option<&DirectoryEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    #[must_use]
    pub fn list_entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }

    pub fn delete_entry(&mut self, id: Uuid) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() < before
    }

    pub fn update_entry(&mut self, updated: DirectoryEntry) -> bool {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == updated.id) {
            *e = updated;
            true
        } else {
            false
        }
    }

    pub fn mark_connected(&mut self, id: Uuid, connected_at: DateTime<Utc>) -> bool {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.last_connected = Some(connected_at);
            true
        } else {
            false
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_entry() -> DirectoryEntry {
        DirectoryEntry::new("My Server", Protocol::Ssh, "192.168.1.10")
    }

    #[test]
    fn add_and_get_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("directory.toml");
        let mut d = Directory::load(&path).unwrap();

        let entry = sample_entry();
        let id = entry.id;
        d.add_entry(entry);

        assert!(d.get_entry(id).is_some());
        assert_eq!(d.get_entry(id).unwrap().name, "My Server");
    }

    #[test]
    fn list_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("directory.toml");
        let mut d = Directory::load(&path).unwrap();

        assert!(d.list_entries().is_empty());
        d.add_entry(sample_entry());
        d.add_entry(sample_entry());
        assert_eq!(d.list_entries().len(), 2);
    }

    #[test]
    fn delete_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("directory.toml");
        let mut d = Directory::load(&path).unwrap();

        let entry = sample_entry();
        let id = entry.id;
        d.add_entry(entry);
        assert!(d.delete_entry(id));
        assert!(d.get_entry(id).is_none());
        assert!(!d.delete_entry(id)); // second delete returns false
    }

    #[test]
    fn update_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("directory.toml");
        let mut d = Directory::load(&path).unwrap();

        let mut entry = sample_entry();
        let id = entry.id;
        d.add_entry(entry.clone());

        entry.name = "Renamed".into();
        assert!(d.update_entry(entry));
        assert_eq!(d.get_entry(id).unwrap().name, "Renamed");
    }

    #[test]
    fn mark_connected_updates_last_connected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("directory.toml");
        let mut d = Directory::load(&path).unwrap();

        let entry = sample_entry();
        let id = entry.id;
        let connected_at = Utc::now();
        d.add_entry(entry);

        assert!(d.mark_connected(id, connected_at));
        assert_eq!(d.get_entry(id).unwrap().last_connected, Some(connected_at));
        assert!(!d.mark_connected(Uuid::new_v4(), connected_at));
    }

    #[test]
    fn save_and_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("directory.toml");

        let mut d = Directory::load(&path).unwrap();
        let entry = sample_entry();
        let id = entry.id;
        d.add_entry(entry);
        d.save().unwrap();

        let d2 = Directory::load(&path).unwrap();
        assert!(d2.get_entry(id).is_some());
        assert_eq!(d2.get_entry(id).unwrap().name, "My Server");
    }

    #[test]
    fn save_leaves_no_temp_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("directory.toml");

        let mut d = Directory::load(&path).unwrap();
        d.add_entry(sample_entry());
        d.save().unwrap();

        let temp_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();

        assert!(temp_files.is_empty());
    }
}
