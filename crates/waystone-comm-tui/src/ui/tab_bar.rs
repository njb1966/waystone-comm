use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use waystone_comm_core::connection::ConnectionStatus;

pub struct TabInfo<'a> {
    pub label: &'a str,
    pub has_unread: bool,
    pub status: &'a ConnectionStatus,
}

/// Render the tab strip showing all open sessions.
pub fn render_tab_bar(f: &mut Frame, area: Rect, tabs: &[TabInfo<'_>], active: usize) {
    let bg = Style::default().bg(Color::Black);

    let mut spans: Vec<Span> = Vec::new();

    for (i, tab) in tabs.iter().enumerate() {
        let is_active = i == active;

        let tab_style = if is_active {
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::DarkGray).fg(Color::Gray)
        };

        let status_color = match tab.status {
            ConnectionStatus::Connected => Color::Green,
            ConnectionStatus::Connecting => Color::Yellow,
            ConnectionStatus::Disconnected => Color::DarkGray,
            ConnectionStatus::Error(_) => Color::Red,
        };

        let indicator = if tab.has_unread {
            Span::styled(
                "● ",
                Style::default()
                    .bg(tab_style.bg.unwrap_or(Color::DarkGray))
                    .fg(Color::Yellow),
            )
        } else {
            Span::styled("  ", tab_style)
        };

        let dot = Span::styled(
            "■ ",
            Style::default()
                .bg(tab_style.bg.unwrap_or(Color::DarkGray))
                .fg(status_color),
        );

        spans.push(Span::styled(" ", tab_style));
        spans.push(dot);
        spans.push(Span::styled(tab.label, tab_style));
        spans.push(indicator);
        spans.push(Span::styled("", tab_style)); // right border
        spans.push(Span::styled(" ", bg));
    }

    // [+] hint for new tab
    spans.push(Span::styled(
        " ^T New  ^W Close  Alt+1-9 Switch ",
        Style::default().bg(Color::Black).fg(Color::DarkGray),
    ));

    f.render_widget(Paragraph::new(Line::from(spans)).style(bg), area);
}
