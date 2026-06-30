//! Log viewer panel — scrollable, searchable view of the session log.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

pub enum LogViewerAction {
    None,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFilter {
    All,
    Script,
    Transfer,
    Session,
}

impl LogFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Script => "script",
            Self::Transfer => "transfer",
            Self::Session => "session",
        }
    }

    fn matches(self, line: &str) -> bool {
        let lower = line.to_lowercase();
        match self {
            Self::All => true,
            Self::Script => lower.contains("[script]"),
            Self::Transfer => lower.contains("[transfer "),
            Self::Session => !lower.contains("[script]") && !lower.contains("[transfer "),
        }
    }
}

pub struct LogViewerPanel {
    /// All log lines loaded from disk.
    lines: Vec<String>,
    list_state: ListState,
    /// Current search query (empty = no filter).
    search: String,
    /// Whether the user is actively typing a search query.
    search_mode: bool,
    /// Indices into `lines` that match the current search.
    filtered: Vec<usize>,
    /// Current coarse log category filter.
    filter: LogFilter,
}

impl LogViewerPanel {
    /// Create a new panel pre-loaded with `lines`.
    pub fn new(lines: Vec<String>) -> Self {
        let len = lines.len();
        let filtered: Vec<usize> = (0..len).collect();
        let mut list_state = ListState::default();
        if !filtered.is_empty() {
            list_state.select(Some(filtered.len() - 1)); // start at bottom
        }
        Self {
            lines,
            list_state,
            search: String::new(),
            search_mode: false,
            filtered,
            filter: LogFilter::All,
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> LogViewerAction {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);

        if self.search_mode {
            match code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.search_mode = false;
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.refilter();
                }
                KeyCode::Char(c) if !ctrl => {
                    self.search.push(c);
                    self.refilter();
                }
                _ => {}
            }
            return LogViewerAction::None;
        }

        match code {
            KeyCode::Esc | KeyCode::F(8) | KeyCode::Char('q') => {
                return LogViewerAction::Close;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::PageUp => self.move_sel(-20),
            KeyCode::PageDown => self.move_sel(20),
            KeyCode::Home => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::End => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(self.filtered.len() - 1));
                }
            }
            KeyCode::Char('g') => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(0));
                }
            }
            KeyCode::Char('G') => {
                if !self.filtered.is_empty() {
                    self.list_state.select(Some(self.filtered.len() - 1));
                }
            }
            KeyCode::Char('/') => {
                self.search_mode = true;
            }
            KeyCode::Char('a') | KeyCode::Char('A') => self.set_filter(LogFilter::All),
            KeyCode::Char('s') | KeyCode::Char('S') => self.set_filter(LogFilter::Script),
            KeyCode::Char('t') | KeyCode::Char('T') => self.set_filter(LogFilter::Transfer),
            KeyCode::Char('c') | KeyCode::Char('C') => self.set_filter(LogFilter::Session),
            KeyCode::Char('n') => {
                self.search.clear();
                self.refilter();
            }
            _ => {}
        }
        LogViewerAction::None
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let title = if self.search.is_empty() {
            format!(
                " Session Log - {} ({} lines) ",
                self.filter.label(),
                self.filtered.len()
            )
        } else {
            format!(
                " Session Log - {} - search: {:?} ({}/{} lines) ",
                self.filter.label(),
                self.search,
                self.filtered.len(),
                self.lines.len()
            )
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(inner);

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&idx| {
                let line = self.lines.get(idx).map(String::as_str).unwrap_or("");
                if !self.search.is_empty() {
                    if let Some(pos) = line.to_lowercase().find(&self.search.to_lowercase()) {
                        let end = pos + self.search.len();
                        let before = &line[..pos];
                        let matched = &line[pos..end];
                        let after = &line[end..];
                        return ListItem::new(Line::from(vec![
                            Span::raw(before.to_string()),
                            Span::styled(
                                matched.to_string(),
                                Style::default().fg(Color::Black).bg(Color::Yellow),
                            ),
                            Span::raw(after.to_string()),
                        ]));
                    }
                }
                ListItem::new(line.to_string())
            })
            .collect();

        let list = List::new(items)
            .style(Style::default().bg(Color::Black))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            )
            .highlight_symbol("▶ ");

        frame.render_stateful_widget(list, chunks[0], &mut self.list_state);

        let status_line = if self.search_mode {
            Line::from(vec![
                Span::styled(" /", Style::default().fg(Color::White)),
                Span::styled(
                    format!("{}_", self.search),
                    Style::default().fg(Color::Yellow),
                ),
            ])
        } else {
            Line::from(vec![
                Span::raw("  "),
                Span::styled("[/]", Style::default().fg(Color::White).bg(Color::DarkGray)),
                Span::raw(" search  "),
                Span::styled("[n]", Style::default().fg(Color::White).bg(Color::DarkGray)),
                Span::raw(" clear  "),
                Span::styled(
                    "[a/s/t/c]",
                    Style::default().fg(Color::White).bg(Color::DarkGray),
                ),
                Span::raw(" all/script/transfer/session  "),
                Span::styled(
                    "[Home/End]",
                    Style::default().fg(Color::White).bg(Color::DarkGray),
                ),
                Span::raw(" top/latest  "),
                Span::styled(
                    "[Esc/q]",
                    Style::default().fg(Color::White).bg(Color::DarkGray),
                ),
                Span::raw(" close"),
            ])
        };

        frame.render_widget(
            Paragraph::new(status_line).style(Style::default().bg(Color::DarkGray)),
            chunks[1],
        );
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn move_sel(&mut self, delta: i32) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
        self.list_state.select(Some(next));
    }

    fn refilter(&mut self) {
        let q = self.search.to_lowercase();
        self.filtered = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| self.filter.matches(line))
            .filter(|(_, line)| q.is_empty() || line.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        let sel = self.list_state.selected().unwrap_or(0);
        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state
                .select(Some(sel.min(self.filtered.len() - 1)));
        }
    }

    fn set_filter(&mut self, filter: LogFilter) {
        self.filter = filter;
        self.refilter();
        if !self.filtered.is_empty() {
            self.list_state.select(Some(self.filtered.len() - 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_panel() -> LogViewerPanel {
        LogViewerPanel::new(vec![
            "[12:00:00] Connected".into(),
            "[12:00:01] [script] on_connect started".into(),
            "[12:00:02] [transfer complete: 42 bytes]".into(),
            "[12:00:03] What is your Alias:".into(),
            "[12:00:04] [script] sent alias".into(),
        ])
    }

    #[test]
    fn filter_script_lines() {
        let mut panel = sample_panel();

        panel.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);

        let lines: Vec<&str> = panel
            .filtered
            .iter()
            .map(|&idx| panel.lines[idx].as_str())
            .collect();
        assert_eq!(
            lines,
            vec![
                "[12:00:01] [script] on_connect started",
                "[12:00:04] [script] sent alias"
            ]
        );
    }

    #[test]
    fn filter_transfer_lines() {
        let mut panel = sample_panel();

        panel.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);

        assert_eq!(panel.filtered.len(), 1);
        assert!(panel.lines[panel.filtered[0]].contains("[transfer complete"));
    }

    #[test]
    fn filter_session_lines_excludes_script_and_transfer() {
        let mut panel = sample_panel();

        panel.handle_key(KeyCode::Char('c'), KeyModifiers::NONE);

        let lines: Vec<&str> = panel
            .filtered
            .iter()
            .map(|&idx| panel.lines[idx].as_str())
            .collect();
        assert_eq!(
            lines,
            vec!["[12:00:00] Connected", "[12:00:03] What is your Alias:"]
        );
    }

    #[test]
    fn search_combines_with_active_filter() {
        let mut panel = sample_panel();

        panel.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for c in "alias".chars() {
            panel.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(panel.filtered.len(), 1);
        assert!(panel.lines[panel.filtered[0]].contains("sent alias"));
    }
}
