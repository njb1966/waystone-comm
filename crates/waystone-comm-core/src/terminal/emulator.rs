use super::screen::TerminalScreen;
use super::state::TerminalState;

// ── Emulation mode ────────────────────────────────────────────────────────────

/// Terminal emulation personality selected from `DirectoryEntry.terminal.emulation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmulationMode {
    #[default]
    Xterm,
    AnsiBbs, // IBM CP437 byte translation for BBS art
    Vt220,
    Vt100,
}

impl EmulationMode {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "ansi" | "ansi-bbs" | "ansi_bbs" | "ansibbs" | "bbs" => Self::AnsiBbs,
            "vt220" => Self::Vt220,
            "vt100" => Self::Vt100,
            _ => Self::Xterm, // xterm, xterm-256color, xterm-256, etc.
        }
    }

    #[must_use]
    pub fn canvas_cols(self, cols: u16) -> u16 {
        match self {
            Self::AnsiBbs => cols.clamp(1, 80),
            _ => cols.max(1),
        }
    }
}

// ── CP437 high-byte table (bytes 0x80–0xFF → Unicode) ────────────────────────

/// Maps IBM CP437 bytes 0x80–0xFF to their Unicode equivalents.
/// Index 0 = byte 0x80, index 127 = byte 0xFF.
#[rustfmt::skip]
const CP437_HIGH: [char; 128] = [
    'Ç','ü','é','â','ä','à','å','ç','ê','ë','è','ï','î','ì','Ä','Å', // 80–8F
    'É','æ','Æ','ô','ö','ò','û','ù','ÿ','Ö','Ü','¢','£','¥','₧','ƒ', // 90–9F
    'á','í','ó','ú','ñ','Ñ','ª','º','¿','⌐','¬','½','¼','¡','«','»', // A0–AF
    '░','▒','▓','│','┤','╡','╢','╖','╕','╣','║','╗','╝','╜','╛','┐', // B0–BF
    '└','┴','┬','├','─','┼','╞','╟','╚','╔','╩','╦','╠','═','╬','╧', // C0–CF
    '╨','╤','╥','╙','╘','╒','╓','╫','╪','┘','┌','█','▄','▌','▐','▀', // D0–DF
    'α','ß','Γ','π','Σ','σ','µ','τ','Φ','Θ','Ω','δ','∞','φ','ε','∩', // E0–EF
    '≡','±','≥','≤','⌠','⌡','÷','≈','°','∙','·','√','ⁿ','²','■',' ', // F0–FF
];

fn cp437_to_char(byte: u8) -> char {
    if byte >= 0x80 {
        CP437_HIGH[(byte - 0x80) as usize]
    } else {
        byte as char
    }
}

// ── TerminalEmulator ──────────────────────────────────────────────────────────

/// VT100/ANSI terminal emulator.
///
/// Owns a `vte::Parser` (byte-level state machine) and a `TerminalState`
/// (the grid + cursor). Bytes from the remote are fed through `process()`.
pub struct TerminalEmulator {
    parser: vte::Parser,
    state: TerminalState,
    emulation: EmulationMode,
    pending_utf8_c2: bool,
    utf8_continuations: u8,
}

impl TerminalEmulator {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_emulation(cols, rows, EmulationMode::default())
    }

    #[must_use]
    pub fn with_emulation(cols: u16, rows: u16, emulation: EmulationMode) -> Self {
        Self {
            parser: vte::Parser::new(),
            state: TerminalState::new(cols, rows),
            emulation,
            pending_utf8_c2: false,
            utf8_continuations: 0,
        }
    }

    /// Feed raw bytes from the remote into the emulator.
    ///
    /// For `AnsiBbs` mode, bytes ≥ 0x80 that are not part of an escape
    /// sequence are translated from CP437 to Unicode before display.
    pub fn process(&mut self, data: &[u8]) {
        for &byte in data {
            self.process_byte(byte);
        }
    }

    fn process_byte(&mut self, byte: u8) {
        if self.emulation == EmulationMode::AnsiBbs && byte >= 0x80 {
            self.feed_cp437_byte(byte);
            return;
        }

        if self.pending_utf8_c2 {
            self.pending_utf8_c2 = false;
            if self.feed_c1_control(byte) {
                return;
            }
            self.feed_printable_byte(0xC2);
            self.feed_printable_byte(byte);
            return;
        }

        if self.utf8_continuations == 0 && byte == 0xC2 {
            self.pending_utf8_c2 = true;
            return;
        }

        if self.utf8_continuations == 0 && self.feed_c1_control(byte) {
            return;
        }

        self.track_utf8_byte(byte);
        self.feed_printable_byte(byte);
    }

    fn feed_c1_control(&mut self, byte: u8) -> bool {
        let seq: &[u8] = match byte {
            0x8E => b"\x1bN",  // SS2
            0x8F => b"\x1bO",  // SS3
            0x90 => b"\x1bP",  // DCS
            0x9B => b"\x1b[",  // CSI
            0x9C => b"\x1b\\", // ST
            0x9D => b"\x1b]",  // OSC
            _ => return false,
        };
        self.utf8_continuations = 0;
        for &b in seq {
            self.parser.advance(&mut self.state, b);
        }
        true
    }

    fn track_utf8_byte(&mut self, byte: u8) {
        if self.utf8_continuations > 0 {
            if byte & 0b1100_0000 == 0b1000_0000 {
                self.utf8_continuations -= 1;
            } else {
                self.utf8_continuations = utf8_continuation_count(byte);
            }
        } else {
            self.utf8_continuations = utf8_continuation_count(byte);
        }
    }

    fn feed_printable_byte(&mut self, byte: u8) {
        self.parser.advance(&mut self.state, byte);
    }

    fn feed_cp437_byte(&mut self, byte: u8) {
        let ch = cp437_to_char(byte);
        let mut buf = [0u8; 4];
        for b in ch.encode_utf8(&mut buf).bytes() {
            self.parser.advance(&mut self.state, b);
        }
    }

    /// The window title set by the remote via `OSC 0` / `OSC 2`.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.state.title.as_deref()
    }

    /// Whether the alternate screen is currently active.
    #[must_use]
    pub fn in_alt_screen(&self) -> bool {
        self.state.in_alt_screen
    }

    /// Whether the terminal is in application cursor keys mode (DECCKM).
    #[must_use]
    pub fn app_cursor_keys(&self) -> bool {
        self.state.app_cursor_keys
    }

    /// Resize the terminal grid and reset the scroll region.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.state.resize(cols, rows);
    }

    /// Return and clear any terminal-generated bytes to send to the remote.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.state.take_output()
    }

    /// Return a snapshot of the current screen for rendering.
    #[must_use]
    pub fn screen(&self) -> TerminalScreen {
        TerminalScreen {
            cols: self.state.cols,
            rows: self.state.rows,
            cells: self.state.grid.clone(),
            cursor_col: self.state.cursor_col,
            cursor_row: self.state.cursor_row,
            cursor_visible: self.state.cursor_visible,
        }
    }

    #[must_use]
    pub fn cols(&self) -> u16 {
        self.state.cols
    }

    #[must_use]
    pub fn rows(&self) -> u16 {
        self.state.rows
    }
}

fn utf8_continuation_count(byte: u8) -> u8 {
    match byte {
        0xC2..=0xDF => 1,
        0xE0..=0xEF => 2,
        0xF0..=0xF4 => 3,
        _ => 0,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::cell::Color;

    fn emu() -> TerminalEmulator {
        TerminalEmulator::new(80, 24)
    }

    fn text(e: &TerminalEmulator, row: u16) -> String {
        let screen = e.screen();
        screen
            .cells
            .get(row as usize)
            .map(|r| {
                r.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .unwrap_or_default()
    }

    fn cell_ch(e: &TerminalEmulator, col: u16, row: u16) -> char {
        e.screen().get(col, row).ch
    }

    // ── Basic text ────────────────────────────────────────────────────────────

    #[test]
    fn plain_text() {
        let mut e = emu();
        e.process(b"Hello");
        assert_eq!(text(&e, 0), "Hello");
        let s = e.screen();
        assert_eq!(s.cursor_col, 5);
        assert_eq!(s.cursor_row, 0);
    }

    #[test]
    fn device_status_report_ok_response() {
        let mut e = emu();
        e.process(b"\x1b[5n");
        assert_eq!(e.take_output(), b"\x1b[0n");
        assert!(e.take_output().is_empty());
    }

    #[test]
    fn cursor_position_report_uses_current_cursor() {
        let mut e = TerminalEmulator::new(80, 25);
        e.process(b"\x1b[s\x1b[255B\x1b[255C\x1b[6n\x1b[u");

        assert_eq!(e.take_output(), b"\x1b[25;80R");
        let s = e.screen();
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn mystic_terminal_detection_probe_replies_without_drawing_escape_bytes() {
        let mut e = TerminalEmulator::with_emulation(80, 25, EmulationMode::AnsiBbs);
        e.process(
            b"\x1b[1;1H\x1b[2J\x1b[1;1H\x1b[?1000h\x0c\
              Mystic BBS Version 1.12 A49\r\n\
              Detecting terminal emulation: \x1b[s\x1b[255B\x1b[255C\x1b[6n\x1b[u",
        );

        assert_eq!(e.take_output(), b"\x1b[25;80R");
        assert_eq!(text(&e, 1), "Mystic BBS Version 1.12 A49");
        assert_eq!(text(&e, 2), "Detecting terminal emulation:");
    }

    #[test]
    fn cr_lf() {
        let mut e = emu();
        e.process(b"line1\r\nline2");
        assert_eq!(text(&e, 0), "line1");
        assert_eq!(text(&e, 1), "line2");
    }

    #[test]
    fn dec_special_graphics_g0_line_drawing() {
        let mut e = emu();
        e.process(b"\x1b(0lqk\x1b(B abc");
        assert_eq!(text(&e, 0), "┌─┐ abc");
    }

    #[test]
    fn dec_special_graphics_g1_shift_out_and_shift_in() {
        let mut e = emu();
        e.process(b"\x1b)0\x0elqk\x0fabc");
        assert_eq!(text(&e, 0), "┌─┐abc");
    }

    #[test]
    fn backspace() {
        let mut e = emu();
        e.process(b"AB\x08C"); // A, B, BS, C → "AC"
        assert_eq!(cell_ch(&e, 0, 0), 'A');
        assert_eq!(cell_ch(&e, 1, 0), 'C');
    }

    // ── Cursor movement ───────────────────────────────────────────────────────

    #[test]
    fn cursor_up_down_forward_back() {
        let mut e = emu();
        e.process(b"\x1b[5;5H"); // CUP: row=5, col=5 (1-based)
        let s = e.screen();
        assert_eq!(s.cursor_row, 4);
        assert_eq!(s.cursor_col, 4);

        e.process(b"\x1b[2A"); // CUU 2
        assert_eq!(e.screen().cursor_row, 2);

        e.process(b"\x1b[3B"); // CUD 3
        assert_eq!(e.screen().cursor_row, 5);

        e.process(b"\x1b[2C"); // CUF 2
        assert_eq!(e.screen().cursor_col, 6);

        e.process(b"\x1b[1D"); // CUB 1
        assert_eq!(e.screen().cursor_col, 5);
    }

    #[test]
    fn cup_home() {
        let mut e = emu();
        e.process(b"Hello\x1b[H"); // CUP no args → 1;1 → 0,0
        let s = e.screen();
        assert_eq!(s.cursor_col, 0);
        assert_eq!(s.cursor_row, 0);
    }

    #[test]
    fn cha() {
        let mut e = emu();
        e.process(b"\x1b[10G"); // CHA col=10 (1-based)
        assert_eq!(e.screen().cursor_col, 9);
    }

    #[test]
    fn vpa() {
        let mut e = emu();
        e.process(b"\x1b[5d"); // VPA row=5 (1-based)
        assert_eq!(e.screen().cursor_row, 4);
    }

    // ── Erase ─────────────────────────────────────────────────────────────────

    #[test]
    fn erase_line_right() {
        let mut e = emu();
        e.process(b"Hello World\x1b[5G\x1b[K"); // write, move to col 5, EL 0
                                                // col 4 (0-based) onward erased
        let s = e.screen();
        assert_eq!(s.get(0, 0).ch, 'H');
        assert_eq!(s.get(3, 0).ch, 'l');
        assert_eq!(s.get(4, 0).ch, ' ');
        assert_eq!(s.get(10, 0).ch, ' ');
    }

    #[test]
    fn erase_display_below() {
        let mut e = emu();
        e.process(b"Line1\r\nLine2\r\nLine3");
        e.process(b"\x1b[2;1H\x1b[J"); // move to row2/col1, ED 0 (below)
        assert_eq!(text(&e, 0), "Line1");
        assert_eq!(text(&e, 1), ""); // erased
        assert_eq!(text(&e, 2), ""); // erased
    }

    #[test]
    fn erase_display_all() {
        let mut e = emu();
        e.process(b"Hello\r\nWorld");
        e.process(b"\x1b[2J"); // ED 2 — entire screen
        assert_eq!(text(&e, 0), "");
        assert_eq!(text(&e, 1), "");
    }

    // ── SGR — colors & attributes ─────────────────────────────────────────────

    #[test]
    fn sgr_bold() {
        let mut e = emu();
        e.process(b"\x1b[1mX"); // bold
        let s = e.screen();
        assert!(s.get(0, 0).style.bold);
        e.process(b"\x1b[0mY"); // reset
        let s2 = e.screen();
        assert!(!s2.get(1, 0).style.bold);
    }

    #[test]
    fn sgr_ansi_colors() {
        let mut e = emu();
        e.process(b"\x1b[31;42mX"); // fg=red(1), bg=green(2)
        let s = e.screen();
        let cell = s.get(0, 0);
        assert_eq!(cell.style.fg, Color::Ansi(1));
        assert_eq!(cell.style.bg, Color::Ansi(2));
    }

    #[test]
    fn sgr_256_color() {
        let mut e = emu();
        e.process(b"\x1b[38;5;200mX"); // fg = palette 200
        assert_eq!(e.screen().get(0, 0).style.fg, Color::Palette(200));
    }

    #[test]
    fn sgr_true_color() {
        let mut e = emu();
        e.process(b"\x1b[38;2;10;20;30mX"); // fg = rgb(10,20,30)
        assert_eq!(e.screen().get(0, 0).style.fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn sgr_bright_fg_bg() {
        let mut e = emu();
        e.process(b"\x1b[91;101mX"); // bright red fg (9), bright red bg (9 bright)
        let s = e.screen();
        let cell = s.get(0, 0);
        assert_eq!(cell.style.fg, Color::Ansi(9)); // 90 - 90 + 8 = 9
        assert_eq!(cell.style.bg, Color::Ansi(9)); // 100 - 100 + 8 = 9
    }

    // ── Cursor visibility ─────────────────────────────────────────────────────

    #[test]
    fn cursor_hide_show() {
        let mut e = emu();
        assert!(e.screen().cursor_visible);
        e.process(b"\x1b[?25l"); // hide
        assert!(!e.screen().cursor_visible);
        e.process(b"\x1b[?25h"); // show
        assert!(e.screen().cursor_visible);
    }

    // ── Save / restore cursor ─────────────────────────────────────────────────

    #[test]
    fn save_restore_cursor() {
        let mut e = emu();
        e.process(b"\x1b[3;7H"); // cursor to row=3,col=7 (1-based)
        e.process(b"\x1b7"); // DECSC
        e.process(b"\x1b[H"); // move to 0,0
        e.process(b"\x1b8"); // DECRC
        let s = e.screen();
        assert_eq!(s.cursor_row, 2); // restored to 3-1=2
        assert_eq!(s.cursor_col, 6); // restored to 7-1=6
    }

    // ── Scroll region ─────────────────────────────────────────────────────────

    #[test]
    fn scroll_region_and_scroll_up() {
        let mut e = emu();
        // Fill rows 0–3 with distinct chars
        for (i, ch) in [b'A', b'B', b'C', b'D'].iter().enumerate() {
            e.process(format!("\x1b[{};1H{}", i + 1, *ch as char).as_bytes());
        }
        // Set scroll region rows 2–4 (1-based)
        e.process(b"\x1b[2;4r");
        // Scroll up 1 within region: B→gone, C→row2, D→row3, blank→row4
        e.process(b"\x1b[1S");
        let s = e.screen();
        assert_eq!(s.get(0, 0).ch, 'A'); // outside region, untouched
        assert_eq!(s.get(0, 1).ch, 'C'); // was row 3, now row 2
        assert_eq!(s.get(0, 2).ch, 'D'); // was row 4, now row 3
        assert_eq!(s.get(0, 3).ch, ' '); // new blank line
    }

    // ── Insert / Delete ───────────────────────────────────────────────────────

    #[test]
    fn insert_delete_lines() {
        let mut e = emu();
        e.process(b"AAAA\r\nBBBB\r\nCCCC");
        // Move to row 2 (0-based 1), insert 1 line
        e.process(b"\x1b[2;1H\x1b[1L");
        assert_eq!(text(&e, 0), "AAAA");
        assert_eq!(text(&e, 1), ""); // inserted blank
        assert_eq!(text(&e, 2), "BBBB");
        assert_eq!(text(&e, 3), "CCCC");
    }

    #[test]
    fn delete_chars() {
        let mut e = emu();
        e.process(b"ABCDE");
        e.process(b"\x1b[1;2H\x1b[2P"); // move col=2(1-based)=1(0-based), delete 2
                                        // "ABCDE" → delete B,C → "ADE  "
        let s = e.screen();
        assert_eq!(s.get(0, 0).ch, 'A');
        assert_eq!(s.get(1, 0).ch, 'D');
        assert_eq!(s.get(2, 0).ch, 'E');
        assert_eq!(s.get(3, 0).ch, ' ');
    }

    // ── Resize ────────────────────────────────────────────────────────────────

    #[test]
    fn resize_preserves_content() {
        let mut e = emu();
        e.process(b"Hello");
        e.resize(100, 30);
        assert_eq!(e.cols(), 100);
        assert_eq!(e.rows(), 30);
        // Content from before resize should still be visible
        assert_eq!(cell_ch(&e, 0, 0), 'H');
    }

    // ── Wrap / pending-wrap ───────────────────────────────────────────────────

    #[test]
    fn auto_wrap() {
        let mut e = TerminalEmulator::new(5, 3);
        e.process(b"ABCDE"); // fills row 0 to col 4, pending_wrap set
        e.process(b"F"); // should wrap to row 1, col 0
        let s = e.screen();
        assert_eq!(s.get(0, 0).ch, 'A');
        assert_eq!(s.get(4, 0).ch, 'E');
        assert_eq!(s.get(0, 1).ch, 'F');
        assert_eq!(s.cursor_col, 1);
        assert_eq!(s.cursor_row, 1);
    }

    #[test]
    fn decawm_off_prevents_full_width_lines_from_scrolling() {
        let mut e = TerminalEmulator::with_emulation(5, 3, EmulationMode::AnsiBbs);

        e.process(b"\x1b[?7lABCDEZ");

        let s = e.screen();
        assert_eq!(text(&e, 0), "ABCDZ");
        assert_eq!(text(&e, 1), "");
        assert_eq!(s.cursor_col, 4);
        assert_eq!(s.cursor_row, 0);
    }

    #[test]
    fn decawm_can_be_reenabled() {
        let mut e = TerminalEmulator::with_emulation(5, 3, EmulationMode::AnsiBbs);

        e.process(b"\x1b[?7lABCDE\x1b[?7hFG");

        assert_eq!(text(&e, 0), "ABCDF");
        assert_eq!(text(&e, 1), "G");
    }

    // ── Tab stops ─────────────────────────────────────────────────────────────

    #[test]
    fn tab_stop() {
        let mut e = emu();
        e.process(b"\t"); // from col 0, next stop is col 8
        assert_eq!(e.screen().cursor_col, 8);
    }

    // ── ESC sequences ─────────────────────────────────────────────────────────

    #[test]
    fn esc_ris_resets_state() {
        let mut e = emu();
        e.process(b"\x1b[1mHello\x1bc"); // bold, write, RIS
        let s = e.screen();
        // After RIS, grid should be blank and style reset
        assert_eq!(s.get(0, 0).ch, ' ');
        assert!(!s.get(0, 0).style.bold);
    }

    #[test]
    fn reverse_index() {
        let mut e = emu();
        // Cursor at top of scroll region (row 0) — RI should scroll down
        e.process(b"FIRST\r\n");
        e.process(b"\x1b[1;1H"); // back to row 0
        e.process(b"\x1bM"); // RI at top → scroll down, blank row inserted at top
        let s = e.screen();
        assert_eq!(s.get(0, 0).ch, ' '); // new blank row at top
                                         // "FIRST" shifted to row 1
        let row1: String = s.cells[1]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string();
        assert_eq!(row1, "FIRST");
    }

    // ── Alternate screen buffer ───────────────────────────────────────────────

    #[test]
    fn alt_screen_enter_and_leave() {
        let mut e = emu();
        e.process(b"main content");
        assert!(!e.in_alt_screen());
        let main_row0 = text(&e, 0);
        assert!(main_row0.contains("main content"));

        // Enter alt screen — main content should disappear.
        e.process(b"\x1b[?1049h");
        assert!(e.in_alt_screen());
        assert!(text(&e, 0).trim().is_empty());

        e.process(b"alt content");
        assert!(text(&e, 0).contains("alt content"));

        // Leave alt screen — main content should be restored.
        e.process(b"\x1b[?1049l");
        assert!(!e.in_alt_screen());
        assert!(text(&e, 0).contains("main content"));
    }

    #[test]
    fn alt_screen_47() {
        let mut e = emu();
        e.process(b"preserved");
        e.process(b"\x1b[?47h");
        assert!(e.in_alt_screen());
        e.process(b"\x1b[?47l");
        assert!(!e.in_alt_screen());
        assert!(text(&e, 0).contains("preserved"));
    }

    // ── OSC title ─────────────────────────────────────────────────────────────

    #[test]
    fn osc_title_set() {
        let mut e = emu();
        assert!(e.title().is_none());
        // OSC 0 ; title BEL
        e.process(b"\x1b]0;My Server\x07");
        assert_eq!(e.title(), Some("My Server"));
    }

    #[test]
    fn osc_title_osc2() {
        let mut e = emu();
        e.process(b"\x1b]2;Another Title\x07");
        assert_eq!(e.title(), Some("Another Title"));
    }

    // ── ICH / ECH ─────────────────────────────────────────────────────────────

    #[test]
    fn insert_chars() {
        let mut e = emu();
        e.process(b"ABCDE");
        // Move cursor to col 2, insert 2 blanks → A B _ _ C D E
        e.process(b"\x1b[1;3H"); // row 1, col 3 (1-based) = col 2 (0-based)
        e.process(b"\x1b[2@"); // ICH 2
        let row = text(&e, 0);
        assert!(row.starts_with("AB  CD"));
    }

    #[test]
    fn erase_chars() {
        let mut e = emu();
        e.process(b"ABCDE");
        e.process(b"\x1b[1;2H"); // col 2 (1-based) = col 1 (0-based)
        e.process(b"\x1b[2X"); // ECH 2 → erase B and C
        let row = text(&e, 0);
        assert_eq!(&row[..5], "A  DE");
    }

    // ── CP437 / ANSI-BBS ──────────────────────────────────────────────────────

    #[test]
    fn cp437_high_bytes_translated() {
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::AnsiBbs);
        // 0xB3 = │ (vertical line in CP437)
        e.process(&[0xB3]);
        assert_eq!(e.screen().get(0, 0).ch, '│');
    }

    #[test]
    fn cp437_block_char() {
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::AnsiBbs);
        // 0xDB = █ (full block)
        e.process(&[0xDB]);
        assert_eq!(e.screen().get(0, 0).ch, '█');
    }

    #[test]
    fn cp437_escape_still_works() {
        // ANSI-BBS should still process normal escape sequences.
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::AnsiBbs);
        e.process(b"\x1b[1mHello");
        let s = e.screen();
        assert!(s.get(0, 0).style.bold);
        assert_eq!(text(&e, 0).trim_end(), "Hello");
    }

    #[test]
    fn ansi_bbs_cp437_9b_is_printable_not_c1_csi() {
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::AnsiBbs);

        e.process(&[0x9B, b'3', b'1', b'm', b'R']);

        let screen = e.screen();
        assert_eq!(text(&e, 0), "¢31mR");
        assert_eq!(screen.get(4, 0).style.fg, Color::Default);
    }

    // ── Application cursor keys ───────────────────────────────────────────────

    #[test]
    fn decckm_toggle() {
        let mut e = emu();
        assert!(!e.app_cursor_keys());
        e.process(b"\x1b[?1h");
        assert!(e.app_cursor_keys());
        e.process(b"\x1b[?1l");
        assert!(!e.app_cursor_keys());
    }

    #[test]
    fn ansi_alias_selects_ansi_bbs() {
        assert_eq!(EmulationMode::parse("ansi"), EmulationMode::AnsiBbs);
        assert_eq!(EmulationMode::parse("ansi-bbs"), EmulationMode::AnsiBbs);
    }

    #[test]
    fn ansi_bbs_canvas_is_capped_at_eighty_columns() {
        assert_eq!(EmulationMode::AnsiBbs.canvas_cols(132), 80);
        assert_eq!(EmulationMode::AnsiBbs.canvas_cols(80), 80);
        assert_eq!(EmulationMode::AnsiBbs.canvas_cols(40), 40);
        assert_eq!(EmulationMode::Xterm.canvas_cols(132), 132);
    }

    #[test]
    fn xterm_mode_preserves_utf8_box_drawing() {
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::Xterm);
        e.process("┌─█┐".as_bytes());
        assert_eq!(text(&e, 0), "┌─█┐");
    }

    #[test]
    fn xterm_mode_preserves_split_utf8_box_drawing() {
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::Xterm);
        let bytes = "┌".as_bytes();

        e.process(&bytes[..1]);
        e.process(&bytes[1..]);

        assert_eq!(text(&e, 0), "┌");
    }

    #[test]
    fn raw_c1_csi_sgr_is_treated_as_escape_sequence() {
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::Xterm);

        e.process(b"\x9b31mR");

        let screen = e.screen();
        let cell = screen.get(0, 0);
        assert_eq!(cell.ch, 'R');
        assert_eq!(cell.style.fg, Color::Ansi(1));
    }

    #[test]
    fn utf8_encoded_c1_csi_sgr_is_treated_as_escape_sequence() {
        let mut e = TerminalEmulator::with_emulation(80, 24, EmulationMode::Xterm);

        e.process(b"\xc2\x9b32mG");

        let screen = e.screen();
        let cell = screen.get(0, 0);
        assert_eq!(cell.ch, 'G');
        assert_eq!(cell.style.fg, Color::Ansi(2));
    }
}
