/// A single cell in the terminal grid.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// The character displayed in this cell. Space = empty.
    pub ch: char,
    /// Visual style for this cell.
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: CellStyle::default(),
        }
    }
}

/// SGR attributes for a cell — matches MASTERPLAN §3.6.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CellStyle {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
}

/// Terminal color value.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Color {
    /// Terminal default (transparent / inherited).
    #[default]
    Default,
    /// ANSI 8-color (indices 0–7) and bright variants (8–15).
    Ansi(u8),
    /// 256-color palette index.
    Palette(u8),
    /// 24-bit true-color RGB.
    Rgb(u8, u8, u8),
}
