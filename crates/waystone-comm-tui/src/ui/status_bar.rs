use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use waystone_comm_core::{connection::ConnectionStatus, transfer::TransferProgress};

/// Render the one-line status bar showing connection info.
/// When `transfer` is `Some`, the transfer progress replaces the host field.
pub fn render_status_bar(
    frame: &mut Frame,
    area: Rect,
    protocol: &str,
    host: &str,
    status: &ConnectionStatus,
    transfer: Option<TransferProgress>,
    message: Option<&str>,
) {
    let (status_str, status_style) = match status {
        ConnectionStatus::Connected => ("CONNECTED", Style::default().fg(Color::Green)),
        ConnectionStatus::Connecting => ("CONNECTING", Style::default().fg(Color::Yellow)),
        ConnectionStatus::Disconnected => ("DISCONNECTED", Style::default().fg(Color::Yellow)),
        ConnectionStatus::Error(_) => ("ERROR", Style::default().fg(Color::Red)),
    };

    let bar_style = Style::default().bg(Color::DarkGray).fg(Color::White);

    let right_spans: Vec<Span> = if let Some(ref xfer) = transfer {
        let dir = match xfer.direction {
            waystone_comm_core::transfer::Direction::Receive => "RECV",
            waystone_comm_core::transfer::Direction::Send => "SEND",
        };
        let pct = xfer
            .percent()
            .map(|p| format!(" {}%", p))
            .unwrap_or_default();
        let speed = if xfer.cps >= 1024 {
            format!(" {:.1} KB/s", xfer.cps as f64 / 1024.0)
        } else {
            format!(" {} B/s", xfer.cps)
        };
        let filename = if xfer.filename.is_empty() {
            "zmodem"
        } else {
            &xfer.filename
        };
        vec![Span::styled(
            format!("{dir} {}: {filename}{}{speed}", xfer.phase.label(), pct),
            bar_style.fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )]
    } else if let Some(message) = message {
        vec![Span::styled(
            message.to_string(),
            bar_style.fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )]
    } else {
        vec![
            Span::styled(
                format!("{protocol} "),
                bar_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("→ {host} "), bar_style),
        ]
    };

    let mut spans = vec![
        Span::styled(
            format!(" [{status_str}] "),
            status_style.bg(Color::DarkGray),
        ),
        Span::styled("│ ", bar_style),
    ];
    spans.extend(right_spans);

    frame.render_widget(Paragraph::new(Line::from(spans)).style(bar_style), area);
}
