/// Serial port connection — wraps the `serialport` crate.
///
/// Serial I/O is blocking, so it runs in `tokio::task::spawn_blocking`.
/// Settings are parsed from the `DirectoryEntry.connection.extra` map,
/// matching the TOML schema in MASTERPLAN §3.4.
use async_trait::async_trait;
use serialport::{DataBits, FlowControl, Parity, StopBits};
use tokio::sync::mpsc;

use crate::connection::{Connection, ConnectionError, ConnectionStatus, Protocol, Result};
use crate::directory::DirectoryEntry;

// ── Serial settings helpers ───────────────────────────────────────────────────

fn parse_data_bits(s: &str) -> DataBits {
    match s.trim() {
        "5" => DataBits::Five,
        "6" => DataBits::Six,
        "7" => DataBits::Seven,
        _ => DataBits::Eight,
    }
}

fn parse_stop_bits(s: &str) -> StopBits {
    match s.trim() {
        "2" => StopBits::Two,
        _ => StopBits::One,
    }
}

fn parse_parity(s: &str) -> Parity {
    match s.trim().to_lowercase().as_str() {
        "odd" => Parity::Odd,
        "even" => Parity::Even,
        _ => Parity::None,
    }
}

fn parse_flow_control(s: &str) -> FlowControl {
    match s.trim().to_lowercase().as_str() {
        "software" | "xon" | "xonxoff" => FlowControl::Software,
        "hardware" | "rtscts" => FlowControl::Hardware,
        _ => FlowControl::None,
    }
}

/// Parsed serial port settings.
#[derive(Debug, Clone)]
pub struct SerialSettings {
    pub port: String,
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub stop_bits: StopBits,
    pub parity: Parity,
    pub flow_control: FlowControl,
    pub timeout_ms: u64,
}

impl SerialSettings {
    /// Parse settings from a `DirectoryEntry`.
    pub fn from_entry(entry: &DirectoryEntry) -> Self {
        let extra = &entry.connection.extra;
        Self {
            port: entry.connection.host.clone(),
            baud_rate: extra
                .get("baud_rate")
                .and_then(|v| v.parse().ok())
                .unwrap_or(9600),
            data_bits: parse_data_bits(extra.get("data_bits").map(String::as_str).unwrap_or("8")),
            stop_bits: parse_stop_bits(extra.get("stop_bits").map(String::as_str).unwrap_or("1")),
            parity: parse_parity(extra.get("parity").map(String::as_str).unwrap_or("none")),
            flow_control: parse_flow_control(
                extra
                    .get("flow_control")
                    .map(String::as_str)
                    .unwrap_or("none"),
            ),
            timeout_ms: extra
                .get("timeout_ms")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1000),
        }
    }
}

// ── Connection ────────────────────────────────────────────────────────────────

/// Channels for communicating with the blocking serial I/O task.
struct SerialHandles {
    tx: mpsc::Sender<Vec<u8>>,   // send data to serial port
    rx: mpsc::Receiver<Vec<u8>>, // receive data from serial port
}

pub struct SerialConnection {
    status: ConnectionStatus,
    handles: Option<SerialHandles>,
}

impl SerialConnection {
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            handles: None,
        }
    }

    /// List all available serial ports on this system.
    #[must_use]
    pub fn available_ports() -> Vec<String> {
        serialport::available_ports()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.port_name)
            .collect()
    }
}

impl Default for SerialConnection {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connection for SerialConnection {
    async fn connect(&mut self, entry: &DirectoryEntry) -> Result<()> {
        self.status = ConnectionStatus::Connecting;

        let settings = SerialSettings::from_entry(entry);

        // Open the port in spawn_blocking — serialport is a blocking API
        let port_result = tokio::task::spawn_blocking(move || {
            serialport::new(&settings.port, settings.baud_rate)
                .data_bits(settings.data_bits)
                .stop_bits(settings.stop_bits)
                .parity(settings.parity)
                .flow_control(settings.flow_control)
                .timeout(std::time::Duration::from_millis(settings.timeout_ms))
                .open()
        })
        .await
        .map_err(|e| ConnectionError::Protocol(e.to_string()))?;

        let port = port_result.map_err(|e| ConnectionError::Protocol(e.to_string()))?;

        // Channel pair for async ↔ blocking bridge
        let (to_serial_tx, mut to_serial_rx) = mpsc::channel::<Vec<u8>>(64);
        let (from_serial_tx, from_serial_rx) = mpsc::channel::<Vec<u8>>(64);

        // Writer task: receives bytes via channel, writes to serial
        let mut write_port = port
            .try_clone()
            .map_err(|e| ConnectionError::Io(e.into()))?;
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            while let Some(data) = to_serial_rx.blocking_recv() {
                if write_port.write_all(&data).is_err() {
                    break;
                }
            }
        });

        // Reader task: reads from serial, sends bytes via channel
        let mut read_port = port;
        let tx = from_serial_tx;
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut buf = vec![0u8; 512];
            loop {
                match read_port.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::TimedOut
                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        // Serial read timeout is normal — just retry
                        continue;
                    }
                    Err(_) => break,
                }
            }
        });

        self.handles = Some(SerialHandles {
            tx: to_serial_tx,
            rx: from_serial_rx,
        });
        self.status = ConnectionStatus::Connected;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<()> {
        // Dropping the channel handles closes the port tasks
        self.handles = None;
        self.status = ConnectionStatus::Disconnected;
        Ok(())
    }

    async fn read(&mut self) -> Result<Vec<u8>> {
        let handles = self
            .handles
            .as_mut()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;

        handles.rx.recv().await.ok_or_else(|| {
            self.status = ConnectionStatus::Disconnected;
            ConnectionError::Disconnected("serial port closed".into())
        })
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let handles = self
            .handles
            .as_mut()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;

        handles
            .tx
            .send(data.to_vec())
            .await
            .map_err(|_| ConnectionError::Disconnected("serial write channel closed".into()))
    }

    fn protocol(&self) -> Protocol {
        Protocol::Serial
    }

    fn status(&self) -> ConnectionStatus {
        self.status.clone()
    }

    fn supports_file_transfer(&self) -> bool {
        true // Zmodem/Xmodem commonly used over serial
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_settings_defaults() {
        use crate::directory::DirectoryEntry;
        let entry = DirectoryEntry::new("router", Protocol::Serial, "/dev/ttyUSB0");
        let s = SerialSettings::from_entry(&entry);
        assert_eq!(s.port, "/dev/ttyUSB0");
        assert_eq!(s.baud_rate, 9600);
        assert!(matches!(s.data_bits, DataBits::Eight));
        assert!(matches!(s.stop_bits, StopBits::One));
        assert!(matches!(s.parity, Parity::None));
        assert!(matches!(s.flow_control, FlowControl::None));
    }

    #[test]
    fn parse_settings_custom() {
        use crate::directory::DirectoryEntry;
        let mut entry = DirectoryEntry::new("router", Protocol::Serial, "/dev/ttyUSB0");
        entry
            .connection
            .extra
            .insert("baud_rate".into(), "115200".into());
        entry.connection.extra.insert("parity".into(), "odd".into());
        entry
            .connection
            .extra
            .insert("flow_control".into(), "hardware".into());
        entry
            .connection
            .extra
            .insert("stop_bits".into(), "2".into());
        entry
            .connection
            .extra
            .insert("data_bits".into(), "7".into());

        let s = SerialSettings::from_entry(&entry);
        assert_eq!(s.baud_rate, 115200);
        assert!(matches!(s.parity, Parity::Odd));
        assert!(matches!(s.flow_control, FlowControl::Hardware));
        assert!(matches!(s.stop_bits, StopBits::Two));
        assert!(matches!(s.data_bits, DataBits::Seven));
    }

    #[test]
    fn parse_parity_variants() {
        assert!(matches!(parse_parity("none"), Parity::None));
        assert!(matches!(parse_parity("odd"), Parity::Odd));
        assert!(matches!(parse_parity("even"), Parity::Even));
        assert!(matches!(parse_parity("ODD"), Parity::Odd));
        assert!(matches!(parse_parity("EVEN"), Parity::Even));
        assert!(matches!(parse_parity("unknown"), Parity::None));
    }

    #[test]
    fn parse_flow_control_variants() {
        assert!(matches!(parse_flow_control("none"), FlowControl::None));
        assert!(matches!(
            parse_flow_control("software"),
            FlowControl::Software
        ));
        assert!(matches!(
            parse_flow_control("hardware"),
            FlowControl::Hardware
        ));
        assert!(matches!(
            parse_flow_control("rtscts"),
            FlowControl::Hardware
        ));
        assert!(matches!(parse_flow_control("xon"), FlowControl::Software));
    }

    #[test]
    fn available_ports_returns_list() {
        // Should not panic; may be empty in CI/test environments
        let ports = SerialConnection::available_ports();
        // Type check is sufficient — list may be empty on machines with no serial ports
        let _: Vec<String> = ports;
    }

    #[test]
    fn default_status() {
        let conn = SerialConnection::new();
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
        assert_eq!(conn.protocol(), Protocol::Serial);
        assert!(conn.supports_file_transfer());
    }

    #[tokio::test]
    async fn disconnect_when_not_connected_ok() {
        let mut conn = SerialConnection::new();
        assert!(conn.disconnect().await.is_ok());
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
    }

    #[tokio::test]
    async fn read_when_not_connected_errors() {
        let mut conn = SerialConnection::new();
        assert!(matches!(
            conn.read().await,
            Err(ConnectionError::Disconnected(_))
        ));
    }

    #[tokio::test]
    async fn write_when_not_connected_errors() {
        let mut conn = SerialConnection::new();
        assert!(matches!(
            conn.write(b"hello").await,
            Err(ConnectionError::Disconnected(_))
        ));
    }
    // Note: a loopback integration test requires physical hardware or a virtual
    // serial pair (e.g. `socat -d -d pty,raw,echo=0 pty,raw,echo=0`).
    // That test lives in tests/serial_integration.rs and is skipped in CI
    // unless the WAYSTONE_COMM_SERIAL_TEST env var is set.
}
