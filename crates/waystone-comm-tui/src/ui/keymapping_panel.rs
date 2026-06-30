//! Key mapping editor panel — view and edit key profile bindings.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};
use waystone_comm_core::keymapping::{
    validate_action_string, AppCommand, KeyAction, KeyBinding, KeyProfile, KeySpec,
};

pub enum KeymappingPanelAction {
    None,
    Close(KeyProfile),
}

enum EditState {
    /// Browsing the binding list.
    Browse,
    /// Editing a binding's action string in-place.
    EditAction {
        idx: usize,
        buf: String,
        error: Option<String>,
    },
    /// Typing a new key specification.
    AddKey { buf: String, error: Option<String> },
    /// Typing the action for the newly added key.
    AddAction {
        key_str: String,
        buf: String,
        error: Option<String>,
    },
}

pub struct KeymappingPanel {
    profile: KeyProfile,
    list_state: ListState,
    edit: EditState,
}

impl KeymappingPanel {
    pub fn new(profile: KeyProfile) -> Self {
        let mut list_state = ListState::default();
        if !profile.bindings.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            profile,
            list_state,
            edit: EditState::Browse,
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> KeymappingPanelAction {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);

        match &mut self.edit {
            // ── Edit action string ────────────────────────────────────────────
            EditState::EditAction { idx, buf, error } => {
                let idx = *idx;
                match code {
                    KeyCode::Esc => {
                        self.edit = EditState::Browse;
                    }
                    KeyCode::Enter => {
                        let s = buf.trim().to_string();
                        match validate_action_string(&s) {
                            Ok(()) => {
                                if let Some(action) = parse_action(&s) {
                                    self.profile.bindings[idx].action = action;
                                }
                                self.edit = EditState::Browse;
                            }
                            Err(e) => {
                                *error = Some(e);
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                        *error = None;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        buf.push(c);
                        *error = None;
                    }
                    _ => {}
                }
            }

            // ── Add new binding — key step ────────────────────────────────────
            EditState::AddKey { buf, error } => match code {
                KeyCode::Esc => {
                    self.edit = EditState::Browse;
                }
                KeyCode::Enter => {
                    let s = buf.trim().to_string();
                    if KeySpec::parse(&s).is_some() {
                        let key_str = s;
                        self.edit = EditState::AddAction {
                            key_str,
                            buf: String::new(),
                            error: None,
                        };
                    } else {
                        *error = Some(format!("unknown key: {s:?}"));
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                    *error = None;
                }
                KeyCode::Char(c) if !ctrl => {
                    buf.push(c);
                    *error = None;
                }
                _ => {}
            },

            // ── Add new binding — action step ─────────────────────────────────
            EditState::AddAction {
                key_str,
                buf,
                error,
            } => {
                let key_str = key_str.clone();
                match code {
                    KeyCode::Esc => {
                        self.edit = EditState::Browse;
                    }
                    KeyCode::Enter => {
                        let s = buf.trim().to_string();
                        match validate_action_string(&s) {
                            Ok(()) => {
                                if let (Some(key), Some(action)) =
                                    (KeySpec::parse(&key_str), parse_action(&s))
                                {
                                    self.profile.bindings.push(KeyBinding {
                                        key,
                                        label: None,
                                        action,
                                    });
                                    let last = self.profile.bindings.len() - 1;
                                    self.list_state.select(Some(last));
                                }
                                self.edit = EditState::Browse;
                            }
                            Err(e) => {
                                *error = Some(e);
                            }
                        }
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                        *error = None;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        buf.push(c);
                        *error = None;
                    }
                    _ => {}
                }
            }

            // ── Browse mode ───────────────────────────────────────────────────
            EditState::Browse => match code {
                KeyCode::Esc | KeyCode::F(9) => {
                    return KeymappingPanelAction::Close(self.profile.clone());
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_selection(-1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_selection(1);
                }
                KeyCode::Enter => {
                    if let Some(idx) = self.list_state.selected() {
                        if idx < self.profile.bindings.len() {
                            let current = self.profile.bindings[idx].action.to_string();
                            self.edit = EditState::EditAction {
                                idx,
                                buf: current,
                                error: None,
                            };
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.edit = EditState::AddKey {
                        buf: String::new(),
                        error: None,
                    };
                }
                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                    if let Some(idx) = self.list_state.selected() {
                        if idx < self.profile.bindings.len() {
                            self.profile.bindings.remove(idx);
                            let new_sel = if self.profile.bindings.is_empty() {
                                None
                            } else {
                                Some(idx.min(self.profile.bindings.len() - 1))
                            };
                            self.list_state.select(new_sel);
                        }
                    }
                }
                _ => {}
            },
        }

        KeymappingPanelAction::None
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.profile.bindings.len();
        if len == 0 {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len as i32) as usize;
        self.list_state.select(Some(next));
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(format!(" Key Mappings — {} ", self.profile.name))
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Layout: [list | prompt/help at bottom]
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(inner);

        self.render_list(frame, chunks[0]);
        self.render_bottom(frame, chunks[1]);
    }

    fn render_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .profile
            .bindings
            .iter()
            .map(|b| {
                let key_str = format!("{:<18}", b.display_key());
                let lbl = b.label.as_deref().unwrap_or("");
                let lbl_str = format!("{:<10}", lbl);
                let action_str = b.display_action();
                let line = Line::from(vec![
                    Span::styled(key_str, Style::default().fg(Color::Cyan)),
                    Span::styled(lbl_str, Style::default().fg(Color::Yellow)),
                    Span::raw(action_str),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .style(Style::default().bg(Color::Black))
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_bottom(&self, frame: &mut Frame, area: Rect) {
        let line = match &self.edit {
            EditState::Browse => Line::from(vec![
                help_span("Enter", "edit"),
                help_span("N", "new"),
                help_span("D/Del", "delete"),
                help_span("F9/Esc", "close"),
            ]),
            EditState::EditAction { buf, error, .. } => {
                if let Some(e) = error {
                    Line::from(Span::styled(
                        format!(" Error: {e}"),
                        Style::default().fg(Color::Red),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(" Action: ", Style::default().fg(Color::White)),
                        Span::styled(format!("{buf}█"), Style::default().fg(Color::Yellow)),
                    ])
                }
            }
            EditState::AddKey { buf, error } => {
                if let Some(e) = error {
                    Line::from(Span::styled(
                        format!(" Error: {e}"),
                        Style::default().fg(Color::Red),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(
                            " New key (e.g. Ctrl+K): ",
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(format!("{buf}█"), Style::default().fg(Color::Green)),
                    ])
                }
            }
            EditState::AddAction {
                key_str,
                buf,
                error,
            } => {
                if let Some(e) = error {
                    Line::from(Span::styled(
                        format!(" Error: {e}"),
                        Style::default().fg(Color::Red),
                    ))
                } else {
                    Line::from(vec![
                        Span::styled(format!(" {key_str} → "), Style::default().fg(Color::Cyan)),
                        Span::styled(format!("{buf}█"), Style::default().fg(Color::Yellow)),
                    ])
                }
            }
        };

        frame.render_widget(
            Paragraph::new(line)
                .block(Block::default().borders(Borders::TOP))
                .style(Style::default().bg(Color::Black)),
            area,
        );
    }
}

fn help_span(key: &str, label: &str) -> Span<'static> {
    Span::raw(format!("  [{key}] {label}"))
}

/// Parse an action string like "app:Quit", "text:hello\r", "passthrough".
fn parse_action(s: &str) -> Option<KeyAction> {
    let (kind, arg) = if let Some(pos) = s.find(':') {
        (&s[..pos], Some(&s[pos + 1..]))
    } else {
        (s, None)
    };
    // Re-use the private parse via the public module path
    // We reconstruct by matching the kind ourselves since KeyAction::parse is not pub.
    match kind {
        "passthrough" => Some(KeyAction::Passthrough),
        "text" => Some(KeyAction::SendText(unescape(arg.unwrap_or("")))),
        "bytes" => {
            let hex = arg.unwrap_or("");
            let bytes = (0..hex.len())
                .step_by(2)
                .filter_map(|i| hex.get(i..i + 2)?.as_bytes().try_into().ok())
                .filter_map(|b: &[u8; 2]| {
                    std::str::from_utf8(b)
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                })
                .collect();
            Some(KeyAction::SendBytes(bytes))
        }
        "script" => Some(KeyAction::RunScript(arg.unwrap_or("").to_string())),
        "app" => {
            let cmd = match arg.unwrap_or("") {
                "Quit" => AppCommand::Quit,
                "OpenDirectory" => AppCommand::OpenDirectory,
                "OpenScripts" => AppCommand::OpenScripts,
                "OpenKeyMapping" => AppCommand::OpenKeyMapping,
                "NewTab" => AppCommand::NewTab,
                "CloseTab" => AppCommand::CloseTab,
                "SendFile" => AppCommand::SendFile,
                "ReceiveFile" => AppCommand::ReceiveFile,
                "ToggleLog" => AppCommand::ToggleLog,
                "OpenCredentials" => AppCommand::OpenCredentials,
                s if s.starts_with("SwitchTab:") => {
                    let n = s["SwitchTab:".len()..].parse().ok()?;
                    AppCommand::SwitchTab(n)
                }
                _ => return None,
            };
            Some(KeyAction::AppCommand(cmd))
        }
        _ => None,
    }
}

fn unescape(s: &str) -> String {
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
                    }
                }
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
