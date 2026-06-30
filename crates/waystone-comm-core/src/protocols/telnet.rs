/// Telnet protocol implementation — RFC 854 + option negotiation.
///
/// Key detail: `0xFF` (IAC — Interpret As Command) can appear anywhere in the
/// data stream. Every byte must be scanned.
///
/// Design: the parser is pure (no I/O). It returns both the clean data bytes
/// AND a list of response commands to send. The connection layer sends them.
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::connection::{Connection, ConnectionError, ConnectionStatus, Protocol, Result};
use crate::directory::DirectoryEntry;
use crate::terminal::EmulationMode;

// ── Telnet constants ──────────────────────────────────────────────────────────

const IAC: u8 = 0xFF;
const WILL: u8 = 0xFB;
const WONT: u8 = 0xFC;
const DO: u8 = 0xFD;
const DONT: u8 = 0xFE;
const SB: u8 = 0xFA;
const SE: u8 = 0xF0;

const OPT_ECHO: u8 = 1;
const OPT_SUPPRESS_GO_AHEAD: u8 = 3;
const OPT_TERMINAL_TYPE: u8 = 24;
const OPT_NAWS: u8 = 31;

const TELQUAL_IS: u8 = 0;
const TELQUAL_SEND: u8 = 1;

// ── Pure parser ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum ParseState {
    Data,
    Iac,
    Negotiation(u8),
    Subnegotiation(Vec<u8>),
    SubnegotiationIac(Vec<u8>),
}

/// Parse result from a chunk of raw telnet bytes.
#[derive(Debug, Default)]
pub struct ParseResult {
    /// Clean data bytes for the terminal emulator.
    pub data: Vec<u8>,
    /// Raw bytes to write back to the server.
    pub responses: Vec<Vec<u8>>,
}

/// Stateful Telnet IAC parser (pure — no I/O).
pub struct TelnetParser {
    state: ParseState,
    cols: u16,
    rows: u16,
    terminal_type: Vec<u8>,
}

impl TelnetParser {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            state: ParseState::Data,
            cols,
            rows,
            terminal_type: b"xterm-256color".to_vec(),
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    pub fn set_terminal_type_from_emulation(&mut self, emulation: &str) {
        self.terminal_type = match EmulationMode::parse(emulation) {
            EmulationMode::AnsiBbs => b"ANSI".to_vec(),
            EmulationMode::Vt100 => b"VT100".to_vec(),
            EmulationMode::Vt220 => b"VT220".to_vec(),
            EmulationMode::Xterm => b"xterm-256color".to_vec(),
        };
    }

    fn naws_msg(&self) -> Vec<u8> {
        vec![
            IAC,
            SB,
            OPT_NAWS,
            (self.cols >> 8) as u8,
            self.cols as u8,
            (self.rows >> 8) as u8,
            self.rows as u8,
            IAC,
            SE,
        ]
    }

    fn terminal_type_msg(&self) -> Vec<u8> {
        let mut msg = vec![IAC, SB, OPT_TERMINAL_TYPE, TELQUAL_IS];
        msg.extend_from_slice(&self.terminal_type);
        msg.extend_from_slice(&[IAC, SE]);
        msg
    }

    fn respond_negotiation(&self, cmd: u8, opt: u8) -> Vec<Vec<u8>> {
        let mut responses = Vec::new();
        match (cmd, opt) {
            (DO, OPT_SUPPRESS_GO_AHEAD) => responses.push(vec![IAC, WILL, OPT_SUPPRESS_GO_AHEAD]),
            (DONT, OPT_SUPPRESS_GO_AHEAD) => responses.push(vec![IAC, WONT, OPT_SUPPRESS_GO_AHEAD]),
            (WILL, OPT_SUPPRESS_GO_AHEAD) => responses.push(vec![IAC, DO, OPT_SUPPRESS_GO_AHEAD]),
            (WONT, OPT_SUPPRESS_GO_AHEAD) => responses.push(vec![IAC, DONT, OPT_SUPPRESS_GO_AHEAD]),

            (WILL, OPT_ECHO) => responses.push(vec![IAC, DO, OPT_ECHO]),
            (WONT, OPT_ECHO) => responses.push(vec![IAC, DONT, OPT_ECHO]),
            (DO, OPT_ECHO) => responses.push(vec![IAC, WONT, OPT_ECHO]),

            (DO, OPT_NAWS) => {
                responses.push(vec![IAC, WILL, OPT_NAWS]);
                responses.push(self.naws_msg());
            }
            (DONT, OPT_NAWS) => responses.push(vec![IAC, WONT, OPT_NAWS]),

            (DO, OPT_TERMINAL_TYPE) => responses.push(vec![IAC, WILL, OPT_TERMINAL_TYPE]),
            (DONT, OPT_TERMINAL_TYPE) => responses.push(vec![IAC, WONT, OPT_TERMINAL_TYPE]),

            // Refuse unknown DO/WILL options
            (DO, opt) => responses.push(vec![IAC, WONT, opt]),
            (WILL, opt) => responses.push(vec![IAC, DONT, opt]),
            _ => {}
        }
        responses
    }

    fn respond_subnegotiation(&self, buf: &[u8]) -> Vec<Vec<u8>> {
        if buf.len() < 2 {
            return vec![];
        }
        match (buf[0], buf[1]) {
            (OPT_TERMINAL_TYPE, TELQUAL_SEND) => vec![self.terminal_type_msg()],
            _ => vec![],
        }
    }

    /// Parse a chunk of raw incoming bytes.
    /// Returns clean data bytes plus any response messages to send.
    pub fn parse(&mut self, raw: &[u8]) -> ParseResult {
        let mut result = ParseResult::default();

        for &byte in raw {
            let next = match std::mem::replace(&mut self.state, ParseState::Data) {
                ParseState::Data => {
                    if byte == IAC {
                        ParseState::Iac
                    } else {
                        result.data.push(byte);
                        ParseState::Data
                    }
                }
                ParseState::Iac => match byte {
                    IAC => {
                        result.data.push(IAC);
                        ParseState::Data
                    }
                    SB => ParseState::Subnegotiation(Vec::new()),
                    WILL | WONT | DO | DONT => ParseState::Negotiation(byte),
                    _ => ParseState::Data,
                },
                ParseState::Negotiation(cmd) => {
                    result.responses.extend(self.respond_negotiation(cmd, byte));
                    ParseState::Data
                }
                ParseState::Subnegotiation(mut buf) => {
                    if byte == IAC {
                        ParseState::SubnegotiationIac(buf)
                    } else {
                        buf.push(byte);
                        ParseState::Subnegotiation(buf)
                    }
                }
                ParseState::SubnegotiationIac(buf) => {
                    if byte == SE {
                        result.responses.extend(self.respond_subnegotiation(&buf));
                        ParseState::Data
                    } else if byte == IAC {
                        let mut buf = buf;
                        buf.push(IAC);
                        ParseState::Subnegotiation(buf)
                    } else {
                        ParseState::Data
                    }
                }
            };
            self.state = next;
        }

        result
    }
}

// ── Connection ────────────────────────────────────────────────────────────────

pub struct TelnetConnection {
    stream: Option<TcpStream>,
    status: ConnectionStatus,
    parser: TelnetParser,
}

impl TelnetConnection {
    #[must_use]
    pub fn new() -> Self {
        Self {
            stream: None,
            status: ConnectionStatus::Disconnected,
            parser: TelnetParser::new(80, 24),
        }
    }

    /// Notify the connection of a terminal resize; sends NAWS if connected.
    async fn send_resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.parser.resize(cols, rows);
        if self.status == ConnectionStatus::Connected {
            let msg = self.parser.naws_msg();
            self.send_raw(&msg).await?;
        }
        Ok(())
    }

    async fn send_raw(&mut self, bytes: &[u8]) -> Result<()> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;
        stream.write_all(bytes).await.map_err(ConnectionError::Io)
    }
}

impl Default for TelnetConnection {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connection for TelnetConnection {
    async fn connect(&mut self, entry: &DirectoryEntry) -> Result<()> {
        self.status = ConnectionStatus::Connecting;
        self.parser.resize(entry.terminal.cols, entry.terminal.rows);
        self.parser
            .set_terminal_type_from_emulation(&entry.terminal.emulation);

        let port = entry.connection.port.unwrap_or(23);
        let addr = format!("{}:{}", entry.connection.host, port);

        let stream = TcpStream::connect(&addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::ConnectionRefused {
                ConnectionError::Refused {
                    host: entry.connection.host.clone(),
                    port,
                }
            } else {
                ConnectionError::Io(e)
            }
        })?;

        stream.set_nodelay(true).map_err(ConnectionError::Io)?;
        self.stream = Some(stream);
        self.status = ConnectionStatus::Connected;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.shutdown().await;
        }
        self.status = ConnectionStatus::Disconnected;
        Ok(())
    }

    async fn read(&mut self) -> Result<Vec<u8>> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.map_err(ConnectionError::Io)?;
        if n == 0 {
            self.status = ConnectionStatus::Disconnected;
            return Err(ConnectionError::Disconnected(
                "server closed connection".into(),
            ));
        }
        buf.truncate(n);

        // Pure parse — returns data + responses to send
        let result = self.parser.parse(&buf);

        // Write all negotiation responses (borrow checker: done after parse returns)
        for response in result.responses {
            if let Some(stream) = self.stream.as_mut() {
                let _ = stream.write_all(&response).await;
            }
        }

        Ok(result.data)
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;

        // Escape 0xFF bytes per RFC 854
        let escaped: Vec<u8> = data
            .iter()
            .flat_map(|&b| if b == IAC { vec![IAC, IAC] } else { vec![b] })
            .collect();

        stream
            .write_all(&escaped)
            .await
            .map_err(ConnectionError::Io)
    }

    async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.send_resize(cols, rows).await
    }

    fn protocol(&self) -> Protocol {
        Protocol::Telnet
    }

    fn status(&self) -> ConnectionStatus {
        self.status.clone()
    }

    fn supports_file_transfer(&self) -> bool {
        true
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> TelnetParser {
        TelnetParser::new(80, 24)
    }

    #[test]
    fn plain_data_passes_through() {
        let mut p = parser();
        let r = p.parse(b"hello world");
        assert_eq!(r.data, b"hello world");
        assert!(r.responses.is_empty());
    }

    #[test]
    fn escaped_iac_becomes_single_byte() {
        let mut p = parser();
        let r = p.parse(&[IAC, IAC]);
        assert_eq!(r.data, &[0xFF]);
        assert!(r.responses.is_empty());
    }

    #[test]
    fn iac_commands_stripped_from_output() {
        let mut p = parser();
        let r = p.parse(&[IAC, WILL, OPT_ECHO]);
        assert!(
            r.data.is_empty(),
            "IAC command bytes must not appear in data"
        );
        // Should emit DO ECHO response
        assert!(!r.responses.is_empty());
        assert_eq!(r.responses[0], &[IAC, DO, OPT_ECHO]);
    }

    #[test]
    fn data_before_and_after_iac_preserved() {
        let mut p = parser();
        let mut input = vec![b'A', b'B'];
        input.extend_from_slice(&[IAC, WILL, OPT_SUPPRESS_GO_AHEAD]);
        input.extend_from_slice(b"CD");
        let r = p.parse(&input);
        assert_eq!(r.data, b"ABCD");
    }

    #[test]
    fn subnegotiation_stripped_from_output() {
        let mut p = parser();
        let input = &[IAC, SB, OPT_TERMINAL_TYPE, TELQUAL_SEND, IAC, SE];
        let r = p.parse(input);
        assert!(r.data.is_empty());
        // Should respond with terminal type IS
        assert!(!r.responses.is_empty());
        assert_eq!(r.responses[0][0], IAC);
        assert_eq!(r.responses[0][1], SB);
        assert_eq!(r.responses[0][2], OPT_TERMINAL_TYPE);
        assert_eq!(r.responses[0][3], TELQUAL_IS);
        assert_eq!(
            &r.responses[0][4..r.responses[0].len() - 2],
            b"xterm-256color"
        );
        assert_eq!(&r.responses[0][r.responses[0].len() - 2..], &[IAC, SE]);
    }

    #[test]
    fn terminal_type_matches_ansi_bbs_emulation() {
        let mut p = parser();
        p.set_terminal_type_from_emulation("ansi-bbs");

        let r = p.parse(&[IAC, SB, OPT_TERMINAL_TYPE, TELQUAL_SEND, IAC, SE]);

        assert_eq!(&r.responses[0][4..r.responses[0].len() - 2], b"ANSI");
    }

    #[test]
    fn terminal_type_matches_vt_emulation() {
        let mut p = parser();
        p.set_terminal_type_from_emulation("vt100");
        let r = p.parse(&[IAC, SB, OPT_TERMINAL_TYPE, TELQUAL_SEND, IAC, SE]);
        assert_eq!(&r.responses[0][4..r.responses[0].len() - 2], b"VT100");

        p.set_terminal_type_from_emulation("vt220");
        let r = p.parse(&[IAC, SB, OPT_TERMINAL_TYPE, TELQUAL_SEND, IAC, SE]);
        assert_eq!(&r.responses[0][4..r.responses[0].len() - 2], b"VT220");
    }

    #[test]
    fn naws_response_on_do_naws() {
        let mut p = TelnetParser::new(220, 50);
        let r = p.parse(&[IAC, DO, OPT_NAWS]);
        // Should get WILL NAWS + the NAWS subnegotiation
        assert_eq!(r.responses.len(), 2);
        assert_eq!(r.responses[0], &[IAC, WILL, OPT_NAWS]);
        let naws = &r.responses[1];
        assert_eq!(naws[0], IAC);
        assert_eq!(naws[1], SB);
        assert_eq!(naws[2], OPT_NAWS);
        assert_eq!(u16::from_be_bytes([naws[3], naws[4]]), 220);
        assert_eq!(u16::from_be_bytes([naws[5], naws[6]]), 50);
        assert_eq!(naws[7], IAC);
        assert_eq!(naws[8], SE);
    }

    #[test]
    fn unknown_do_refused_with_wont() {
        let mut p = parser();
        let r = p.parse(&[IAC, DO, 99]); // unknown option 99
        assert_eq!(r.responses[0], &[IAC, WONT, 99]);
    }

    #[test]
    fn unknown_will_refused_with_dont() {
        let mut p = parser();
        let r = p.parse(&[IAC, WILL, 99]);
        assert_eq!(r.responses[0], &[IAC, DONT, 99]);
    }

    #[test]
    fn outbound_iac_escaped() {
        let data = &[0x41u8, 0xFF, 0x42];
        let escaped: Vec<u8> = data
            .iter()
            .flat_map(|&b| if b == IAC { vec![IAC, IAC] } else { vec![b] })
            .collect();
        assert_eq!(escaped, &[0x41, 0xFF, 0xFF, 0x42]);
    }

    #[test]
    fn default_status() {
        let conn = TelnetConnection::new();
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
        assert_eq!(conn.protocol(), Protocol::Telnet);
        assert!(conn.supports_file_transfer());
    }

    #[tokio::test]
    async fn disconnect_when_not_connected_ok() {
        let mut conn = TelnetConnection::new();
        assert!(conn.disconnect().await.is_ok());
    }

    #[tokio::test]
    async fn write_when_not_connected_errors() {
        let mut conn = TelnetConnection::new();
        assert!(matches!(
            conn.write(b"hello").await,
            Err(ConnectionError::Disconnected(_))
        ));
    }

    /// Integration test: connect → receive plain data → send → receive echo.
    /// IAC parsing is tested exhaustively in unit tests above; this test
    /// exercises the full connect/read/write/disconnect lifecycle.
    #[tokio::test]
    async fn connect_read_write_disconnect() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Send plain greeting, then echo whatever client sends
            tokio::io::AsyncWriteExt::write_all(&mut sock, b"Hello")
                .await
                .unwrap();
            let mut buf = vec![0u8; 64];
            let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut sock, &buf[..n])
                .await
                .unwrap();
        });

        use crate::directory::DirectoryEntry;
        let mut entry = DirectoryEntry::new("bbs", Protocol::Telnet, "127.0.0.1");
        entry.connection.port = Some(port);

        let mut conn = TelnetConnection::new();
        conn.connect(&entry).await.unwrap();
        assert_eq!(conn.status(), ConnectionStatus::Connected);

        let data = conn.read().await.unwrap();
        assert_eq!(data, b"Hello");

        conn.write(b"World").await.unwrap();
        let echo = conn.read().await.unwrap();
        assert_eq!(echo, b"World");

        conn.disconnect().await.unwrap();
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
    }
}
