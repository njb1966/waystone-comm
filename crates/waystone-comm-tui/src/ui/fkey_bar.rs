use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Render the F-key help bar using labels from the active key profile.
///
/// `labels` is a slice of `(key_str, action_label)` pairs, e.g.
/// `[("F3", "Scripts"), ("F10", "Quit")]`.
pub fn render_fkey_bar(frame: &mut Frame, area: Rect, labels: &[(String, String)]) {
    let key_style = Style::default().fg(Color::Black).bg(Color::White);
    let label_style = Style::default().fg(Color::Black).bg(Color::Cyan);
    let sep_style = Style::default().fg(Color::Blue).bg(Color::Cyan);

    let mut spans = vec![Span::styled(" ", label_style)];

    for (i, (key, label)) in labels.iter().enumerate() {
        spans.push(Span::styled(key.clone(), key_style));
        spans.push(Span::styled(format!("{label} "), label_style));
        if i + 1 < labels.len() {
            spans.push(Span::styled("│", sep_style));
        }
    }

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line).style(label_style), area);
}
