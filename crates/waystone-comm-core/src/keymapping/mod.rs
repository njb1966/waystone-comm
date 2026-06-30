//! Key mapping and macro system.
//!
//! A [`KeyProfile`] maps key+modifier combinations to [`KeyAction`]s.
//! Profiles are stored as TOML files in `~/.config/waystone-comm/key_profiles/`.
//! Sessions fall back to the "Default" profile unless overridden per-entry.

use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsutil::atomic_write;

// ── Key specification ─────────────────────────────────────────────────────────

/// A key code independent of any terminal library.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    F(u8),
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

impl fmt::Display for KeyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyCode::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            KeyCode::F(n) => write!(f, "F{n}"),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::Backspace => write!(f, "Backspace"),
            KeyCode::Escape => write!(f, "Escape"),
            KeyCode::Delete => write!(f, "Delete"),
            KeyCode::Insert => write!(f, "Insert"),
            KeyCode::Up => write!(f, "Up"),
            KeyCode::Down => write!(f, "Down"),
            KeyCode::Left => write!(f, "Left"),
            KeyCode::Right => write!(f, "Right"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            KeyCode::PageUp => write!(f, "PageUp"),
            KeyCode::PageDown => write!(f, "PageDown"),
        }
    }
}

impl KeyCode {
    fn parse(s: &str) -> Option<Self> {
        if let Some(n) = s.strip_prefix('F').or_else(|| s.strip_prefix('f')) {
            if let Ok(n) = n.parse::<u8>() {
                return Some(KeyCode::F(n));
            }
        }
        match s {
            "Enter" | "Return" => Some(KeyCode::Enter),
            "Tab" => Some(KeyCode::Tab),
            "Backspace" => Some(KeyCode::Backspace),
            "Escape" | "Esc" => Some(KeyCode::Escape),
            "Delete" | "Del" => Some(KeyCode::Delete),
            "Insert" | "Ins" => Some(KeyCode::Insert),
            "Up" => Some(KeyCode::Up),
            "Down" => Some(KeyCode::Down),
            "Left" => Some(KeyCode::Left),
            "Right" => Some(KeyCode::Right),
            "Home" => Some(KeyCode::Home),
            "End" => Some(KeyCode::End),
            "PageUp" | "PgUp" => Some(KeyCode::PageUp),
            "PageDown" | "PgDn" | "PgDown" => Some(KeyCode::PageDown),
            "Space" => Some(KeyCode::Char(' ')),
            s if s.chars().count() == 1 => s
                .chars()
                .next()
                .map(|ch| KeyCode::Char(ch.to_ascii_lowercase())),
            _ => None,
        }
    }
}

/// A key+modifier combination.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeySpec {
    pub fn new(code: KeyCode) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub fn ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }

    /// Parse from a human-readable string like "Ctrl+F3", "Alt+1", "Ctrl+Alt+S".
    pub fn parse(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut rest = s;

        loop {
            if let Some(r) = rest.strip_prefix("Ctrl+") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("ctrl+") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("Alt+") {
                alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("alt+") {
                alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("Shift+") {
                shift = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("shift+") {
                shift = true;
                rest = r;
            } else {
                break;
            }
        }

        let code = KeyCode::parse(rest)?;
        Some(Self {
            code,
            ctrl,
            alt,
            shift,
        })
    }
}

impl fmt::Display for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            write!(f, "Ctrl+")?;
        }
        if self.alt {
            write!(f, "Alt+")?;
        }
        if self.shift {
            write!(f, "Shift+")?;
        }
        write!(f, "{}", self.code)
    }
}

// ── Actions ───────────────────────────────────────────────────────────────────

/// Application-level commands that do not send bytes to the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    Quit,
    OpenDirectory,
    OpenScripts,
    OpenKeyMapping,
    NewTab,
    CloseTab,
    SwitchTab(usize), // 1-based
    SendFile,
    ReceiveFile,
    ToggleLog,
    OpenCredentials,
}

impl AppCommand {
    fn parse(s: &str) -> Option<Self> {
        if let Some(n) = s.strip_prefix("SwitchTab:") {
            return n.parse::<usize>().ok().map(AppCommand::SwitchTab);
        }
        match s {
            "Quit" => Some(AppCommand::Quit),
            "OpenDirectory" => Some(AppCommand::OpenDirectory),
            "OpenScripts" => Some(AppCommand::OpenScripts),
            "OpenKeyMapping" => Some(AppCommand::OpenKeyMapping),
            "NewTab" => Some(AppCommand::NewTab),
            "CloseTab" => Some(AppCommand::CloseTab),
            "SendFile" => Some(AppCommand::SendFile),
            "ReceiveFile" => Some(AppCommand::ReceiveFile),
            "ToggleLog" => Some(AppCommand::ToggleLog),
            "OpenCredentials" => Some(AppCommand::OpenCredentials),
            _ => None,
        }
    }

    fn to_arg(&self) -> String {
        match self {
            AppCommand::SwitchTab(n) => format!("SwitchTab:{n}"),
            AppCommand::Quit => "Quit".into(),
            AppCommand::OpenDirectory => "OpenDirectory".into(),
            AppCommand::OpenScripts => "OpenScripts".into(),
            AppCommand::OpenKeyMapping => "OpenKeyMapping".into(),
            AppCommand::NewTab => "NewTab".into(),
            AppCommand::CloseTab => "CloseTab".into(),
            AppCommand::SendFile => "SendFile".into(),
            AppCommand::ReceiveFile => "ReceiveFile".into(),
            AppCommand::ToggleLog => "ToggleLog".into(),
            AppCommand::OpenCredentials => "OpenCredentials".into(),
        }
    }
}

impl fmt::Display for AppCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "app:{}", self.to_arg())
    }
}

/// What a bound key does when pressed.
#[derive(Debug, Clone)]
pub enum KeyAction {
    /// Send a text string (supports `\r`, `\n`, `\t`, `\x1b` escapes).
    SendText(String),
    /// Send raw bytes (specified as a hex string).
    SendBytes(Vec<u8>),
    /// Run a named script from the script store.
    RunScript(String),
    /// Execute an application-level command.
    AppCommand(AppCommand),
    /// Pass the key through to the terminal as normal.
    Passthrough,
}

impl fmt::Display for KeyAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyAction::SendText(t) => write!(f, "text:{}", t.escape_default()),
            KeyAction::SendBytes(b) => {
                write!(f, "bytes:")?;
                for byte in b {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
            KeyAction::RunScript(n) => write!(f, "script:{n}"),
            KeyAction::AppCommand(c) => write!(f, "{c}"),
            KeyAction::Passthrough => write!(f, "passthrough"),
        }
    }
}

impl KeyAction {
    /// Parse the action portion of a binding string.
    fn parse(kind: &str, arg: Option<&str>) -> Option<Self> {
        match kind {
            "passthrough" => Some(KeyAction::Passthrough),
            "text" => Some(KeyAction::SendText(unescape_text(arg.unwrap_or("")))),
            "bytes" => {
                let hex = arg.unwrap_or("");
                let bytes = (0..hex.len())
                    .step_by(2)
                    .filter_map(|i| {
                        hex.get(i..i + 2)
                            .and_then(|h| u8::from_str_radix(h, 16).ok())
                    })
                    .collect();
                Some(KeyAction::SendBytes(bytes))
            }
            "script" => Some(KeyAction::RunScript(arg.unwrap_or("").to_string())),
            "app" => AppCommand::parse(arg.unwrap_or("")).map(KeyAction::AppCommand),
            _ => None,
        }
    }
}

/// Expand `\r`, `\n`, `\t`, `\x##`, `\\` in a text string.
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('x') => {
                    let h: String = chars.by_ref().take(2).collect();
                    if let Ok(b) = u8::from_str_radix(&h, 16) {
                        out.push(b as char);
                    } else {
                        out.push_str("\\x");
                        out.push_str(&h);
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ── Binding ───────────────────────────────────────────────────────────────────

/// One entry in a key profile — maps a key to an action with an optional label.
#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: KeySpec,
    /// Short label shown in the F-key bar (e.g. "Send", "Scripts").
    pub label: Option<String>,
    pub action: KeyAction,
}

impl KeyBinding {
    pub fn display_key(&self) -> String {
        self.key.to_string()
    }

    pub fn display_action(&self) -> String {
        self.action.to_string()
    }
}

// ── Serde helpers (TOML flat struct) ──────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct RawBinding {
    key: String,
    #[serde(default)]
    label: Option<String>,
    /// "app", "text", "bytes", "script", "passthrough"
    kind: String,
    #[serde(default)]
    arg: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RawProfile {
    name: String,
    #[serde(default)]
    bindings: Vec<RawBinding>,
}

impl From<&KeyBinding> for RawBinding {
    fn from(b: &KeyBinding) -> Self {
        let (kind, arg) = match &b.action {
            KeyAction::SendText(t) => ("text".into(), Some(t.clone())),
            KeyAction::SendBytes(bytes) => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                ("bytes".into(), Some(hex))
            }
            KeyAction::RunScript(n) => ("script".into(), Some(n.clone())),
            KeyAction::AppCommand(c) => ("app".into(), Some(c.to_arg())),
            KeyAction::Passthrough => ("passthrough".into(), None),
        };
        RawBinding {
            key: b.key.to_string(),
            label: b.label.clone(),
            kind,
            arg,
        }
    }
}

// ── KeyProfile ────────────────────────────────────────────────────────────────

/// A named collection of key bindings.
#[derive(Debug, Clone)]
pub struct KeyProfile {
    pub name: String,
    pub bindings: Vec<KeyBinding>,
}

impl KeyProfile {
    /// Look up the action for a key spec. Returns `None` if unbound.
    pub fn lookup(&self, key: &KeySpec) -> Option<&KeyAction> {
        self.bindings
            .iter()
            .find(|b| &b.key == key)
            .map(|b| &b.action)
    }

    /// Returns (key_label, action_label) pairs for F1–F12, in order.
    /// Entries that have no binding in this profile use empty strings.
    pub fn fkey_bar_labels(&self) -> Vec<(String, String)> {
        // F1–F12 first (in order), then any other labeled bindings (e.g. Ctrl+Q).
        let mut labels: Vec<(String, String)> = (1u8..=12)
            .filter_map(|n| {
                let spec = KeySpec::new(KeyCode::F(n));
                let binding = self.bindings.iter().find(|b| b.key == spec)?;
                let label = binding.label.clone().unwrap_or_default();
                if label.is_empty() {
                    return None;
                }
                Some((format!("F{n}"), label))
            })
            .collect();

        // Non-F-key labeled bindings.
        for b in &self.bindings {
            if matches!(b.key.code, KeyCode::F(_)) {
                continue;
            }
            let label = b.label.as_deref().unwrap_or("").to_string();
            if label.is_empty() {
                continue;
            }
            let key_name = key_display_name(&b.key);
            labels.push((key_name, label));
        }

        labels
    }

    /// The default profile matching the MASTERPLAN §4.6 layout.
    pub fn default_profile() -> Self {
        use AppCommand::*;
        use KeyCode::*;

        let b = |code: KeyCode, ctrl: bool, alt: bool, label: &str, action: KeyAction| KeyBinding {
            key: KeySpec {
                code,
                ctrl,
                alt,
                shift: false,
            },
            label: if label.is_empty() {
                None
            } else {
                Some(label.to_string())
            },
            action,
        };
        let app = |cmd: AppCommand| KeyAction::AppCommand(cmd);

        KeyProfile {
            name: "Default".into(),
            bindings: vec![
                b(F(2), false, false, "Dir", app(OpenDirectory)),
                b(F(3), false, false, "Scripts", app(OpenScripts)),
                b(F(5), false, false, "Creds", app(OpenCredentials)),
                b(F(6), false, false, "Send", app(SendFile)),
                b(F(7), false, false, "Recv", app(ReceiveFile)),
                b(F(8), false, false, "Log", app(ToggleLog)),
                b(F(9), false, false, "Keys", app(OpenKeyMapping)),
                b(Char('q'), true, false, "Quit", app(Quit)),
                // Ctrl combos (no label — not shown in F-key bar)
                b(Char('t'), true, false, "", app(NewTab)),
                b(Char('w'), true, false, "", app(CloseTab)),
                // Alternate upload shortcut for terminals that reserve F6.
                b(Char('u'), false, true, "", app(SendFile)),
                // Alt+1–9 switch tabs
                b(Char('1'), false, true, "", app(SwitchTab(1))),
                b(Char('2'), false, true, "", app(SwitchTab(2))),
                b(Char('3'), false, true, "", app(SwitchTab(3))),
                b(Char('4'), false, true, "", app(SwitchTab(4))),
                b(Char('5'), false, true, "", app(SwitchTab(5))),
                b(Char('6'), false, true, "", app(SwitchTab(6))),
                b(Char('7'), false, true, "", app(SwitchTab(7))),
                b(Char('8'), false, true, "", app(SwitchTab(8))),
                b(Char('9'), false, true, "", app(SwitchTab(9))),
            ],
        }
    }

    fn from_raw(raw: RawProfile) -> Self {
        let bindings = raw
            .bindings
            .into_iter()
            .filter_map(|rb| {
                let key = KeySpec::parse(&rb.key)?;
                let action = KeyAction::parse(&rb.kind, rb.arg.as_deref())?;
                Some(KeyBinding {
                    key,
                    label: rb.label,
                    action,
                })
            })
            .collect();
        KeyProfile {
            name: raw.name,
            bindings,
        }
    }

    fn to_raw(&self) -> RawProfile {
        RawProfile {
            name: self.name.clone(),
            bindings: self.bindings.iter().map(RawBinding::from).collect(),
        }
    }
}

// ── KeyProfileStore ───────────────────────────────────────────────────────────

/// Manages key profiles stored under `~/.config/waystone-comm/key_profiles/`.
pub struct KeyProfileStore {
    dir: PathBuf,
}

impl KeyProfileStore {
    pub fn new_default() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("waystone-comm")
            .join("key_profiles");
        Self { dir }
    }

    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn profile_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.toml"))
    }

    /// Load a named profile. Returns `None` if the file does not exist.
    pub fn load(&self, name: &str) -> Option<KeyProfile> {
        let path = self.profile_path(name);
        let text = std::fs::read_to_string(&path).ok()?;
        let raw: RawProfile = toml::from_str(&text).ok()?;
        Some(KeyProfile::from_raw(raw))
    }

    /// Load the named profile, falling back to the built-in default.
    pub fn load_or_default(&self, name: &str) -> KeyProfile {
        self.load(name).unwrap_or_else(KeyProfile::default_profile)
    }

    /// Save a profile to disk. Creates the directory if needed.
    pub fn save(&self, profile: &KeyProfile) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let raw = profile.to_raw();
        let text = toml::to_string_pretty(&raw).map_err(std::io::Error::other)?;
        atomic_write(&self.profile_path(&profile.name), text.as_bytes())
    }

    /// Return the names of all saved profiles (without `.toml` extension).
    pub fn list(&self) -> Vec<String> {
        std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name();
                let s = name.to_str()?;
                s.strip_suffix(".toml").map(str::to_string)
            })
            .collect()
    }

    /// Load the "Default" profile from disk, or return the built-in default.
    /// Also ensures the Default profile is persisted to disk if it didn't exist.
    pub fn load_global_default(&self) -> KeyProfile {
        if let Some(p) = self.load("Default") {
            return p;
        }
        let p = KeyProfile::default_profile();
        self.save(&p).ok();
        p
    }
}

// ── Convenience re-export ─────────────────────────────────────────────────────

/// A parsed key path for display ("Ctrl+T", "F3", etc.)
pub fn format_key(spec: &KeySpec) -> String {
    spec.to_string()
}

/// Compact key name for the fkey bar (e.g. "^Q", "^T", "Alt+1").
fn key_display_name(spec: &KeySpec) -> String {
    if let KeyCode::Char(c) = spec.code {
        if spec.ctrl {
            return format!("^{}", c.to_ascii_uppercase());
        }
        if spec.alt {
            return format!("A+{}", c.to_ascii_uppercase());
        }
    }
    spec.to_string()
}

/// Validate a binding's action string in the form "kind:arg" or "app:Cmd".
/// Returns an error message if invalid.
pub fn validate_action_string(s: &str) -> Result<(), String> {
    let (kind, arg) = if let Some(pos) = s.find(':') {
        (&s[..pos], Some(&s[pos + 1..]))
    } else {
        (s, None)
    };
    KeyAction::parse(kind, arg)
        .map(|_| ())
        .ok_or_else(|| format!("unknown action: {s:?}"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fkey() {
        let spec = KeySpec::parse("F3").unwrap();
        assert_eq!(spec.code, KeyCode::F(3));
        assert!(!spec.ctrl && !spec.alt && !spec.shift);
    }

    #[test]
    fn parse_ctrl_modifier() {
        let spec = KeySpec::parse("Ctrl+T").unwrap();
        assert_eq!(spec.code, KeyCode::Char('t'));
        assert!(spec.ctrl);
        assert!(!spec.alt);
    }

    #[test]
    fn parse_alt_digit() {
        let spec = KeySpec::parse("Alt+1").unwrap();
        assert_eq!(spec.code, KeyCode::Char('1'));
        assert!(spec.alt);
    }

    #[test]
    fn parse_ctrl_alt() {
        let spec = KeySpec::parse("Ctrl+Alt+S").unwrap();
        assert!(spec.ctrl && spec.alt);
        assert_eq!(spec.code, KeyCode::Char('s'));
    }

    #[test]
    fn parse_rejects_empty_key_code() {
        assert!(KeySpec::parse("").is_none());
        assert!(KeySpec::parse("Ctrl+").is_none());
    }

    #[test]
    fn parse_rejects_unknown_multi_character_key_code() {
        assert!(KeySpec::parse("NoSuchKey").is_none());
        assert!(KeySpec::parse("Ctrl+NoSuchKey").is_none());
    }

    #[test]
    fn roundtrip_display_parse() {
        let original = KeySpec::parse("Ctrl+F6").unwrap();
        let s = original.to_string();
        let parsed = KeySpec::parse(&s).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn default_profile_has_quit() {
        let p = KeyProfile::default_profile();
        let spec = KeySpec::parse("Ctrl+Q").unwrap();
        let action = p.lookup(&spec).unwrap();
        assert!(matches!(action, KeyAction::AppCommand(AppCommand::Quit)));
    }

    #[test]
    fn default_profile_has_directory_on_f2() {
        let p = KeyProfile::default_profile();
        let spec = KeySpec::parse("F2").unwrap();
        let action = p.lookup(&spec).unwrap();
        assert!(matches!(
            action,
            KeyAction::AppCommand(AppCommand::OpenDirectory)
        ));
    }

    #[test]
    fn default_profile_has_transfer_keys() {
        let p = KeyProfile::default_profile();
        let send = p.lookup(&KeySpec::parse("F6").unwrap()).unwrap();
        let send_alt = p.lookup(&KeySpec::parse("Alt+U").unwrap()).unwrap();
        let recv = p.lookup(&KeySpec::parse("F7").unwrap()).unwrap();
        assert!(matches!(send, KeyAction::AppCommand(AppCommand::SendFile)));
        assert!(matches!(
            send_alt,
            KeyAction::AppCommand(AppCommand::SendFile)
        ));
        assert!(matches!(
            recv,
            KeyAction::AppCommand(AppCommand::ReceiveFile)
        ));
    }

    #[test]
    fn fkey_bar_labels_returns_labeled_entries() {
        let p = KeyProfile::default_profile();
        let labels = p.fkey_bar_labels();
        assert!(labels.iter().any(|(k, l)| k == "F2" && l == "Dir"));
        assert!(labels.iter().any(|(k, l)| k == "F3" && l == "Scripts"));
        assert!(labels.iter().any(|(k, l)| k == "^Q" && l == "Quit"));
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyProfileStore::new(dir.path());
        let profile = KeyProfile::default_profile();
        store.save(&profile).unwrap();
        let loaded = store.load("Default").unwrap();
        assert_eq!(loaded.name, "Default");
        assert_eq!(loaded.bindings.len(), profile.bindings.len());
    }

    #[test]
    fn save_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyProfileStore::new(dir.path());
        let profile = KeyProfile::default_profile();

        store.save(&profile).unwrap();

        let temp_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();

        assert!(temp_files.is_empty());
    }

    #[test]
    fn unescape_text_sequences() {
        assert_eq!(unescape_text("hello\\r"), "hello\r");
        assert_eq!(unescape_text("\\x1b[A"), "\x1b[A");
        assert_eq!(unescape_text("a\\\\b"), "a\\b");
    }

    #[test]
    fn validate_action_string_ok() {
        assert!(validate_action_string("app:Quit").is_ok());
        assert!(validate_action_string("text:hello\\r").is_ok());
        assert!(validate_action_string("passthrough").is_ok());
    }

    #[test]
    fn validate_action_string_bad() {
        assert!(validate_action_string("nonsense").is_err());
        assert!(validate_action_string("app:NotACommand").is_err());
    }
}
