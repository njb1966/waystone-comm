use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::connection::{Connection, ConnectionError, ConnectionStatus, Protocol, Result};
use crate::directory::DirectoryEntry;

/// Raw TCP connection — plain socket, no framing, no protocol overhead.
///
/// Optional TLS support is planned (rustls) but not yet wired in Phase 1.
/// Used for protocol debugging, custom services, and TCP-based BBS systems.
pub struct RawConnection {
    stream: Option<TcpStream>,
    status: ConnectionStatus,
}

impl RawConnection {
    #[must_use]
    pub fn new() -> Self {
        Self {
            stream: None,
            status: ConnectionStatus::Disconnected,
        }
    }
}

impl Default for RawConnection {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Connection for RawConnection {
    async fn connect(&mut self, entry: &DirectoryEntry) -> Result<()> {
        self.status = ConnectionStatus::Connecting;

        let port = entry.connection.port.unwrap_or(4242); // Raw TCP has no standard port
        let addr = format!("{}:{}", entry.connection.host, port);

        let stream = TcpStream::connect(&addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::ConnectionRefused {
                ConnectionError::Refused {
                    host: entry.connection.host.clone(),
                    port,
                }
            } else if e.kind() == std::io::ErrorKind::TimedOut {
                ConnectionError::Timeout { seconds: 30 }
            } else {
                ConnectionError::Io(e)
            }
        })?;

        // Disable Nagle's algorithm for low-latency interactive use
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
        Ok(buf)
    }

    async fn write(&mut self, data: &[u8]) -> Result<()> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| ConnectionError::Disconnected("not connected".into()))?;
        stream.write_all(data).await.map_err(ConnectionError::Io)
    }

    fn protocol(&self) -> Protocol {
        Protocol::Raw
    }

    fn status(&self) -> ConnectionStatus {
        self.status.clone()
    }

    fn supports_file_transfer(&self) -> bool {
        true // Zmodem can run over raw TCP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_status_is_disconnected() {
        let conn = RawConnection::new();
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
        assert_eq!(conn.protocol(), Protocol::Raw);
        assert!(conn.supports_file_transfer());
    }

    #[tokio::test]
    async fn disconnect_when_not_connected_is_ok() {
        let mut conn = RawConnection::new();
        // Disconnecting when already disconnected must not panic or error
        assert!(conn.disconnect().await.is_ok());
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
    }

    #[tokio::test]
    async fn read_when_not_connected_returns_error() {
        let mut conn = RawConnection::new();
        let result = conn.read().await;
        assert!(result.is_err());
        assert!(matches!(result, Err(ConnectionError::Disconnected(_))));
    }

    #[tokio::test]
    async fn write_when_not_connected_returns_error() {
        let mut conn = RawConnection::new();
        let result = conn.write(b"hello").await;
        assert!(result.is_err());
        assert!(matches!(result, Err(ConnectionError::Disconnected(_))));
    }

    /// Integration test: loopback echo via a local TCP listener.
    #[tokio::test]
    async fn connect_read_write_disconnect() {
        use tokio::net::TcpListener;

        // Spin up a minimal echo server on a random port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let port = addr.port();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 64];
            let n = tokio::io::AsyncReadExt::read(&mut sock, &mut buf)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut sock, &buf[..n])
                .await
                .unwrap();
        });

        let entry = {
            use crate::connection::Protocol;
            use crate::directory::DirectoryEntry;
            let mut e = DirectoryEntry::new("test", Protocol::Raw, "127.0.0.1");
            e.connection.port = Some(port);
            e
        };

        let mut conn = RawConnection::new();
        conn.connect(&entry).await.unwrap();
        assert_eq!(conn.status(), ConnectionStatus::Connected);

        conn.write(b"hello").await.unwrap();
        let data = conn.read().await.unwrap();
        assert_eq!(data, b"hello");

        conn.disconnect().await.unwrap();
        assert_eq!(conn.status(), ConnectionStatus::Disconnected);
    }
}
