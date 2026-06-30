use super::cell::Cell;

/// A snapshot of the full terminal grid, passed to the TUI layer for rendering.
#[derive(Debug, Clone)]
pub struct TerminalScreen {
    pub cols: u16,
    pub rows: u16,
    /// Row-major: `cells[row][col]`.
    pub cells: Vec<Vec<Cell>>,
    pub cursor_col: u16,
    pub cursor_row: u16,
    pub cursor_visible: bool,
}

impl TerminalScreen {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![vec![Cell::default(); cols as usize]; rows as usize],
            cursor_col: 0,
            cursor_row: 0,
            cursor_visible: true,
        }
    }

    /// Get the cell at (col, row). Returns a default cell if out of bounds.
    #[must_use]
    pub fn get(&self, col: u16, row: u16) -> &Cell {
        static DEFAULT: once_cell::sync::Lazy<Cell> = once_cell::sync::Lazy::new(Cell::default);
        self.cells
            .get(row as usize)
            .and_then(|r| r.get(col as usize))
            .unwrap_or(&DEFAULT)
    }
}
