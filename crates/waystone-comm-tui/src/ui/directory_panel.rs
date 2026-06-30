use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use uuid::Uuid;
use waystone_comm_core::{
    connection::Protocol,
    directory::{Directory, DirectoryEntry},
};

// ── Row type (flat list built from grouped entries) ───────────────────────────

#[derive(Debug, Clone)]
enum Row {
    GroupHeader(String),
    Entry(Uuid),
}

// ── Entry form state ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct EntryForm {
    fields: [String; 8], // name, protocol, host, port, username, emulation, credential UUID, legacy SSH
    focused: usize,
    error: Option<String>,
    editing_id: Option<Uuid>,
}

const FORM_LABELS: [&str; 8] = [
    "Name",
    "Protocol (ssh/telnet/serial/raw)",
    "Host / serial device",
    "Port (optional)",
    "Username (optional)",
    "Emulation (xterm-256color/vt100/ansi-bbs)",
    "Credential UUID (optional)",
    "Legacy SSH (yes/no)",
];

impl EntryForm {
    fn new_entry() -> Self {
        let mut form = Self::default();
        form.fields[1] = "ssh".into();
        form.fields[5] = "xterm-256color".into();
        form.fields[7] = "no".into();
        form
    }

    fn from_entry(entry: &DirectoryEntry) -> Self {
        let mut form = Self {
            editing_id: Some(entry.id),
            ..Self::default()
        };
        form.fields[0] = entry.name.clone();
        form.fields[1] = protocol_form_value(&entry.protocol).to_string();
        form.fields[2] = entry.connection.host.clone();
        form.fields[3] = entry
            .connection
            .port
            .map(|p| p.to_string())
            .unwrap_or_default();
        form.fields[4] = entry.connection.username.clone().unwrap_or_default();
        form.fields[5] = entry.terminal.emulation.clone();
        form.fields[6] = entry
            .credential_id
            .map(|id| id.to_string())
            .unwrap_or_default();
        form.fields[7] = if is_legacy_ssh_entry(entry) {
            "yes".into()
        } else {
            "no".into()
        };
        form
    }

    fn current_field_mut(&mut self) -> &mut String {
        &mut self.fields[self.focused]
    }

    fn handle_char(&mut self, c: char) {
        self.current_field_mut().push(c);
        self.error = None;
    }

    fn handle_backspace(&mut self) {
        self.current_field_mut().pop();
        self.error = None;
    }

    fn next_field(&mut self) {
        self.focused = (self.focused + 1) % FORM_LABELS.len();
    }

    fn prev_field(&mut self) {
        self.focused = (self.focused + FORM_LABELS.len() - 1) % FORM_LABELS.len();
    }

    fn build_entry(&self, existing: Option<&DirectoryEntry>) -> Result<DirectoryEntry, String> {
        let name = self.fields[0].trim().to_string();
        if name.is_empty() {
            return Err("Name is required".into());
        }
        let proto_str = self.fields[1].trim().to_lowercase();
        let protocol = match proto_str.as_str() {
            "ssh" => Protocol::Ssh,
            "telnet" => Protocol::Telnet,
            "serial" => Protocol::Serial,
            "raw" => Protocol::Raw,
            "" => Protocol::Ssh,
            other => {
                return Err(format!(
                    "Unknown protocol: '{other}' (use ssh/telnet/serial/raw)"
                ))
            }
        };
        let host = self.fields[2].trim().to_string();
        if host.is_empty() {
            return Err(match protocol {
                Protocol::Serial => "Serial device path is required".into(),
                _ => "Host is required".into(),
            });
        }
        let legacy_ssh = parse_bool_field(&self.fields[7])?;
        if legacy_ssh && protocol != Protocol::Ssh {
            return Err("Legacy SSH applies only to ssh entries".into());
        }
        let mut entry = existing
            .cloned()
            .unwrap_or_else(|| DirectoryEntry::new(name.clone(), protocol.clone(), host.clone()));
        entry.name = name;
        entry.protocol = protocol;
        entry.connection.host = host;
        entry.connection.port = None;
        entry.connection.username = None;
        entry.credential_id = None;
        let port_str = self.fields[3].trim();
        if !port_str.is_empty() {
            entry.connection.port = Some(
                port_str
                    .parse::<u16>()
                    .map_err(|_| "Port must be a number 1–65535".to_string())?,
            );
        }
        let username = self.fields[4].trim().to_string();
        if !username.is_empty() {
            entry.connection.username = Some(username);
        }
        let emulation = self.fields[5].trim().to_lowercase();
        if !emulation.is_empty() {
            entry.terminal.emulation = match emulation.as_str() {
                "xterm" | "xterm-256color" | "xterm-256" => "xterm-256color".into(),
                "vt100" => "vt100".into(),
                "vt220" => "vt220".into(),
                "ansi" | "ansi-bbs" | "ansi_bbs" | "ansibbs" | "bbs" => "ansi-bbs".into(),
                other => {
                    return Err(format!(
                        "Unknown emulation: '{other}' (use xterm-256color/vt100/ansi-bbs)"
                    ))
                }
            };
        }
        let credential_id = self.fields[6].trim();
        if !credential_id.is_empty() {
            entry.credential_id = Some(
                Uuid::parse_str(credential_id)
                    .map_err(|_| "Credential UUID must be a valid UUID".to_string())?,
            );
        }
        if legacy_ssh {
            entry
                .connection
                .extra
                .insert("legacy_ssh".into(), "true".into());
        } else {
            entry.connection.extra.remove("legacy_ssh");
        }
        Ok(entry)
    }
}

fn protocol_form_value(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::Ssh => "ssh",
        Protocol::Telnet => "telnet",
        Protocol::Serial => "serial",
        Protocol::Raw => "raw",
        Protocol::Sftp => "sftp",
        Protocol::Ftp => "ftp",
        Protocol::Ftps => "ftps",
        Protocol::Rlogin => "rlogin",
        Protocol::Mosh => "mosh",
        Protocol::Gemini => "gemini",
        Protocol::Gopher => "gopher",
        Protocol::Irc => "irc",
        Protocol::Nntp => "nntp",
        Protocol::Finger => "finger",
        Protocol::Http => "http",
        Protocol::Https => "https",
        Protocol::WebSocket => "websocket",
        Protocol::Tftp => "tftp",
    }
}

fn parse_bool_field(value: &str) -> Result<bool, String> {
    match value.trim().to_lowercase().as_str() {
        "" | "no" | "n" | "false" | "0" | "off" => Ok(false),
        "yes" | "y" | "true" | "1" | "on" => Ok(true),
        other => Err(format!("Legacy SSH must be yes/no, got '{other}'")),
    }
}

fn is_legacy_ssh_entry(entry: &DirectoryEntry) -> bool {
    entry
        .connection
        .extra
        .get("legacy_ssh")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

// ── Group form state ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct GroupForm {
    entry_id: Uuid,
    entry_name: String,
    value: String,
}

impl GroupForm {
    fn from_entry(entry: &DirectoryEntry) -> Self {
        Self {
            entry_id: entry.id,
            entry_name: entry.name.clone(),
            value: entry.group.clone().unwrap_or_default(),
        }
    }

    fn handle_char(&mut self, c: char) {
        self.value.push(c);
    }

    fn handle_backspace(&mut self) {
        self.value.pop();
    }
}

// ── Panel mode ────────────────────────────────────────────────────────────────

enum Mode {
    Browse,
    Searching,
    ConfirmDelete(Uuid, String), // id + name
    EntryForm(Box<EntryForm>),
    GroupForm(GroupForm),
    StatusMsg(String), // transient one-line message
}

// ── DirectoryPanel ────────────────────────────────────────────────────────────

pub struct DirectoryPanel {
    directory: Directory,
    rows: Vec<Row>,
    list_state: ListState,
    search: String,
    mode: Mode,
}

/// Action returned to the caller after handling a key event.
pub enum PanelAction {
    /// Stay in the directory panel.
    None,
    /// Connect to this entry.
    Connect(Box<DirectoryEntry>),
    /// Open the credential manager overlay.
    OpenCredentials,
    /// Open the script viewer/creator for the selected entry.
    OpenScripts(Option<(String, bool)>),
    /// User quit (F10 / Ctrl+C).
    Quit,
}

impl DirectoryPanel {
    /// Consume the panel and return the underlying directory (e.g. when
    /// transitioning from directory mode to session mode).
    pub fn into_directory(self) -> Directory {
        self.directory
    }

    /// Show an error message in the sidebar (used for connection failures).
    pub fn show_error(&mut self, msg: String) {
        self.mode = Mode::StatusMsg(format!("Connection failed: {msg}"));
    }

    pub fn is_entry_form_open(&self) -> bool {
        matches!(self.mode, Mode::EntryForm(_))
    }

    pub fn set_form_credential_id(&mut self, id: Uuid) -> bool {
        if let Mode::EntryForm(form) = &mut self.mode {
            form.fields[6] = id.to_string();
            form.focused = 6;
            form.error = None;
            true
        } else {
            false
        }
    }

    pub fn selected_entry_script_context(&self) -> Option<(String, bool)> {
        let id = self.selected_entry_id()?;
        self.directory
            .get_entry(id)
            .map(|entry| (entry.name.clone(), entry.credential_id.is_some()))
    }

    pub fn new(directory: Directory) -> Self {
        let mut panel = Self {
            directory,
            rows: Vec::new(),
            list_state: ListState::default(),
            search: String::new(),
            mode: Mode::Browse,
        };
        panel.rebuild_rows();
        if !panel.rows.is_empty() {
            panel.list_state.select(Some(0));
        }
        panel
    }

    // ── Row building ──────────────────────────────────────────────────────────

    fn rebuild_rows(&mut self) {
        let query = self.search.to_lowercase();
        let entries = self.directory.list_entries();

        // Collect matching entries, grouped.
        let mut groups: Vec<(Option<String>, Vec<&DirectoryEntry>)> = Vec::new();

        for entry in entries {
            let matches = query.is_empty()
                || entry.name.to_lowercase().contains(&query)
                || entry.connection.host.to_lowercase().contains(&query)
                || entry.tags.iter().any(|t| t.to_lowercase().contains(&query));

            if !matches {
                continue;
            }

            let group_key = entry.group.clone();
            if let Some(g) = groups.iter_mut().find(|(k, _)| *k == group_key) {
                g.1.push(entry);
            } else {
                groups.push((group_key, vec![entry]));
            }
        }

        let mut rows = Vec::new();
        for (group, entries) in groups {
            if let Some(name) = group {
                rows.push(Row::GroupHeader(name));
            }
            for e in entries {
                rows.push(Row::Entry(e.id));
            }
        }
        self.rows = rows;

        // Keep selection valid.
        let sel = self.list_state.selected().unwrap_or(0);
        if self.rows.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(sel.min(self.rows.len() - 1)));
        }
    }

    fn selected_entry_id(&self) -> Option<Uuid> {
        let idx = self.list_state.selected()?;
        match self.rows.get(idx)? {
            Row::Entry(id) => Some(*id),
            Row::GroupHeader(_) => None,
        }
    }

    fn move_to_next_entry(&mut self, forward: bool) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let start = self.list_state.selected().unwrap_or(0);
        let mut idx = start;
        for _ in 0..len {
            idx = if forward {
                (idx + 1) % len
            } else {
                (idx + len - 1) % len
            };
            if matches!(self.rows[idx], Row::Entry(_)) {
                self.list_state.select(Some(idx));
                return;
            }
        }
    }

    fn move_selection(&mut self, forward: bool) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0);
        let next = if forward {
            (cur + 1) % len
        } else {
            (cur + len - 1) % len
        };
        self.list_state.select(Some(next));
    }

    // ── Key handling ──────────────────────────────────────────────────────────

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> PanelAction {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);

        match &mut self.mode {
            Mode::StatusMsg(_) => {
                // Any key clears the message.
                self.mode = Mode::Browse;
                return PanelAction::None;
            }

            Mode::ConfirmDelete(id, name) => {
                let id = *id;
                let name = name.clone();
                match code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.directory.delete_entry(id);
                        let save_result = self.directory.save();
                        self.rebuild_rows();
                        self.mode = Mode::StatusMsg(match save_result {
                            Ok(()) => format!("Deleted '{name}'"),
                            Err(err) => format!("Deleted '{name}', but save failed: {err}"),
                        });
                    }
                    _ => {
                        self.mode = Mode::Browse;
                    }
                }
                return PanelAction::None;
            }

            Mode::Searching => match code {
                KeyCode::Esc => {
                    self.search.clear();
                    self.mode = Mode::Browse;
                    self.rebuild_rows();
                    if !self.rows.is_empty() {
                        self.list_state.select(Some(0));
                    }
                }
                KeyCode::Enter => {
                    self.mode = Mode::Browse;
                    // Advance to first entry row.
                    if !self.rows.is_empty() {
                        self.list_state.select(Some(0));
                        if matches!(self.rows[0], Row::GroupHeader(_)) {
                            self.move_to_next_entry(true);
                        }
                    }
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.rebuild_rows();
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.rebuild_rows();
                }
                _ => {}
            },

            Mode::EntryForm(form) => {
                match code {
                    KeyCode::Esc => {
                        self.mode = Mode::Browse;
                    }
                    KeyCode::Tab | KeyCode::Down => {
                        form.next_field();
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        form.prev_field();
                    }
                    KeyCode::F(5) => {
                        return PanelAction::OpenCredentials;
                    }
                    KeyCode::Enter => {
                        let existing = form
                            .editing_id
                            .and_then(|id| self.directory.get_entry(id).cloned());
                        match form.build_entry(existing.as_ref()) {
                            Ok(entry) => {
                                let msg = if form.editing_id.is_some() {
                                    self.directory.update_entry(entry);
                                    "Entry updated."
                                } else {
                                    self.directory.add_entry(entry);
                                    "Entry saved."
                                };
                                let save_result = self.directory.save();
                                self.rebuild_rows();
                                self.mode = Mode::StatusMsg(match save_result {
                                    Ok(()) => msg.into(),
                                    Err(err) => format!("{msg} Save failed: {err}"),
                                });
                            }
                            Err(msg) => {
                                form.error = Some(msg);
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        form.handle_backspace();
                    }
                    KeyCode::Char(c) => {
                        form.handle_char(c);
                    }
                    _ => {}
                }
                return PanelAction::None;
            }

            Mode::GroupForm(form) => {
                match code {
                    KeyCode::Esc => {
                        self.mode = Mode::Browse;
                    }
                    KeyCode::Enter => {
                        let value = form.value.trim().to_string();
                        let id = form.entry_id;
                        let entry_name = form.entry_name.clone();
                        let mut entry = self.directory.get_entry(id).cloned();
                        if let Some(entry) = entry.as_mut() {
                            entry.group = if value.is_empty() { None } else { Some(value) };
                            self.directory.update_entry(entry.clone());
                            let save_result = self.directory.save();
                            self.rebuild_rows();
                            self.mode = Mode::StatusMsg(match save_result {
                                Ok(()) => format!("Updated group for '{entry_name}'"),
                                Err(err) => {
                                    format!(
                                        "Updated group for '{entry_name}', but save failed: {err}"
                                    )
                                }
                            });
                        } else {
                            self.mode = Mode::StatusMsg("Selected entry no longer exists.".into());
                        }
                    }
                    KeyCode::Backspace => {
                        form.handle_backspace();
                    }
                    KeyCode::Char(c) => {
                        form.handle_char(c);
                    }
                    _ => {}
                }
                return PanelAction::None;
            }

            Mode::Browse => match code {
                KeyCode::Char('q') if ctrl => return PanelAction::Quit,
                KeyCode::Char('c') if ctrl => return PanelAction::Quit,
                KeyCode::F(3) => {
                    return PanelAction::OpenScripts(self.selected_entry_script_context())
                }
                KeyCode::F(5) => return PanelAction::OpenCredentials,

                KeyCode::Up | KeyCode::Char('k') => self.move_selection(false),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(true),

                KeyCode::Enter => {
                    if let Some(id) = self.selected_entry_id() {
                        if let Some(entry) = self.directory.get_entry(id) {
                            return PanelAction::Connect(Box::new(entry.clone()));
                        }
                    }
                }

                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.mode = Mode::EntryForm(Box::new(EntryForm::new_entry()));
                }

                KeyCode::Char('d') | KeyCode::Char('D') => {
                    if let Some(id) = self.selected_entry_id() {
                        if let Some(entry) = self.directory.get_entry(id) {
                            let name = entry.name.clone();
                            self.mode = Mode::ConfirmDelete(id, name);
                        }
                    }
                }

                KeyCode::Char('e') | KeyCode::Char('E') => {
                    if let Some(id) = self.selected_entry_id() {
                        if let Some(entry) = self.directory.get_entry(id) {
                            self.mode = Mode::EntryForm(Box::new(EntryForm::from_entry(entry)));
                        }
                    }
                }

                KeyCode::Char('g') | KeyCode::Char('G') => {
                    if let Some(id) = self.selected_entry_id() {
                        if let Some(entry) = self.directory.get_entry(id) {
                            self.mode = Mode::GroupForm(GroupForm::from_entry(entry));
                        }
                    }
                }

                KeyCode::Char('/') => {
                    self.mode = Mode::Searching;
                }

                _ => {}
            },
        }

        PanelAction::None
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        // Outer layout: toolbar=1, body=*, fkeys=1
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // title bar
                Constraint::Length(1), // search/toolbar
                Constraint::Min(1),    // list + sidebar
                Constraint::Length(1), // fkey bar
            ])
            .split(area);

        self.render_title(f, outer[0]);
        self.render_toolbar(f, outer[1]);
        self.render_body(f, outer[2]);
        self.render_fkeys(f, outer[3]);

        // Overlay modal for new-entry form.
        if let Mode::EntryForm(_) = &self.mode {
            self.render_new_entry_form(f, area);
        }
        if let Mode::GroupForm(_) = &self.mode {
            self.render_group_form(f, area);
        }
    }

    fn render_title(&self, f: &mut Frame, area: Rect) {
        let title = Span::styled(
            format!(
                " DIALING DIRECTORY — Waystone Comm v{} ",
                env!("CARGO_PKG_VERSION")
            ),
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        );
        f.render_widget(Paragraph::new(Line::from(title)), area);
    }

    fn render_toolbar(&self, f: &mut Frame, area: Rect) {
        let searching = matches!(self.mode, Mode::Searching);
        let search_label = if searching || !self.search.is_empty() {
            format!(
                " Search: {}{} ",
                self.search,
                if searching { "_" } else { "" }
            )
        } else {
            " Press / to search ".into()
        };
        let count = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Entry(_)))
            .count();
        let right = format!("{count} entries ");

        let total_w = area.width as usize;
        let left_w = search_label.len().min(total_w);
        let right_w = right.len().min(total_w.saturating_sub(left_w));
        let pad = total_w.saturating_sub(left_w + right_w);

        let line = Line::from(vec![
            Span::styled(&search_label, Style::default().fg(Color::Yellow)),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }

    fn render_body(&mut self, f: &mut Frame, area: Rect) {
        // Split body: list (60%) + sidebar (40%)
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);

        self.render_list(f, chunks[0]);
        self.render_sidebar(f, chunks[1]);
    }

    fn render_list(&mut self, f: &mut Frame, area: Rect) {
        let selected_id = self.selected_entry_id();

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| match row {
                Row::GroupHeader(name) => ListItem::new(Line::from(vec![
                    Span::styled(" \u{1F4C1} ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        name.clone(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])),
                Row::Entry(id) => {
                    let entry = self.directory.get_entry(*id);
                    let (name, proto, host, last) = entry
                        .map(|e| {
                            let last = e
                                .last_connected
                                .as_ref()
                                .map(|dt| {
                                    let secs =
                                        chrono::Utc::now().signed_duration_since(*dt).num_seconds();
                                    format_age(secs)
                                })
                                .unwrap_or_else(|| "never".into());
                            (
                                e.name.clone(),
                                e.protocol.to_string(),
                                e.connection.host.clone(),
                                last,
                            )
                        })
                        .unwrap_or_default();

                    let highlight = selected_id == Some(*id);
                    let style = if highlight {
                        Style::default().bg(Color::DarkGray)
                    } else {
                        Style::default()
                    };

                    ListItem::new(Line::from(vec![
                        Span::styled("  \u{25BA} ", Style::default().fg(Color::Cyan)),
                        Span::styled(
                            format!("{:<20}", truncate(&name, 20)),
                            style.fg(Color::White),
                        ),
                        Span::styled(
                            format!(" {:<7}", truncate(&proto, 7)),
                            style.fg(Color::Green),
                        ),
                        Span::styled(
                            format!(" {:<20}", truncate(&host, 20)),
                            style.fg(Color::Gray),
                        ),
                        Span::styled(format!(" {}", last), style.fg(Color::DarkGray)),
                    ]))
                }
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Entries ");

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(Color::DarkGray));

        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_sidebar(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Details ");

        let content: Vec<Line> = if let Some(id) = self.selected_entry_id() {
            if let Some(e) = self.directory.get_entry(id) {
                let last = e
                    .last_connected
                    .as_ref()
                    .map(|dt| {
                        let secs = chrono::Utc::now().signed_duration_since(*dt).num_seconds();
                        format_age(secs)
                    })
                    .unwrap_or_else(|| "never".into());

                let mut lines = vec![
                    Line::from(vec![
                        Span::styled("Name:     ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            e.name.clone(),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("Protocol: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(e.protocol.to_string(), Style::default().fg(Color::Green)),
                    ]),
                    Line::from(vec![
                        Span::styled("Host:     ", Style::default().fg(Color::DarkGray)),
                        Span::styled(e.connection.host.clone(), Style::default().fg(Color::Cyan)),
                    ]),
                ];
                if let Some(port) = e.connection.port {
                    lines.push(Line::from(vec![
                        Span::styled("Port:     ", Style::default().fg(Color::DarkGray)),
                        Span::styled(port.to_string(), Style::default().fg(Color::White)),
                    ]));
                }
                if let Some(user) = &e.connection.username {
                    lines.push(Line::from(vec![
                        Span::styled("User:     ", Style::default().fg(Color::DarkGray)),
                        Span::styled(user.clone(), Style::default().fg(Color::White)),
                    ]));
                }
                lines.push(Line::from(vec![
                    Span::styled("Emulation:", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!(" {}", e.terminal.emulation),
                        Style::default().fg(Color::White),
                    ),
                ]));
                if let Some(credential_id) = e.credential_id {
                    lines.push(Line::from(vec![
                        Span::styled("Cred ID:  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(credential_id.to_string(), Style::default().fg(Color::Cyan)),
                    ]));
                }
                if e.protocol == Protocol::Ssh {
                    lines.push(Line::from(vec![
                        Span::styled("Legacy:   ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            if is_legacy_ssh_entry(e) { "yes" } else { "no" },
                            Style::default().fg(if is_legacy_ssh_entry(e) {
                                Color::Yellow
                            } else {
                                Color::DarkGray
                            }),
                        ),
                    ]));
                }
                if let Some(group) = &e.group {
                    lines.push(Line::from(vec![
                        Span::styled("Group:    ", Style::default().fg(Color::DarkGray)),
                        Span::styled(group.clone(), Style::default().fg(Color::Yellow)),
                    ]));
                }
                if !e.tags.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("Tags:     ", Style::default().fg(Color::DarkGray)),
                        Span::styled(e.tags.join(", "), Style::default().fg(Color::Magenta)),
                    ]));
                }
                lines.push(Line::from(vec![
                    Span::styled("Last:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled(last, Style::default().fg(Color::White)),
                ]));
                if let Some(notes) = &e.notes {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "Notes:",
                        Style::default().fg(Color::DarkGray),
                    )));
                    for note_line in notes.lines() {
                        lines.push(Line::from(Span::styled(
                            note_line.to_string(),
                            Style::default().fg(Color::Gray),
                        )));
                    }
                }
                lines
            } else {
                vec![Line::from("")]
            }
        } else {
            vec![Line::from(Span::styled(
                "Select an entry",
                Style::default().fg(Color::DarkGray),
            ))]
        };

        // Confirm-delete overlay inside sidebar.
        if let Mode::ConfirmDelete(_, name) = &self.mode {
            let msg = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "DELETE ENTRY",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!("'{name}'")),
                Line::from(""),
                Line::from(Span::styled(
                    "Press Y to confirm",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(Span::styled(
                    "Any other key cancels",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            f.render_widget(
                Paragraph::new(msg).block(block).wrap(Wrap { trim: false }),
                area,
            );
            return;
        }

        // Status message overlay inside sidebar.
        if let Mode::StatusMsg(msg) = &self.mode {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    msg.clone(),
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Press any key to continue",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            f.render_widget(
                Paragraph::new(lines)
                    .block(block)
                    .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }

        f.render_widget(
            Paragraph::new(content)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_fkeys(&self, f: &mut Frame, area: Rect) {
        let spans = match &self.mode {
            Mode::Searching => vec![key_span("Enter", "confirm"), key_span("Esc", "cancel")],
            Mode::GroupForm(_) => vec![
                key_span("Enter", "save"),
                key_span("Esc", "cancel"),
                key_span("Empty", "clear group"),
            ],
            Mode::EntryForm(_) => vec![
                key_span("Tab", "next field"),
                key_span("F5", "credentials"),
                key_span("Enter", "save"),
                key_span("Esc", "cancel"),
            ],
            _ => vec![
                key_span("Enter", "Connect"),
                key_span("N", "New"),
                key_span("E", "Edit"),
                key_span("D", "Delete"),
                key_span("G", "Group"),
                key_span("F3", "Scripts"),
                key_span("F5", "Creds"),
                key_span("/", "Search"),
                key_span("^Q", "Quit"),
            ],
        };

        let mut line_spans = Vec::new();
        for (i, s) in spans.into_iter().enumerate() {
            if i > 0 {
                line_spans.push(Span::raw("  "));
            }
            line_spans.extend(s);
        }

        f.render_widget(
            Paragraph::new(Line::from(line_spans)).style(Style::default().bg(Color::DarkGray)),
            area,
        );
    }

    fn render_new_entry_form(&self, f: &mut Frame, area: Rect) {
        // Center a modal dialog.
        let form = if let Mode::EntryForm(form) = &self.mode {
            form
        } else {
            return;
        };

        let modal_w = 72u16.min(area.width.saturating_sub(4));
        let modal_h = 20u16.min(area.height.saturating_sub(4));
        let modal_x = (area.width.saturating_sub(modal_w)) / 2;
        let modal_y = (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect::new(modal_x, modal_y, modal_w, modal_h);

        f.render_widget(Clear, modal_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(if form.editing_id.is_some() {
                " Edit Entry "
            } else {
                " New Entry "
            });

        let inner = block.inner(modal_area);
        f.render_widget(block, modal_area);

        let mut lines: Vec<Line> = Vec::new();
        for (i, label) in FORM_LABELS.iter().enumerate() {
            let focused = form.focused == i;
            let label_style = if focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let value = &form.fields[i];
            let cursor = if focused { "_" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(format!("{label:<36}: "), label_style),
                Span::styled(
                    format!("{value}{cursor}"),
                    Style::default().fg(Color::White),
                ),
            ]));
            lines.push(Line::from(""));
        }

        if let Some(err) = &form.error {
            lines.push(Line::from(Span::styled(
                format!("  {err}"),
                Style::default().fg(Color::Red),
            )));
        }

        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn render_group_form(&self, f: &mut Frame, area: Rect) {
        let form = if let Mode::GroupForm(form) = &self.mode {
            form
        } else {
            return;
        };

        let modal_w = 58u16.min(area.width.saturating_sub(4));
        let modal_h = 9u16.min(area.height.saturating_sub(4));
        let modal_x = (area.width.saturating_sub(modal_w)) / 2;
        let modal_y = (area.height.saturating_sub(modal_h)) / 2;
        let modal_area = Rect::new(modal_x, modal_y, modal_w, modal_h);

        f.render_widget(Clear, modal_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Assign Group ");

        let inner = block.inner(modal_area);
        f.render_widget(block, modal_area);

        let lines = vec![
            Line::from(vec![
                Span::styled("Entry: ", Style::default().fg(Color::DarkGray)),
                Span::styled(form.entry_name.clone(), Style::default().fg(Color::White)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Group: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!("{}_", form.value),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Leave blank to clear the group.",
                Style::default().fg(Color::DarkGray),
            )),
        ];

        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn key_span<'a>(key: &'a str, label: &'a str) -> Vec<Span<'a>> {
    vec![
        Span::styled(
            format!(" {key} "),
            Style::default()
                .bg(Color::Gray)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {label}"), Style::default().fg(Color::White)),
    ]
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

fn format_age(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_form_new_entry_has_operator_defaults() {
        let form = EntryForm::new_entry();

        assert_eq!(form.fields[1], "ssh");
        assert_eq!(form.fields[5], "xterm-256color");
        assert_eq!(form.fields[7], "no");
    }

    #[test]
    fn entry_form_builds_ansi_bbs_legacy_ssh_entry() {
        let mut form = EntryForm::default();
        form.fields[0] = "Mystic".into();
        form.fields[1] = "ssh".into();
        form.fields[2] = "mystic-anet.online".into();
        form.fields[3] = "22".into();
        form.fields[4] = "bbsuser".into();
        form.fields[5] = "ansi".into();
        form.fields[7] = "yes".into();

        let entry = form.build_entry(None).unwrap();

        assert_eq!(entry.protocol, Protocol::Ssh);
        assert_eq!(entry.connection.host, "mystic-anet.online");
        assert_eq!(entry.connection.port, Some(22));
        assert_eq!(entry.connection.username.as_deref(), Some("bbsuser"));
        assert_eq!(entry.terminal.emulation, "ansi-bbs");
        assert_eq!(
            entry.connection.extra.get("legacy_ssh").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn entry_form_edit_preserves_id_and_can_clear_legacy_ssh() {
        let mut existing = DirectoryEntry::new("Old", Protocol::Ssh, "old.example");
        let id = existing.id;
        existing
            .connection
            .extra
            .insert("legacy_ssh".into(), "true".into());

        let mut form = EntryForm::from_entry(&existing);
        form.fields[0] = "New".into();
        form.fields[2] = "new.example".into();
        form.fields[7] = "no".into();

        let entry = form.build_entry(Some(&existing)).unwrap();

        assert_eq!(entry.id, id);
        assert_eq!(entry.name, "New");
        assert_eq!(entry.connection.host, "new.example");
        assert!(!entry.connection.extra.contains_key("legacy_ssh"));
    }

    #[test]
    fn entry_form_rejects_legacy_ssh_for_non_ssh_entries() {
        let mut form = EntryForm::new_entry();
        form.fields[0] = "Retroboard".into();
        form.fields[1] = "telnet".into();
        form.fields[2] = "bbs.retroboardbbs.com".into();
        form.fields[7] = "yes".into();

        let err = form.build_entry(None).unwrap_err();

        assert_eq!(err, "Legacy SSH applies only to ssh entries");
    }

    #[test]
    fn entry_form_serial_requires_device_path() {
        let mut form = EntryForm::new_entry();
        form.fields[0] = "Local Modem".into();
        form.fields[1] = "serial".into();
        form.fields[2].clear();

        let err = form.build_entry(None).unwrap_err();

        assert_eq!(err, "Serial device path is required");
    }

    #[test]
    fn entry_form_f5_opens_credentials() {
        let directory = Directory::load(std::env::temp_dir().join(format!(
            "waystone-comm-directory-test-{}.toml",
            Uuid::new_v4()
        )))
        .unwrap();
        let mut panel = DirectoryPanel::new(directory);
        panel.mode = Mode::EntryForm(Box::new(EntryForm::new_entry()));

        let action = panel.handle_key(KeyCode::F(5), KeyModifiers::NONE);

        assert!(matches!(action, PanelAction::OpenCredentials));
    }

    #[test]
    fn set_form_credential_id_updates_open_entry_form() {
        let directory = Directory::load(std::env::temp_dir().join(format!(
            "waystone-comm-directory-test-{}.toml",
            Uuid::new_v4()
        )))
        .unwrap();
        let mut panel = DirectoryPanel::new(directory);
        panel.mode = Mode::EntryForm(Box::new(EntryForm::new_entry()));
        let id = Uuid::new_v4();

        assert!(panel.set_form_credential_id(id));

        let Mode::EntryForm(form) = &panel.mode else {
            panic!("expected entry form");
        };
        assert_eq!(form.fields[6], id.to_string());
        assert_eq!(form.focused, 6);
    }

    #[test]
    fn group_shortcut_assigns_selected_entry_group() {
        let path = std::env::temp_dir().join(format!(
            "waystone-comm-directory-test-{}.toml",
            Uuid::new_v4()
        ));
        let mut directory = Directory::load(&path).unwrap();
        let entry = DirectoryEntry::new("GameSrv", Protocol::Telnet, "gamesrv.example");
        let entry_id = entry.id;
        directory.add_entry(entry);
        let mut panel = DirectoryPanel::new(directory);

        panel.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('B'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('B'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('S'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(
            panel
                .directory
                .get_entry(entry_id)
                .unwrap()
                .group
                .as_deref(),
            Some("BBS")
        );
        assert!(path.exists());
    }

    #[test]
    fn group_form_empty_value_clears_existing_group() {
        let path = std::env::temp_dir().join(format!(
            "waystone-comm-directory-test-{}.toml",
            Uuid::new_v4()
        ));
        let mut directory = Directory::load(path).unwrap();
        let mut entry = DirectoryEntry::new("GameSrv", Protocol::Telnet, "gamesrv.example");
        entry.group = Some("BBS".into());
        let entry_id = entry.id;
        directory.add_entry(entry);
        let mut panel = DirectoryPanel::new(directory);
        panel.list_state.select(Some(1));

        panel.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(panel.directory.get_entry(entry_id).unwrap().group, None);
    }
}
