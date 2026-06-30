//! Session logging — timestamped text, HTML with ANSI color, JSON-lines, and raw modes.
//!
//! Log files live under `~/.config/waystone-comm/logs/<entry-name>/`.
//! Date rotation happens automatically; optional size-based rotation caps each file.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::PathBuf,
};

use chrono::Local;
use regex::Regex;
use serde::{Deserialize, Serialize};

// ── Log format ────────────────────────────────────────────────────────────────

/// Output format for session log files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Timestamped plain text — escape sequences stripped (default).
    #[default]
    Text,
    /// ANSI escape codes rendered as `<span style="color:...">` HTML.
    Html,
    /// One JSON object per line: `{"ts":"HH:MM:SS","text":"..."}`.
    Json,
    /// Raw bytes written exactly as received.
    Raw,
}

// ── Log settings ──────────────────────────────────────────────────────────────

/// Per-entry logging configuration stored in `DirectoryEntry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSettings {
    /// Whether to write a log file for this session.
    pub enabled: bool,
    /// Format of the log file.
    pub format: LogFormat,
    /// Rotate to a new file when this size (in bytes) is exceeded. 0 = no limit.
    pub max_size_bytes: u64,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            format: LogFormat::Text,
            max_size_bytes: 10 * 1024 * 1024, // 10 MB
        }
    }
}

// ── Credential scrubber ───────────────────────────────────────────────────────

/// Redacts common credential patterns in log output.
///
/// Patterns covered (case-insensitive):
/// - `password=<value>`
/// - `Password: <value>`
/// - `passwd=<value>`
/// - `secret=<value>`
/// - `token=<value>`
/// - `Authorization: Bearer <value>`
pub struct CredentialScrubber {
    patterns: Vec<(Regex, String)>,
}

impl CredentialScrubber {
    /// Build a scrubber with the default patterns.
    #[must_use]
    pub fn new() -> Self {
        let raw = [
            // key=value style
            (r"(?i)(password|passwd|secret|token|api_key)=\S+", "$1=***"),
            // "Password: value" header style
            (r"(?i)(Password):\s*\S+", "$1: ***"),
            // "Authorization: Bearer <token>" — keep "Bearer", redact the token
            (r"(?i)(Authorization):\s*Bearer\s+\S+", "$1: Bearer ***"),
            // Bare Bearer tokens
            (r"(?i)Bearer\s+\S+", "Bearer ***"),
        ];
        let patterns = raw
            .iter()
            .filter_map(|(pat, repl)| Regex::new(pat).ok().map(|r| (r, (*repl).to_string())))
            .collect();
        Self { patterns }
    }

    /// Return `text` with credential patterns replaced by `***`.
    #[must_use]
    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (re, repl) in &self.patterns {
            let replaced = re.replace_all(&out, repl.as_str()).into_owned();
            out = replaced;
        }
        out
    }
}

impl Default for CredentialScrubber {
    fn default() -> Self {
        Self::new()
    }
}

// ── SessionLog ────────────────────────────────────────────────────────────────

/// Writes session output to a log file.
///
/// Supports date rotation (always) and optional size-based rotation.
/// Format is controlled by [`LogSettings`].
pub struct SessionLog {
    writer: Option<BufWriter<File>>,
    /// Directory holding all log files for this session entry.
    log_dir: PathBuf,
    /// Date string of the currently-open file (`YYYY-MM-DD`).
    current_date: String,
    /// Sequence number appended when a file is size-rotated within a day.
    rotation_seq: u32,
    /// Bytes written to the current file (used for size rotation).
    current_size: u64,
    settings: LogSettings,
    scrubber: CredentialScrubber,
    /// For HTML format: whether we have an open `<pre>` block.
    html_started: bool,
}

impl SessionLog {
    /// Open (or create) a session log for `entry_name` using the given settings.
    ///
    /// # Errors
    /// Returns an error if the log directory cannot be created.
    pub fn new(
        entry_name: impl Into<String>,
        log_dir: impl Into<PathBuf>,
        settings: LogSettings,
    ) -> std::io::Result<Self> {
        let entry_name = sanitise_name(entry_name.into());
        let log_dir = log_dir.into().join(&entry_name);
        fs::create_dir_all(&log_dir)?;

        let mut log = Self {
            writer: None,
            log_dir,
            current_date: String::new(),
            rotation_seq: 0,
            current_size: 0,
            settings,
            scrubber: CredentialScrubber::new(),
            html_started: false,
        };
        log.rotate_if_needed()?;
        Ok(log)
    }

    /// Convenience constructor using the default config path and settings.
    ///
    /// # Errors
    /// Returns an error if the log directory cannot be created.
    pub fn with_default_path(entry_name: impl Into<String>) -> std::io::Result<Self> {
        let base = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("waystone-comm")
            .join("logs");
        Self::new(entry_name, base, LogSettings::default())
    }

    /// Write raw bytes from the session to the log.
    ///
    /// # Errors
    /// Returns an error if the underlying write fails.
    pub fn write_bytes(&mut self, data: &[u8]) -> std::io::Result<()> {
        if !self.settings.enabled {
            return Ok(());
        }
        self.rotate_if_needed()?;

        let bytes_written = match self.settings.format {
            LogFormat::Raw => self.write_raw(data)?,
            LogFormat::Text => self.write_text(data)?,
            LogFormat::Json => self.write_json(data)?,
            LogFormat::Html => self.write_html(data)?,
        };

        self.current_size += bytes_written as u64;

        // Size-based rotation: open a fresh file next call.
        if self.settings.max_size_bytes > 0 && self.current_size >= self.settings.max_size_bytes {
            self.rotation_seq += 1;
            self.current_date.clear(); // force rotate_if_needed on next write
        }

        Ok(())
    }

    /// Return the most recent `max_lines` lines from the current log file as plain text.
    ///
    /// Used by the log viewer panel. Returns an empty `Vec` if logging is disabled
    /// or no log file exists yet.
    #[must_use]
    pub fn load_recent_lines(&self, max_lines: usize) -> Vec<String> {
        let path = self.current_log_path();
        let Ok(content) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        // For HTML/Raw formats, serve raw bytes as-is so viewer shows them.
        // For JSON, strip the JSON envelope and show the text field.
        let lines: Vec<String> = match self.settings.format {
            LogFormat::Json => content
                .lines()
                .filter_map(|l| {
                    // Minimal parse: extract "text":"..." from the JSON object.
                    let start = l.find("\"text\":\"")? + 8;
                    let rest = &l[start..];
                    let end = rest.rfind('"')?;
                    Some(rest[..end].replace("\\n", "\n").replace("\\\"", "\""))
                })
                .collect(),
            _ => content.lines().map(str::to_string).collect(),
        };
        if lines.len() <= max_lines {
            lines
        } else {
            lines[lines.len() - max_lines..].to_vec()
        }
    }

    /// Return all log file paths for this entry, newest first.
    #[must_use]
    pub fn list_log_files(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(&self.log_dir) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "log"))
            .collect();
        paths.sort_by(|a, b| b.cmp(a)); // newest first (lexicographic on YYYY-MM-DD)
        paths
    }

    // ── Write helpers by format ───────────────────────────────────────────────

    fn writer_mut(&mut self) -> std::io::Result<&mut BufWriter<File>> {
        self.writer.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "log writer unavailable after rotation",
            )
        })
    }

    fn write_raw(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let w = self.writer_mut()?;
        w.write_all(data)?;
        w.flush()?;
        Ok(data.len())
    }

    fn write_text(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let printable = strip_non_printable(data);
        if printable.is_empty() {
            return Ok(0);
        }
        let text = self.scrubber.scrub(&printable);

        let now = Local::now();
        let timestamp = now.format("[%H:%M:%S] ");
        let w = self.writer_mut()?;

        let mut written = 0usize;
        let mut first = true;
        for line in text.split('\n') {
            if !first {
                writeln!(w)?;
                written += 1;
            }
            if !line.is_empty() {
                let s = format!("{timestamp}{line}");
                w.write_all(s.as_bytes())?;
                written += s.len();
            }
            first = false;
        }
        w.flush()?;
        Ok(written)
    }

    fn write_json(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let printable = strip_non_printable(data);
        if printable.is_empty() {
            return Ok(0);
        }
        let text = self.scrubber.scrub(&printable);
        let ts = Local::now().format("%H:%M:%S").to_string();

        let w = self.writer_mut()?;
        let mut written = 0usize;
        for line in text.split('\n') {
            if line.is_empty() {
                continue;
            }
            // Escape double-quotes and backslashes in the text.
            let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
            let record = format!("{{\"ts\":\"{ts}\",\"text\":\"{escaped}\"}}\n");
            w.write_all(record.as_bytes())?;
            written += record.len();
        }
        w.flush()?;
        Ok(written)
    }

    fn write_html(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let mut written = 0usize;

        if !self.html_started {
            let header = "<html><head><meta charset=\"utf-8\"><style>\
                body{background:#000;font-family:monospace;} \
                pre{color:#ccc;margin:0;}\
                </style></head><body><pre>\n";
            let w = self.writer_mut()?;
            w.write_all(header.as_bytes())?;
            written += header.len();
            self.html_started = true;
        }

        let html = ansi_to_html(data);
        let scrubbed = self.scrubber.scrub(&html);
        let w = self.writer_mut()?;
        w.write_all(scrubbed.as_bytes())?;
        written += scrubbed.len();
        w.flush()?;
        Ok(written)
    }

    // ── Rotation ──────────────────────────────────────────────────────────────

    fn rotate_if_needed(&mut self) -> std::io::Result<()> {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let same_day = today == self.current_date;

        if same_day && self.writer.is_some() {
            return Ok(());
        }

        // Close the previous HTML file cleanly.
        if self.html_started {
            if let Some(ref mut w) = self.writer {
                w.write_all(b"</pre></body></html>\n")?;
                w.flush()?;
            }
            self.html_started = false;
        }

        if !same_day {
            self.rotation_seq = 0;
        }
        self.current_date = today;
        self.current_size = 0;

        let path = self.current_log_path();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }

    /// Return the path of the currently-open log file.
    pub fn current_path(&self) -> PathBuf {
        self.current_log_path()
    }

    fn current_log_path(&self) -> PathBuf {
        let ext = match self.settings.format {
            LogFormat::Html => "html",
            LogFormat::Json => "jsonl",
            _ => "log",
        };
        if self.rotation_seq == 0 {
            self.log_dir.join(format!("{}.{ext}", self.current_date))
        } else {
            self.log_dir
                .join(format!("{}.{}.{ext}", self.current_date, self.rotation_seq))
        }
    }
}

/// Strip terminal escape sequences and non-printable bytes, preserving
/// newlines and printable ASCII/UTF-8.
fn strip_non_printable(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            // ESC — skip escape sequence
            0x1B => {
                i += 1;
                if i < data.len() {
                    match data[i] {
                        b'[' => {
                            // CSI: skip until final byte (0x40–0x7E)
                            i += 1;
                            while i < data.len() && !(0x40..=0x7E).contains(&data[i]) {
                                i += 1;
                            }
                        }
                        b']' => {
                            // OSC: skip until ST (ESC \) or BEL
                            i += 1;
                            while i < data.len() {
                                if data[i] == 0x07 {
                                    break;
                                }
                                if data[i] == 0x1B && i + 1 < data.len() && data[i + 1] == b'\\' {
                                    i += 1;
                                    break;
                                }
                                i += 1;
                            }
                        }
                        _ => {} // Other ESC sequences: skip the one following byte
                    }
                }
            }
            // Keep newline and carriage return (rendered as newline)
            b'\n' => out.push('\n'),
            b'\r' => {} // strip bare CR
            // Keep printable ASCII
            0x20..=0x7E => out.push(b as char),
            // Try to decode multi-byte UTF-8 starting here
            0x80..=0xFF => {
                if let Some(ch) = try_utf8(&data[i..]) {
                    let len = ch.len_utf8();
                    out.push(ch);
                    i += len;
                    continue;
                }
                // Otherwise skip the byte
            }
            _ => {} // Other control characters: skip
        }
        i += 1;
    }
    out
}

/// Attempt to decode a UTF-8 scalar starting at the beginning of `bytes`.
fn try_utf8(bytes: &[u8]) -> Option<char> {
    let s = std::str::from_utf8(bytes).ok().or_else(|| {
        // Try progressively shorter slices (up to 4 bytes for UTF-8 max)
        for len in (1..=4.min(bytes.len())).rev() {
            if let Ok(s) = std::str::from_utf8(&bytes[..len]) {
                return Some(s);
            }
        }
        None
    })?;
    s.chars().next()
}

/// Replace characters that are not valid in file/directory names.
fn sanitise_name(name: String) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect()
}

// ── ANSI → HTML converter ─────────────────────────────────────────────────────

/// Convert raw bytes containing ANSI escape sequences to HTML.
///
/// SGR color attributes are rendered as inline `style` attributes inside
/// `<span>` tags.  All other escape sequences are stripped.  Plain text is
/// HTML-escaped.
fn ansi_to_html(data: &[u8]) -> String {
    // ANSI 16-color palette (foreground; background adds 10).
    const COLORS: [&str; 8] = [
        "#000000", "#cc0000", "#00cc00", "#cccc00", "#0000cc", "#cc00cc", "#00cccc", "#cccccc",
    ];
    const BRIGHT: [&str; 8] = [
        "#555555", "#ff5555", "#55ff55", "#ffff55", "#5555ff", "#ff55ff", "#55ffff", "#ffffff",
    ];

    let mut out = String::with_capacity(data.len() * 2);
    let mut span_open = false;
    let mut i = 0;

    macro_rules! push_span {
        ($style:expr) => {{
            if span_open {
                out.push_str("</span>");
            }
            out.push_str("<span style=\"");
            out.push_str($style);
            out.push_str("\">");
            span_open = true;
        }};
    }

    while i < data.len() {
        let b = data[i];
        match b {
            0x1B => {
                i += 1;
                if i >= data.len() {
                    break;
                }
                match data[i] {
                    b'[' => {
                        // CSI sequence: collect params until final byte 0x40–0x7E.
                        i += 1;
                        let start = i;
                        while i < data.len() && !(0x40..=0x7E).contains(&data[i]) {
                            i += 1;
                        }
                        if i >= data.len() {
                            break;
                        }
                        let final_byte = data[i];
                        if final_byte == b'm' {
                            // SGR — parse numeric params.
                            let param_str = std::str::from_utf8(&data[start..i]).unwrap_or("");
                            let params: Vec<u32> = param_str
                                .split(';')
                                .filter(|s| !s.is_empty())
                                .filter_map(|s| s.parse().ok())
                                .collect();
                            let params = if params.is_empty() {
                                vec![0u32]
                            } else {
                                params
                            };
                            let mut j = 0;
                            while j < params.len() {
                                match params[j] {
                                    0 => {
                                        if span_open {
                                            out.push_str("</span>");
                                            span_open = false;
                                        }
                                    }
                                    1 => push_span!("font-weight:bold"),
                                    3 => push_span!("font-style:italic"),
                                    4 => push_span!("text-decoration:underline"),
                                    n @ 30..=37 => {
                                        push_span!(&format!("color:{}", COLORS[(n - 30) as usize]));
                                    }
                                    n @ 90..=97 => {
                                        push_span!(&format!("color:{}", BRIGHT[(n - 90) as usize]));
                                    }
                                    n @ 40..=47 => {
                                        push_span!(&format!(
                                            "background-color:{}",
                                            COLORS[(n - 40) as usize]
                                        ));
                                    }
                                    n @ 100..=107 => {
                                        push_span!(&format!(
                                            "background-color:{}",
                                            BRIGHT[(n - 100) as usize]
                                        ));
                                    }
                                    38 if j + 2 < params.len() && params[j + 1] == 5 => {
                                        // 256-color fg: approximate as #rrggbb.
                                        let css = xterm256_to_css(params[j + 2] as u8);
                                        push_span!(&format!("color:{css}"));
                                        j += 2;
                                    }
                                    48 if j + 2 < params.len() && params[j + 1] == 5 => {
                                        let css = xterm256_to_css(params[j + 2] as u8);
                                        push_span!(&format!("background-color:{css}"));
                                        j += 2;
                                    }
                                    38 if j + 4 < params.len() && params[j + 1] == 2 => {
                                        let (r, g, b2) =
                                            (params[j + 2], params[j + 3], params[j + 4]);
                                        push_span!(&format!("color:rgb({r},{g},{b2})"));
                                        j += 4;
                                    }
                                    48 if j + 4 < params.len() && params[j + 1] == 2 => {
                                        let (r, g, b2) =
                                            (params[j + 2], params[j + 3], params[j + 4]);
                                        push_span!(&format!("background-color:rgb({r},{g},{b2})"));
                                        j += 4;
                                    }
                                    _ => {}
                                }
                                j += 1;
                            }
                        }
                        // Non-SGR CSI sequences are silently dropped.
                    }
                    b']' => {
                        // OSC: skip until BEL or ST.
                        i += 1;
                        while i < data.len() {
                            if data[i] == 0x07 {
                                break;
                            }
                            if data[i] == 0x1B && i + 1 < data.len() && data[i + 1] == b'\\' {
                                i += 1;
                                break;
                            }
                            i += 1;
                        }
                    }
                    _ => {} // Other ESC sequences: skip.
                }
            }
            b'\n' => out.push('\n'),
            b'\r' => {}
            b'&' => out.push_str("&amp;"),
            b'<' => out.push_str("&lt;"),
            b'>' => out.push_str("&gt;"),
            0x20..=0x7E => out.push(b as char),
            0x80..=0xFF => {
                if let Some(ch) = try_utf8(&data[i..]) {
                    let len = ch.len_utf8();
                    // HTML-escape only the few relevant chars.
                    match ch {
                        '&' => out.push_str("&amp;"),
                        '<' => out.push_str("&lt;"),
                        '>' => out.push_str("&gt;"),
                        _ => out.push(ch),
                    }
                    i += len;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if span_open {
        out.push_str("</span>");
    }
    out
}

/// Map an xterm 256-color index to a CSS `#rrggbb` string.
fn xterm256_to_css(n: u8) -> String {
    match n {
        0..=15 => {
            // Standard 16 colors (approximate).
            let palette: [&str; 16] = [
                "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080",
                "#c0c0c0", "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff",
                "#00ffff", "#ffffff",
            ];
            palette[n as usize].to_string()
        }
        16..=231 => {
            let n = n - 16;
            let b = (n % 6) * 51;
            let g = ((n / 6) % 6) * 51;
            let r = (n / 36) * 51;
            format!("#{r:02x}{g:02x}{b:02x}")
        }
        232..=255 => {
            let v = 8 + (n - 232) * 10;
            format!("#{v:02x}{v:02x}{v:02x}")
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_log(dir: &std::path::Path) -> SessionLog {
        SessionLog::new("test-session", dir, LogSettings::default()).expect("create log")
    }

    #[test]
    fn creates_log_directory() {
        let tmp = tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        make_log(&log_dir);
        let session_dir = log_dir.join("test-session");
        assert!(session_dir.is_dir());
    }

    #[test]
    fn creates_dated_log_file() {
        let tmp = tempdir().unwrap();
        let mut log = make_log(tmp.path());
        log.write_bytes(b"hello").unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let expected = tmp.path().join("test-session").join(format!("{today}.log"));
        assert!(expected.exists(), "log file should exist at {expected:?}");
    }

    #[test]
    fn missing_writer_returns_error_for_each_format() {
        let tmp = tempdir().unwrap();

        let mut log = SessionLog::new("raw", tmp.path(), LogSettings::default()).unwrap();
        log.writer = None;
        assert_missing_writer(log.write_raw(b"hello"));

        let mut log = SessionLog::new("text", tmp.path(), LogSettings::default()).unwrap();
        log.writer = None;
        assert_missing_writer(log.write_text(b"hello"));

        let mut log = SessionLog::new("json", tmp.path(), LogSettings::default()).unwrap();
        log.writer = None;
        assert_missing_writer(log.write_json(b"hello"));

        let mut log = SessionLog::new("html", tmp.path(), LogSettings::default()).unwrap();
        log.writer = None;
        assert_missing_writer(log.write_html(b"hello"));
    }

    fn assert_missing_writer(result: std::io::Result<usize>) {
        let err = result.unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::NotConnected);
        assert!(err.to_string().contains("log writer unavailable"));
    }

    #[test]
    fn log_content_has_timestamp_prefix() {
        let tmp = tempdir().unwrap();
        let mut log = make_log(tmp.path());
        log.write_bytes(b"hello world").unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let path = tmp.path().join("test-session").join(format!("{today}.log"));
        let content = fs::read_to_string(&path).unwrap();

        // Should start with [HH:MM:SS]
        assert!(content.starts_with('['), "content: {content:?}");
        assert!(content.contains("hello world"), "content: {content:?}");
    }

    #[test]
    fn strips_escape_sequences() {
        let tmp = tempdir().unwrap();
        let mut log = make_log(tmp.path());
        // ESC[1m = bold SGR, ESC[0m = reset
        log.write_bytes(b"\x1b[1mBold\x1b[0m Normal").unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let path = tmp.path().join("test-session").join(format!("{today}.log"));
        let content = fs::read_to_string(&path).unwrap();

        assert!(content.contains("Bold Normal"), "content: {content:?}");
        assert!(
            !content.contains('\x1b'),
            "ESC should be stripped: {content:?}"
        );
    }

    #[test]
    fn multiline_each_line_timestamped() {
        let tmp = tempdir().unwrap();
        let mut log = make_log(tmp.path());
        log.write_bytes(b"line1\nline2\nline3").unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let path = tmp.path().join("test-session").join(format!("{today}.log"));
        let content = fs::read_to_string(&path).unwrap();

        // Each non-empty line should be prefixed
        for line in content.lines() {
            if !line.is_empty() {
                assert!(
                    line.starts_with('['),
                    "line should be timestamped: {line:?}"
                );
            }
        }
        assert!(content.contains("line1"));
        assert!(content.contains("line2"));
        assert!(content.contains("line3"));
    }

    #[test]
    fn appends_on_reopen() {
        let tmp = tempdir().unwrap();
        {
            let mut log = make_log(tmp.path());
            log.write_bytes(b"first").unwrap();
        }
        {
            let mut log = make_log(tmp.path());
            log.write_bytes(b"second").unwrap();
        }

        let today = Local::now().format("%Y-%m-%d").to_string();
        let path = tmp.path().join("test-session").join(format!("{today}.log"));
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("first"));
        assert!(content.contains("second"));
    }

    #[test]
    fn sanitises_entry_name() {
        let tmp = tempdir().unwrap();
        // Entry name with path-separator characters
        let mut log =
            SessionLog::new("host/user:port", tmp.path(), LogSettings::default()).unwrap();
        log.write_bytes(b"ok").unwrap();
        // Directory should use sanitised name
        let sanitised = tmp.path().join("host_user_port");
        assert!(sanitised.is_dir());
    }

    #[test]
    fn strip_non_printable_keeps_ascii() {
        let result = strip_non_printable(b"Hello, World!");
        assert_eq!(result, "Hello, World!");
    }

    #[test]
    fn strip_non_printable_removes_csi() {
        let result = strip_non_printable(b"\x1b[31mRed\x1b[0m");
        assert_eq!(result, "Red");
    }

    #[test]
    fn strip_non_printable_keeps_newlines() {
        let result = strip_non_printable(b"a\nb");
        assert_eq!(result, "a\nb");
    }

    #[test]
    fn strip_non_printable_drops_cr() {
        let result = strip_non_printable(b"a\r\nb");
        assert_eq!(result, "a\nb");
    }

    // ── Credential scrubber ───────────────────────────────────────────────────

    #[test]
    fn scrubber_redacts_password_equals() {
        let s = CredentialScrubber::new();
        let out = s.scrub("login password=s3cr3t done");
        assert!(out.contains("password=***"), "got: {out}");
        assert!(!out.contains("s3cr3t"), "got: {out}");
    }

    #[test]
    fn scrubber_redacts_bearer_token() {
        let s = CredentialScrubber::new();
        let out = s.scrub("Authorization: Bearer abc123xyz");
        assert!(out.contains("Bearer ***"), "got: {out}");
        assert!(!out.contains("abc123xyz"), "got: {out}");
    }

    #[test]
    fn scrubber_leaves_normal_text_alone() {
        let s = CredentialScrubber::new();
        let text = "echo hello world";
        assert_eq!(s.scrub(text), text);
    }

    // ── JSON format ───────────────────────────────────────────────────────────

    #[test]
    fn json_format_produces_valid_lines() {
        let tmp = tempdir().unwrap();
        let settings = LogSettings {
            format: LogFormat::Json,
            ..LogSettings::default()
        };
        let mut log = SessionLog::new("json-test", tmp.path(), settings).unwrap();
        log.write_bytes(b"hello json").unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let path = tmp.path().join("json-test").join(format!("{today}.jsonl"));
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("\"text\":\"hello json\""),
            "content: {content}"
        );
        assert!(content.contains("\"ts\":"), "content: {content}");
    }

    // ── HTML format ───────────────────────────────────────────────────────────

    #[test]
    fn html_format_wraps_in_pre() {
        let tmp = tempdir().unwrap();
        let settings = LogSettings {
            format: LogFormat::Html,
            ..LogSettings::default()
        };
        let mut log = SessionLog::new("html-test", tmp.path(), settings).unwrap();
        log.write_bytes(b"\x1b[31mRed text\x1b[0m").unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let path = tmp.path().join("html-test").join(format!("{today}.html"));
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("<pre>"), "content: {content}");
        assert!(content.contains("Red text"), "content: {content}");
        assert!(
            content.contains("color:"),
            "should have color span: {content}"
        );
    }

    // ── Disabled logging ──────────────────────────────────────────────────────

    #[test]
    fn disabled_log_writes_nothing() {
        let tmp = tempdir().unwrap();
        let settings = LogSettings {
            enabled: false,
            ..LogSettings::default()
        };
        let mut log = SessionLog::new("disabled-test", tmp.path(), settings).unwrap();
        log.write_bytes(b"should not appear").unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        // File should not exist (no write happened, but directory will exist).
        let path = tmp
            .path()
            .join("disabled-test")
            .join(format!("{today}.log"));
        if path.exists() {
            let content = fs::read_to_string(&path).unwrap();
            assert!(content.is_empty(), "should have written nothing: {content}");
        }
    }

    // ── load_recent_lines ─────────────────────────────────────────────────────

    #[test]
    fn load_recent_lines_returns_written_content() {
        let tmp = tempdir().unwrap();
        let mut log = make_log(tmp.path());
        log.write_bytes(b"alpha\nbeta\ngamma").unwrap();
        let lines = log.load_recent_lines(100);
        let joined = lines.join("\n");
        assert!(joined.contains("alpha"), "lines: {lines:?}");
        assert!(joined.contains("gamma"), "lines: {lines:?}");
    }

    #[test]
    fn load_recent_lines_respects_max() {
        let tmp = tempdir().unwrap();
        let mut log = make_log(tmp.path());
        for i in 0..20 {
            log.write_bytes(format!("line{i}\n").as_bytes()).unwrap();
        }
        let lines = log.load_recent_lines(5);
        assert!(lines.len() <= 5, "got {} lines", lines.len());
    }
}
