use std::{
    collections::HashMap, fs::OpenOptions, io::Write, path::PathBuf, sync::Arc, time::Duration,
};

use anyhow::Context;
use chrono::Utc;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    DefaultTerminal,
};
use tokio::sync::mpsc;
use uuid::Uuid;
use waystone_comm_core::{
    connection::{Connection, ConnectionError, ConnectionStatus, Protocol},
    credentials::{public_key_from_private_openssh, Credential, CredentialKind, CredentialManager},
    directory::{Directory, DirectoryEntry},
    history::{SessionHistoryDb, OUTCOME_CONNECTED, OUTCOME_ERROR},
    keymapping::{
        AppCommand, KeyAction, KeyCode as KmKeyCode, KeyProfile, KeyProfileStore, KeySpec,
    },
    logging::SessionLog,
    protocols::{
        raw::RawConnection, serial::SerialConnection, ssh::SshConnection, telnet::TelnetConnection,
    },
    scripting::{ScriptCommand, ScriptEngine, ScriptStore, SessionApi},
    terminal::{EmulationMode, TerminalEmulator},
    transfer::{
        find_zmodem_signature, BlockingByteStream, TransferPhase, TransferProgress, TransferStats,
        ZmodemReceiver, ZmodemSender,
    },
};

use crate::ui::{
    render_fkey_bar, render_status_bar, render_tab_bar, render_terminal_pane, CredentialPanel,
    CredentialPanelAction, DirectoryPanel, EntryScriptContext, KeymappingPanel,
    KeymappingPanelAction, LogViewerAction, LogViewerPanel, PanelAction, ScriptPanel,
    ScriptPanelAction, TabInfo,
};

/// Render loop frame rate target (approx 60 fps).
const TICK_MS: u64 = 16;

/// Read timeout — if no data arrives from the connection within this window,
/// we go back to checking input and re-rendering. Short enough to stay responsive.
const READ_TIMEOUT_MS: u64 = 10;

// ── Welcome screen ────────────────────────────────────────────────────────────

/// Display a static welcome screen (used when no connection is active).
#[allow(dead_code)]
pub fn run_welcome(mut terminal: DefaultTerminal) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| {
            let area = f.area();
            let lines = vec![
                Line::from(vec![Span::styled(
                    format!("Waystone Comm v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(Color::Green),
                )]),
                Line::from(""),
                Line::from("Usage: waystone-comm connect <ssh|telnet|serial|raw> ..."),
                Line::from("       waystone-comm list"),
                Line::from(""),
                Line::from("Press Ctrl+Q or Ctrl+C to quit."),
            ];
            let para = Paragraph::new(lines).centered();
            f.render_widget(para, area);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

// ── Raw capture replay ───────────────────────────────────────────────────────

/// Replay a raw byte capture through the terminal renderer without opening a
/// network connection. This is intended for deterministic ANSI/BBS debugging.
pub fn run_replay(
    mut terminal: DefaultTerminal,
    data: Vec<u8>,
    title: String,
    emulation: EmulationMode,
) -> anyhow::Result<()> {
    let (cols, rows) = crossterm::terminal::size().context("query terminal size")?;
    let mut emulator = replay_emulator(cols, rows, emulation, &data);

    loop {
        let screen = emulator.screen();
        let title = title.clone();
        terminal.draw(move |f| {
            let area = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(area);

            f.render_widget(
                Paragraph::new(format!(" Replay - {title} "))
                    .style(Style::default().bg(Color::Cyan).fg(Color::Black)),
                chunks[0],
            );
            render_terminal_pane(f, chunks[1], &screen);
            f.render_widget(
                Paragraph::new(" Ctrl+Q Quit | Ctrl+C Quit ")
                    .style(Style::default().bg(Color::DarkGray).fg(Color::White)),
                chunks[2],
            );
        })?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {}
                },
                Event::Resize(w, h) => {
                    emulator = replay_emulator(w, h, emulation, &data);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn replay_emulator(
    cols: u16,
    rows: u16,
    emulation: EmulationMode,
    data: &[u8],
) -> TerminalEmulator {
    let cols = emulation.canvas_cols(cols);
    let term_rows = rows.saturating_sub(2).max(1);
    let mut emulator = TerminalEmulator::with_emulation(cols, term_rows, emulation);
    emulator.process(data);
    emulator
}

// ── Multi-session types ───────────────────────────────────────────────────────

#[derive(Debug)]
enum SessionMsg {
    Data(Vec<u8>),
    #[allow(dead_code)]
    Disconnected,
    Warning(String),
    Error(String),
}

type TransferTap = Arc<std::sync::Mutex<Option<std::sync::mpsc::SyncSender<Vec<u8>>>>>;
type SharedTransferProgress = Arc<std::sync::Mutex<Option<TransferProgress>>>;

struct TabSession {
    #[allow(dead_code)]
    id: Uuid,
    entry: DirectoryEntry,
    emulator: TerminalEmulator,
    log: SessionLog,
    write_tx: mpsc::Sender<Vec<u8>>,
    resize_tx: mpsc::Sender<(u16, u16)>,
    script_control_tx: mpsc::Sender<ScriptCommand>,
    read_rx: mpsc::Receiver<SessionMsg>,
    status: ConnectionStatus,
    has_unread: bool,
    /// Forwards a copy of incoming data to the `on_connect` script's `wait_for`.
    #[allow(dead_code)]
    script_data_tx: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    /// Log messages produced by `s.log()` / `s.notify()` inside scripts.
    script_log: Arc<std::sync::Mutex<Vec<String>>>,
    /// Values exposed through `s.credential(key)` for this entry's selected credential.
    script_credentials: HashMap<String, String>,
    /// Active key profile (per-entry or global default).
    key_profile: KeyProfile,
    /// Open log viewer for this session (toggled by F8 / ToggleLog).
    log_viewer: Option<LogViewerPanel>,
    /// Row ID in `session_logs` for this session (None until begin_session succeeds).
    history_session_id: Option<uuid::Uuid>,
    /// Wall-clock instant when this session connected (for duration tracking).
    connected_at: std::time::Instant,
    /// Bytes received from the remote (updated in drain_sessions).
    bytes_recv: i64,
    /// True once end_session / record_connection has been written for this tab.
    history_recorded: bool,
    /// When a transfer is active, incoming bytes are redirected to this tap
    /// instead of being routed to the terminal emulator.
    transfer_tap: TransferTap,
    /// Live progress updated by the blocking transfer thread.
    transfer_progress: SharedTransferProgress,
    /// Signals transfer completion (Ok = stats, Err = error message).
    transfer_done_rx: Option<tokio::sync::oneshot::Receiver<Result<Vec<TransferStats>, String>>>,
    /// Short UI message for local actions that do not come from the remote.
    local_message: Option<String>,
}

// ── Background I/O task ───────────────────────────────────────────────────────

async fn session_io_task(
    mut conn: Box<dyn Connection>,
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    mut resize_rx: mpsc::Receiver<(u16, u16)>,
    mut script_control_rx: mpsc::Receiver<ScriptCommand>,
    read_tx: mpsc::Sender<SessionMsg>,
    script_data_tx: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    transfer_tap: TransferTap,
) {
    let mut script_data_warning_sent = false;

    loop {
        tokio::select! {
            result = conn.read() => {
                match result {
                    Ok(data) if data.is_empty() => {
                        // Empty data means the protocol consumed all bytes as control/
                        // negotiation (e.g. Telnet IAC). Not a disconnect — keep reading.
                        continue;
                    }
                    Ok(data) => {
                        // If a transfer tap is active, redirect data to the blocking
                        // transfer thread instead of the terminal emulator.
                        let tap = match active_transfer_tap(&transfer_tap) {
                            Ok(tap) => tap,
                            Err(message) => {
                                let _ = read_tx.send(SessionMsg::Error(message.to_string())).await;
                                break;
                            }
                        };
                        let tap_result = tap.map(|tx| tx.try_send(data.clone()));
                        if let Some(result) = tap_result {
                            match result {
                                Ok(()) => continue,
                                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                                    let _ = read_tx
                                        .send(SessionMsg::Error(
                                            "transfer data buffer full; aborting session to avoid corrupting transfer".into(),
                                        ))
                                        .await;
                                    break;
                                }
                                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                                    let _ = read_tx
                                        .send(SessionMsg::Error(
                                            "transfer data receiver closed; aborting session".into(),
                                        ))
                                        .await;
                                    break;
                                }
                            }
                        }
                        // Fork a copy to the script's blocking data channel.
                        if let Some(ref tx) = script_data_tx {
                            match tx.try_send(data.clone()) {
                                Ok(()) => {
                                    script_data_warning_sent = false;
                                }
                                Err(std::sync::mpsc::TrySendError::Full(_))
                                    if !script_data_warning_sent =>
                                {
                                    script_data_warning_sent = true;
                                    let _ = read_tx
                                        .send(SessionMsg::Warning(
                                            "script data buffer full; wait_for data may be incomplete".into(),
                                        ))
                                        .await;
                                }
                                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {}
                                Err(_) => {}
                            }
                        }
                        if read_tx.send(SessionMsg::Data(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(ConnectionError::Disconnected(_)) => {
                        let _ = read_tx.send(SessionMsg::Disconnected).await;
                        break;
                    }
                    Err(e) => {
                        let _ = read_tx.send(SessionMsg::Error(e.to_string())).await;
                        break;
                    }
                }
            }
            result = write_rx.recv() => {
                match result {
                    Some(bytes) => {
                        match conn.write(&bytes).await {
                            Ok(()) => {}
                            Err(ConnectionError::Disconnected(_)) => {
                                let _ = read_tx.send(SessionMsg::Disconnected).await;
                                break;
                            }
                            Err(e) => {
                                let _ = read_tx.send(SessionMsg::Error(e.to_string())).await;
                                break;
                            }
                        }
                    }
                    None => break,
                }
            }
            result = resize_rx.recv() => {
                match result {
                    Some((cols, rows)) => {
                        match conn.resize(cols, rows).await {
                            Ok(()) => {}
                            Err(ConnectionError::Disconnected(_)) => {
                                let _ = read_tx.send(SessionMsg::Disconnected).await;
                                break;
                            }
                            Err(e) => {
                                let _ = read_tx.send(SessionMsg::Error(e.to_string())).await;
                                break;
                            }
                        }
                    }
                    None => break,
                }
            }
            result = script_control_rx.recv() => {
                match result {
                    Some(ScriptCommand::Disconnect) => {
                        let _ = conn.disconnect().await;
                        let _ = read_tx.send(SessionMsg::Disconnected).await;
                        break;
                    }
                    None => {}
                }
            }
        }
    }
}

fn active_transfer_tap(
    transfer_tap: &TransferTap,
) -> Result<Option<std::sync::mpsc::SyncSender<Vec<u8>>>, &'static str> {
    transfer_tap
        .lock()
        .map(|guard| guard.clone())
        .map_err(|_| "transfer state unavailable; aborting session")
}

fn set_transfer_tap(
    transfer_tap: &TransferTap,
    tap: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
) -> Result<(), &'static str> {
    let mut guard = transfer_tap
        .lock()
        .map_err(|_| "transfer state unavailable")?;
    *guard = tap;
    Ok(())
}

fn set_transfer_progress(
    transfer_progress: &SharedTransferProgress,
    progress: Option<TransferProgress>,
) -> Result<(), &'static str> {
    let mut guard = transfer_progress
        .lock()
        .map_err(|_| "transfer progress unavailable")?;
    *guard = progress;
    Ok(())
}

fn clear_transfer_state(session: &mut TabSession) -> Result<(), &'static str> {
    set_transfer_tap(&session.transfer_tap, None)?;
    set_transfer_progress(&session.transfer_progress, None)?;
    Ok(())
}

async fn script_credentials_for_entry(
    entry: &DirectoryEntry,
) -> (HashMap<String, String>, Option<String>) {
    let Some(credential_id) = entry.credential_id else {
        return (HashMap::new(), None);
    };

    let manager = match CredentialManager::open_default().await {
        Ok(manager) => manager,
        Err(err) => {
            return (
                HashMap::new(),
                Some(format!("[credential] store unavailable: {err}")),
            );
        }
    };

    let credential = match manager.retrieve(credential_id).await {
        Ok(credential) => credential,
        Err(err) => {
            return (
                HashMap::new(),
                Some(format!(
                    "[credential] unable to retrieve {credential_id}: {err}"
                )),
            );
        }
    };

    (script_values_for_credential(credential), None)
}

fn script_values_for_credential(credential: Credential) -> HashMap<String, String> {
    let secret = credential.secret.expose().to_string();
    let mut values = HashMap::new();
    values.insert("id".to_string(), credential.id.to_string());
    values.insert("name".to_string(), credential.name);
    values.insert("kind".to_string(), credential.kind.to_string());
    values.insert("secret".to_string(), secret.clone());

    if let Some(username) = credential.username {
        values.insert("username".to_string(), username.clone());
        values.insert("user".to_string(), username);
    }

    match credential.kind {
        CredentialKind::Password => {
            values.insert("password".to_string(), secret);
        }
        CredentialKind::Token => {
            values.insert("token".to_string(), secret);
        }
        CredentialKind::SshKey => {
            values.insert("key".to_string(), secret.clone());
            values.insert("ssh_key".to_string(), secret.clone());
            values.insert("private_key".to_string(), secret);
        }
    }

    values
}

// ── Spawn a session from a DirectoryEntry ─────────────────────────────────────

async fn spawn_session(
    mut entry: DirectoryEntry,
    cols: u16,
    rows: u16,
    engine: Arc<ScriptEngine>,
) -> Result<TabSession, String> {
    let emulation = EmulationMode::parse(&entry.terminal.emulation);
    let cols = emulation.canvas_cols(cols);
    entry.terminal.cols = cols;
    entry.terminal.rows = rows.max(1);

    let mut conn: Box<dyn Connection> = match entry.protocol {
        Protocol::Ssh => Box::new(SshConnection::new()),
        Protocol::Telnet => Box::new(TelnetConnection::new()),
        Protocol::Serial => Box::new(SerialConnection::new()),
        Protocol::Raw => Box::new(RawConnection::new()),
        ref other => return Err(format!("Protocol {other} not yet implemented")),
    };
    conn.connect(&entry).await.map_err(|e| e.to_string())?;

    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(256);
    let (resize_tx, resize_rx) = mpsc::channel::<(u16, u16)>(32);
    let (script_control_tx, script_control_rx) = mpsc::channel::<ScriptCommand>(32);
    let (read_tx, read_rx) = mpsc::channel::<SessionMsg>(1024);

    let script_log: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let (script_credentials, credential_warning) = script_credentials_for_entry(&entry).await;
    if let Some(warning) = credential_warning {
        append_script_log(&script_log, warning);
    }

    // Wire on_connect script if the entry has one.
    let script_data_tx = if let Some(script) = ScriptStore::new_default().entry_script(&entry.name)
    {
        if entry.credential_id.is_none() {
            append_script_log(
                &script_log,
                "[credential] no credential attached; s.credential(...) returns empty strings"
                    .into(),
            );
        }
        match engine.compile(&script.content) {
            Ok(ast) => {
                let (stx, srx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1024);
                let api = SessionApi::with_credentials_and_control(
                    entry.name.clone(),
                    write_tx.clone(),
                    srx,
                    Arc::clone(&script_log),
                    script_credentials.clone(),
                    script_control_tx.clone(),
                );
                let eng = Arc::clone(&engine);
                let script_log_for_hook = Arc::clone(&script_log);
                tokio::task::spawn_blocking(move || {
                    append_script_log(&script_log_for_hook, "on_connect started".into());
                    match eng.run_hook(&ast, "on_connect", api) {
                        Ok(()) => {
                            append_script_log(&script_log_for_hook, "on_connect completed".into());
                        }
                        Err(e) => {
                            append_script_log(
                                &script_log_for_hook,
                                format!("on_connect failed: {e}"),
                            );
                            eprintln!("[script] on_connect error: {e}");
                        }
                    }
                });
                Some(stx)
            }
            Err(e) => {
                append_script_log(&script_log, format!("[script compile error] {e}"));
                None
            }
        }
    } else {
        None
    };

    let transfer_tap: TransferTap = Arc::new(std::sync::Mutex::new(None));

    tokio::spawn(session_io_task(
        conn,
        write_rx,
        resize_rx,
        script_control_rx,
        read_tx,
        script_data_tx.clone(),
        Arc::clone(&transfer_tap),
    ));

    let log_base = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("waystone-comm")
        .join("logs");
    let log =
        SessionLog::new(&entry.name, log_base, entry.log.clone()).map_err(|e| e.to_string())?;
    let emulator = TerminalEmulator::with_emulation(cols, rows, emulation);

    let store = KeyProfileStore::new_default();
    let profile_name = entry.key_profile.as_deref().unwrap_or("Default");
    let key_profile = store.load_or_default(profile_name);

    Ok(TabSession {
        id: Uuid::new_v4(),
        entry,
        emulator,
        log,
        write_tx,
        resize_tx,
        script_control_tx,
        read_rx,
        status: ConnectionStatus::Connected,
        has_unread: false,
        script_data_tx,
        script_log,
        script_credentials,
        key_profile,
        log_viewer: None,
        history_session_id: None,
        connected_at: std::time::Instant::now(),
        bytes_recv: 0,
        history_recorded: false,
        transfer_tap,
        transfer_progress: Arc::new(std::sync::Mutex::new(None)),
        transfer_done_rx: None,
        local_message: None,
    })
}

// ── Drain all pending channel data into emulators ────────────────────────────

fn send_terminal_output(session: &mut TabSession) {
    let output = session.emulator.take_output();
    if !output.is_empty() {
        queue_session_write(session, output);
    }
}

fn queue_session_write(session: &mut TabSession, bytes: Vec<u8>) {
    match session.write_tx.try_send(bytes) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            session.status = ConnectionStatus::Disconnected;
            session.local_message = Some("Connection write channel closed".to_string());
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            session.local_message = Some("Connection write buffer full".to_string());
        }
    }
}

fn queue_session_resize(session: &mut TabSession, size: (u16, u16)) {
    match session.resize_tx.try_send(size) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            session.status = ConnectionStatus::Disconnected;
            session.local_message = Some("Connection resize channel closed".to_string());
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            session.local_message = Some("Connection resize buffer full".to_string());
        }
    }
}

fn write_session_log(session: &mut TabSession, bytes: &[u8], context: &str) {
    if let Err(err) = session.log.write_bytes(bytes) {
        session.local_message = Some(format!("{context} log write failed: {err}"));
    }
}

fn append_script_log(script_log: &Arc<std::sync::Mutex<Vec<String>>>, message: String) {
    if let Ok(mut log) = script_log.lock() {
        log.push(message);
    }
}

fn redact_script_log_message(message: &str, credentials: &HashMap<String, String>) -> String {
    let mut redacted = message.to_string();
    for key in [
        "secret",
        "password",
        "token",
        "key",
        "ssh_key",
        "private_key",
    ] {
        let Some(value) = credentials.get(key) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        redacted = redacted.replace(value, "***");
    }
    redacted
}

fn script_local_status(message: &str) -> Option<(u8, String)> {
    let msg = message.trim();
    if msg.is_empty() {
        return None;
    }

    if let Some(err) = msg.strip_prefix("[script compile error] ") {
        return Some((3, format!("Script compile error: {err}")));
    }
    if msg.starts_with("[credential]") {
        return Some((3, msg.to_string()));
    }
    if msg.starts_with("on_connect failed:") {
        return Some((3, format!("Script {msg}")));
    }
    if msg.contains(" prompt not seen") {
        return Some((2, format!("Script warning: {msg}")));
    }
    if msg == "on_connect started" {
        return Some((1, "Script on_connect started".into()));
    }
    if msg == "on_connect completed" {
        return Some((1, "Script on_connect completed".into()));
    }
    if msg.starts_with("sent ") {
        return Some((1, format!("Script: {msg}")));
    }
    if msg.starts_with("named script ") {
        return Some((1, msg.to_string()));
    }

    None
}

fn merge_script_status(current: &mut Option<(u8, String)>, next: (u8, String)) {
    if current
        .as_ref()
        .map(|(priority, _)| next.0 >= *priority)
        .unwrap_or(true)
    {
        *current = Some(next);
    }
}

fn run_named_script_for_session(session: &mut TabSession, engine: Arc<ScriptEngine>, name: String) {
    let script_name = name.trim();
    if script_name.is_empty() {
        session.local_message = Some("Script name is empty.".to_string());
        return;
    }

    let store = ScriptStore::new_default();
    let Some(script) = store.named_script(script_name) else {
        session.local_message = Some(format!("Script not found: {script_name}"));
        return;
    };

    let ast = match engine.compile(&script.content) {
        Ok(ast) => ast,
        Err(err) => {
            session.local_message = Some(format!("Script compile failed: {err}"));
            return;
        }
    };

    let (_data_tx, data_rx) = std::sync::mpsc::channel();
    let script_log = Arc::clone(&session.script_log);
    let api = SessionApi::with_credentials_and_control(
        session.entry.name.clone(),
        session.write_tx.clone(),
        data_rx,
        Arc::clone(&script_log),
        session.script_credentials.clone(),
        session.script_control_tx.clone(),
    );
    let script_label = script.name.clone();
    session.local_message = Some(format!("Script started: {script_label}"));

    tokio::task::spawn_blocking(move || {
        append_script_log(&script_log, format!("named script {script_label} started"));
        match engine.run_hook(&ast, "on_connect", api) {
            Ok(()) => {
                append_script_log(
                    &script_log,
                    format!("named script {script_label} completed"),
                );
            }
            Err(err) => {
                append_script_log(
                    &script_log,
                    format!("named script {script_label} failed: {err}"),
                );
            }
        }
    });
}

fn take_directory_from_panel(dir_panel: &mut Option<DirectoryPanel>) -> Result<Directory, String> {
    dir_panel
        .take()
        .map(DirectoryPanel::into_directory)
        .ok_or_else(|| "Directory state unavailable.".to_string())
}

fn drain_sessions(tabs: &mut [TabSession], active: usize, engine: &ScriptEngine) {
    for (i, session) in tabs.iter_mut().enumerate() {
        // Flush any log messages produced by scripts into the session log.
        let script_messages = if let Ok(mut buf) = session.script_log.try_lock() {
            buf.drain(..).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut script_status: Option<(u8, String)> = None;
        for msg in script_messages {
            let redacted = redact_script_log_message(&msg, &session.script_credentials);
            write_session_log(
                session,
                format!("[script] {redacted}\n").as_bytes(),
                "Script",
            );
            if let Some(status) = script_local_status(&redacted) {
                merge_script_status(&mut script_status, status);
            }
        }
        if let Some((_, message)) = script_status {
            session.local_message = Some(message);
        }

        // Poll transfer completion.
        let transfer_result = session.transfer_done_rx.as_mut().map(|rx| rx.try_recv());
        let transfer_done = if let Some(result) = transfer_result {
            match result {
                Ok(result) => {
                    let clear_result = clear_transfer_state(session);
                    let msg = match result {
                        Ok(stats) => {
                            let total: u64 = stats.iter().map(|s| s.bytes).sum();
                            let local_message = format!("Transfer complete: {total} bytes");
                            session.local_message = Some(match clear_result {
                                Ok(()) => local_message,
                                Err(err) => format!("{local_message}; {err}"),
                            });
                            format!("\r\n[transfer complete: {total} bytes]\r\n")
                        }
                        Err(e) => {
                            let local_message = format!("Transfer error: {e}");
                            session.local_message = Some(match clear_result {
                                Ok(()) => local_message,
                                Err(err) => format!("{local_message}; {err}"),
                            });
                            format!("\r\n[transfer error: {e}]\r\n")
                        }
                    };
                    session.emulator.process(msg.as_bytes());
                    write_session_log(session, msg.as_bytes(), "Transfer status");
                    true
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    session.local_message = Some(match clear_transfer_state(session) {
                        Ok(()) => "Transfer aborted".to_string(),
                        Err(err) => format!("Transfer aborted; {err}"),
                    });
                    session.emulator.process(b"\r\n[transfer aborted]\r\n");
                    write_session_log(session, b"\r\n[transfer aborted]\r\n", "Transfer status");
                    true
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => false,
            }
        } else {
            false
        };
        if transfer_done {
            session.transfer_done_rx = None;
        }

        let mut data_this_frame: Vec<u8> = Vec::new();

        loop {
            match session.read_rx.try_recv() {
                Ok(SessionMsg::Data(data)) => {
                    session.bytes_recv += data.len() as i64;
                    // Auto-detect Zmodem initiation if no transfer is active.
                    if session.transfer_done_rx.is_none() {
                        if let Some(pos) = find_zmodem_signature(&data) {
                            // Display any data before the signature normally.
                            if pos > 0 {
                                let pre = &data[..pos];
                                write_session_log(session, pre, "Session");
                                session.emulator.process(pre);
                                send_terminal_output(session);
                                data_this_frame.extend_from_slice(pre);
                                if i != active {
                                    session.has_unread = true;
                                }
                            }
                            // Start receiving — pass the signature bytes as initial data.
                            start_zmodem_receive(session, data[pos..].to_vec());
                            break; // tap is now set; stop draining for this frame
                        }
                    }
                    write_session_log(session, &data, "Session");
                    session.emulator.process(&data);
                    send_terminal_output(session);
                    data_this_frame.extend_from_slice(&data);
                    if i != active {
                        session.has_unread = true;
                    }
                }
                Ok(SessionMsg::Disconnected) => {
                    session.status = ConnectionStatus::Disconnected;
                    session.emulator.process(b"\r\n[disconnected]\r\n");
                    break;
                }
                Ok(SessionMsg::Warning(message)) => {
                    session.local_message = Some(message);
                }
                Ok(SessionMsg::Error(e)) => {
                    let notice = format!("\r\n[connection error: {e}]\r\n");
                    session.emulator.process(notice.as_bytes());
                    session.status = ConnectionStatus::Error(e);
                    break;
                }
                Err(_) => break,
            }
        }

        // Run on_data hook if the entry script defines it.
        if !data_this_frame.is_empty() {
            if let Some(script) = ScriptStore::new_default().entry_script(&session.entry.name) {
                if let Ok(ast) = engine.compile(&script.content) {
                    if let Ok(s) = std::str::from_utf8(&data_this_frame) {
                        let (_, dummy_rx) = std::sync::mpsc::channel();
                        let (dummy_tx, _) = mpsc::channel(1);
                        let api = SessionApi::with_credentials_and_control(
                            session.entry.name.clone(),
                            dummy_tx,
                            dummy_rx,
                            Arc::clone(&session.script_log),
                            session.script_credentials.clone(),
                            session.script_control_tx.clone(),
                        );
                        if let Err(err) = engine.run_on_data(&ast, api, s) {
                            session.local_message = Some(format!("Script hook failed: {err}"));
                        }
                    }
                }
            }
        }
    }
}

// ── Transfer helpers ──────────────────────────────────────────────────────────

/// Start a Zmodem receive for `session`. `initial_bytes` contains the bytes
/// already captured from the session stream (including the trigger signature).
fn start_zmodem_receive(session: &mut TabSession, initial_bytes: Vec<u8>) {
    let (tap_tx, tap_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    if !initial_bytes.is_empty() {
        if let Err(err) = tap_tx.try_send(initial_bytes) {
            session.local_message = Some(format!("Transfer start failed: {err}"));
            return;
        }
    }
    if let Err(err) = set_transfer_tap(&session.transfer_tap, Some(tap_tx)) {
        session.local_message = Some(format!("Transfer start failed: {err}"));
        return;
    }

    let write_tx = session.write_tx.clone();
    let progress = Arc::clone(&session.transfer_progress);
    let progress_warning = set_transfer_progress(
        &progress,
        Some(TransferProgress {
            direction: waystone_comm_core::transfer::Direction::Receive,
            phase: TransferPhase::Waiting,
            filename: "zmodem".to_string(),
            bytes: 0,
            total: None,
            cps: 0,
        }),
    )
    .err();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    session.transfer_done_rx = Some(done_rx);
    session.local_message = Some(match progress_warning {
        Some(err) => format!("Zmodem receive started; {err}"),
        None => "Zmodem receive started".to_string(),
    });

    let dest_dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));

    tokio::task::spawn_blocking(move || {
        let mut stream = BlockingByteStream::new(tap_rx, write_tx);
        let result = ZmodemReceiver::new()
            .receive(&mut stream, &dest_dir, progress)
            .map_err(|e| e.to_string());
        done_tx.send(result).ok();
    });
}

/// Start a Zmodem send of `path` over `session`.
fn start_zmodem_send(session: &mut TabSession, path: PathBuf) {
    let (tap_tx, tap_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    if let Err(err) = set_transfer_tap(&session.transfer_tap, Some(tap_tx)) {
        session.local_message = Some(format!("Transfer start failed: {err}"));
        return;
    }

    let write_tx = session.write_tx.clone();
    let progress = Arc::clone(&session.transfer_progress);
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let total = path.metadata().map(|m| m.len()).ok();
    let progress_warning = set_transfer_progress(
        &progress,
        Some(TransferProgress {
            direction: waystone_comm_core::transfer::Direction::Send,
            phase: TransferPhase::Waiting,
            filename,
            bytes: 0,
            total,
            cps: 0,
        }),
    )
    .err();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    session.transfer_done_rx = Some(done_rx);

    tokio::task::spawn_blocking(move || {
        let result = ZmodemSender::new()
            .send(
                &mut BlockingByteStream::new(tap_rx, write_tx),
                &[path.as_path()],
                progress,
            )
            .map_err(|e| e.to_string());
        done_tx.send(result).ok();
    });

    session.local_message = Some(match progress_warning {
        Some(err) => format!("Zmodem send started; {err}"),
        None => "Zmodem send started".to_string(),
    });
}

// ── Session view renderer ─────────────────────────────────────────────────────

fn render_session_view(
    f: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    tabs: &[TabSession],
    active: usize,
    file_prompt: Option<&str>,
    fkey_labels: &[(String, String)],
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(1),    // terminal
            Constraint::Length(1), // status bar
            Constraint::Length(1), // fkey bar / file prompt
        ])
        .split(area);

    // Tab bar
    let tab_infos: Vec<TabInfo> = tabs
        .iter()
        .map(|t| TabInfo {
            label: &t.entry.name,
            has_unread: t.has_unread,
            status: &t.status,
        })
        .collect();
    render_tab_bar(f, chunks[0], &tab_infos, active);

    // Terminal pane + status bar
    if let Some(session) = tabs.get(active) {
        let screen = session.emulator.screen();
        render_terminal_pane(f, chunks[1], &screen);

        let transfer = session
            .transfer_progress
            .lock()
            .ok()
            .and_then(|g| g.clone());

        render_status_bar(
            f,
            chunks[2],
            &session.entry.protocol.to_string(),
            &session.entry.connection.host,
            &session.status,
            transfer,
            session.local_message.as_deref(),
        );
    }

    // Bottom row: file-send prompt or F-key bar
    if let Some(prompt) = file_prompt {
        let prompt_line = Line::from(vec![
            Span::styled(
                " Send file: ",
                Style::default().bg(Color::Cyan).fg(Color::Black),
            ),
            Span::styled(
                format!("{prompt}█"),
                Style::default().bg(Color::Cyan).fg(Color::Black),
            ),
            Span::styled(
                "  [Enter] send  [Esc] cancel",
                Style::default().bg(Color::Cyan).fg(Color::DarkGray),
            ),
        ]);
        f.render_widget(
            Paragraph::new(prompt_line).style(Style::default().bg(Color::Cyan)),
            chunks[3],
        );
    } else {
        render_fkey_bar(f, chunks[3], fkey_labels);
    }
}

// ── Key profile helpers ───────────────────────────────────────────────────────

/// Convert a crossterm key event to our library-agnostic `KeySpec`.
fn crossterm_to_keyspec(code: KeyCode, modifiers: KeyModifiers) -> Option<KeySpec> {
    let km_code = match code {
        KeyCode::Char(c) => KmKeyCode::Char(c.to_ascii_lowercase()),
        KeyCode::F(n) => KmKeyCode::F(n),
        KeyCode::Enter => KmKeyCode::Enter,
        KeyCode::Tab => KmKeyCode::Tab,
        KeyCode::Backspace => KmKeyCode::Backspace,
        KeyCode::Esc => KmKeyCode::Escape,
        KeyCode::Delete => KmKeyCode::Delete,
        KeyCode::Insert => KmKeyCode::Insert,
        KeyCode::Up => KmKeyCode::Up,
        KeyCode::Down => KmKeyCode::Down,
        KeyCode::Left => KmKeyCode::Left,
        KeyCode::Right => KmKeyCode::Right,
        KeyCode::Home => KmKeyCode::Home,
        KeyCode::End => KmKeyCode::End,
        KeyCode::PageUp => KmKeyCode::PageUp,
        KeyCode::PageDown => KmKeyCode::PageDown,
        _ => return None,
    };
    Some(KeySpec {
        code: km_code,
        ctrl: modifiers.contains(KeyModifiers::CONTROL),
        alt: modifiers.contains(KeyModifiers::ALT),
        shift: modifiers.contains(KeyModifiers::SHIFT),
    })
}

/// Dispatch an `AppCommand` to the appropriate app state. Quit is handled by
/// the caller checking the profile action after this returns.
#[allow(clippy::too_many_arguments)]
fn execute_app_command(
    cmd: AppCommand,
    tabs: &mut Vec<TabSession>,
    active: &mut usize,
    dir_panel: &mut Option<DirectoryPanel>,
    directory_store: &mut Option<Directory>,
    script_panel: &mut Option<ScriptPanel>,
    keymapping_panel: &mut Option<KeymappingPanel>,
    file_prompt: &mut Option<String>,
    global_key_profile: &KeyProfile,
) {
    match cmd {
        AppCommand::Quit => { /* handled by caller */ }
        AppCommand::OpenDirectory => {
            if let Some(store) = directory_store.take() {
                *dir_panel = Some(DirectoryPanel::new(store));
            }
        }
        AppCommand::OpenScripts => {
            let entry_context = tabs.get(*active).map(|session| EntryScriptContext {
                name: session.entry.name.clone(),
                credential_attached: session.entry.credential_id.is_some(),
            });
            *script_panel = Some(ScriptPanel::new(&ScriptStore::new_default(), entry_context));
        }
        AppCommand::OpenKeyMapping => {
            let profile = tabs
                .get(*active)
                .map(|s| s.key_profile.clone())
                .unwrap_or_else(|| global_key_profile.clone());
            *keymapping_panel = Some(KeymappingPanel::new(profile));
        }
        AppCommand::NewTab => {
            if let Some(store) = directory_store.take() {
                *dir_panel = Some(DirectoryPanel::new(store));
            }
        }
        AppCommand::CloseTab => {
            if !tabs.is_empty() {
                tabs.remove(*active);
                if tabs.is_empty() {
                    if let Some(store) = directory_store.take() {
                        *dir_panel = Some(DirectoryPanel::new(store));
                    }
                } else {
                    *active = (*active).min(tabs.len() - 1);
                }
            }
        }
        AppCommand::SwitchTab(n) => {
            let idx = n.saturating_sub(1);
            if idx < tabs.len() {
                *active = idx;
                tabs[*active].has_unread = false;
            }
        }
        AppCommand::SendFile => {
            if tabs
                .get(*active)
                .map(|s| s.transfer_done_rx.is_none())
                .unwrap_or(false)
            {
                *file_prompt = Some(String::new());
            }
        }
        AppCommand::ReceiveFile => {
            if let Some(session) = tabs.get_mut(*active) {
                if session.transfer_done_rx.is_none() {
                    queue_session_write(session, b"rz\r".to_vec());
                }
            }
        }
        AppCommand::ToggleLog => {
            if let Some(session) = tabs.get_mut(*active) {
                if session.log_viewer.is_some() {
                    session.log_viewer = None;
                } else {
                    let lines = session.log.load_recent_lines(2000);
                    session.log_viewer = Some(LogViewerPanel::new(lines));
                }
            }
        }
        AppCommand::OpenCredentials => { /* handled by caller with async context */ }
    }
}

// ── History helpers ───────────────────────────────────────────────────────────

/// Spawn an async task that writes `end_session` + `record_connection` for a
/// tab that has just disconnected or is being closed.
fn record_session_end(
    history: &Arc<SessionHistoryDb>,
    session: &mut TabSession,
    outcome: &'static str,
) {
    if session.history_recorded {
        return;
    }
    session.history_recorded = true;

    let Some(session_id) = session.history_session_id else {
        return;
    };

    let hist = Arc::clone(history);
    let entry_id = session.entry.id;
    let bytes_recv = session.bytes_recv;
    let log_path = session.log.current_path().to_string_lossy().to_string();
    let duration = session.connected_at.elapsed().as_secs() as i64;

    tokio::spawn(async move {
        if let Err(err) = hist
            .end_session(session_id, 0, bytes_recv, Some(&log_path))
            .await
        {
            eprintln!("[history] failed to end session {session_id}: {err}");
        }
        if let Err(err) = hist
            .record_connection(entry_id, Some(duration), outcome)
            .await
        {
            eprintln!("[history] failed to record connection for {entry_id}: {err}");
        }
    });
}

async fn backfill_directory_last_connected(
    history: &SessionHistoryDb,
    directory: &mut Directory,
) -> anyhow::Result<()> {
    let entries: Vec<(Uuid, Option<chrono::DateTime<Utc>>)> = directory
        .list_entries()
        .iter()
        .map(|entry| (entry.id, entry.last_connected))
        .collect();

    let mut changed = false;
    for (entry_id, current) in entries {
        let Some(latest) = history.latest_successful_connection(entry_id).await? else {
            continue;
        };
        if current.map(|ts| latest > ts).unwrap_or(true) {
            changed |= directory.mark_connected(entry_id, latest);
        }
    }

    if changed {
        directory.save()?;
    }

    Ok(())
}

async fn record_open_sessions_on_exit(history: &SessionHistoryDb, tabs: &mut [TabSession]) {
    for session in tabs {
        if session.history_recorded {
            continue;
        }
        session.history_recorded = true;

        let Some(session_id) = session.history_session_id else {
            continue;
        };

        let outcome = match session.status {
            ConnectionStatus::Error(_) => OUTCOME_ERROR,
            _ => OUTCOME_CONNECTED,
        };
        let log_path = session.log.current_path().to_string_lossy().to_string();
        let duration = session.connected_at.elapsed().as_secs() as i64;

        if let Err(err) = history
            .end_session(session_id, 0, session.bytes_recv, Some(&log_path))
            .await
        {
            eprintln!("[history] failed to end session {session_id}: {err}");
        }
        if let Err(err) = history
            .record_connection(session.entry.id, Some(duration), outcome)
            .await
        {
            eprintln!(
                "[history] failed to record connection for {}: {err}",
                session.entry.id
            );
        }
    }
}

async fn open_credential_panel(
    credential_manager: &Arc<tokio::sync::Mutex<Option<CredentialManager>>>,
    select_mode: bool,
) -> CredentialPanel {
    let mut guard = credential_manager.lock().await;
    if guard.is_none() {
        match CredentialManager::open_default().await {
            Ok(mgr) => {
                *guard = Some(mgr);
            }
            Err(e) => {
                eprintln!("[credentials] failed to open: {e}");
            }
        }
    }
    let (items, error) = if let Some(mgr) = guard.as_ref() {
        match mgr.list().await {
            Ok(items) => (items, None),
            Err(err) => (vec![], Some(format!("Unable to list credentials: {err}"))),
        }
    } else {
        (vec![], Some("Credential store is unavailable.".to_string()))
    };
    let mut panel = if select_mode {
        CredentialPanel::picker(items)
    } else {
        CredentialPanel::new(items)
    };
    if let Some(error) = error {
        panel.show_error(error);
    }
    panel
}

// ── Multi-session TUI entry point ─────────────────────────────────────────────

/// Run the full multi-session TUI. Opens the dialing directory first; sessions
/// accumulate as tabs. Ctrl+T reopens the directory to add another tab.
pub async fn run_multi_session(
    mut terminal: DefaultTerminal,
    mut directory: Directory,
) -> anyhow::Result<()> {
    let (mut cols, rows) = crossterm::terminal::size().context("query terminal size")?;
    let mut term_rows = rows.saturating_sub(3).max(1); // tab bar + status + fkeys

    let engine = Arc::new(ScriptEngine::new());

    // Open session history database (best-effort — log on failure, continue).
    let history_db: Arc<SessionHistoryDb> = {
        match SessionHistoryDb::open_default().await {
            Ok(db) => {
                let db = Arc::new(db);
                // Seed connection_history from TOML last_connected timestamps.
                if let Err(err) = db.import_from_directory(directory.list_entries()).await {
                    eprintln!("[history] failed to import directory entries: {err}");
                }
                db
            }
            Err(e) => {
                eprintln!("[history] failed to open history.db: {e}");
                // Fall back to a temporary database so the app still works.
                let temp_path = std::env::temp_dir()
                    .join(format!("waystone-comm-history-{}.db", Uuid::new_v4()));
                Arc::new(SessionHistoryDb::open(&temp_path).await.with_context(|| {
                    format!(
                        "failed to create temporary history database at {}",
                        temp_path.display()
                    )
                })?)
            }
        }
    };

    if let Err(err) = backfill_directory_last_connected(&history_db, &mut directory).await {
        eprintln!("[history] failed to backfill directory last-connected data: {err}");
    }

    // `directory` lives here; panels borrow it by taking ownership temporarily.
    let mut directory_store: Option<Directory> = None;
    let mut dir_panel: Option<DirectoryPanel> = Some(DirectoryPanel::new(directory));

    let mut tabs: Vec<TabSession> = Vec::new();
    let mut active: usize = 0;
    let mut script_panel: Option<ScriptPanel> = None;
    let mut keymapping_panel: Option<KeymappingPanel> = None;
    let mut credential_panel: Option<CredentialPanel> = None;
    let mut credential_selects_directory_form = false;
    let mut file_prompt: Option<String> = None;

    // Credential manager — opened lazily on first use.
    let credential_manager: Arc<tokio::sync::Mutex<Option<CredentialManager>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Load or create the global default key profile.
    let profile_store = KeyProfileStore::new_default();
    let mut global_key_profile = profile_store.load_global_default();

    loop {
        // Always drain all session channels (keeps background emulators live).
        drain_sessions(&mut tabs, active, &engine);

        // Record history for sessions that just disconnected.
        for session in &mut tabs {
            if session.history_recorded {
                continue;
            }
            let outcome = match &session.status {
                ConnectionStatus::Disconnected => Some(OUTCOME_CONNECTED),
                ConnectionStatus::Error(_) => Some(OUTCOME_ERROR),
                _ => None,
            };
            if let Some(outcome) = outcome {
                record_session_end(&history_db, session, outcome);
            }
        }

        // Compute F-key labels from the active session's profile (or global default).
        let fkey_labels: Vec<(String, String)> = tabs
            .get(active)
            .map(|s| s.key_profile.fkey_bar_labels())
            .unwrap_or_else(|| global_key_profile.fkey_bar_labels());

        // Render
        terminal.draw(|f| {
            let area = f.area();
            if let Some(panel) = dir_panel.as_mut() {
                panel.render(f, area);
            } else if !tabs.is_empty() {
                render_session_view(f, area, &tabs, active, file_prompt.as_deref(), &fkey_labels);
            }
            // Overlay panels (rendered on top in z-order)
            if let Some(kp) = keymapping_panel.as_mut() {
                kp.render(f, area);
            } else if let Some(sp) = script_panel.as_mut() {
                sp.render(f, area);
            } else if let Some(cp) = credential_panel.as_mut() {
                cp.render(f, area);
            } else if let Some(lv) = tabs.get_mut(active).and_then(|s| s.log_viewer.as_mut()) {
                lv.render(f, area);
            }
        })?;

        // Events
        if event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    append_key_debug(&key);

                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                    // File-send prompt captures all input while open.
                    if file_prompt.is_some() {
                        match key.code {
                            KeyCode::Esc => {
                                file_prompt = None;
                            }
                            KeyCode::Enter => {
                                let typed_path = file_prompt.take().unwrap_or_default();
                                let path = PathBuf::from(expand_home(&typed_path));
                                if let Some(session) = tabs.get_mut(active) {
                                    if path.is_file() {
                                        start_zmodem_send(session, path);
                                    } else {
                                        session.local_message =
                                            Some(format!("Send file not found: {typed_path}"));
                                    }
                                }
                            }
                            KeyCode::Backspace => {
                                if let Some(ref mut p) = file_prompt {
                                    p.pop();
                                }
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if let Some(ref mut p) = file_prompt {
                                    p.push(c);
                                }
                            }
                            _ => {}
                        }
                        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                        continue;
                    }

                    // Key mapping panel captures all input while open.
                    if let Some(kp) = keymapping_panel.as_mut() {
                        match kp.handle_key(key.code, key.modifiers) {
                            KeymappingPanelAction::Close(updated) => {
                                // Persist the updated profile and propagate to all sessions.
                                let save_result = profile_store.save(&updated);
                                // Update all sessions using the Default profile.
                                for tab in &mut tabs {
                                    if tab.entry.key_profile.is_none() {
                                        tab.key_profile = updated.clone();
                                    }
                                }
                                if let Err(err) = save_result {
                                    if let Some(session) = tabs.get_mut(active) {
                                        session.local_message =
                                            Some(format!("Key profile save failed: {err}"));
                                    }
                                }
                                global_key_profile = updated;
                                keymapping_panel = None;
                            }
                            KeymappingPanelAction::None => {}
                        }
                        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                        continue;
                    }

                    // Script panel captures all input while open.
                    if let Some(sp) = script_panel.as_mut() {
                        match sp.handle_key(key.code, key.modifiers) {
                            ScriptPanelAction::Close => {
                                script_panel = None;
                            }
                            ScriptPanelAction::None => {}
                        }
                        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                        continue;
                    }

                    // Log viewer captures all input while open.
                    if let Some(lv) = tabs.get_mut(active).and_then(|s| s.log_viewer.as_mut()) {
                        match lv.handle_key(key.code, key.modifiers) {
                            LogViewerAction::Close => {
                                if let Some(s) = tabs.get_mut(active) {
                                    s.log_viewer = None;
                                }
                            }
                            LogViewerAction::None => {}
                        }
                        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                        continue;
                    }

                    // Credential panel captures all input while open.
                    if let Some(cp) = credential_panel.as_mut() {
                        let action = cp.handle_key(key.code, key.modifiers);
                        match action {
                            CredentialPanelAction::Close => {
                                credential_panel = None;
                                credential_selects_directory_form = false;
                            }
                            CredentialPanelAction::Select(summary) => {
                                if credential_selects_directory_form {
                                    if let Some(panel) = dir_panel.as_mut() {
                                        panel.set_form_credential_id(summary.id);
                                    }
                                    credential_panel = None;
                                    credential_selects_directory_form = false;
                                }
                            }
                            CredentialPanelAction::Store(cred) => {
                                let id = cred.id;
                                let name = cred.name.clone();
                                let kind = cred.kind.clone();
                                let mut guard = credential_manager.lock().await;
                                if let Some(mgr) = guard.as_mut() {
                                    match mgr.store(&cred).await {
                                        Ok(_) => {
                                            match mgr.list().await {
                                                Ok(items) => cp.replace_items(items),
                                                Err(err) => cp.show_error(format!(
                                                    "Credential saved, but refresh failed: {err}"
                                                )),
                                            }
                                            if credential_selects_directory_form {
                                                if let Some(panel) = dir_panel.as_mut() {
                                                    panel.set_form_credential_id(id);
                                                }
                                                if kind != CredentialKind::SshKey {
                                                    credential_panel = None;
                                                    credential_selects_directory_form = false;
                                                }
                                            } else if kind != CredentialKind::SshKey {
                                                cp.show_credential_id(name, kind, id);
                                            } else {
                                                cp.show_status("SSH key credential saved.");
                                            }
                                        }
                                        Err(err) => {
                                            cp.show_error(format!(
                                                "Unable to save credential: {err}"
                                            ));
                                        }
                                    }
                                } else {
                                    cp.show_error("Credential store is unavailable.");
                                }
                            }
                            CredentialPanelAction::Delete(id) => {
                                let mut guard = credential_manager.lock().await;
                                if let Some(mgr) = guard.as_mut() {
                                    match mgr.delete(id).await {
                                        Ok(()) => match mgr.list().await {
                                            Ok(items) => {
                                                cp.replace_items(items);
                                                cp.show_status("Credential deleted.");
                                            }
                                            Err(err) => cp.show_error(format!(
                                                "Credential deleted, but refresh failed: {err}"
                                            )),
                                        },
                                        Err(err) => {
                                            cp.show_error(format!(
                                                "Unable to delete credential: {err}"
                                            ));
                                        }
                                    }
                                } else {
                                    cp.show_error("Credential store is unavailable.");
                                }
                            }
                            CredentialPanelAction::ViewPublicKey(id) => {
                                let guard = credential_manager.lock().await;
                                if let Some(mgr) = guard.as_ref() {
                                    match mgr.retrieve(id).await {
                                        Ok(cred) if cred.kind == CredentialKind::SshKey => {
                                            match public_key_from_private_openssh(
                                                cred.secret.expose(),
                                                "",
                                            ) {
                                                Ok(pubkey) => {
                                                    cp.show_public_key(cred.name, cred.id, pubkey);
                                                }
                                                Err(e) => cp.show_error(format!(
                                                    "Unable to derive public key: {e}"
                                                )),
                                            }
                                        }
                                        Ok(_) => {
                                            cp.show_error(
                                                "Only SSH key credentials have public keys.",
                                            );
                                        }
                                        Err(e) => {
                                            cp.show_error(format!(
                                                "Unable to retrieve credential: {e}"
                                            ));
                                        }
                                    }
                                }
                            }
                            CredentialPanelAction::None => {}
                        }
                        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                        continue;
                    }

                    if let Some(panel) = dir_panel.as_mut() {
                        match panel.handle_key(key.code, key.modifiers) {
                            PanelAction::Connect(entry) => {
                                let protocol_str = entry.protocol.to_string();
                                let host = entry.connection.host.clone();
                                let entry_id = entry.id;
                                match spawn_session(*entry, cols, term_rows, Arc::clone(&engine))
                                    .await
                                {
                                    Ok(mut session) => {
                                        let mut startup_warnings = Vec::new();
                                        // Record session start in history DB.
                                        let hist = Arc::clone(&history_db);
                                        let h = hist
                                            .begin_session(
                                                entry_id,
                                                &protocol_str,
                                                Some(host.as_str()),
                                            )
                                            .await;
                                        match h {
                                            Ok(id) => {
                                                session.history_session_id = Some(id);
                                            }
                                            Err(err) => {
                                                startup_warnings
                                                    .push(format!("History start failed: {err}"));
                                            }
                                        }
                                        match take_directory_from_panel(&mut dir_panel) {
                                            Ok(mut directory) => {
                                                if directory.mark_connected(entry_id, Utc::now()) {
                                                    if let Err(err) = directory.save() {
                                                        startup_warnings.push(format!(
                                                            "Directory update not saved: {err}"
                                                        ));
                                                    }
                                                }
                                                directory_store = Some(directory);
                                            }
                                            Err(err) => {
                                                startup_warnings.push(err);
                                            }
                                        }
                                        if !startup_warnings.is_empty() {
                                            session.local_message =
                                                Some(startup_warnings.join("; "));
                                        }
                                        tabs.push(session);
                                        active = tabs.len() - 1;
                                    }
                                    Err(e) => {
                                        // Record failed connection attempt.
                                        let history_result = history_db
                                            .record_connection(entry_id, Some(0), OUTCOME_ERROR)
                                            .await;
                                        match history_result {
                                            Ok(()) => panel.show_error(e),
                                            Err(err) => panel.show_error(format!(
                                                "{e}; failed to record attempt: {err}"
                                            )),
                                        }
                                    }
                                }
                            }
                            PanelAction::Quit => {
                                if tabs.is_empty() {
                                    break; // quit with no sessions
                                }
                                // ESC/F10 from directory with active sessions → back to sessions
                                match take_directory_from_panel(&mut dir_panel) {
                                    Ok(directory) => directory_store = Some(directory),
                                    Err(err) => {
                                        if let Some(session) = tabs.get_mut(active) {
                                            session.local_message = Some(err);
                                        } else {
                                            eprintln!("[directory] {err}");
                                        }
                                    }
                                }
                            }
                            PanelAction::OpenCredentials => {
                                credential_selects_directory_form = panel.is_entry_form_open();
                                credential_panel = Some(
                                    open_credential_panel(
                                        &credential_manager,
                                        credential_selects_directory_form,
                                    )
                                    .await,
                                );
                            }
                            PanelAction::OpenScripts(entry_context) => {
                                let entry_context =
                                    entry_context.map(|(name, credential_attached)| {
                                        EntryScriptContext {
                                            name,
                                            credential_attached,
                                        }
                                    });
                                script_panel = Some(ScriptPanel::new(
                                    &ScriptStore::new_default(),
                                    entry_context,
                                ));
                            }
                            PanelAction::None => {}
                        }
                    } else {
                        // Emergency exit always works.
                        if ctrl && key.code == KeyCode::Char('c') {
                            break;
                        }

                        // Core transfer keys should work even if a saved key profile
                        // is missing or stale.
                        if is_send_file_shortcut(&key) {
                            execute_app_command(
                                AppCommand::SendFile,
                                &mut tabs,
                                &mut active,
                                &mut dir_panel,
                                &mut directory_store,
                                &mut script_panel,
                                &mut keymapping_panel,
                                &mut file_prompt,
                                &global_key_profile,
                            );
                            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                            continue;
                        }

                        if let KeyCode::F(7) = key.code {
                            execute_app_command(
                                AppCommand::ReceiveFile,
                                &mut tabs,
                                &mut active,
                                &mut dir_panel,
                                &mut directory_store,
                                &mut script_panel,
                                &mut keymapping_panel,
                                &mut file_prompt,
                                &global_key_profile,
                            );
                            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                            continue;
                        }

                        // Look up the key in the active session's profile.
                        let keyspec = crossterm_to_keyspec(key.code, key.modifiers);
                        let profile_action = keyspec
                            .as_ref()
                            .and_then(|ks| {
                                tabs.get(active).map(|s| s.key_profile.lookup(ks).cloned())
                            })
                            .flatten();

                        let mut handled = false;

                        if let Some(action) = profile_action {
                            match action {
                                KeyAction::AppCommand(AppCommand::CloseTab) => {
                                    handled = true;
                                    // Record history before the tab is dropped.
                                    if let Some(session) = tabs.get_mut(active) {
                                        let outcome = match &session.status {
                                            ConnectionStatus::Error(_) => OUTCOME_ERROR,
                                            _ => OUTCOME_CONNECTED,
                                        };
                                        record_session_end(&history_db, session, outcome);
                                    }
                                    execute_app_command(
                                        AppCommand::CloseTab,
                                        &mut tabs,
                                        &mut active,
                                        &mut dir_panel,
                                        &mut directory_store,
                                        &mut script_panel,
                                        &mut keymapping_panel,
                                        &mut file_prompt,
                                        &global_key_profile,
                                    );
                                }
                                KeyAction::AppCommand(AppCommand::OpenCredentials) => {
                                    handled = true;
                                    credential_selects_directory_form = false;
                                    credential_panel = Some(
                                        open_credential_panel(&credential_manager, false).await,
                                    );
                                }
                                KeyAction::AppCommand(AppCommand::Quit) => {
                                    break;
                                }
                                KeyAction::AppCommand(cmd) => {
                                    handled = true;
                                    execute_app_command(
                                        cmd,
                                        &mut tabs,
                                        &mut active,
                                        &mut dir_panel,
                                        &mut directory_store,
                                        &mut script_panel,
                                        &mut keymapping_panel,
                                        &mut file_prompt,
                                        &global_key_profile,
                                    );
                                }
                                KeyAction::SendText(text) => {
                                    handled = true;
                                    if let Some(session) = tabs.get_mut(active) {
                                        queue_session_write(session, text.into_bytes());
                                    }
                                }
                                KeyAction::SendBytes(bytes) => {
                                    handled = true;
                                    if let Some(session) = tabs.get_mut(active) {
                                        queue_session_write(session, bytes);
                                    }
                                }
                                KeyAction::RunScript(name) => {
                                    handled = true;
                                    if let Some(session) = tabs.get_mut(active) {
                                        run_named_script_for_session(
                                            session,
                                            Arc::clone(&engine),
                                            name,
                                        );
                                    }
                                }
                                KeyAction::Passthrough => {}
                            }
                        }

                        if !handled {
                            let app_cursor = tabs
                                .get(active)
                                .map(|s| s.emulator.app_cursor_keys())
                                .unwrap_or(false);
                            let bytes = encode_key(key.code, key.modifiers, app_cursor);
                            if !bytes.is_empty() {
                                if let Some(session) = tabs.get_mut(active) {
                                    queue_session_write(session, bytes);
                                }
                            }
                        }
                    }
                }

                Event::Resize(w, h) => {
                    cols = w;
                    term_rows = h.saturating_sub(3).max(1);
                    for session in &mut tabs {
                        let emulation = EmulationMode::parse(&session.entry.terminal.emulation);
                        let session_cols = emulation.canvas_cols(w);
                        session.emulator.resize(session_cols, term_rows);
                        queue_session_resize(session, (session_cols, term_rows));
                    }
                }

                _ => {}
            }
        }

        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
    }

    record_open_sessions_on_exit(&history_db, &mut tabs).await;

    Ok(())
}

// ── Dialing directory ─────────────────────────────────────────────────────────

/// Run the interactive dialing directory.
///
/// Returns `Ok(Some(entry))` if the user selected an entry to connect to,
/// or `Ok(None)` if they quit.
#[allow(dead_code)]
pub fn run_directory(
    mut terminal: ratatui::DefaultTerminal,
    directory: Directory,
) -> anyhow::Result<Option<DirectoryEntry>> {
    let mut panel = DirectoryPanel::new(directory);

    loop {
        terminal.draw(|f| {
            panel.render(f, f.area());
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match panel.handle_key(key.code, key.modifiers) {
                        PanelAction::Connect(entry) => return Ok(Some(*entry)),
                        PanelAction::Quit => return Ok(None),
                        PanelAction::OpenCredentials => {}
                        PanelAction::OpenScripts(_) => {}
                        PanelAction::None => {}
                    }
                }
            }
        }
    }
}

// ── Connected session ─────────────────────────────────────────────────────────

/// Run the full TUI for an active `connection` described by `entry`.
///
/// Returns when the user quits (Ctrl+Q / Ctrl+C) or the connection drops.
pub async fn run_session(
    mut terminal: DefaultTerminal,
    mut connection: Box<dyn Connection>,
    entry: DirectoryEntry,
) -> anyhow::Result<()> {
    let (cols, rows) = crossterm::terminal::size().context("query terminal size")?;
    let term_rows = rows.saturating_sub(3).max(1);
    let emulation = EmulationMode::parse(&entry.terminal.emulation);
    let term_cols = emulation.canvas_cols(cols);
    let mut emulator = TerminalEmulator::with_emulation(term_cols, term_rows, emulation);
    let mut log = SessionLog::with_default_path(&entry.name).context("open session log")?;
    let mut raw_capture = open_raw_capture(&entry)?;
    let mut status = ConnectionStatus::Connected;
    let mut error_msg: Option<String> = None;

    loop {
        // ── Render ──────────────────────────────────────────────────────────
        let screen = emulator.screen();
        let proto_str = entry.protocol.to_string();
        let host = entry.connection.host.clone();
        let status_clone = status.clone();
        let err_clone = error_msg.clone();

        terminal.draw(move |f| {
            let area = f.area();
            // Layout: [title=1][terminal=*][status=1][fkeys=1]
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(area);

            // Title bar
            let title = format!(" Waystone Comm v{} ", env!("CARGO_PKG_VERSION"));
            f.render_widget(
                Paragraph::new(title).style(Style::default().bg(Color::Cyan).fg(Color::Black)),
                chunks[0],
            );

            // Terminal pane
            render_terminal_pane(f, chunks[1], &screen);

            // Status bar
            render_status_bar(f, chunks[2], &proto_str, &host, &status_clone, None, None);

            // F-key bar (or error overlay)
            if let Some(ref msg) = err_clone {
                f.render_widget(
                    Paragraph::new(format!(" Error: {msg}"))
                        .style(Style::default().bg(Color::Red).fg(Color::White)),
                    chunks[3],
                );
            } else {
                let labels = KeyProfile::default_profile().fkey_bar_labels();
                render_fkey_bar(f, chunks[3], &labels);
            }
        })?;

        // ── Input ────────────────────────────────────────────────────────────
        if event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {
                        let bytes = encode_key(key.code, key.modifiers, emulator.app_cursor_keys());
                        if !bytes.is_empty() {
                            if let Err(err) = connection.write(&bytes).await {
                                let msg = format!("write failed: {err}");
                                error_msg = Some(msg.clone());
                                status = ConnectionStatus::Error(msg);
                            }
                        }
                    }
                },
                Event::Resize(w, h) => {
                    let term_rows = h.saturating_sub(3).max(1);
                    let term_cols = emulation.canvas_cols(w);
                    emulator.resize(term_cols, term_rows);
                    if let Err(err) = connection.resize(term_cols, term_rows).await {
                        let msg = format!("resize failed: {err}");
                        error_msg = Some(msg.clone());
                        status = ConnectionStatus::Error(msg);
                    }
                }
                _ => {}
            }
        }

        // ── Read from connection ──────────────────────────────────────────────
        match tokio::time::timeout(Duration::from_millis(READ_TIMEOUT_MS), connection.read()).await
        {
            Ok(Ok(data)) => {
                let mut io_warning = None;
                if let Some(capture) = raw_capture.as_mut() {
                    if let Err(err) = capture.write_all(&data) {
                        io_warning = Some(format!("Raw capture write failed: {err}"));
                    } else if let Err(err) = capture.flush() {
                        io_warning = Some(format!("Raw capture flush failed: {err}"));
                    }
                }
                if let Err(err) = log.write_bytes(&data) {
                    io_warning = Some(format!("Session log write failed: {err}"));
                }
                emulator.process(&data);
                let output = emulator.take_output();
                if !output.is_empty() {
                    if let Err(err) = connection.write(&output).await {
                        let msg = format!("terminal response write failed: {err}");
                        io_warning = Some(msg.clone());
                        status = ConnectionStatus::Error(msg);
                    }
                }
                error_msg = io_warning;
            }
            Ok(Err(e)) => {
                // Connection error — show once then exit.
                let msg = e.to_string();
                error_msg = Some(msg.clone());
                status = ConnectionStatus::Error(msg);
                // One final render to show the error banner.
                let screen = emulator.screen();
                let proto_str = entry.protocol.to_string();
                let host = entry.connection.host.clone();
                let st = status.clone();
                let em = error_msg.clone();
                terminal.draw(move |f| {
                    let area = f.area();
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(1),
                            Constraint::Min(1),
                            Constraint::Length(1),
                            Constraint::Length(1),
                        ])
                        .split(area);
                    f.render_widget(
                        Paragraph::new(format!(" Waystone Comm v{} ", env!("CARGO_PKG_VERSION")))
                            .style(Style::default().bg(Color::Cyan).fg(Color::Black)),
                        chunks[0],
                    );
                    render_terminal_pane(f, chunks[1], &screen);
                    render_status_bar(f, chunks[2], &proto_str, &host, &st, None, None);
                    if let Some(ref m) = em {
                        f.render_widget(
                            Paragraph::new(format!(" Error: {m}"))
                                .style(Style::default().bg(Color::Red).fg(Color::White)),
                            chunks[3],
                        );
                    }
                })?;
                tokio::time::sleep(Duration::from_millis(2000)).await;
                break;
            }
            Err(_) => {
                // Timeout — no data yet, continue render loop.
                tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
            }
        }
    }

    Ok(())
}

// ── Key encoding ──────────────────────────────────────────────────────────────

fn open_raw_capture(entry: &DirectoryEntry) -> anyhow::Result<Option<std::fs::File>> {
    let Some(path) = entry.connection.extra.get("raw_capture_path") else {
        return Ok(None);
    };

    let path = expand_home(path);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open raw capture file {path}"))?;
    Ok(Some(file))
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

fn append_key_debug(key: &KeyEvent) {
    let Ok(path) = std::env::var("WAYSTONE_COMM_KEY_DEBUG") else {
        return;
    };
    let path = expand_home(&path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        writeln!(file, "{key:?}").ok();
    }
}

fn is_send_file_shortcut(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::F(6) => true,
        KeyCode::Char('u' | 'U') => key.modifiers.contains(KeyModifiers::ALT),
        _ => false,
    }
}

/// Encode a crossterm `KeyCode` + `KeyModifiers` into the byte sequence that
/// an xterm-256color terminal would send for that key.
fn encode_key(code: KeyCode, modifiers: KeyModifiers, app_cursor: bool) -> Vec<u8> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    match code {
        KeyCode::Char(c) => {
            if ctrl {
                // Ctrl+A..Z → 0x01..0x1A
                let upper = c.to_ascii_uppercase();
                if upper.is_ascii_alphabetic() {
                    return vec![(upper as u8) - b'A' + 1];
                }
                match c {
                    ' ' => return vec![0x00],
                    '[' => return vec![0x1B],
                    '\\' => return vec![0x1C],
                    ']' => return vec![0x1D],
                    '^' => return vec![0x1E],
                    '_' => return vec![0x1F],
                    _ => {}
                }
            }
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf);
            buf[..c.len_utf8()].to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x08],
        KeyCode::Delete => vec![0x7F],
        KeyCode::Esc => vec![0x1B],
        // Application cursor keys mode (DECCKM): CSI → SS3
        KeyCode::Up => {
            if app_cursor {
                b"\x1bOA".to_vec()
            } else {
                b"\x1b[A".to_vec()
            }
        }
        KeyCode::Down => {
            if app_cursor {
                b"\x1bOB".to_vec()
            } else {
                b"\x1b[B".to_vec()
            }
        }
        KeyCode::Right => {
            if app_cursor {
                b"\x1bOC".to_vec()
            } else {
                b"\x1b[C".to_vec()
            }
        }
        KeyCode::Left => {
            if app_cursor {
                b"\x1bOD".to_vec()
            } else {
                b"\x1b[D".to_vec()
            }
        }
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        KeyCode::F(5) => b"\x1b[15~".to_vec(),
        KeyCode::F(6) => b"\x1b[17~".to_vec(),
        KeyCode::F(7) => b"\x1b[18~".to_vec(),
        KeyCode::F(8) => b"\x1b[19~".to_vec(),
        KeyCode::F(9) => b"\x1b[20~".to_vec(),
        KeyCode::F(10) => b"\x1b[21~".to_vec(),
        KeyCode::F(11) => b"\x1b[23~".to_vec(),
        KeyCode::F(12) => b"\x1b[24~".to_vec(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedConnection {
        reads: VecDeque<Vec<u8>>,
        disconnect_count: Arc<std::sync::atomic::AtomicUsize>,
        block_when_empty: bool,
    }

    impl ScriptedConnection {
        fn new(reads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                reads: reads.into_iter().collect(),
                disconnect_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                block_when_empty: false,
            }
        }

        fn blocking_with_disconnect_count(
            disconnect_count: Arc<std::sync::atomic::AtomicUsize>,
        ) -> Self {
            Self {
                reads: VecDeque::new(),
                disconnect_count,
                block_when_empty: true,
            }
        }
    }

    fn poison_transfer_tap(transfer_tap: &TransferTap) {
        let transfer_tap = Arc::clone(transfer_tap);
        let _ = std::panic::catch_unwind(move || {
            let _guard = transfer_tap.lock().unwrap();
            panic!("poison transfer tap");
        });
    }

    #[async_trait::async_trait]
    impl Connection for ScriptedConnection {
        async fn connect(
            &mut self,
            _entry: &DirectoryEntry,
        ) -> waystone_comm_core::connection::Result<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> waystone_comm_core::connection::Result<()> {
            self.disconnect_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        async fn read(&mut self) -> waystone_comm_core::connection::Result<Vec<u8>> {
            if let Some(data) = self.reads.pop_front() {
                Ok(data)
            } else if self.block_when_empty {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Err(ConnectionError::Disconnected("test complete".into()))
            } else {
                Err(ConnectionError::Disconnected("test complete".into()))
            }
        }

        async fn write(&mut self, _data: &[u8]) -> waystone_comm_core::connection::Result<()> {
            Ok(())
        }

        fn protocol(&self) -> Protocol {
            Protocol::Raw
        }

        fn status(&self) -> ConnectionStatus {
            ConnectionStatus::Connected
        }

        fn supports_file_transfer(&self) -> bool {
            true
        }
    }

    #[test]
    fn backspace_sends_ctrl_h_for_bbs_editors() {
        assert_eq!(
            encode_key(KeyCode::Backspace, KeyModifiers::empty(), false),
            vec![0x08]
        );
    }

    #[test]
    fn delete_sends_del_for_bbs_editors() {
        assert_eq!(
            encode_key(KeyCode::Delete, KeyModifiers::empty(), false),
            vec![0x7F]
        );
    }

    #[test]
    fn password_credential_values_are_exposed_to_scripts() {
        let credential = Credential::new(
            "bbs-login",
            CredentialKind::Password,
            Some("calvusrex".into()),
            "swordfish",
        );

        let values = script_values_for_credential(credential);

        assert_eq!(
            values.get("username").map(String::as_str),
            Some("calvusrex")
        );
        assert_eq!(values.get("user").map(String::as_str), Some("calvusrex"));
        assert_eq!(
            values.get("password").map(String::as_str),
            Some("swordfish")
        );
        assert_eq!(values.get("secret").map(String::as_str), Some("swordfish"));
        assert_eq!(values.get("kind").map(String::as_str), Some("Password"));
    }

    #[test]
    fn token_and_ssh_key_credentials_use_specific_script_aliases() {
        let token = Credential::new("api-token", CredentialKind::Token, None, "tok123");
        let token_values = script_values_for_credential(token);
        assert_eq!(
            token_values.get("token").map(String::as_str),
            Some("tok123")
        );
        assert!(!token_values.contains_key("password"));

        let ssh_key = Credential::new("door-key", CredentialKind::SshKey, None, "PRIVATE");
        let key_values = script_values_for_credential(ssh_key);
        assert_eq!(key_values.get("key").map(String::as_str), Some("PRIVATE"));
        assert_eq!(
            key_values.get("private_key").map(String::as_str),
            Some("PRIVATE")
        );
        assert!(!key_values.contains_key("password"));
    }

    #[test]
    fn script_log_redacts_password_values_but_keeps_username() {
        let credential = Credential::new(
            "bbs-login",
            CredentialKind::Password,
            Some("calvusrex".into()),
            "swordfish",
        );
        let values = script_values_for_credential(credential);

        let redacted = redact_script_log_message(
            "user calvusrex password swordfish secret swordfish",
            &values,
        );

        assert_eq!(redacted, "user calvusrex password *** secret ***");
    }

    #[test]
    fn script_log_redacts_token_and_private_key_values() {
        let token = Credential::new("api-token", CredentialKind::Token, None, "tok123");
        let token_values = script_values_for_credential(token);
        assert_eq!(
            redact_script_log_message("token=tok123 raw tok123", &token_values),
            "token=*** raw ***"
        );

        let key = Credential::new("ssh-key", CredentialKind::SshKey, None, "PRIVATEKEY");
        let key_values = script_values_for_credential(key);
        assert_eq!(
            redact_script_log_message("key PRIVATEKEY private_key PRIVATEKEY", &key_values),
            "key *** private_key ***"
        );
    }

    #[test]
    fn missing_credential_warning_is_human_readable() {
        let script_log = Arc::new(std::sync::Mutex::new(Vec::new()));

        append_script_log(
            &script_log,
            "[credential] no credential attached; s.credential(...) returns empty strings".into(),
        );

        let log = script_log.lock().unwrap();
        assert_eq!(
            log[0],
            "[credential] no credential attached; s.credential(...) returns empty strings"
        );
    }

    #[test]
    fn script_status_reports_prompt_timeouts() {
        assert_eq!(
            script_local_status("alias prompt not seen"),
            Some((2, "Script warning: alias prompt not seen".into()))
        );
        assert_eq!(
            script_local_status("on_connect failed: boom"),
            Some((3, "Script on_connect failed: boom".into()))
        );
        assert_eq!(
            script_local_status("[script compile error] bad token"),
            Some((3, "Script compile error: bad token".into()))
        );
    }

    #[test]
    fn script_status_keeps_warning_over_completion() {
        let mut status = None;
        merge_script_status(
            &mut status,
            script_local_status("alias prompt not seen").unwrap(),
        );
        merge_script_status(
            &mut status,
            script_local_status("on_connect completed").unwrap(),
        );

        assert_eq!(
            status,
            Some((2, "Script warning: alias prompt not seen".into()))
        );
    }

    #[test]
    fn send_file_shortcut_accepts_f6_and_alt_u() {
        assert!(is_send_file_shortcut(&KeyEvent::new(
            KeyCode::F(6),
            KeyModifiers::empty()
        )));
        assert!(is_send_file_shortcut(&KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::ALT
        )));
        assert!(!is_send_file_shortcut(&KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::empty()
        )));
    }

    #[test]
    fn append_script_log_ignores_poisoned_buffer() {
        let script_log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let poisoned = Arc::clone(&script_log);
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison script log");
        });

        append_script_log(&script_log, "message".to_string());
    }

    #[test]
    fn take_directory_from_panel_reports_missing_panel() {
        let mut dir_panel = None;

        match take_directory_from_panel(&mut dir_panel) {
            Ok(_) => panic!("expected missing directory panel error"),
            Err(err) => assert_eq!(err, "Directory state unavailable."),
        }
    }

    #[test]
    fn take_directory_from_panel_returns_owned_directory() {
        let path = std::env::temp_dir().join(format!("waystone-comm-test-{}.toml", Uuid::new_v4()));
        let mut directory = Directory::load(path).unwrap();
        let entry = DirectoryEntry::new("Test", Protocol::Telnet, "example.com");
        let entry_id = entry.id;
        directory.add_entry(entry);
        let mut dir_panel = Some(DirectoryPanel::new(directory));

        let directory = take_directory_from_panel(&mut dir_panel).unwrap();

        assert!(dir_panel.is_none());
        assert!(directory.get_entry(entry_id).is_some());
    }

    #[tokio::test]
    async fn session_io_task_errors_when_transfer_tap_is_full() {
        let (_write_tx, write_rx) = mpsc::channel(1);
        let (_resize_tx, resize_rx) = mpsc::channel(1);
        let (_script_control_tx, script_control_rx) = mpsc::channel(1);
        let (read_tx, mut read_rx) = mpsc::channel(4);
        let (tap_tx, tap_rx) = std::sync::mpsc::sync_channel(1);
        tap_tx.try_send(vec![b'x']).unwrap();
        let transfer_tap = Arc::new(std::sync::Mutex::new(Some(tap_tx)));

        let task = tokio::spawn(session_io_task(
            Box::new(ScriptedConnection::new([b"payload".to_vec()])),
            write_rx,
            resize_rx,
            script_control_rx,
            read_tx,
            None,
            transfer_tap,
        ));

        match read_rx.recv().await {
            Some(SessionMsg::Error(message)) => {
                assert!(message.contains("transfer data buffer full"));
            }
            other => panic!("expected transfer tap error, got {other:?}"),
        }

        task.await.unwrap();
        drop(tap_rx);
    }

    #[tokio::test]
    async fn session_io_task_errors_when_transfer_tap_lock_is_poisoned() {
        let (_write_tx, write_rx) = mpsc::channel(1);
        let (_resize_tx, resize_rx) = mpsc::channel(1);
        let (_script_control_tx, script_control_rx) = mpsc::channel(1);
        let (read_tx, mut read_rx) = mpsc::channel(4);
        let transfer_tap = Arc::new(std::sync::Mutex::new(None));
        poison_transfer_tap(&transfer_tap);

        let task = tokio::spawn(session_io_task(
            Box::new(ScriptedConnection::new([b"payload".to_vec()])),
            write_rx,
            resize_rx,
            script_control_rx,
            read_tx,
            None,
            transfer_tap,
        ));

        match read_rx.recv().await {
            Some(SessionMsg::Error(message)) => {
                assert!(message.contains("transfer state unavailable"));
            }
            other => panic!("expected transfer state error, got {other:?}"),
        }

        task.await.unwrap();
    }

    #[tokio::test]
    async fn session_io_task_disconnects_on_script_command() {
        let (_write_tx, write_rx) = mpsc::channel(1);
        let (_resize_tx, resize_rx) = mpsc::channel(1);
        let (script_control_tx, script_control_rx) = mpsc::channel(1);
        let (read_tx, mut read_rx) = mpsc::channel(4);
        let disconnect_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let task = tokio::spawn(session_io_task(
            Box::new(ScriptedConnection::blocking_with_disconnect_count(
                Arc::clone(&disconnect_count),
            )),
            write_rx,
            resize_rx,
            script_control_rx,
            read_tx,
            None,
            Arc::new(std::sync::Mutex::new(None)),
        ));

        script_control_tx
            .send(ScriptCommand::Disconnect)
            .await
            .unwrap();

        assert!(matches!(
            read_rx.recv().await,
            Some(SessionMsg::Disconnected)
        ));
        task.await.unwrap();
        assert_eq!(
            disconnect_count.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[test]
    fn set_transfer_tap_reports_poisoned_lock() {
        let transfer_tap = Arc::new(std::sync::Mutex::new(None));
        poison_transfer_tap(&transfer_tap);

        assert_eq!(
            set_transfer_tap(&transfer_tap, None),
            Err("transfer state unavailable")
        );
    }

    #[tokio::test]
    async fn session_io_task_warns_when_script_data_channel_is_full() {
        let (_write_tx, write_rx) = mpsc::channel(1);
        let (_resize_tx, resize_rx) = mpsc::channel(1);
        let (_script_control_tx, script_control_rx) = mpsc::channel(1);
        let (read_tx, mut read_rx) = mpsc::channel(4);
        let (script_tx, script_rx) = std::sync::mpsc::sync_channel(1);
        script_tx.try_send(vec![b'x']).unwrap();

        let task = tokio::spawn(session_io_task(
            Box::new(ScriptedConnection::new([b"payload".to_vec()])),
            write_rx,
            resize_rx,
            script_control_rx,
            read_tx,
            Some(script_tx),
            Arc::new(std::sync::Mutex::new(None)),
        ));

        match read_rx.recv().await {
            Some(SessionMsg::Warning(message)) => {
                assert!(message.contains("script data buffer full"));
            }
            other => panic!("expected script data warning, got {other:?}"),
        }
        match read_rx.recv().await {
            Some(SessionMsg::Data(data)) => assert_eq!(data, b"payload"),
            other => panic!("expected original data after warning, got {other:?}"),
        }

        task.await.unwrap();
        drop(script_rx);
    }

    #[tokio::test]
    async fn session_io_task_ignores_closed_script_data_channel() {
        let (_write_tx, write_rx) = mpsc::channel(1);
        let (_resize_tx, resize_rx) = mpsc::channel(1);
        let (_script_control_tx, script_control_rx) = mpsc::channel(1);
        let (read_tx, mut read_rx) = mpsc::channel(4);
        let (script_tx, script_rx) = std::sync::mpsc::sync_channel(1);
        drop(script_rx);

        let task = tokio::spawn(session_io_task(
            Box::new(ScriptedConnection::new([b"payload".to_vec()])),
            write_rx,
            resize_rx,
            script_control_rx,
            read_tx,
            Some(script_tx),
            Arc::new(std::sync::Mutex::new(None)),
        ));

        match read_rx.recv().await {
            Some(SessionMsg::Data(data)) => assert_eq!(data, b"payload"),
            other => panic!("expected original data without warning, got {other:?}"),
        }

        task.await.unwrap();
    }
}
