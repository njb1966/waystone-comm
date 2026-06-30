mod app;
mod ui;

use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, Write},
    panic,
    process::{Command as ProcessCommand, Stdio},
    sync::Once,
};

use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use waystone_comm_core::{
    connection::{Connection, Protocol},
    directory::{Directory, DirectoryEntry},
    protocols::{
        raw::RawConnection, serial::SerialConnection, ssh::SshConnection, telnet::TelnetConnection,
    },
    terminal::EmulationMode,
};

// ── CLI definition (MASTERPLAN §3.8) ─────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "waystone-comm",
    version,
    about = "Waystone Comm — a modern terminal emulator for retro and modern protocols"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Connect to a remote host.
    Connect {
        #[command(subcommand)]
        protocol: ConnectProtocol,
    },
    /// Replay a raw capture file through the terminal renderer.
    Replay {
        /// Raw capture path
        path: String,
        /// Terminal emulation (xterm-256color, vt100, ansi-bbs)
        #[arg(long, default_value = "xterm-256color")]
        emulation: String,
    },
    /// List saved directory entries.
    List,
}

#[derive(Subcommand)]
enum ConnectProtocol {
    /// Connect via SSH.
    Ssh {
        /// user@host, host, user@host:port, or host:port
        target: String,
        /// Port (default: 22)
        #[arg(short, long, default_value_t = 22)]
        port: u16,
        /// Path to private key file
        #[arg(short, long)]
        identity: Option<String>,
        /// Terminal emulation (xterm-256color, vt100, ansi-bbs)
        #[arg(long, default_value = "xterm-256color")]
        emulation: String,
        /// Enable legacy SSH algorithms for older BBS servers
        #[arg(long)]
        legacy_ssh: bool,
        /// Prompt for an SSH password before entering the TUI
        #[arg(long)]
        ask_password: bool,
        /// Read SSH password from this environment variable
        #[arg(long, value_name = "VAR")]
        password_env: Option<String>,
        /// Write an exact raw byte capture of the session to this file
        #[arg(long, value_name = "PATH")]
        raw_capture: Option<String>,
    },
    /// Connect via Telnet.
    Telnet {
        /// host or host:port
        target: String,
        /// Terminal emulation (xterm-256color, vt100, ansi-bbs)
        #[arg(long, default_value = "xterm-256color")]
        emulation: String,
    },
    /// Connect via serial port.
    Serial {
        /// Serial device path (e.g. /dev/ttyUSB0)
        device: String,
        /// Baud rate (default: 9600)
        #[arg(short, long, default_value_t = 9600)]
        baud: u32,
        /// Data bits (5/6/7/8, default: 8)
        #[arg(long, default_value_t = 8)]
        data_bits: u8,
        /// Stop bits (1/2, default: 1)
        #[arg(long, default_value_t = 1)]
        stop_bits: u8,
        /// Parity (none/odd/even, default: none)
        #[arg(long, default_value = "none")]
        parity: String,
        /// Flow control (none/software/hardware, default: none)
        #[arg(long, default_value = "none")]
        flow: String,
        /// Terminal emulation (xterm-256color, vt100, ansi-bbs)
        #[arg(long, default_value = "xterm-256color")]
        emulation: String,
    },
    /// Connect via raw TCP.
    Raw {
        /// host:port
        target: String,
        /// Terminal emulation (xterm-256color, vt100, ansi-bbs)
        #[arg(long, default_value = "xterm-256color")]
        emulation: String,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

static TERMINAL_PANIC_HOOK: Once = Once::new();

struct TerminalRestoreGuard;

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

fn init_tui_terminal() -> (ratatui::DefaultTerminal, TerminalRestoreGuard) {
    install_terminal_panic_hook();
    (ratatui::init(), TerminalRestoreGuard)
}

fn install_terminal_panic_hook() {
    TERMINAL_PANIC_HOOK.call_once(|| {
        let previous_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            ratatui::restore();
            previous_hook(panic_info);
        }));
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            let dir = Directory::load_default().context("load dialing directory")?;
            let (terminal, _restore_guard) = init_tui_terminal();
            app::run_multi_session(terminal, dir).await
        }
        Some(Command::List) => run_list(),
        Some(Command::Replay { path, emulation }) => {
            let emulation = normalize_emulation(&emulation)?;
            let data = std::fs::read(&path).with_context(|| format!("read raw capture {path}"))?;
            let (terminal, _restore_guard) = init_tui_terminal();
            app::run_replay(terminal, data, path, EmulationMode::parse(&emulation))
        }
        Some(Command::Connect { protocol }) => {
            let (conn, entry) = open_connect(protocol).await?;
            let (terminal, _restore_guard) = init_tui_terminal();
            app::run_session(terminal, conn, entry).await
        }
    }
}

// ── Connect from directory ────────────────────────────────────────────────────

/// Open a connection for an entry chosen from the dialing directory.
#[allow(dead_code)]
async fn connect_entry(
    terminal: ratatui::DefaultTerminal,
    entry: DirectoryEntry,
) -> anyhow::Result<()> {
    let conn: Box<dyn Connection> = match entry.protocol {
        Protocol::Ssh => {
            let mut c = Box::new(SshConnection::new());
            c.connect(&entry).await.context("SSH connect")?;
            c
        }
        Protocol::Telnet => {
            let mut c = Box::new(TelnetConnection::new());
            c.connect(&entry).await.context("Telnet connect")?;
            c
        }
        Protocol::Serial => {
            let mut c = Box::new(SerialConnection::new());
            c.connect(&entry).await.context("Serial connect")?;
            c
        }
        Protocol::Raw => {
            let mut c = Box::new(RawConnection::new());
            c.connect(&entry).await.context("Raw TCP connect")?;
            c
        }
        other => anyhow::bail!("Protocol {other} not yet implemented"),
    };
    app::run_session(terminal, conn, entry).await
}

// ── `waystone-comm list` ───────────────────────────────────────────────────────────

fn run_list() -> anyhow::Result<()> {
    let dir = Directory::load_default().context("load dialing directory")?;
    let entries = dir.list_entries();

    if entries.is_empty() {
        println!("No saved entries. Use the TUI to add entries.");
        return Ok(());
    }

    println!("{:<36}  {:<20}  {:<10}  Host", "ID", "Name", "Protocol");
    println!("{}", "-".repeat(80));
    for e in entries {
        println!(
            "{:<36}  {:<20}  {:<10}  {}",
            e.id, e.name, e.protocol, e.connection.host
        );
    }
    Ok(())
}

// ── `waystone-comm connect` ────────────────────────────────────────────────────────

async fn open_connect(
    protocol: ConnectProtocol,
) -> anyhow::Result<(Box<dyn Connection>, DirectoryEntry)> {
    match protocol {
        ConnectProtocol::Ssh {
            target,
            port,
            identity,
            emulation,
            legacy_ssh,
            ask_password,
            password_env,
            raw_capture,
        } => {
            let (user, host, port) = parse_ssh_target(&target, port)?;
            let mut entry =
                DirectoryEntry::new(format!("{user}@{host}"), Protocol::Ssh, host.clone());
            entry.connection.port = Some(port);
            entry.connection.username = Some(user.clone());
            apply_terminal_size_and_emulation(&mut entry, &emulation, 3)?;
            if let Some(key) = identity {
                entry.connection.extra.insert("key_path".into(), key);
            }
            if legacy_ssh {
                entry
                    .connection
                    .extra
                    .insert("legacy_ssh".into(), "true".into());
            }
            if let Some(password) =
                resolve_ssh_password(&user, &host, port, ask_password, password_env)?
            {
                entry.connection.extra.insert("password".into(), password);
            }
            if let Some(path) = raw_capture {
                entry
                    .connection
                    .extra
                    .insert("raw_capture_path".into(), path);
            }

            let mut conn = Box::new(SshConnection::new());
            conn.connect(&entry).await.context("SSH connect")?;
            Ok((conn, entry))
        }

        ConnectProtocol::Telnet { target, emulation } => {
            let (host, port) = parse_host_port(&target, 23)?;
            let mut entry =
                DirectoryEntry::new(format!("{host}:{port}"), Protocol::Telnet, host.clone());
            entry.connection.port = Some(port);
            apply_terminal_size_and_emulation(&mut entry, &emulation, 3)?;

            let mut conn = Box::new(TelnetConnection::new());
            conn.connect(&entry).await.context("Telnet connect")?;
            Ok((conn, entry))
        }

        ConnectProtocol::Serial {
            device,
            baud,
            data_bits,
            stop_bits,
            parity,
            flow,
            emulation,
        } => {
            let mut entry = DirectoryEntry::new(&device, Protocol::Serial, &device);
            entry.connection.host = device.clone();
            apply_terminal_size_and_emulation(&mut entry, &emulation, 3)?;
            entry
                .connection
                .extra
                .insert("baud_rate".into(), baud.to_string());
            entry
                .connection
                .extra
                .insert("data_bits".into(), data_bits.to_string());
            entry
                .connection
                .extra
                .insert("stop_bits".into(), stop_bits.to_string());
            entry.connection.extra.insert("parity".into(), parity);
            entry.connection.extra.insert("flow_control".into(), flow);

            let mut conn = Box::new(SerialConnection::new());
            conn.connect(&entry).await.context("Serial connect")?;
            Ok((conn, entry))
        }

        ConnectProtocol::Raw { target, emulation } => {
            let (host, port) = parse_host_port(&target, 0)?;
            if port == 0 {
                bail!("Port is required for raw TCP: use host:port");
            }
            let mut entry =
                DirectoryEntry::new(format!("{host}:{port}"), Protocol::Raw, host.clone());
            entry.connection.port = Some(port);
            apply_terminal_size_and_emulation(&mut entry, &emulation, 3)?;

            let mut conn = Box::new(RawConnection::new());
            conn.connect(&entry).await.context("Raw TCP connect")?;
            Ok((conn, entry))
        }
    }
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

fn parse_user_at_host(target: &str) -> anyhow::Result<(String, String)> {
    if let Some((user, host)) = target.split_once('@') {
        Ok((user.to_string(), host.to_string()))
    } else {
        // No user specified — use current OS user.
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .unwrap_or_else(|_| "user".to_string());
        Ok((user, target.to_string()))
    }
}

fn parse_ssh_target(target: &str, cli_port: u16) -> anyhow::Result<(String, String, u16)> {
    let (user, host_port) = parse_user_at_host(target)?;
    let has_embedded_port = host_port.rsplit_once(':').is_some();
    let (host, parsed_port) = parse_host_port(&host_port, cli_port)?;
    let port = if has_embedded_port && cli_port != 22 {
        cli_port
    } else {
        parsed_port
    };
    Ok((user, host, port))
}

fn parse_host_port(target: &str, default_port: u16) -> anyhow::Result<(String, u16)> {
    if let Some((host, port_str)) = target.rsplit_once(':') {
        let port: u16 = port_str
            .parse()
            .with_context(|| format!("invalid port: {port_str}"))?;
        Ok((host.to_string(), port))
    } else {
        Ok((target.to_string(), default_port))
    }
}

fn apply_terminal_size_and_emulation(
    entry: &mut DirectoryEntry,
    emulation: &str,
    reserved_rows: u16,
) -> anyhow::Result<()> {
    let (cols, rows) = crossterm::terminal::size().context("query terminal size")?;
    let emulation = normalize_emulation(emulation)?;
    entry.terminal.cols = EmulationMode::parse(&emulation).canvas_cols(cols);
    entry.terminal.rows = rows.saturating_sub(reserved_rows).max(1);
    entry.terminal.emulation = emulation;
    Ok(())
}

fn normalize_emulation(value: &str) -> anyhow::Result<String> {
    match value.trim().to_lowercase().as_str() {
        "" | "xterm" | "xterm-256" | "xterm-256color" => Ok("xterm-256color".into()),
        "vt100" => Ok("vt100".into()),
        "vt220" => Ok("vt220".into()),
        "ansi" | "ansi-bbs" | "ansi_bbs" | "ansibbs" | "bbs" => Ok("ansi-bbs".into()),
        other => bail!("Unknown emulation '{other}'. Use xterm-256color, vt100, or ansi-bbs"),
    }
}

fn resolve_ssh_password(
    user: &str,
    host: &str,
    port: u16,
    ask_password: bool,
    password_env: Option<String>,
) -> anyhow::Result<Option<String>> {
    if let Some(var) = password_env {
        let password = std::env::var(&var)
            .with_context(|| format!("read SSH password from environment variable {var}"))?;
        return Ok(Some(password));
    }

    if ask_password {
        return read_password_from_tty(&format!("{user}@{host}:{port} password: "))
            .map(Some)
            .context("read SSH password");
    }

    Ok(None)
}

fn read_password_from_tty(prompt: &str) -> anyhow::Result<String> {
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("open /dev/tty")?;
    write!(tty, "{prompt}")?;
    tty.flush()?;

    let stty_stdin = Stdio::from(tty.try_clone()?);
    let _ = ProcessCommand::new("stty")
        .arg("-echo")
        .stdin(stty_stdin)
        .status();

    let mut line = String::new();
    let read_result = {
        let mut reader = BufReader::new(tty.try_clone()?);
        reader.read_line(&mut line)
    };

    let stty_stdin = Stdio::from(tty.try_clone()?);
    let _ = ProcessCommand::new("stty")
        .arg("echo")
        .stdin(stty_stdin)
        .status();
    writeln!(tty)?;

    read_result?;
    while line.ends_with(['\n', '\r']) {
        line.pop();
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ssh_target_defaults_to_cli_port() {
        let (_user, host, port) = parse_ssh_target("example.com", 22).unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 22);
    }

    #[test]
    fn parse_ssh_target_accepts_host_port() {
        let (_user, host, port) = parse_ssh_target("ariabbs.com:1991", 22).unwrap();
        assert_eq!(host, "ariabbs.com");
        assert_eq!(port, 1991);
    }

    #[test]
    fn parse_ssh_target_accepts_user_host_port() {
        let (user, host, port) = parse_ssh_target("bbs@ariabbs.com:1991", 22).unwrap();
        assert_eq!(user, "bbs");
        assert_eq!(host, "ariabbs.com");
        assert_eq!(port, 1991);
    }

    #[test]
    fn parse_ssh_target_prefers_explicit_cli_port() {
        let (_user, host, port) = parse_ssh_target("ariabbs.com:1991", 2222).unwrap();
        assert_eq!(host, "ariabbs.com");
        assert_eq!(port, 2222);
    }

    #[test]
    fn parse_ssh_target_rejects_bad_port() {
        let err = parse_ssh_target("ariabbs.com:ssh", 22).unwrap_err();
        assert!(err.to_string().contains("invalid port"));
    }

    #[test]
    fn normalize_emulation_accepts_bbs_aliases() {
        assert_eq!(normalize_emulation("ansi").unwrap(), "ansi-bbs");
        assert_eq!(normalize_emulation("xterm").unwrap(), "xterm-256color");
    }

    #[test]
    fn ansi_bbs_emulation_caps_wide_terminal_width() {
        assert_eq!(EmulationMode::parse("ansi-bbs").canvas_cols(200), 80);
        assert_eq!(EmulationMode::parse("xterm-256color").canvas_cols(200), 200);
    }
}
