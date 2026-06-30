//! Credential manager panel — list, add, delete, and generate SSH keys.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use waystone_comm_core::credentials::{
    generate_ssh_keypair, Credential, CredentialKind, CredentialSummary,
};

pub enum CredentialPanelAction {
    None,
    Close,
    /// Use this credential in the caller's current context.
    Select(CredentialSummary),
    /// A new credential was created; caller should store it.
    Store(Credential),
    /// Caller should delete this credential.
    Delete(uuid::Uuid),
    /// Caller should retrieve and display the selected SSH public key.
    ViewPublicKey(uuid::Uuid),
}

#[derive(Debug)]
enum PanelState {
    /// Browsing the credential list.
    Browse,
    /// Adding a new credential — collecting name.
    AddName { buf: String },
    /// Adding — choosing kind (P=Password, T=Token, K=SSH Key).
    AddKind { name: String },
    /// Adding — collecting username.
    AddUsername {
        name: String,
        kind: CredentialKind,
        buf: String,
    },
    /// Adding — collecting secret (masked).
    AddSecret {
        name: String,
        kind: CredentialKind,
        username: String,
        buf: String,
    },
    /// Showing generated SSH public key for copying.
    ShowPubkey {
        name: String,
        id: uuid::Uuid,
        pubkey: String,
    },
    /// Showing a credential UUID for copying into a directory entry.
    ShowCredentialId {
        name: String,
        kind: CredentialKind,
        id: uuid::Uuid,
    },
}

pub struct CredentialPanel {
    /// Loaded list of credential summaries (no secrets).
    items: Vec<CredentialSummary>,
    list_state: ListState,
    state: PanelState,
    /// Most recent status/error message.
    message: Option<(String, bool)>, // (text, is_error)
    /// In select mode, Enter chooses a credential instead of opening details.
    select_mode: bool,
}

impl CredentialPanel {
    pub fn new(items: Vec<CredentialSummary>) -> Self {
        Self::with_select_mode(items, false)
    }

    pub fn picker(items: Vec<CredentialSummary>) -> Self {
        Self::with_select_mode(items, true)
    }

    fn with_select_mode(items: Vec<CredentialSummary>, select_mode: bool) -> Self {
        let mut list_state = ListState::default();
        if !items.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            items,
            list_state,
            state: PanelState::Browse,
            message: None,
            select_mode,
        }
    }

    pub fn replace_items(&mut self, items: Vec<CredentialSummary>) {
        self.items = items;
        if self.items.is_empty() {
            self.list_state.select(None);
        } else {
            let idx = self.list_state.selected().unwrap_or(0);
            self.list_state.select(Some(idx.min(self.items.len() - 1)));
        }
    }

    pub fn show_public_key(&mut self, name: String, id: uuid::Uuid, pubkey: String) {
        self.state = PanelState::ShowPubkey { name, id, pubkey };
        self.message = None;
    }

    pub fn show_credential_id(&mut self, name: String, kind: CredentialKind, id: uuid::Uuid) {
        self.state = PanelState::ShowCredentialId { name, kind, id };
        self.message = None;
    }

    pub fn show_error(&mut self, message: impl Into<String>) {
        self.state = PanelState::Browse;
        self.message = Some((message.into(), true));
    }

    pub fn show_status(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), false));
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CredentialPanelAction {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);

        match &mut self.state {
            PanelState::ShowPubkey { .. } | PanelState::ShowCredentialId { .. } => {
                // Any key dismisses the detail display.
                self.state = PanelState::Browse;
                return CredentialPanelAction::None;
            }

            PanelState::AddName { buf } => match code {
                KeyCode::Esc => {
                    self.state = PanelState::Browse;
                }
                KeyCode::Enter => {
                    let name = buf.trim().to_string();
                    if name.is_empty() {
                        self.message = Some(("Name cannot be empty.".into(), true));
                    } else {
                        self.state = PanelState::AddKind { name };
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !ctrl => {
                    buf.push(c);
                }
                _ => {}
            },

            PanelState::AddKind { name } => {
                let name = name.clone();
                match code {
                    KeyCode::Esc => {
                        self.state = PanelState::Browse;
                    }
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        self.state = PanelState::AddUsername {
                            name,
                            kind: CredentialKind::Password,
                            buf: String::new(),
                        };
                    }
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        self.state = PanelState::AddUsername {
                            name,
                            kind: CredentialKind::Token,
                            buf: String::new(),
                        };
                    }
                    KeyCode::Char('k') | KeyCode::Char('K') => {
                        // SSH key: generate immediately, skip username/secret prompts.
                        match generate_ssh_keypair(&name, "") {
                            Ok((cred, pubkey)) => {
                                let show_name = name.clone();
                                let id = cred.id;
                                let action = CredentialPanelAction::Store(cred);
                                self.state = PanelState::ShowPubkey {
                                    name: show_name,
                                    id,
                                    pubkey,
                                };
                                self.message = Some(("SSH keypair generated.".into(), false));
                                return action;
                            }
                            Err(e) => {
                                self.message = Some((format!("Key generation failed: {e}"), true));
                                self.state = PanelState::Browse;
                            }
                        }
                    }
                    _ => {}
                }
            }

            PanelState::AddUsername { name, kind, buf } => match code {
                KeyCode::Esc => {
                    self.state = PanelState::Browse;
                }
                KeyCode::Enter => {
                    let username = buf.trim().to_string();
                    let name = name.clone();
                    let kind = kind.clone();
                    self.state = PanelState::AddSecret {
                        name,
                        kind,
                        username,
                        buf: String::new(),
                    };
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !ctrl => {
                    buf.push(c);
                }
                _ => {}
            },

            PanelState::AddSecret {
                name,
                kind,
                username,
                buf,
            } => match code {
                KeyCode::Esc => {
                    self.state = PanelState::Browse;
                }
                KeyCode::Enter => {
                    let secret = buf.clone();
                    if secret.is_empty() {
                        self.message = Some(("Secret cannot be empty.".into(), true));
                    } else {
                        let cred = Credential::new(
                            name.clone(),
                            kind.clone(),
                            if username.is_empty() {
                                None
                            } else {
                                Some(username.clone())
                            },
                            &secret,
                        );
                        self.state = PanelState::Browse;
                        self.message = Some(("Saving credential...".into(), false));
                        return CredentialPanelAction::Store(cred);
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if !ctrl => {
                    buf.push(c);
                }
                _ => {}
            },

            PanelState::Browse => match code {
                KeyCode::Esc | KeyCode::F(5) => {
                    return CredentialPanelAction::Close;
                }
                KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.state = PanelState::AddName { buf: String::new() };
                    self.message = None;
                }
                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Delete => {
                    if let Some(idx) = self.list_state.selected() {
                        if idx < self.items.len() {
                            return CredentialPanelAction::Delete(self.items[idx].id);
                        }
                    }
                }
                KeyCode::Enter if self.select_mode => {
                    if let Some(idx) = self.list_state.selected() {
                        if let Some(item) = self.items.get(idx) {
                            return CredentialPanelAction::Select(item.clone());
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Char('v') | KeyCode::Char('V') => {
                    if let Some(idx) = self.list_state.selected() {
                        if let Some(item) = self.items.get(idx) {
                            if item.kind == CredentialKind::SshKey {
                                return CredentialPanelAction::ViewPublicKey(item.id);
                            }
                            self.state = PanelState::ShowCredentialId {
                                name: item.name.clone(),
                                kind: item.kind.clone(),
                                id: item.id,
                            };
                        }
                    }
                }
                _ => {}
            },
        }

        CredentialPanelAction::None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);

        let block = Block::default()
            .title(" Credential Manager ")
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(inner);

        self.render_body(frame, chunks[0]);
        self.render_bottom(frame, chunks[1]);
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        match &self.state {
            PanelState::ShowPubkey { name, id, pubkey } => {
                let lines = vec![
                    Line::from(Span::styled(
                        format!(" Public key for \"{name}\""),
                        Style::default().fg(Color::Green),
                    )),
                    Line::from(format!(" Credential ID: {id}")),
                    Line::from(""),
                    Line::from(Span::styled(
                        " BEGIN PUBLIC KEY ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        pubkey.clone(),
                        Style::default().fg(Color::Yellow),
                    )),
                    Line::from(Span::styled(
                        " END PUBLIC KEY ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        " Copy the public key into authorized_keys. Press any key to close.",
                        Style::default().fg(Color::Cyan),
                    )),
                ];
                render_detail_box(frame, area, " SSH Public Key ", lines);
                return;
            }
            PanelState::ShowCredentialId { name, kind, id } => {
                let id_text = id.to_string();
                let lines = vec![
                    Line::from(Span::styled(
                        format!(" Credential ID for \"{name}\" ({kind})"),
                        Style::default().fg(Color::Green),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        " BEGIN CREDENTIAL UUID ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        format!("[{id_text}]"),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        " END CREDENTIAL UUID ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        " Paste the 36-character UUID between brackets into the directory entry's Credential UUID field.",
                        Style::default().fg(Color::Cyan),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        " Press any key to close.",
                        Style::default().fg(Color::DarkGray),
                    )),
                ];
                render_detail_box(frame, area, " Credential UUID ", lines);
                return;
            }
            PanelState::Browse => {}
            _ => {}
        }

        // List view (Browse) or overlay text (all other states are rendered in bottom bar).
        let items: Vec<ListItem> = self
            .items
            .iter()
            .map(|s| {
                let kind = format!("[{}]", s.kind);
                let user = s.username.as_deref().unwrap_or("");
                let line = Line::from(vec![
                    Span::styled(format!("{kind:<12}"), Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{:<24}", s.name), Style::default()),
                    Span::styled(format!("{user:<16}"), Style::default().fg(Color::DarkGray)),
                    Span::styled(s.id.to_string(), Style::default().fg(Color::DarkGray)),
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
        let line = match &self.state {
            PanelState::Browse => {
                let msg_span = if let Some((msg, is_err)) = &self.message {
                    Span::styled(
                        format!("  {msg}"),
                        Style::default().fg(if *is_err { Color::Red } else { Color::Green }),
                    )
                } else {
                    Span::raw("")
                };
                let mut spans = vec![
                    help_span("N", "new"),
                    if self.select_mode {
                        help_span("Enter", "select")
                    } else {
                        help_span("Enter/V", "view")
                    },
                    help_span("D/Del", "delete"),
                    help_span("F5/Esc", "close"),
                    msg_span,
                ];
                if self.select_mode {
                    spans.insert(2, help_span("V", "view"));
                }
                Line::from(spans)
            }
            PanelState::AddName { buf } => prompt_line("Name: ", buf, false),
            PanelState::AddKind { .. } => Line::from(vec![
                Span::styled(" Kind: ", Style::default().fg(Color::White)),
                help_span("P", "password"),
                help_span("T", "token"),
                help_span("K", "SSH keypair (generate)"),
            ]),
            PanelState::AddUsername { buf, .. } => {
                prompt_line("Username (Enter to skip): ", buf, false)
            }
            PanelState::AddSecret { buf, .. } => {
                // Mask the secret input.
                let masked = "*".repeat(buf.len());
                prompt_line("Secret: ", &masked, false)
            }
            PanelState::ShowPubkey { .. } | PanelState::ShowCredentialId { .. } => Line::from(""),
        };

        frame.render_widget(
            Paragraph::new(line)
                .block(Block::default().borders(Borders::TOP))
                .style(Style::default().bg(Color::Black)),
            area,
        );
    }

    fn move_sel(&mut self, delta: i32) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len as i32) as usize;
        self.list_state.select(Some(next));
    }
}

fn help_span(key: &str, label: &str) -> Span<'static> {
    Span::raw(format!("  [{key}] {label}"))
}

fn prompt_line<'a>(prompt: &'a str, buf: &str, _masked: bool) -> Line<'a> {
    Line::from(vec![
        Span::styled(prompt, Style::default().fg(Color::White)),
        Span::styled(format!("{buf}█"), Style::default().fg(Color::Yellow)),
    ])
}

fn render_detail_box(
    frame: &mut Frame,
    area: Rect,
    title: &'static str,
    lines: Vec<Line<'static>>,
) {
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::default().bg(Color::Black))
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn password_summary(name: &str) -> CredentialSummary {
        CredentialSummary {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            kind: CredentialKind::Password,
            username: Some("sysop".into()),
        }
    }

    #[test]
    fn picker_enter_selects_credential() {
        let summary = password_summary("Bottomless Abyss");
        let mut panel = CredentialPanel::picker(vec![summary.clone()]);

        let action = panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        match action {
            CredentialPanelAction::Select(selected) => {
                assert_eq!(selected.id, summary.id);
                assert_eq!(selected.name, summary.name);
            }
            _ => panic!("expected credential selection"),
        }
    }

    #[test]
    fn normal_enter_views_credential_id() {
        let summary = password_summary("Retroboard");
        let mut panel = CredentialPanel::new(vec![summary.clone()]);

        let action = panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert!(matches!(action, CredentialPanelAction::None));
        assert!(matches!(
            panel.state,
            PanelState::ShowCredentialId { id, .. } if id == summary.id
        ));
    }

    #[test]
    fn delete_action_does_not_optimistically_remove_item() {
        let summary = password_summary("Retroboard");
        let mut panel = CredentialPanel::new(vec![summary.clone()]);

        let action = panel.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);

        assert!(matches!(
            action,
            CredentialPanelAction::Delete(id) if id == summary.id
        ));
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.items[0].id, summary.id);
    }
}
