use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
/// Script viewer/editor panel with basic Rhai keyword highlighting.
use waystone_comm_core::scripting::{Script, ScriptEngine, ScriptStore};

pub enum ScriptPanelAction {
    None,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryScriptContext {
    pub name: String,
    pub credential_attached: bool,
}

pub struct ScriptPanel {
    store: ScriptStore,
    entry_context: Option<EntryScriptContext>,
    scripts: Vec<Script>,
    list_state: ListState,
    scroll: u16,
    message: Option<String>,
    editor: Option<EditorState>,
}

#[derive(Debug, Clone)]
struct EditorState {
    path: PathBuf,
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

impl EditorState {
    fn new(script: &Script) -> Self {
        Self::from_path_content(script.path.clone(), &script.content)
    }

    fn from_path_content(path: PathBuf, content: &str) -> Self {
        let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            path,
            lines,
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    fn content(&self) -> String {
        let mut content = self.lines.join("\n");
        content.push('\n');
        content
    }

    fn current_line_mut(&mut self) -> &mut String {
        &mut self.lines[self.cursor_row]
    }

    fn insert_char(&mut self, c: char) {
        let col = self.cursor_col.min(self.lines[self.cursor_row].len());
        self.current_line_mut().insert(col, c);
        self.cursor_col = col + c.len_utf8();
    }

    fn insert_newline(&mut self) {
        let col = self.cursor_col.min(self.lines[self.cursor_row].len());
        let tail = self.current_line_mut().split_off(col);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_row, tail);
    }

    fn insert_line_below(&mut self) {
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_row, String::new());
    }

    fn delete_current_line(&mut self) {
        if self.lines.len() == 1 {
            self.lines[0].clear();
            self.cursor_col = 0;
            return;
        }

        self.lines.remove(self.cursor_row);
        if self.cursor_row >= self.lines.len() {
            self.cursor_row = self.lines.len() - 1;
        }
        self.clamp_col();
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let row = self.cursor_row;
            let prev = self.lines[row][..self.cursor_col]
                .char_indices()
                .last()
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            self.lines[row].replace_range(prev..self.cursor_col, "");
            self.cursor_col = prev;
        } else if self.cursor_row > 0 {
            let line = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
            self.lines[self.cursor_row].push_str(&line);
        }
    }

    fn delete(&mut self) {
        let row = self.cursor_row;
        if self.cursor_col < self.lines[row].len() {
            let end = self.lines[row][self.cursor_col..]
                .char_indices()
                .nth(1)
                .map(|(idx, _)| self.cursor_col + idx)
                .unwrap_or_else(|| self.lines[row].len());
            self.lines[row].replace_range(self.cursor_col..end, "");
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col = self.lines[self.cursor_row][..self.cursor_col]
                .char_indices()
                .last()
                .map(|(idx, _)| idx)
                .unwrap_or(0);
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].len();
        }
    }

    fn move_right(&mut self) {
        let line_len = self.lines[self.cursor_row].len();
        if self.cursor_col < line_len {
            self.cursor_col = self.lines[self.cursor_row][self.cursor_col..]
                .char_indices()
                .nth(1)
                .map(|(idx, _)| self.cursor_col + idx)
                .unwrap_or(line_len);
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.clamp_col();
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.clamp_col();
        }
    }

    fn clamp_col(&mut self) {
        self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
    }
}

impl ScriptPanel {
    pub fn new(store: &ScriptStore, entry_context: Option<EntryScriptContext>) -> Self {
        let scripts = store.list_scripts();
        let mut list_state = ListState::default();
        if !scripts.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            store: store.clone(),
            entry_context,
            scripts,
            list_state,
            scroll: 0,
            message: None,
            editor: None,
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> ScriptPanelAction {
        if self.editor.is_some() {
            self.handle_editor_key(code, modifiers);
            return ScriptPanelAction::None;
        }

        match code {
            KeyCode::Esc | KeyCode::F(3) => return ScriptPanelAction::Close,
            KeyCode::Char('n') | KeyCode::Char('N') => self.create_entry_script(),
            KeyCode::Char('l') | KeyCode::Char('L') => self.insert_login_template(),
            KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Enter => self.start_editing(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(false);
                self.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(true);
                self.scroll = 0;
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            _ => {}
        }
        ScriptPanelAction::None
    }

    fn handle_editor_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('s' | 'S')) {
            self.save_editor();
            return;
        }
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('o' | 'O')) {
            if let Some(editor) = &mut self.editor {
                editor.insert_line_below();
            }
            return;
        }
        if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('k' | 'K')) {
            if let Some(editor) = &mut self.editor {
                editor.delete_current_line();
            }
            return;
        }

        match code {
            KeyCode::Char('l') | KeyCode::Char('L')
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert_login_template();
            }
            KeyCode::Esc => {
                self.editor = None;
                self.message = Some("Edit cancelled; changes not saved.".into());
            }
            KeyCode::Enter => {
                if let Some(editor) = &mut self.editor {
                    editor.insert_newline();
                }
            }
            KeyCode::Backspace => {
                if let Some(editor) = &mut self.editor {
                    editor.backspace();
                }
            }
            KeyCode::Delete => {
                if let Some(editor) = &mut self.editor {
                    editor.delete();
                }
            }
            KeyCode::Left => {
                if let Some(editor) = &mut self.editor {
                    editor.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(editor) = &mut self.editor {
                    editor.move_right();
                }
            }
            KeyCode::Up => {
                if let Some(editor) = &mut self.editor {
                    editor.move_up();
                }
            }
            KeyCode::Down => {
                if let Some(editor) = &mut self.editor {
                    editor.move_down();
                }
            }
            KeyCode::Tab => {
                if let Some(editor) = &mut self.editor {
                    for _ in 0..4 {
                        editor.insert_char(' ');
                    }
                }
            }
            KeyCode::Char(c)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(editor) = &mut self.editor {
                    editor.insert_char(c);
                }
            }
            _ => {}
        }
    }

    fn create_entry_script(&mut self) {
        let Some(entry_name) = self
            .entry_context
            .as_ref()
            .map(|context| context.name.clone())
        else {
            self.message =
                Some("Open from a directory entry or session to create an entry script.".into());
            return;
        };

        let path = self.store.entry_script_path(&entry_name);
        if path.exists() {
            self.message = Some(format!("Entry script already exists: {}", path.display()));
        } else if let Err(err) = self
            .store
            .save_entry_script(&entry_name, &entry_script_template(&entry_name))
        {
            self.message = Some(format!("Unable to create entry script: {err}"));
            return;
        } else {
            self.message = Some(format!("Created entry script: {}", path.display()));
        }

        self.scripts = self.store.list_scripts();
        let target = path.file_stem().and_then(|s| s.to_str());
        let selected = self.scripts.iter().position(|script| {
            Some(script.name.as_str()) == target && script.path.parent() == path.parent()
        });
        if let Some(index) = selected {
            self.list_state.select(Some(index));
            self.scroll = 0;
        } else if !self.scripts.is_empty() {
            self.list_state.select(Some(0));
        }
        self.start_editing();
    }

    fn start_editing(&mut self) {
        if let Some(script) = self
            .list_state
            .selected()
            .and_then(|index| self.scripts.get(index))
        {
            self.editor = Some(EditorState::new(script));
            self.message = Some("Editing script. Ctrl+S saves; Esc cancels.".into());
        } else {
            self.message = Some("Select or create a script before editing.".into());
        }
    }

    fn insert_login_template(&mut self) {
        let Some(context) = self.entry_context.clone() else {
            self.message = Some(
                "Open from a session or selected directory entry before inserting a login template."
                    .into(),
            );
            return;
        };

        let path = self
            .editor
            .as_ref()
            .map(|editor| editor.path.clone())
            .or_else(|| {
                self.list_state
                    .selected()
                    .and_then(|index| self.scripts.get(index))
                    .map(|script| script.path.clone())
            })
            .unwrap_or_else(|| self.store.entry_script_path(&context.name));

        let content = bbs_login_template(&context.name);
        self.editor = Some(EditorState::from_path_content(path, &content));
        self.scroll = 0;
        self.message =
            Some("BBS login template inserted. Review prompts, then Ctrl+S saves.".into());
    }

    fn save_editor(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        let content = editor.content();
        if let Err(err) = ScriptEngine::new().compile(&content) {
            self.message = Some(format!("Script not saved: {err}"));
            return;
        }

        if let Some(parent) = editor.path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                self.message = Some(format!("Unable to create script directory: {err}"));
                return;
            }
        }

        match std::fs::write(&editor.path, content) {
            Ok(()) => {
                let saved_path = editor.path.clone();
                self.editor = None;
                self.scripts = self.store.list_scripts();
                self.select_script_path(&saved_path);
                self.message = Some(format!("Saved script: {}", saved_path.display()));
            }
            Err(err) => {
                self.message = Some(format!("Unable to save script: {err}"));
            }
        }
    }

    fn select_script_path(&mut self, path: &PathBuf) {
        if let Some(index) = self.scripts.iter().position(|script| script.path == *path) {
            self.list_state.select(Some(index));
            self.scroll = 0;
        }
    }

    fn move_selection(&mut self, forward: bool) {
        if self.scripts.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0);
        let next = if forward {
            (cur + 1) % self.scripts.len()
        } else {
            (cur + self.scripts.len() - 1) % self.scripts.len()
        };
        self.list_state.select(Some(next));
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        f.render_widget(Clear, area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

        // Title
        f.render_widget(
            Paragraph::new(" SCRIPTS ").style(
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            chunks[0],
        );

        // Body: list (30%) + viewer (70%)
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(chunks[1]);

        self.render_list(f, body[0]);
        self.render_viewer(f, body[1]);

        let hints = if self.editor.is_some() {
            Line::from(vec![
                Span::styled(
                    " Ctrl+S ",
                    Style::default().bg(Color::Gray).fg(Color::Black),
                ),
                Span::styled(" Save  ", Style::default().fg(Color::White)),
                Span::styled(" L ", Style::default().bg(Color::Gray).fg(Color::Black)),
                Span::styled(" Login template  ", Style::default().fg(Color::White)),
                Span::styled(" Esc ", Style::default().bg(Color::Gray).fg(Color::Black)),
                Span::styled(" Cancel edit  ", Style::default().fg(Color::White)),
                Span::styled(
                    " Ctrl+O ",
                    Style::default().bg(Color::Gray).fg(Color::Black),
                ),
                Span::styled(" Insert line  ", Style::default().fg(Color::White)),
                Span::styled(
                    " Ctrl+K ",
                    Style::default().bg(Color::Gray).fg(Color::Black),
                ),
                Span::styled(" Delete line  ", Style::default().fg(Color::White)),
                Span::styled(" ↑↓←→ ", Style::default().bg(Color::Gray).fg(Color::Black)),
                Span::styled(" Move", Style::default().fg(Color::White)),
            ])
        } else {
            Line::from(vec![
                Span::styled(" N ", Style::default().bg(Color::Gray).fg(Color::Black)),
                Span::styled(" New entry script  ", Style::default().fg(Color::White)),
                Span::styled(" L ", Style::default().bg(Color::Gray).fg(Color::Black)),
                Span::styled(" Login template  ", Style::default().fg(Color::White)),
                Span::styled(
                    " E/Enter ",
                    Style::default().bg(Color::Gray).fg(Color::Black),
                ),
                Span::styled(" Edit  ", Style::default().fg(Color::White)),
                Span::styled(" ↑↓ ", Style::default().bg(Color::Gray).fg(Color::Black)),
                Span::styled(" Navigate  ", Style::default().fg(Color::White)),
                Span::styled(
                    " PgUp/PgDn ",
                    Style::default().bg(Color::Gray).fg(Color::Black),
                ),
                Span::styled(" Scroll  ", Style::default().fg(Color::White)),
                Span::styled(" Esc ", Style::default().bg(Color::Gray).fg(Color::Black)),
                Span::styled(" Close", Style::default().fg(Color::White)),
            ])
        };
        f.render_widget(
            Paragraph::new(hints).style(Style::default().bg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn render_list(&mut self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .scripts
            .iter()
            .map(|s| {
                let dir = s
                    .path
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{dir}/"), Style::default().fg(Color::DarkGray)),
                    Span::styled(s.name.clone(), Style::default().fg(Color::White)),
                ]))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Scripts ");

        if self.scripts.is_empty() {
            let mut text = "No scripts found.\nPress N to create an entry script.\n\nScripts live under:\n~/.config/waystone-comm/scripts/".to_string();
            if self.entry_context.is_none() {
                text.push_str("\n\nOpen from a session or selected directory entry to create one.");
            }
            if let Some(message) = &self.message {
                text.push_str("\n\n");
                text.push_str(message);
            }
            f.render_widget(
                Paragraph::new(text)
                    .block(block)
                    .style(Style::default().bg(Color::Black).fg(Color::DarkGray)),
                area,
            );
        } else {
            f.render_stateful_widget(
                List::new(items)
                    .block(block)
                    .style(Style::default().bg(Color::Black))
                    .highlight_style(Style::default().bg(Color::DarkGray)),
                area,
                &mut self.list_state,
            );
        }
    }

    fn render_viewer(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(if self.editor.is_some() {
                " Editor "
            } else {
                " Source "
            });

        if let Some(editor) = &self.editor {
            self.render_editor(f, area, block, editor);
            return;
        }

        let script = self.list_state.selected().and_then(|i| self.scripts.get(i));

        let lines: Vec<Line> = match script {
            None => vec![Line::from(Span::styled(
                "Select a script",
                Style::default().fg(Color::DarkGray),
            ))],
            Some(s) => {
                let mut lines = Vec::new();
                if let Some(message) = &self.message {
                    lines.push(Line::from(Span::styled(
                        message.clone(),
                        Style::default().fg(Color::Yellow),
                    )));
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::styled("Path: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        s.path.display().to_string(),
                        Style::default().fg(Color::Cyan),
                    ),
                ]));
                if let Some(context) = &self.entry_context {
                    lines.push(Line::from(vec![
                        Span::styled("Entry: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(context.name.clone(), Style::default().fg(Color::White)),
                        Span::styled("  Credential: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            if context.credential_attached {
                                "attached"
                            } else {
                                "none"
                            },
                            Style::default().fg(if context.credential_attached {
                                Color::Green
                            } else {
                                Color::Yellow
                            }),
                        ),
                    ]));
                }
                lines.push(Line::from(""));
                lines.extend(highlight_rhai(&s.content));
                lines
            }
        };

        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .style(Style::default().bg(Color::Black))
                .scroll((self.scroll, 0))
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    fn render_editor(&self, f: &mut Frame, area: Rect, block: Block, editor: &EditorState) {
        let mut lines = Vec::new();
        if let Some(message) = &self.message {
            lines.push(Line::from(Span::styled(
                message.clone(),
                Style::default().fg(Color::Yellow),
            )));
        }
        lines.push(Line::from(vec![
            Span::styled("Path: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                editor.path.display().to_string(),
                Style::default().fg(Color::Cyan),
            ),
        ]));
        if let Some(context) = &self.entry_context {
            lines.push(Line::from(vec![
                Span::styled("Entry: ", Style::default().fg(Color::DarkGray)),
                Span::styled(context.name.clone(), Style::default().fg(Color::White)),
                Span::styled("  Credential: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    if context.credential_attached {
                        "attached"
                    } else {
                        "none"
                    },
                    Style::default().fg(if context.credential_attached {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                ),
            ]));
        }
        lines.push(Line::from(""));

        for (row, line) in editor.lines.iter().enumerate() {
            let mut display = line.clone();
            if row == editor.cursor_row {
                let col = editor.cursor_col.min(display.len());
                display.insert(col, '█');
            }
            lines.push(Line::from(Span::styled(
                display,
                Style::default().fg(Color::White),
            )));
        }

        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .style(Style::default().bg(Color::Black))
                .wrap(Wrap { trim: false }),
            area,
        );
    }
}

fn entry_script_template(entry_name: &str) -> String {
    format!(
        r#"// Entry script for {entry_name}
// This file runs automatically when the matching directory entry connects.
// Common credential keys: username, password, token, private_key, name, id.

fn on_connect(s) {{
    s.log("Connected to " + s.entry_name());

    // Example login flow:
    // if s.wait_for("login:", 10.0) {{
    //     s.send(s.credential("username") + "\r\n");
    // }}
    // if s.wait_for("Password:", 10.0) {{
    //     s.send(s.credential("password") + "\r\n");
    // }}
}}

fn on_data(s, data) {{
    // Runs when new text arrives. Keep this hook quick.
}}
"#
    )
}

fn bbs_login_template(entry_name: &str) -> String {
    format!(
        r#"// BBS login script for {entry_name}
// Sends the entry credential's username/login number and password.
// It intentionally stops before any sysop password prompt.

fn on_connect(s) {{
    if s.wait_for("What is your Alias:", 10.0) {{
        s.send(s.credential("username") + "\r\n");
        s.log("sent alias");
    }} else {{
        s.log("alias prompt not seen");
        return;
    }}

    if s.wait_for("What is your Password:", 10.0) {{
        s.send(s.credential("password") + "\r\n");
        s.log("sent password");
    }} else {{
        s.log("password prompt not seen");
    }}
}}

fn on_data(s, data) {{
}}
"#
    )
}

// ── Rhai keyword highlighting ─────────────────────────────────────────────────

/// Very simple line-by-line syntax highlighting for Rhai scripts.
fn highlight_rhai(source: &str) -> Vec<Line<'static>> {
    source.lines().map(highlight_line).collect()
}

fn highlight_line(line: &str) -> Line<'static> {
    // Comment
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Tokenise into spans: keywords, strings, numbers, identifiers, punctuation
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut chars = line.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if c == '/' && chars.peek().map(|(_, c)| *c) == Some('/') {
            // Rest of line is comment
            spans.push(Span::styled(
                line[i..].to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            break;
        } else if c == '"' {
            // String literal
            let start = i;
            let mut end = i + 1;
            for (j, ch) in chars.by_ref() {
                end = j + ch.len_utf8();
                if ch == '"' {
                    break;
                }
            }
            spans.push(Span::styled(
                line[start..end].to_string(),
                Style::default().fg(Color::Green),
            ));
        } else if c.is_ascii_digit() {
            // Number
            let start = i;
            let mut end = i + 1;
            while let Some(&(j, ch)) = chars.peek() {
                if ch.is_ascii_digit() || ch == '.' {
                    end = j + 1;
                    chars.next();
                } else {
                    break;
                }
            }
            spans.push(Span::styled(
                line[start..end].to_string(),
                Style::default().fg(Color::Cyan),
            ));
        } else if c.is_alphabetic() || c == '_' {
            // Identifier or keyword
            let start = i;
            let mut end = i + c.len_utf8();
            while let Some(&(j, ch)) = chars.peek() {
                if ch.is_alphanumeric() || ch == '_' {
                    end = j + ch.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let word = &line[start..end];
            let style = if is_keyword(word) {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_builtin(word) {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(Color::White)
            };
            spans.push(Span::styled(word.to_string(), style));
        } else {
            // Punctuation / whitespace — pass through
            let style = if "(){}[];,".contains(c) {
                Style::default().fg(Color::Gray)
            } else {
                Style::default().fg(Color::White)
            };
            spans.push(Span::styled(c.to_string(), style));
        }
    }

    Line::from(spans)
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "fn" | "let"
            | "if"
            | "else"
            | "while"
            | "for"
            | "in"
            | "return"
            | "true"
            | "false"
            | "break"
            | "continue"
            | "import"
            | "export"
            | "as"
            | "switch"
            | "do"
            | "loop"
    )
}

fn is_builtin(word: &str) -> bool {
    // SessionApi methods — highlight in magenta
    matches!(
        word,
        "send"
            | "send_raw"
            | "wait_for"
            | "wait_ms"
            | "log"
            | "notify"
            | "set_var"
            | "get_var"
            | "entry_name"
            | "disconnect"
            | "reconnect"
            | "credential"
            | "upload"
            | "download"
            | "run_local"
            | "timestamp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_entry_script_enters_editor() {
        let dir = test_script_dir();
        let store = ScriptStore::new(&dir);
        let mut panel = ScriptPanel::new(&store, Some(test_context(false)));

        panel.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);

        assert!(panel.editor.is_some());
        assert!(store.entry_script_path("GameSrv").exists());
        assert_eq!(
            panel
                .entry_context
                .as_ref()
                .map(|context| context.credential_attached),
            Some(false)
        );
    }

    #[test]
    fn editor_ctrl_s_saves_changes() {
        let dir = test_script_dir();
        let store = ScriptStore::new(&dir);
        store
            .save_entry_script("GameSrv", "fn on_connect(s) {\n}\n")
            .unwrap();
        let mut panel = ScriptPanel::new(&store, Some(test_context(true)));

        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Down, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);

        let saved = std::fs::read_to_string(store.entry_script_path("GameSrv")).unwrap();
        assert!(saved.contains("x"));
        assert!(panel.editor.is_none());
    }

    #[test]
    fn editor_can_insert_and_delete_lines() {
        let dir = test_script_dir();
        let store = ScriptStore::new(&dir);
        store
            .save_entry_script("GameSrv", "fn on_connect(s) {\n}\n")
            .unwrap();
        let mut panel = ScriptPanel::new(&store, Some(test_context(true)));

        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('o'), KeyModifiers::CONTROL);
        panel.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char(' '), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);

        let saved = std::fs::read_to_string(store.entry_script_path("GameSrv")).unwrap();
        assert_eq!(saved, "fn on_connect(s) {\n// x\n}\n");

        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Down, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('k'), KeyModifiers::CONTROL);
        panel.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);

        let saved = std::fs::read_to_string(store.entry_script_path("GameSrv")).unwrap();
        assert_eq!(saved, "fn on_connect(s) {\n}\n");
    }

    #[test]
    fn editor_refuses_to_save_invalid_script() {
        let dir = test_script_dir();
        let store = ScriptStore::new(&dir);
        store
            .save_entry_script("GameSrv", "fn on_connect(s) {}\n")
            .unwrap();
        let mut panel = ScriptPanel::new(&store, Some(test_context(true)));

        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);

        let saved = std::fs::read_to_string(store.entry_script_path("GameSrv")).unwrap();
        assert_eq!(saved, "fn on_connect(s) {}\n");
        assert!(panel.editor.is_some());
        assert!(panel
            .message
            .as_deref()
            .unwrap_or_default()
            .starts_with("Script not saved:"));
    }

    #[test]
    fn login_template_can_create_entry_script() {
        let dir = test_script_dir();
        let store = ScriptStore::new(&dir);
        let mut panel = ScriptPanel::new(&store, Some(test_context(true)));

        panel.handle_key(KeyCode::Char('l'), KeyModifiers::NONE);

        let editor = panel.editor.as_ref().expect("login template editor");
        assert_eq!(editor.path, store.entry_script_path("GameSrv"));
        assert!(editor.content().contains("What is your Alias:"));
        assert!(editor.content().contains("What is your Password:"));
        assert!(editor.content().contains("stops before any sysop password"));

        panel.handle_key(KeyCode::Char('s'), KeyModifiers::CONTROL);

        let saved = std::fs::read_to_string(store.entry_script_path("GameSrv")).unwrap();
        assert!(saved.contains("s.credential(\"username\")"));
        assert!(saved.contains("s.credential(\"password\")"));
        assert!(panel.editor.is_none());
    }

    #[test]
    fn login_template_replaces_current_editor_buffer() {
        let dir = test_script_dir();
        let store = ScriptStore::new(&dir);
        store
            .save_entry_script("GameSrv", "fn on_connect(s) {\n    s.log(\"old\");\n}\n")
            .unwrap();
        let mut panel = ScriptPanel::new(&store, Some(test_context(true)));

        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('l'), KeyModifiers::NONE);

        let editor = panel.editor.as_ref().expect("login template editor");
        assert!(!editor.content().contains("old"));
        assert!(editor.content().contains("sent alias"));
    }

    fn test_script_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "waystone-comm-script-panel-test-{}",
            uuid::Uuid::new_v4()
        ))
    }

    fn test_context(credential_attached: bool) -> EntryScriptContext {
        EntryScriptContext {
            name: "GameSrv".into(),
            credential_attached,
        }
    }
}
