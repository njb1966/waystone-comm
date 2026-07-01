# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

---

## Build & Development Commands

```bash
# Build entire workspace
cargo build --workspace

# Run the TUI binary
cargo run --bin waystone-comm

# Run with arguments
cargo run --bin waystone-comm -- connect ssh user@host
cargo run --bin waystone-comm -- connect telnet host:23
cargo run --bin waystone-comm -- replay /tmp/waystone-comm-deadparrot.raw --emulation xterm-256color
cargo run --bin waystone-comm -- list

# Test entire workspace
cargo test --workspace

# Test a single test by name (in a specific crate)
cargo test -p waystone-comm-core test_name

# Lint (must pass before commit — zero warnings allowed)
cargo clippy -- -D warnings

# Format (must run before commit)
cargo fmt

# Performance benchmarks
cargo bench
```

---

## Waystone Comm AI Build Guide
### Instructions for Claude (or any AI assistant) working on this codebase

---

> This file tells an AI assistant how to systematically build Waystone Comm by
> referencing the appropriate sections of `MASTERPLAN.md`. Read this file
> first. Read the referenced MASTERPLAN sections before writing any code.
> Work one phase at a time. Never skip ahead.

---

## How to Use This File

When a user asks you to work on Waystone Comm, follow this workflow:

1. **Read this file** in full before doing anything
2. **Identify the current phase** (ask the user if unsure — check `PHASE_STATUS` below)
3. **Read the referenced MASTERPLAN sections** for that phase
4. **Implement exactly what is specified** — no more, no less per session
5. **Update `PHASE_STATUS`** when a deliverable is complete
6. **Keep docs current** when behavior, verified status, or supported commands change. Update
   the current-status section of `MASTERPLAN.md` when project direction or verified scope changes.

---

## PHASE_STATUS
> Update this section as work is completed. Use ✅ Done | 🔄 In Progress | ⬜ Not Started

### Phase 1 — Core Engine (Target: v0.1.0)
| Deliverable | Status | Notes |
|---|---|---|
| Cargo workspace scaffold | ✅ | Two-crate workspace; all Phase 1 deps wired |
| `Connection` trait + session manager | ✅ | Trait, SessionManager, broadcast event bus |
| SSH v2 implementation | ✅ | russh; TOFU known_hosts; Ed25519/RSA/password auth |
| Telnet implementation | ✅ | RFC 854; SUPPRESS-GO-AHEAD, ECHO, NAWS, TERMINAL-TYPE |
| Serial port implementation | ✅ | serialport crate; spawn_blocking bridge; flow/parity |
| Raw TCP implementation | ✅ | TcpStream wrapper; TCP_NODELAY |
| VT100/ANSI terminal emulator (basic) | ✅ | vte crate; SGR, cursor/erase/scroll/wrap coverage |
| Ratatui TUI shell (single session) | ✅ | Historical Phase 1 shell; current TUI is tabbed |
| Basic dialing directory (TOML, no UI) | ✅ | Historical Phase 1 storage; current TUI has full directory UI |
| Session logging to file | ✅ | Timestamped; escape-stripped; date-rotating log files |
| `waystone-comm connect` CLI command | ✅ | clap; ssh/telnet/serial/raw subcommands; `list` |

### Phase 2 — ProComm Feature Parity (Target: v0.3.0)
| Deliverable | Status | Notes |
|---|---|---|
| Dialing directory full UI | ✅ | Grouped list, search, sidebar, new/delete, connects via entry |
| Tabbed sessions | ✅ | Background tasks per session; tab bar with ● indicator; Ctrl+T/W, Alt+1-9 |
| ASPECT-style scripting engine (Rhai) | ✅ | ScriptEngine + SessionApi; on_connect/on_data hooks; F3 script viewer |
| Full terminal emulation suite | ✅ | Alt screen ?1049/?47; CP437 ANSI-BBS; UTF-8 ANSI/xterm; ICH/ECH; OSC title; DECCKM; IND |
| File transfer protocols | ✅ | Xmodem/Ymodem/Zmodem core + TUI integration; Zmodem receive live-validated; F6/Alt+U Zmodem send live-validated on Retroboard/WWIV over Telnet and The Bottomless Abyss over SSH |
| Key mapping & macro keys | ✅ | KeyProfile/KeySpec/KeyAction in core; keymapping_panel TUI; profile-based dispatch in app |
| Session logging (full features) | ✅ | LogFormat (text/html/json/raw); size+date rotation; CredentialScrubber; LogViewerPanel TUI (F8) |
| Credential manager | ✅ | AES-256-GCM encrypted SQLite + OS keychain; SSH keypair/password storage; CredentialPanel TUI (F5) |
| Upgrade directory to SQLite | ✅ | history.db: session_logs + connection_history; begin/end_session wiring in app; TOML last_connected migration |
| BBS compatibility recovery | ✅ | Mystic A-Net SSH, Dead Parrot SSH, Retroboard Telnet, and The Bottomless Abyss SSH validated; raw capture replay added |

### Phase 3 — Waystone Browser Integration & Protocol Polish (Target: v0.6.0)
| Deliverable | Status | Notes |
|---|---|---|
| Waystone Browser handoff | ⬜ | Open Gemini, Gopher, Spartan, HTTP, and HTTPS links in Waystone Browser |
| Mosh | ⬜ | |
| Rlogin | ⬜ | |
| IRC client | ⬜ | |
| NNTP client | ⬜ | |
| Finger protocol | ⬜ | |
| WebSocket | ⬜ | |
| SFTP file browser | ⬜ | |

### Phase 4 — AI Integration (Target: v0.8.0)
| Deliverable | Status | Notes |
|---|---|---|
| AI assistant panel (TUI) | ⬜ | |
| Script generation | ⬜ | |
| Smart connect | ⬜ | |
| Log analysis | ⬜ | |
| Session diff | ⬜ | |
| Anomaly detection | ⬜ | |
| Privacy controls | ⬜ | |
| Local AI (Ollama) support | ⬜ | |

### Phase 5 — Polish & Ecosystem (Target: v1.0.0)
| Deliverable | Status | Notes |
|---|---|---|
| Plugin architecture | ⬜ | |
| Theming system | ⬜ | |
| Tauri GUI wrapper | ⬜ | |
| Cross-platform packaging | ⬜ | |
| Documentation site | ⬜ | |
| Man page | ⬜ | |

---

## Implementation Architecture

> This section describes what is **actually built** in the current recovered codebase. Read this
> before working on additional Phase 2+ tasks.

### Data Flow (end-to-end)

```
User keypress
  → crossterm event (waystone-comm-tui/src/app.rs: run_multi_session/run_session)
    → encode_key() → raw bytes
      → Box<dyn Connection>::write()          # active protocol
        → remote host

Remote data
  → Box<dyn Connection>::read() → Vec<u8>
    → SessionManager::notify_data()           # broadcasts SessionEvent::DataReceived
      → TerminalEmulator::process()           # vte parser → TerminalState (vte::Perform)
        → TerminalScreen snapshot
          → render_terminal_pane()            # Ratatui spans with style mapping
            → terminal output
```

### Key Types and Where They Live

| Type | File | Role |
|---|---|---|
| `Connection` trait | `connection/mod.rs` | All protocols implement this |
| `ConnectionStatus`, `Protocol`, `ConnectionError` | `connection/mod.rs` | Shared enums/error type |
| `Session`, `SessionManager`, `SessionEvent` | `connection/session_manager.rs` | Session lifecycle + event bus |
| `TerminalEmulator` | `terminal/emulator.rs` | Entry point: `process(&[u8])`, `screen()`, `resize()` |
| `TerminalState` | `terminal/state.rs` | Implements `vte::Perform`; owns the cell grid |
| `TerminalScreen` | `terminal/screen.rs` | Immutable snapshot for rendering |
| `Cell`, `CellStyle`, `Color` | `terminal/cell.rs` | Leaf types for the cell grid |
| `DirectoryEntry`, `Directory` | `directory/mod.rs` | TOML-backed entry store |
| `SessionLog` | `logging/mod.rs` | Escape-stripped, timestamped file writer |
| `CredentialManager` | `credentials/mod.rs` | Encrypted credential and SSH-key storage |
| `SessionHistoryDb` | `history/mod.rs` | SQLite session and connection history |

### Protocol Extension Points

All four protocols (`ssh.rs`, `telnet.rs`, `serial.rs`, `raw.rs`) implement `Connection`. To add a new protocol:
1. Create `crates/waystone-comm-core/src/protocols/<name>.rs`
2. Implement `Connection` trait (connect/disconnect/read/write/protocol/status/supports_file_transfer)
3. Add variant to `Protocol` enum in `connection/mod.rs`
4. Wire up in `waystone-comm-tui/src/main.rs` `ConnectProtocol` match

### Implemented Phase 2 Modules

These modules are implemented and covered by tests:
- `crates/waystone-comm-core/src/scripting/mod.rs` — Rhai scripting engine and session API
- `crates/waystone-comm-core/src/transfer/` — Xmodem/Ymodem/Zmodem core support
- `crates/waystone-comm-core/src/credentials/mod.rs` — encrypted credential store and SSH key generation
- `crates/waystone-comm-core/src/keymapping/mod.rs` — key profiles and macro dispatch
- `crates/waystone-comm-core/src/history/mod.rs` — SQLite session history

`crates/waystone-comm-core/src/ai/mod.rs` remains reserved for deferred Phase 4 work.

### TUI Render Loop (app.rs)

- 60 fps target (`TICK_MS = 16`)
- Event poll timeout: 10 ms (`READ_TIMEOUT_MS`)
- Single-session layout (top-to-bottom): title bar (1 line) | terminal pane (fills) |
  status bar (1 line) | F-key bar (1 line)
- Multi-session layout adds a tab bar and can open the dialing directory, credential panel,
  script panel, log viewer, and key mapping panel over active sessions.
- `encode_key()` maps `crossterm::KeyCode` to raw byte sequences sent to the active connection
- `run_replay()` replays raw capture bytes through the same emulator and terminal pane without
  opening a network connection.

### Terminal Emulator Architecture

`TerminalEmulator` owns a `vte::Parser` and a `TerminalState`. Bytes fed to `process()` are parsed by the vte crate, which calls methods on `TerminalState` (which implements `vte::Perform`). `TerminalState` maintains the live grid; `screen()` returns a cloned `TerminalScreen` snapshot for rendering.

### Config Paths (runtime)

| Item | Path |
|---|---|
| Dialing directory | `~/.config/waystone-comm/directory.toml` |
| SSH known hosts | `~/.config/waystone-comm/known_hosts` |
| Session logs | `~/.config/waystone-comm/logs/<entry-name>/<YYYY-MM-DD>.log` |
| Session history DB | `~/.config/waystone-comm/history.db` |
| Credential DB | `~/.config/waystone-comm/credentials.db` |
| Machine key (credential encryption) | `~/.config/waystone-comm/machine.key` |
| Key profiles | `~/.config/waystone-comm/keys/<profile-name>.toml` |
| Scripts | `~/.config/waystone-comm/scripts/<entry-name>.rhai` |

---

## Build Instructions by Phase

The phase instructions below are historical implementation scaffolding from the
original plan. Before acting on them, check `PHASE_STATUS`, the current recovery
status in `MASTERPLAN.md`, and the actual code. Do not reintroduce old Phase 1
limitations such as placeholder directory UI, F10 quit, or single-session-only
behavior.

---

### 🔧 PHASE 1 — Core Engine

**MASTERPLAN reference:** §3 (Phase 1), §2 (Architecture), §14 (File Structure), §15 (Dependencies)

**Before writing any code, read:**
- MASTERPLAN §2.1 — High-level component map
- MASTERPLAN §2.2 — Core `Connection` trait definition
- MASTERPLAN §2.3 — Session model
- MASTERPLAN §2.4 — Event system
- MASTERPLAN §14 — File and directory structure
- MASTERPLAN §15.1 — Core dependencies

**Step 1.1 — Scaffold the Cargo workspace**

Create the workspace as defined in MASTERPLAN §14:
```
waystone-comm/
├── Cargo.toml          (workspace)
├── crates/
│   ├── waystone-comm-core/  (library)
│   └── waystone-comm-tui/   (binary)
```

Workspace `Cargo.toml` should define both members and share dependency versions via `[workspace.dependencies]`. Use Rust 2021 edition.

The `waystone-comm-core` crate is a library. `waystone-comm-tui` is the binary that depends on `waystone-comm-core`.

**Step 1.2 — Implement the `Connection` trait**

File: `crates/waystone-comm-core/src/connection/mod.rs`

Use the exact trait signature from MASTERPLAN §2.2. Add:
- `ConnectionStatus` enum: `Disconnected | Connecting | Connected | Error(String)`
- `Protocol` enum listing all protocols from MASTERPLAN §8.1
- `ConnectionError` type (use `thiserror` crate)

Do not implement any concrete protocol yet — just the trait and types.

**Step 1.3 — Session Manager**

File: `crates/waystone-comm-core/src/connection/session_manager.rs`

Implements:
- `SessionManager` struct holding a `HashMap<Uuid, Session>`
- Async methods: `open_session`, `close_session`, `get_session`, `list_sessions`
- Uses Tokio broadcast channel for the event bus (MASTERPLAN §2.4)
- Session struct as defined in MASTERPLAN §2.3

**Step 1.4 — SSH Protocol**

File: `crates/waystone-comm-core/src/protocols/ssh.rs`

Reference: MASTERPLAN §3.2

Use the `russh` crate. Implement:
1. `SshConnection` struct implementing `Connection` trait
2. Auth methods: password, public key (Ed25519 first, RSA second)
3. Parse connection options from `DirectoryEntry` (TOML config format from §3.2)
4. Known hosts file management at `~/.config/waystone-comm/known_hosts`
5. Keepalive via Tokio interval timer

Do NOT implement port forwarding or X11 in Phase 1 — those are Phase 2 stretch goals.

**Step 1.5 — Telnet Protocol**

File: `crates/waystone-comm-core/src/protocols/telnet.rs`

Reference: MASTERPLAN §3.3

Implement RFC 854 + the option negotiations listed in §3.3. Use a state machine for option negotiation. Key detail: Telnet interprets `0xFF` as IAC (Interpret As Command) — all bytes must be scanned.

Required options (implement in this order):
1. SUPPRESS-GO-AHEAD (simplest, always agree)
2. ECHO
3. NAWS (window size — send on connect and on terminal resize)
4. TERMINAL-TYPE (report "xterm-256color")

**Step 1.6 — Serial Protocol**

File: `crates/waystone-comm-core/src/protocols/serial.rs`

Reference: MASTERPLAN §3.4

Use `serialport` crate. Implement:
1. `SerialConnection` struct implementing `Connection` trait
2. Parse all settings from MASTERPLAN §3.4 config example
3. Auto-detect available ports: expose `SerialConnection::available_ports() -> Vec<String>`
4. Run serial I/O in a separate Tokio task (serial is blocking — use `spawn_blocking`)

**Step 1.7 — Raw TCP**

File: `crates/waystone-comm-core/src/protocols/raw.rs`

Reference: MASTERPLAN §3.5

Simple `TcpStream` wrapper. Optional TLS via `rustls`. This is the simplest protocol — implement it to validate the `Connection` trait works end-to-end before more complex protocols.

**Step 1.8 — Terminal Emulator (Basic)**

File: `crates/waystone-comm-core/src/terminal/mod.rs`

Reference: MASTERPLAN §3.6, §4.4 (full list for Phase 2)

Phase 1 scope — implement only:
- A `TerminalEmulator` struct with an internal grid of `Cell { char, style }`
- VT100 escape sequence parser (use a state machine — the `vte` crate is a good parser)
- SGR attributes listed in §3.6
- Cursor movement sequences
- Erase commands
- UTF-8 input handling

The terminal emulator outputs a `TerminalScreen` (grid of styled cells) that the TUI layer renders.

**Step 1.9 — Basic Dialing Directory (TOML only)**

File: `crates/waystone-comm-core/src/directory/mod.rs`

Reference: MASTERPLAN §10.1 (data model)

Phase 1 scope — no UI, just:
1. `DirectoryEntry` struct matching MASTERPLAN §10.1 schema
2. Load/save to `~/.config/waystone-comm/directory.toml`
3. CRUD operations: `add_entry`, `get_entry`, `list_entries`, `delete_entry`
4. No SQLite yet — pure TOML for Phase 1

**Step 1.10 — Session Logging (Basic)**

File: `crates/waystone-comm-core/src/logging/mod.rs`

Reference: MASTERPLAN §4.7 (full spec for Phase 2)

Phase 1 scope:
- `SessionLog` struct that writes raw bytes to a file
- Timestamped log format: `[HH:MM:SS] <bytes as printable text>`
- Log path: `~/.config/waystone-comm/logs/<entry-name>/<date>.log`
- Auto-create directories

**Step 1.11 — Ratatui TUI Shell**

File: `crates/waystone-comm-tui/src/`

Reference: MASTERPLAN §3.7 (Phase 1 layout), §9.1 (layout principles)

Phase 1 TUI — baseline shell; current app has tabbed sessions:
1. Main app loop: `crossterm` event loop + Ratatui render loop
2. Terminal pane: renders `TerminalScreen` from the emulator
3. Status bar: connection status, protocol, host
4. F-key bar: F2 Dir, F3 Scripts, F5 Creds, F6 Send, F7 Recv, F8 Log, F9 Keys; Ctrl+Q quits
5. Input handling: pass keypresses to active `Connection::write()`
6. Window resize: update terminal emulator dimensions + send NAWS for Telnet

**Step 1.12 — CLI Entry Point**

File: `crates/waystone-comm-tui/src/main.rs`

Reference: MASTERPLAN §3.8

Use `clap` for argument parsing. Implement the commands listed in §3.8:
- `waystone-comm` — launch TUI dialing directory
- `waystone-comm connect ssh user@host`
- `waystone-comm connect serial /dev/ttyUSB0 --baud 115200`
- `waystone-comm connect telnet host[:port]`
- `waystone-comm connect raw host:port`
- `waystone-comm replay /tmp/capture.raw --emulation xterm-256color`
- `waystone-comm list` — print saved entries

---

### 🔧 PHASE 2 — ProComm Feature Parity

**MASTERPLAN reference:** §4 (all subsections), §9 (UI spec), §10 (data models), §11 (scripting)

**Before starting Phase 2, confirm:**
- All Phase 1 deliverables are ✅ in PHASE_STATUS above
- `waystone-comm connect ssh`, `telnet`, `serial`, `raw` all work end-to-end
- Terminal emulator passes basic vttest cases

**Step 2.1 — Dialing Directory UI**

Reference: MASTERPLAN §4.1

Build the full dialing directory panel as shown in §4.1 using Ratatui widgets:
- Tree widget: groups + entries with expand/collapse
- Search bar (fuzzy search across name, host, tags)
- Sort options: name, last connected, protocol
- Entry detail sidebar (notes, last connection time)
- CRUD keyboard shortcuts: Enter, E, N, D, G as shown in §4.1
- Import from `~/.ssh/config`: parse `Host` blocks → create entries

**Step 2.2 — Tabbed Sessions**

Reference: MASTERPLAN §4.2

Upgrade the TUI from single-session to multi-session:
- Tab bar widget at top (see §9.2 layout)
- Each tab has its own `Session` instance
- Unread activity indicator `●` on background tabs
- Keyboard: `Alt+1..9` to switch tabs, `Ctrl+T` new tab, `Ctrl+W` close tab
- Split view: `Ctrl+|` vertical split, `Ctrl+-` horizontal split (max 4 panes)

**Step 2.3 — Scripting Engine**

Reference: MASTERPLAN §4.3, §11 (full scripting spec)

1. Integrate `rhai` crate into `waystone-comm-core`
2. Create `ScriptRunner` struct that holds a Rhai `Engine` + `Scope`
3. Register all session API functions listed in MASTERPLAN §4.3 as Rhai native functions
4. Implement hook points: `on_connect`, `on_disconnect`, `on_data`, `on_match`, `on_keypress`, `on_timer`
5. Wire hooks into `SessionManager` event bus
6. Script storage as defined in MASTERPLAN §11.2
7. Script editor: basic TUI editor widget with Rhai syntax highlighting (use a simple regex-based highlighter)

See example scripts in MASTERPLAN §11.3 — use these as test cases.

**Step 2.4 — Full Terminal Emulation**

Reference: MASTERPLAN §4.4

Extend the Phase 1 terminal emulator to support all terminal types in the §4.4 table.

Priority order:
1. Xterm-256color (add 256 color SGR codes)
2. Xterm TrueColor (add 24-bit SGR codes: `38;2;r;g;b`)
3. ANSI-BBS (PC character set, blink attribute)
4. VT220 (add VT220-specific sequences)
5. Avatar (BBS-specific control codes)
6. Remaining types as time permits

Run vttest against each implemented terminal type. Document test results.

**Step 2.5 — File Transfer Protocols**

Reference: MASTERPLAN §4.5, §8.2

Implement in order of usefulness:
1. Zmodem (most important — auto-start on `**\x18B0` signature)
2. Ymodem (batch)
3. Xmodem + Xmodem-CRC (legacy but still needed for old gear)
4. Kermit (for medical/industrial)
5. TFTP (Phase 3 stretch if needed)

Transfer UI (per §4.5):
- Progress bar with speed + ETA in status bar area
- Detect Zmodem initiation automatically in data stream
- Transfer queue widget

**Step 2.6 — Key Mapping & Macros**

Reference: MASTERPLAN §4.6

1. `KeyProfile` struct: maps `KeyEvent` → `KeyAction` enum
2. `KeyAction` variants: `SendText(String)`, `RunScript(String)`, `AppCommand(Command)`, `SendBytes(Vec<u8>)`
3. Per-entry key profile override or fall back to global default
4. Key mapping editor UI (see §4.6 layout)
5. F-key toolbar at bottom: render current profile's F-key labels

**Step 2.7 — Full Session Logging**

Reference: MASTERPLAN §4.7

Upgrade logging from Phase 1:
1. Log formats: raw, timestamped text, HTML with ANSI color rendering, JSON
2. Log rotation: by size (default 10MB) and date
3. Built-in log viewer: searchable, scrollable, with timestamp filtering
4. Credential scrubbing: scan log for patterns matching known credentials and redact

**Step 2.8 — Credential Manager**

Reference: MASTERPLAN §4.8, §10.3

1. Try OS keychain first via `keyring` crate
2. Fallback: AES-256-GCM encrypted SQLite using `sqlx` + `aes-gcm` crate
3. `CredentialManager` API: `store`, `retrieve`, `delete`, `list`
4. Credentials referenced by UUID in `DirectoryEntry` (never inline)
5. SSH key management: generate Ed25519 keypair, import existing key, associate with entry
6. Credential manager UI: list, add, edit, delete credentials

**Step 2.9 — Upgrade Directory to SQLite**

Reference: MASTERPLAN §10.2

Replace Phase 1 TOML-only directory with hybrid storage:
- TOML for entry configuration (human-editable, version-controllable)
- SQLite (`~/.config/waystone-comm/history.db`) for session logs and connection history
- Schema from MASTERPLAN §10.2
- Migration path: auto-import existing TOML entries on first run with SQLite

---

### 🔧 PHASE 3 — Waystone Browser Integration & Protocol Polish

**MASTERPLAN reference:** §5 (all subsections), §8.1 (protocol table)

**Before starting Phase 3, confirm:**
- All Phase 2 deliverables are ✅
- Scripting engine passes all example script tests
- File transfer working for at least Zmodem + Xmodem
- Full tabbed session UI working

**Step 3.1 — Waystone Browser Handoff**

Reference: MASTERPLAN §5.2

Do not implement native Gemini, Gopher, Spartan, HTTP, or HTML browsing in
Waystone Comm. Those protocols belong in Waystone Browser.

1. Detect `gemini://`, `gopher://`, `spartan://`, `http://`, and `https://`
   links in terminal output or quick-connect input.
2. Launch Waystone Browser through a configurable command.
3. Keep handoff optional and transparent.
4. Consider reverse handoff later: Waystone Browser can launch `ssh://` and
   `telnet://` targets in Waystone Comm.

**Step 3.2 — IRC Client**

Reference: MASTERPLAN §5.5

1. IRC protocol: RFC 1459 + common IRCv3 extensions (message tags, SASL)
2. Multi-network: each network is a separate session tab
3. Channel list left panel, message area center, nick list right
4. Highlights: keyword list triggers `●` indicator + optional notification
5. Logging: per-channel, same format as session logging
6. SASL PLAIN and EXTERNAL auth methods

**Step 3.3 — Mosh**

Reference: MASTERPLAN §5.3

Mosh requires `mosh-server` on the remote host. This is an acceptable dependency — document clearly.

1. SSH bootstrap: use existing SSH implementation to start `mosh-server` and get UDP port/key
2. SSSP (Mosh State Synchronization Protocol) over UDP
3. Roaming: handle IP address changes gracefully
4. Surface as a connection type in dialing directory: `protocol = "mosh"` (falls back to SSH)

**Step 3.4 — NNTP (Usenet)**

Reference: MASTERPLAN §5.6

1. RFC 3977 NNTP implementation
2. TLS/STARTTLS support
3. Newsgroup browser: group list → article list → article view
4. Thread view: sort by References header
5. Article caching for offline reading: store in `~/.config/waystone-comm/nntp_cache/`
6. Posting support (compose in a simple TUI editor)

**Step 3.5 — SFTP File Browser**

Reference: MASTERPLAN §5.9

This lives inside the SSH session — it's a mode switch, not a new protocol.

1. Two-pane file browser (see §5.9 layout)
2. Operations: copy, move, mkdir, delete, rename
3. Tab switches focus between local and remote pane
4. Progress indicator for transfers (reuse file transfer progress widget)
5. Integrated into SSH session: hotkey `Ctrl+F` to open file browser for current session

**Step 3.6 — Remaining Protocols**

Reference: MASTERPLAN §5.4, §5.7, §5.8

Implement in this order (all are relatively small):
1. **Finger** (§5.7) — 30–50 lines, RFC 1288
2. **WebSocket** (§5.8) — use `tokio-tungstenite` crate
3. **Rlogin** (§5.4) — RFC 1282, flag as legacy/insecure in UI
4. **FTP/FTPS** (§8.1) — use `suppaftp` crate

---

### 🔧 PHASE 4 — AI Integration Layer

**MASTERPLAN reference:** §6 (all subsections), §12 (AI feature spec)

**Before starting Phase 4, confirm:**
- All Phase 3 deliverables are ✅
- At minimum: SSH, Telnet, Waystone Browser handoff, and IRC all working and tested

**Step 4.1 — AI Client Module**

Reference: MASTERPLAN §6.1, §12.1

File: `crates/waystone-comm-core/src/ai/mod.rs`

1. `AIClient` struct from §12.1
2. HTTP client using `reqwest` — POST to `https://api.anthropic.com/v1/messages`
3. Streaming response support (`stream: true`)
4. Configurable base URL (supports Ollama: `http://localhost:11434`)
5. Privacy guard: `CredentialScrubber` runs on all context before sending (§6.3)
6. Read API key from env var `ANTHROPIC_API_KEY` (never from config file)

**Step 4.2 — AI Configuration**

Reference: MASTERPLAN §6.4

Add `[ai]` section to config file as specified in §6.4. Validate on startup:
- If `enabled = false`, skip all AI features silently
- If `enabled = true` but no API key found, show clear setup instructions in F1 help
- `local_only_mode = true` switches base URL to Ollama without code changes

**Step 4.3 — AI Assistant Panel**

Reference: MASTERPLAN §6.1

TUI panel layout from §6.1:
1. Toggleable sidebar (F5 key, or `Ctrl+Space`)
2. Chat history display (scrollable)
3. Input field at bottom
4. "Thinking..." indicator during API call (streaming response updates in real-time)
5. Context indicator: shows what session data is being shared

**Step 4.4 — Script Generation Feature**

Reference: MASTERPLAN §6.2, §12.2

1. Use prompt template from MASTERPLAN §12.2
2. Inject: entry name, protocol, host, last 50 lines of session output, full script API reference
3. Parse AI response → populate script editor with generated Rhai code
4. User must review and explicitly save — never auto-execute generated scripts
5. "Explain this script" mode: send existing script → get plain-English explanation

**Step 4.5 — Smart Connect**

Reference: MASTERPLAN §6.2 (Smart Connect row)

When user types in quick-connect bar:
1. If input matches `user@host` or `host:port` patterns → infer SSH
2. If input starts with `gemini://`, `gopher://`, `spartan://`, `http://`, or
   `https://` → hand off to Waystone Browser
3. If input starts with `telnet://` or port 23 → open Telnet
4. Otherwise: send host + port to AI → get protocol suggestion
5. AI suggestion shown as non-blocking suggestion, user confirms

**Step 4.6 — Log Analysis**

Reference: MASTERPLAN §6.2, §12.3

Accessible from log viewer:
1. "Summarize session" button: sends last N lines to AI using §12.3 template
2. "Explain error" button: sends error context → get diagnosis + fix suggestions
3. "Session diff" between two logs: identify what changed (config changes, new processes, etc.)
4. Results displayed in AI panel — never overwrite the log itself

**Step 4.7 — Anomaly Detection (Optional/Background)**

Reference: MASTERPLAN §6.2 (Anomaly Alerts row)

Opt-in feature (default off):
1. Subscribe to `SessionEvent::DataReceived` for monitored sessions
2. Batch data into 5-second windows
3. Send batches to AI with context: "alert me if you see errors, security issues, or unusual output"
4. On alert: system notification + `●` indicator + message in AI panel
5. Rate-limit API calls: max 1 call per 30 seconds per session

---

### 🔧 PHASE 5 — Polish & Ecosystem

**MASTERPLAN reference:** §7 (all subsections)

**Before starting Phase 5, confirm:**
- All Phase 4 deliverables are ✅
- Application is usable as a daily driver for SSH + serial work
- At least 3 beta users have tested it

**Step 5.1 — Plugin Architecture**

Reference: MASTERPLAN §7.1

1. `Plugin` trait from §7.1
2. Plugin loading: Rhai scripts as plugins (safest, simplest)
3. Optional: dynamic library plugins via `libloading` crate (document security implications)
4. Plugin manager UI: list installed plugins, enable/disable, install from file
5. Plugin config: each plugin gets its own TOML section in main config

**Step 5.2 — Theming System**

Reference: MASTERPLAN §7.2, §9.3

1. `Theme` struct mapping all color roles from §9.3
2. Theme stored as TOML: `~/.config/waystone-comm/themes/`
3. Built-in themes: Classic (ProComm green-on-black), Solarized Dark, Dracula, Nord, Gruvbox
4. Theme editor: cycle through color roles, pick with a color picker widget
5. Per-session theme: override in `DirectoryEntry` config

**Step 5.3 — Documentation**

Reference: MASTERPLAN §7.5

1. `man waystone-comm` — write comprehensive man page (mdoc format)
2. mdBook docs: mirror MASTERPLAN structure for user-facing docs
3. F1 help: context-sensitive — different content depending on active panel
4. Script cookbook: translate §11.3 examples + add 45 more covering common use cases
5. Migration guide: PuTTY import (already implemented in §2.1), SSH config import, ProComm `.dir` format

**Step 5.4 — Cross-Platform Packaging**

Reference: MASTERPLAN §7.4

Use GitHub Actions CI/CD:
1. Linux: build AppImage via `cargo-appimage`, `.deb` via `cargo-deb`, `.rpm` via `cargo-rpm`
2. macOS: build universal binary (x86_64 + aarch64), package as `.dmg`
3. Windows: cross-compile or Windows runner, package as `.msi` via WiX
4. All platforms: `cargo install waystone-comm` must work
5. Publish to: crates.io, AUR (write PKGBUILD), Homebrew (write formula), Flatpak (write manifest)

**Step 5.5 — Tauri GUI Wrapper (Optional)**

Reference: MASTERPLAN §7.3

Only start this if there is user demand. The TUI is the primary interface.

1. Create `crates/waystone-comm-gui/` workspace member
2. Tauri app embeds `waystone-comm-core` as a library dependency
3. Frontend: minimal React/TypeScript UI that wraps the TUI in a webview
4. Adds: native menus, system tray, file drag-and-drop, native font rendering
5. Package separately as `waystone-comm-gui` — never replace the TUI binary

---

## General Coding Standards

These apply throughout all phases:

### Rust Style
- Run `cargo clippy -- -D warnings` before every commit. Fix all warnings.
- Run `cargo fmt` before every commit.
- Use `#[must_use]` on all `Result`-returning functions.
- Prefer `async fn` over manual `Future` impls everywhere.
- Use `thiserror` for error types in library code (`waystone-comm-core`).
- Use `anyhow` for error handling in binary code (`waystone-comm-tui`).
- No `unwrap()` or `expect()` in library code. Use `?` operator.
- `unwrap()` is allowed in tests.

### Security
- Never log credentials. `SessionLog` must run `CredentialScrubber` on all output.
- Never store credentials in `DirectoryEntry` config — always use `CredentialManager`.
- Sanitize all terminal escape sequences from untrusted sources before passing to the emulator.
- Use `zeroize` crate on all types holding secret data.
- SSH host key verification is mandatory — no option to skip.

### Testing
- Every protocol implementation must have an integration test (see MASTERPLAN §13.2).
- Terminal emulator changes must include vttest results.
- Run `cargo test --workspace` before any phase is marked complete.
- Performance targets from MASTERPLAN §13.4 must be verified with `cargo bench`.

### Git Workflow
- Branch naming: `phase1/ssh-implementation`, `phase2/dialing-directory-ui`
- Commit messages: `[Phase N] Short description of what changed`
- Tag releases: `v0.1.0`, `v0.3.0`, etc. matching MASTERPLAN milestone targets
- Never commit API keys, credentials, or `target/` directory

### File Organization
- Follow the directory structure in MASTERPLAN §14 exactly.
- One protocol per file in `crates/waystone-comm-core/src/protocols/`.
- One UI component per file in `crates/waystone-comm-tui/src/ui/`.
- Keep `waystone-comm-core` completely free of TUI/Ratatui imports — it is a pure library.

---

## Common Patterns & Helpers

### Connecting via the Connection trait
```rust
// Standard pattern for all protocol connections
let mut conn: Box<dyn Connection> = Box::new(SshConnection::new());
conn.connect(&entry).await?;

// Read loop
loop {
    let data = conn.read().await?;
    terminal.process(&data);
    if conn.status() == ConnectionStatus::Disconnected { break; }
}
```

### Rhai script execution
```rust
// Standard hook execution pattern
if let Some(script) = &session.entry.scripts.on_connect {
    let mut runner = ScriptRunner::new(session_api.clone());
    runner.call_fn("on_connect", &[session_value]).await?;
}
```

### AI context building
```rust
// Always scrub before sending to AI
let context = AIContext {
    entry_name: session.entry.name.clone(),
    protocol: session.entry.protocol.to_string(),
    host: session.entry.connection.host.clone(),
    recent_output: scrubber.scrub(&session.log.last_n_lines(100)),
};
```

---

## Troubleshooting Common Issues

**Terminal renders garbage / wrong escape sequences:**
- Check `DirectoryEntry.terminal.emulation` setting
- Run `vttest` on the affected terminal type
- Reference MASTERPLAN §4.4 for the full emulation table

**SSH connection fails with host key error:**
- Check `~/.config/waystone-comm/known_hosts`
- Never bypass host key verification — surface the error clearly to the user

**Serial port not detected on Linux:**
- User must be in `dialout` group: `sudo usermod -a -G dialout $USER`
- Document this prominently in setup guide and F1 help

**Zmodem not auto-starting:**
- Check that `on_data` hook is scanning for `**\x18B0` signature in the data stream
- Confirm Zmodem receiver is registered before the SSH/Telnet session starts

**AI panel empty / not responding:**
- Check `ANTHROPIC_API_KEY` environment variable
- Check `[ai] enabled = true` in config
- Check network connectivity to `api.anthropic.com`
- For local AI: verify `ollama serve` is running

---

## Quick Reference — MASTERPLAN Section Map

| I need to know about... | MASTERPLAN section |
|---|---|
| Overall architecture and component map | §2 |
| SSH implementation details | §3.2 |
| Telnet option negotiations | §3.3 |
| Serial port settings | §3.4 |
| Phase 1 TUI layout | §3.7 |
| Dialing directory UI and operations | §4.1 |
| Tab/split session management | §4.2 |
| Script API (all built-in functions) | §4.3 |
| Terminal emulation type list | §4.4 |
| File transfer protocol details | §4.5, §8.2 |
| Key mapping and macro system | §4.6 |
| Session logging formats | §4.7 |
| Credential storage architecture | §4.8, §10.3 |
| Waystone Browser handoff | §5.2 |
| IRC client features | §5.5 |
| SFTP file browser layout | §5.9 |
| AI feature list and triggers | §6.2 |
| AI privacy settings | §6.4 |
| Plugin architecture | §7.1 |
| All protocol ports and transports | §8.1 |
| Full UI layout spec | §9.2 |
| Color theme roles | §9.3 |
| DirectoryEntry data model | §10.1 |
| SQLite schema | §10.2 |
| Scripting engine file layout | §11.2 |
| Example scripts | §11.3 |
| AI prompt templates | §12.2, §12.3 |
| Testing requirements | §13 |
| Full directory/file structure | §14 |
| All crate dependencies | §15.1 |

---

*End of CLAUDE.md*
*This file is maintained alongside the codebase. Update PHASE_STATUS as work progresses.*
