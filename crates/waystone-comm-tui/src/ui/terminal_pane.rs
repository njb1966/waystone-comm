use ratatui::{
    layout::Rect,
    style::{Color as RColor, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph},
    Frame,
};
use waystone_comm_core::terminal::{CellStyle, Color, TerminalScreen};

/// Render the terminal grid into `area`.
///
/// Cells are rendered as styled `Span`s, one row per `Line`. The cursor
/// position is forwarded to ratatui so the terminal cursor is placed
/// correctly. Cursor rendering is skipped when the emulator reports
/// `cursor_visible = false` or when the cursor is outside `area`.
pub fn render_terminal_pane(frame: &mut Frame, area: Rect, screen: &TerminalScreen) {
    let rows = (screen.rows as usize).min(area.height as usize);
    let cols = (screen.cols as usize).min(area.width as usize);

    let mut lines: Vec<Line> = Vec::with_capacity(rows);

    for row in 0..rows {
        let mut spans: Vec<Span> = Vec::with_capacity(cols);
        for col in 0..cols {
            let cell = screen.get(col as u16, row as u16);
            let style = convert_style(&cell.style);
            // Use a space for zero-width or control characters.
            let ch = if cell.ch.is_control() { ' ' } else { cell.ch };
            spans.push(Span::styled(ch.to_string(), style));
        }
        lines.push(Line::from(spans));
    }

    let text = Text::from(lines);
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default())
            .style(Style::default().bg(RColor::Black)),
        area,
    );

    // Place the hardware cursor.
    if screen.cursor_visible {
        let cur_col = area.x + screen.cursor_col.min(area.width.saturating_sub(1));
        let cur_row = area.y + screen.cursor_row.min(area.height.saturating_sub(1));
        frame.set_cursor_position((cur_col, cur_row));
    }
}

// ── Color / style conversion ──────────────────────────────────────────────────

fn convert_color(color: &Color) -> RColor {
    match color {
        Color::Default => RColor::Reset,
        Color::Ansi(n) => RColor::Indexed(*n),
        Color::Palette(n) => RColor::Indexed(*n),
        Color::Rgb(r, g, b) => RColor::Rgb(*r, *g, *b),
    }
}

fn convert_fg_color(style: &CellStyle) -> RColor {
    match (&style.fg, style.bold) {
        // Many ANSI BBSes use bold low-intensity foreground colors to request
        // the bright 8-color range. Ratatui leaves that policy to the terminal,
        // so make it explicit for consistent ANSI-art rendering.
        (Color::Ansi(n @ 0..=7), true) => RColor::Indexed(n + 8),
        (color, _) => convert_color(color),
    }
}

fn convert_style(style: &CellStyle) -> Style {
    let bg = match &style.bg {
        Color::Default => RColor::Black,
        color => convert_color(color),
    };
    let mut s = Style::default().fg(convert_fg_color(style)).bg(bg);

    let mut modifier = Modifier::empty();
    if style.bold {
        modifier |= Modifier::BOLD;
    }
    if style.dim {
        modifier |= Modifier::DIM;
    }
    if style.italic {
        modifier |= Modifier::ITALIC;
    }
    if style.underline {
        modifier |= Modifier::UNDERLINED;
    }
    if style.blink {
        modifier |= Modifier::SLOW_BLINK;
    }
    if style.reverse {
        modifier |= Modifier::REVERSED;
    }
    if style.strikethrough {
        modifier |= Modifier::CROSSED_OUT;
    }
    // invisible: render fg same as bg so the character is hidden.
    if style.invisible {
        s = s.fg(bg);
    }

    s.add_modifier(modifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_low_ansi_foreground_renders_bright() {
        let style = CellStyle {
            fg: Color::Ansi(0),
            bold: true,
            ..CellStyle::default()
        };

        let converted = convert_style(&style);

        assert_eq!(converted.fg, Some(RColor::Indexed(8)));
        assert!(converted.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bold_does_not_brighten_background() {
        let style = CellStyle {
            bg: Color::Ansi(7),
            bold: true,
            ..CellStyle::default()
        };

        let converted = convert_style(&style);

        assert_eq!(converted.bg, Some(RColor::Indexed(7)));
    }

    #[test]
    fn nonbold_low_ansi_foreground_stays_low_intensity() {
        let style = CellStyle {
            fg: Color::Ansi(1),
            ..CellStyle::default()
        };

        let converted = convert_style(&style);

        assert_eq!(converted.fg, Some(RColor::Indexed(1)));
    }
}
