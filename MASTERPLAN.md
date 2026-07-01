# MASTERPLAN.md — Waystone Comm
## A Universal Communications Terminal for the Modern Era
### Inspired by ProComm Plus · Built for Linux, macOS, Windows

---

> **Vision:** Waystone Comm is a spiritual successor to ProComm Plus — a
> keyboard-driven terminal communications application for SSH, Telnet, Serial,
> Raw TCP, BBS workflows, scripts, logging, and file transfers. It complements
> Waystone Browser, which owns Gemini, Gopher, Spartan, and web browsing.

---

## Current Recovery Status

Waystone Comm is continuing from the existing codebase. The verified core is SSH,
Telnet, Serial, Raw TCP, tabbed TUI sessions, terminal emulation, selectable
CP437 ANSI-BBS mode, UTF-8 ANSI/xterm rendering, host-bound SSH TOFU,
password/filesystem-key/credential-backed SSH auth, logging, key mappings,
scripts, history, credential UI, dialing-directory edit flow, Zmodem receive
against live BBSes, Zmodem upload/send against Retroboard/WWIV over Telnet and
The Bottomless Abyss over SSH, and transfer core tests.

Live BBS validation currently includes Mystic A-Net over SSH with `ansi-bbs`,
Dead Parrot BBS over SSH with `xterm-256color`, Retroboard over Telnet, and The
Bottomless Abyss over SSH. Raw capture replay is available for deterministic
terminal-rendering debugging.

The next work should harden usability rather than expand scope. Treat broader
BBS compatibility testing, docs accuracy, and small UX fixes as active Phase 2
work. Native Gemini, Gopher, Spartan, and web browsing are out of scope for
Waystone Comm because they belong in Waystone Browser; future work should focus
on link handoff between the apps. Treat IRC, NNTP, Mosh, SFTP browser, AI
features, packaging, plugins, theming, and GUI wrapper as deferred.

Release candidate `v0.3.0-rc.2` is the current Phase 2 RC. It is intended for
source/tarball testers and live BBS soak testing, not broad packaged
distribution yet.

Rust MSRV is currently `1.85`, enforced in workspace metadata. Do not update
transitive dependencies in a way that raises MSRV unless the README and this
section are updated in the same change.

---

## Table of Contents

1. [Project Overview](#1-project-overview)
2. [Architecture](#2-architecture)
3. [Phase 1 — Core Engine](#3-phase-1--core-engine)
4. [Phase 2 — ProComm Feature Parity](#4-phase-2--procomm-feature-parity)
5. [Phase 3 — Waystone Browser Integration & Protocol Polish](#5-phase-3--waystone-browser-integration--protocol-polish)
6. [Phase 4 — AI Integration Layer](#6-phase-4--ai-integration-layer)
7. [Phase 5 — Polish & Ecosystem](#7-phase-5--polish--ecosystem)
8. [Protocol Reference](#8-protocol-reference)
9. [UI/UX Specification](#9-uiux-specification)
10. [Data Models](#10-data-models)
11. [Scripting Engine Spec](#11-scripting-engine-spec)
12. [AI Feature Spec](#12-ai-feature-spec)
13. [Testing Strategy](#13-testing-strategy)
14. [File & Directory Structure](#14-file--directory-structure)
15. [Dependencies & Licenses](#15-dependencies--licenses)

---

## 1. Project Overview

### 1.1 Goals

- Provide a focused application for terminal communications protocols and BBS workflows
- Recreate the ProComm Plus UX: dialing directory, F-key macros, ASPECT-style scripting, session logging
- Add modern capabilities: tabbed sessions, true-color, Unicode, Waystone app handoff, and optional AI assistance
- Ship as a native TUI (terminal UI) application with an optional GUI wrapper
- Cross-platform: Linux (primary), macOS, Windows (WSL and native)
- 100% open source (MIT License)

### 1.2 Non-Goals (v1.0)

- Full graphical remote desktop (RDP/VNC rendering) — protocol support yes, display no
- Mobile clients
- Cloud sync (planned for post-v1.0)
- Native Gemini, Gopher, Spartan, or HTML/web browsing; those belong in Waystone Browser

### 1.3 Name & Identity

- **Application name:** Waystone Comm
- **Binary name:** `waystone-comm`
- **Config directory:** `~/.config/waystone-comm/`
- **Repository:** `github.com/[owner]/waystone-comm`

### 1.4 Technology Stack

| Layer | Choice | Rationale |
|---|---|---|
| Language | Rust | Performance, safety, excellent async, great ecosystem |
| TUI framework | Ratatui | Best-in-class Rust TUI, active community |
| Async runtime | Tokio | Industry standard for async Rust networking |
| SSH | `russh` | Pure Rust SSH v2 implementation |
| Serial | `serialport` crate | Cross-platform serial port access |
| Scripting | `rhai` (embedded) + Python via `pyo3` | Fast scripting with optional Python power |
| AI | Anthropic API (Claude) | Natural language session scripting and analysis |
| Config/Storage | TOML (config) + SQLite (history/logs) | Human-editable config, queryable history |
| GUI wrapper (Phase 5) | Tauri | Web frontend over native backend |

---

## 2. Architecture

### 2.1 High-Level Component Map

```
┌─────────────────────────────────────────────────────────────────┐
│                         Waystone Comm Process                         │
│                                                                   │
│  ┌──────────────┐    ┌──────────────┐    ┌────────────────────┐  │
│  │  TUI Layer   │    │  Session Mgr │    │   AI Assistant     │  │
│  │  (Ratatui)   │◄──►│  (Tokio)     │◄──►│  (Claude API)      │  │
│  └──────────────┘    └──────┬───────┘    └────────────────────┘  │
│                             │                                     │
│         ┌───────────────────┼───────────────────┐                │
│         ▼                   ▼                   ▼                │
│  ┌─────────────┐   ┌──────────────┐   ┌──────────────────┐      │
│  │  Protocol   │   │  Scripting   │   │  Dialing         │      │
│  │  Engine     │   │  Engine      │   │  Directory       │      │
│  │  (trait)    │   │  (rhai/py)   │   │  (SQLite/TOML)   │      │
│  └──────┬──────┘   └──────────────┘   └──────────────────┘      │
│         │                                                         │
│  ┌──────┴────────────────────────────────────────────┐           │
│  │              Protocol Implementations              │           │
│  │       SSH │ Telnet │ Serial │ Raw TCP │ ...       │           │
│  └───────────────────────────────────────────────────┘           │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 Core Traits

All protocol connections implement the `Connection` trait:

```rust
pub trait Connection: Send + Sync {
    async fn connect(&mut self, entry: &DirectoryEntry) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    async fn read(&mut self) -> Result<Vec<u8>>;
    async fn write(&mut self, data: &[u8]) -> Result<()>;
    fn protocol(&self) -> Protocol;
    fn status(&self) -> ConnectionStatus;
    fn supports_file_transfer(&self) -> bool;
}
```

### 2.3 Session Model

Each open connection is a `Session`:

```rust
pub struct Session {
    pub id: Uuid,
    pub entry: DirectoryEntry,
    pub connection: Box<dyn Connection>,
    pub terminal: TerminalEmulator,
    pub log: SessionLog,
    pub script: Option<ScriptRunner>,
    pub created_at: DateTime<Utc>,
}
```

### 2.4 Event System

All inter-component communication uses an async event bus:

```
SessionEvent::DataReceived(session_id, bytes)
SessionEvent::Connected(session_id)
SessionEvent::Disconnected(session_id, reason)
SessionEvent::TransferProgress(session_id, TransferStats)
UIEvent::KeyPress(KeyEvent)
UIEvent::Resize(u16, u16)
AIEvent::ResponseReady(String)
ScriptEvent::Output(String)
```

---

## 3. Phase 1 — Core Engine

**Goal:** A working terminal application with SSH, Telnet, Serial, and Raw TCP.
**Target milestone:** v0.1.0
**Estimated scope:** ~4,000–6,000 lines of Rust

### 3.1 Deliverables

- [ ] Project scaffold (Cargo workspace, module structure)
- [ ] `Connection` trait and session manager
- [ ] SSH v2 implementation (password + key auth)
- [ ] Telnet implementation (RFC 854 + common options)
- [ ] Serial port implementation (baud rate, parity, flow control)
- [ ] Raw TCP socket connection
- [ ] VT100 / ANSI terminal emulator (basic)
- [ ] Ratatui TUI shell (single session, no tabs yet)
- [ ] Basic dialing directory (TOML file, no UI)
- [ ] Session logging to file
- [ ] `waystone-comm connect` CLI command

### 3.2 SSH Implementation

**Library:** `russh` crate

Required features:
- SSH v2 only (v1 is cryptographically broken — document this)
- Auth methods: password, public key (RSA, Ed25519, ECDSA), keyboard-interactive
- SSH agent forwarding
- Port forwarding (local and remote)
- X11 forwarding (Phase 2)
- `known_hosts` file management
- Connection multiplexing (ControlMaster equivalent)

**Config per entry:**
```toml
[entry.my-server]
protocol = "ssh"
host = "192.168.1.10"
port = 22
username = "admin"
auth = "key"
key_path = "~/.ssh/id_ed25519"
keepalive_interval = 30
compression = true
```

### 3.3 Telnet Implementation

**Standard:** RFC 854, RFC 855 (option negotiation)

Required option negotiations:
- ECHO (RFC 857)
- SUPPRESS-GO-AHEAD (RFC 858)
- TERMINAL-TYPE (RFC 1091)
- NAWS — Negotiate About Window Size (RFC 1073)
- LINEMODE (RFC 1116)

BBS-specific: ANSI color, Avatar protocol detection

### 3.4 Serial Implementation

**Library:** `serialport` crate

Settings per entry:
```toml
[entry.router-console]
protocol = "serial"
port = "/dev/ttyUSB0"        # or COM3 on Windows
baud_rate = 9600
data_bits = 8
stop_bits = 1
parity = "none"              # none | odd | even | mark | space
flow_control = "none"        # none | software | hardware
timeout_ms = 1000
```

Auto-detect available serial ports at startup.

### 3.5 Raw TCP

Simple TCP socket with optional TLS. Used for:
- Protocol debugging
- Custom services
- Connecting to TCP-based BBS systems

### 3.6 Terminal Emulator (Phase 1 — Basic)

Implement a VT100/ANSI emulator sufficient for basic terminal use:
- 7-bit and 8-bit control sequences
- SGR attributes: bold, dim, italic, underline, blink, reverse, colors (8, 16)
- Cursor movement: absolute, relative, save/restore
- Erase in line/display
- Scrolling regions
- Mouse reporting (X10, VT200 modes)
- UTF-8 character handling
- Soft terminal reset (DECSTR)

Full terminal emulation list is in Phase 2 (§4.4).

### 3.7 Phase 1 TUI Layout

```
┌──────────────────────────────────────────────────────────────┐
│ Waystone Comm v0.1.0                              [SSH] Connected  │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│  (terminal output area — full width/height)                  │
│                                                              │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ F1:Help  F2:Dir  F3:Log  F9:Config  F10:Quit                 │
└──────────────────────────────────────────────────────────────┘
```

### 3.8 Phase 1 CLI Interface

```
waystone-comm                          # Launch TUI, show dialing directory
waystone-comm connect ssh user@host    # Quick connect via SSH
waystone-comm connect serial /dev/ttyUSB0 --baud 115200
waystone-comm connect telnet bbs.example.com
waystone-comm connect raw 10.0.0.1:4242
waystone-comm list                     # List saved entries
```

---

## 4. Phase 2 — ProComm Feature Parity

**Goal:** Full recreation of ProComm Plus's core feature set, modernized.
**Target milestone:** v0.3.0
**Estimated scope:** ~8,000–12,000 additional lines

### 4.1 Dialing Directory (Full UI)

The Dialing Directory is the heart of Waystone Comm. It is a structured, searchable phonebook of every saved connection.

**UI Layout:**
```
┌─ DIALING DIRECTORY ─────────────────────────────────────────┐
│ [Search: ________]  [Sort: Name▼]  [Filter: All ▼]          │
├─────────────────────────────────────────────────────────────┤
│ 📁 Production Servers                                        │
│   ► web-01        SSH    10.0.1.10    Last: 2h ago  ✓       │
│   ► web-02        SSH    10.0.1.11    Last: 1d ago  ✓       │
│   ► db-primary    SSH    10.0.1.20    Last: 3d ago  ✓       │
│ 📁 BBS Systems                                              │
│   ► Level 29      Telnet bbs.lvl29    Last: 1w ago          │
│   ► Throwback BBS Telnet throwback    Last: never           │
│ 📁 Serial / Local                                           │
│   ► Router Con    Serial /dev/ttyUSB0 Last: 5h ago          │
│   ► Arduino Dev   Serial /dev/ttyACM0 Last: 2d ago          │
├─────────────────────────────────────────────────────────────┤
│ Enter:Connect  E:Edit  N:New  D:Delete  G:Group  Space:Tag  │
└─────────────────────────────────────────────────────────────┘
```

**DirectoryEntry data model** (see §10.1 for full schema):
- Name, protocol, host/port, credentials reference
- Group/folder assignment
- Tags (free-form)
- Per-entry script (on-connect, on-disconnect, on-data)
- Terminal emulation override
- Key mapping override
- Notes (markdown, displayed in sidebar)
- Connection history (last N connections, stored in SQLite)
- Colors / icon (visual differentiation)

**Operations:**
- CRUD for entries and groups
- Import from PuTTY sessions, SSH config file (`~/.ssh/config`)
- Export to JSON / TOML
- Duplicate entry
- Bulk operations (tag, move, delete)
- Quick-connect bar (type hostname → auto-detect protocol)

### 4.2 Tabbed Sessions

Multiple simultaneous sessions in a tab bar:

```
[SSH: web-01] [Telnet: lvl29 ●] [Serial: ttyUSB0] [+]
```

- `●` indicator for sessions with unread output
- Tabs reorderable via keyboard
- Split view: horizontal and vertical splits (up to 4 panes)
- Session groups: open all entries in a folder as a tab group
- Background sessions: keep connected even when not in focus
- Session broadcaster: type to multiple sessions simultaneously

### 4.3 ASPECT-Style Scripting Engine

ProComm's ASPECT language modernized. Waystone Comm uses **Rhai** (Rust-native scripting) as the default engine, with optional Python via PyO3.

**Script hooks:**
```rust
// Available hook points
on_connect(session)         // Fires when connection is established
on_disconnect(session)      // Fires on disconnect
on_data(session, data)      // Fires on each data chunk received
on_match(pattern, session)  // Fires when regex matches output
on_keypress(key, session)   // Fires on specific key combinations
on_timer(interval, session) // Fires on interval
```

**Built-in script functions:**
```javascript
// Rhai script example — auto-login sequence
fn on_connect(s) {
    s.wait_for("login:", 10);
    s.send(s.credential("username") + "\n");
    s.wait_for("Password:", 5);
    s.send(s.credential("password") + "\n");
    s.wait_for("$", 10);
    s.log("Login complete");
    s.notify("Connected to " + s.entry_name());
}
```

**Script functions API:**
| Function | Description |
|---|---|
| `s.send(text)` | Send text to remote |
| `s.send_raw(bytes)` | Send raw bytes |
| `s.wait_for(pattern, timeout)` | Wait for regex match in output |
| `s.wait_ms(ms)` | Wait N milliseconds |
| `s.log(message)` | Write to session log |
| `s.notify(message)` | System notification |
| `s.upload(path, protocol)` | Send file (zmodem/xmodem/etc) |
| `s.download(path, protocol)` | Receive file |
| `s.run_local(cmd)` | Run local shell command |
| `s.credential(key)` | Retrieve credential securely |
| `s.set_var(key, val)` | Store session variable |
| `s.get_var(key)` | Retrieve session variable |
| `s.disconnect()` | Close the connection |
| `s.reconnect()` | Reconnect with same settings |

**Script management UI:**
- Script editor with syntax highlighting
- Script library (per-entry scripts + global scripts)
- Script import/export
- Script testing/dry-run mode
- AI-assisted script generation (Phase 4)

### 4.4 Full Terminal Emulation Suite

Implement all terminal types ProComm Plus supported, plus modern additions:

| Terminal | Use Case | Priority |
|---|---|---|
| VT52 | Legacy equipment | Low |
| VT100 | Universal baseline | Critical |
| VT220 | Enhanced VT100 | High |
| VT320 | VT220 + printing | Medium |
| ANSI / ANSI-BBS | BBS color art | Critical |
| Avatar (AVT/0+) | BBS protocol | Medium |
| RIP (v1) | BBS graphics | Low |
| Xterm | Modern standard | Critical |
| Xterm-256color | 256 color support | Critical |
| Xterm TrueColor | 16M color support | High |
| Linux console | Local terminals | Medium |
| SCO ANSI | Legacy SCO Unix | Low |

Terminal emulator must pass **vttest** for supported terminal types.

### 4.5 File Transfer Protocols

All classic protocols plus modern additions:

| Protocol | Direction | Notes |
|---|---|---|
| **Xmodem** | Both | Original + 1K variant |
| **Xmodem-CRC** | Both | CRC error checking |
| **Ymodem** | Both | Batch transfers |
| **Ymodem-G** | Both | Streaming (no ACK) |
| **Zmodem** | Both | Auto-detect, crash recovery |
| **Zmodem-8K** | Both | Large block variant |
| **Kermit** | Both | Used in medical/industrial |
| **TFTP** | Both | Embedded device firmware |
| **SCP** | Both | SSH-based (via SSH session) |
| **SFTP** | Both | SSH file browser |
| **FTP/FTPS** | Both | Classic FTP with TLS option |

File transfer UI:
- Progress bar with speed, ETA, bytes transferred
- Batch queue (multiple files)
- Auto-trigger on Zmodem detection in stream
- Transfer history log

### 4.6 Key Mapping & Macro Keys

```
┌─ KEY MAPPING EDITOR ──────────────────────────────────────┐
│ Profile: [Default ▼]  [New Profile]                        │
├───────────────┬───────────────────────────────────────────┤
│ KEY           │ ACTION                                     │
│ F1            │ Show help                                  │
│ F2            │ Open dialing directory                     │
│ F3            │ Toggle session log                         │
│ F4            │ [unassigned]                               │
│ F5            │ Open AI assistant                          │
│ F6            │ Send file (Zmodem)                         │
│ F7            │ Receive file                               │
│ F8            │ [unassigned]                               │
│ F9            │ Open settings                              │
│ F10           │ Quit / disconnect                          │
│ Alt+1..9      │ Switch to tab 1–9                          │
│ Ctrl+Alt+S    │ Run script...                              │
└───────────────┴───────────────────────────────────────────┘
```

- Key profiles: per-entry override or global default
- Macro keys: assign text strings, scripts, or commands to any key combo
- Keyboard toolbar: visible F-key bar at bottom of screen (toggleable)

### 4.7 Session Logging

- Auto-log toggle per entry
- Log formats: raw bytes, printable text, timestamped, HTML (with color)
- Log rotation by size or date
- Log viewer: built-in searchable log browser
- Log export: text, HTML, JSON
- Privacy: credential scrubbing option (redact passwords from logs)

### 4.8 Credential Manager

Secure local credential storage:
- Backed by OS keychain (libsecret/Keychain/Credential Manager) or encrypted SQLite
- Per-entry credential references (never store plaintext in config)
- SSH key management: generate, import, associate with entries
- Master password option for portable installs
- Import from PuTTY, SSH agent, `~/.ssh/config`

---

## 5. Phase 3 — Waystone Browser Integration & Protocol Polish

**Goal:** Keep Waystone Comm focused on terminal communication while integrating
cleanly with the broader Waystone app line.
**Target milestone:** v0.6.0
**Estimated scope:** ~2,000–4,000 additional lines before any large new protocol

### 5.1 Product Boundary

Waystone Browser owns document/navigation protocols:

- Gemini
- Gopher
- Spartan
- HTTP/HTTPS and HTML/web browsing

Waystone Comm should not duplicate those native browser features. Its core
territory remains terminal communications:

- SSH
- Telnet
- Serial
- Raw TCP
- ANSI/BBS workflows
- dialing directory entries
- credentials, scripts, logs, history, and X/Y/Zmodem transfers

### 5.2 Waystone Browser Handoff

Future integration should route browser-style links to Waystone Browser instead
of implementing native renderers inside Waystone Comm:

- Open `gemini://`, `gopher://`, `spartan://`, `http://`, and `https://` links
  in Waystone Browser.
- Let Waystone Browser hand `ssh://`, `telnet://`, and BBS-oriented links back
  to Waystone Comm where appropriate.
- Provide a small launcher abstraction instead of hard-coding one binary name.
- Keep URL handoff optional and transparent; Waystone Comm must remain usable as
  a standalone terminal client.

### 5.3 Mosh (Mobile Shell)

- SSH-like but uses UDP, handles roaming and connection interruption
- Requires `mosh-server` on remote — document this dependency
- Seamless handoff when IP changes (laptop lid close/open)
- Integrate with existing SSH directory entries (auto-fallback)

### 5.4 Rlogin

- RFC 1282 implementation
- Marked as legacy/insecure in UI
- Primarily for connecting to old Unix systems

### 5.5 IRC Client

A built-in IRC client is a natural fit for the terminal-centric community Waystone Comm serves.

**Features:**
- IRC and IRCv3 protocol support
- TLS connections
- Multiple networks simultaneously (separate tabs)
- Channel list, nick list
- Logging per channel
- Highlight keywords, mentions
- CTCP support
- NickServ / SASL authentication
- IRC-over-Tor option

### 5.6 NNTP (Usenet)

- RFC 3977 implementation
- Newsgroup subscription and browsing
- Thread view
- Offline reading (article caching)
- Posting support
- TLS/STARTTLS

### 5.7 Finger Protocol

- RFC 1288
- Query user info on remote systems
- Small but satisfying to include for completeness

### 5.8 WebSocket (Raw)

- Connect to WebSocket endpoints
- Useful for connecting to modern services and IoT
- Pairs well with scripting engine

### 5.9 SFTP File Browser

A full two-pane file browser accessible from any SSH session:

```
┌─ LOCAL ──────────────────┬─ REMOTE: web-01 ──────────────┐
│ /home/user/              │ /var/www/html/                 │
│ ..                       │ ..                             │
│ 📁 Documents             │ 📁 assets                      │
│ 📁 Projects              │ 📁 css                         │
│ 📄 notes.txt    4.2K     │ 📄 index.html     8.1K        │
│ 📄 config.toml  1.1K     │ 📄 app.js        24.3K        │
├──────────────────────────┴───────────────────────────────┤
│ F5:Copy  F6:Move  F7:Mkdir  F8:Delete  Tab:Switch Pane   │
└──────────────────────────────────────────────────────────┘
```

---

## 6. Phase 4 — AI Integration Layer

**Goal:** Weave Claude AI throughout the application to assist, automate, and enhance every workflow.
**Target milestone:** v0.8.0
**Estimated scope:** ~3,000–5,000 lines + API integration

### 6.1 AI Assistant Panel

Accessible via F5 or `Ctrl+Space`. A sidebar or overlay panel:

```
┌─ AI ASSISTANT ────────────────────────────────────────────┐
│ Session: web-01 (SSH)                        [×] Close    │
├────────────────────────────────────────────────────────────┤
│ ┌──────────────────────────────────────────────────────┐  │
│ │ Claude: I can see you're connected to web-01. What   │  │
│ │ would you like help with?                             │  │
│ └──────────────────────────────────────────────────────┘  │
│                                                            │
│ > Write a login script for this server                     │
│                                                            │
│ [Send] [Clear] [History]                                   │
└────────────────────────────────────────────────────────────┘
```

### 6.2 AI Feature Set

| Feature | Description | Trigger |
|---|---|---|
| **Script Generation** | Describe automation → get working Rhai script | F5 panel |
| **Smart Connect** | Paste any host string → AI detects protocol/port | Quick-connect bar |
| **Log Summary** | Summarize last N lines or entire session log | F5 panel |
| **Session Diff** | Compare two session logs, highlight changes | Log viewer |
| **Command Explain** | Explain what the last command did | Right-click / hotkey |
| **Error Diagnosis** | Detect errors in output → suggest fixes | Auto / F5 |
| **Protocol Detect** | Connect to unknown port → identify protocol | Raw connection |
| **BBS Navigator** | Read BBS menus → suggest navigation steps | Telnet/BBS mode |
| **Anomaly Alerts** | Background monitoring of serial/SSH output | Always-on option |
| **Config Generator** | Generate per-entry connection configs from description | Dialing directory |

### 6.3 AI Context Management

The AI always receives relevant context:
- Current session protocol and host
- Last N lines of session output (configurable, default 100)
- Entry name and notes from dialing directory
- Current script (if editing)
- Session log summary

Context is never sent without user opt-in. A clear indicator shows when session data is being shared with the AI API.

### 6.4 Privacy & AI Settings

```toml
[ai]
enabled = true
provider = "anthropic"           # anthropic | local (ollama) | none
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-sonnet-4-20250514"
max_context_lines = 100
auto_suggest = false              # Proactively suggest actions
scrub_credentials = true          # Never send passwords to API
local_only_mode = false           # Use only local models
```

### 6.5 Local AI Option

For users who require full privacy:
- Support Ollama as a backend (same API interface)
- Document recommended local models (e.g., CodeLlama for script generation)
- Graceful degradation when model capability is lower

---

## 7. Phase 5 — Polish & Ecosystem

**Goal:** Production quality, packaging, plugin system, optional GUI.
**Target milestone:** v1.0.0

### 7.1 Plugin Architecture

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> &str;
    fn on_load(&mut self, ctx: &mut AppContext) -> Result<()>;
    fn on_session_event(&mut self, event: &SessionEvent) -> Result<()>;
    fn on_ui_event(&mut self, event: &UIEvent) -> Result<()>;
}
```

- Plugins distributed as Rhai scripts or shared libraries (`.so`/`.dll`/`.dylib`)
- Plugin manager UI: install, enable, disable, update
- Plugin registry (community-maintained list)

**First-party plugins to develop:**
- `waystone-comm-theme-pack` — collection of color themes
- `waystone-comm-bbs-extras` — extended BBS art/ANSI rendering
- `waystone-comm-devops` — Kubernetes/Docker session helpers

### 7.2 Theming System

Full color theme support:
- Built-in themes: Classic (ProComm green-on-black), Solarized, Dracula, Nord, Gruvbox, Tokyo Night
- Theme editor: customize all UI colors
- Per-session theme overrides
- Theme import/export (compatible with common terminal theme formats)

### 7.3 Tauri GUI Wrapper (Optional)

An optional graphical wrapper for users who prefer a native GUI:
- Embeds the Rust core as a library
- Adds: native menus, system tray, drag-and-drop file transfer, native font rendering
- Not a replacement for the TUI — both remain supported
- Packaged separately as `waystone-comm-gui`

### 7.4 Cross-Platform Packaging

| Platform | Format |
|---|---|
| Linux | AppImage, `.deb`, `.rpm`, Flatpak, AUR |
| macOS | `.dmg`, Homebrew formula |
| Windows | `.msi` installer, Winget, Scoop |
| All | Cargo install (`cargo install waystone-comm`) |

### 7.5 Documentation

- `man waystone-comm` — comprehensive man page
- Online docs site (mdBook)
- In-app help (`F1`) — context-sensitive
- Script cookbook: 50+ example scripts
- Migration guide: importing from PuTTY, SecureCRT, ProComm Plus `.dir` files

### 7.6 Community & Contribution

- GitHub Discussions for feature requests
- Plugin submission process
- Script sharing repository
- BBS directory: curated list of active BBS systems with importable entries

---

## 8. Protocol Reference

### 8.1 Protocol Summary Table

| Protocol | Port | Transport | Auth | File Transfer | Status |
|---|---|---|---|---|---|
| SSH v2 | 22 | TCP/TLS | Key/Password/GSSAPI | SCP, SFTP | Phase 1 |
| Telnet | 23 | TCP | None/BBS | None (Zmodem in-band) | Phase 1 |
| Serial | — | RS-232 | None | Xmodem/Ymodem/Zmodem | Phase 1 |
| Raw TCP | any | TCP | None | None | Phase 1 |
| Rlogin | 513 | TCP | Host-based | None | Phase 2 |
| Mosh | 60000+ | UDP | SSH bootstrap | None | Phase 3 |
| Browser handoff | varies | external app | varies | Browser-owned | Phase 3 |
| IRC | 6667/6697 | TCP/TLS | NickServ/SASL | DCC | Phase 3 |
| NNTP | 119/563 | TCP/TLS | Password | Attachments | Phase 3 |
| SFTP | 22 | TCP/TLS | SSH | Full filesystem | Phase 2 |
| FTP/FTPS | 21 | TCP/TLS | Password | Full | Phase 3 |
| TFTP | 69 | UDP | None | Full | Phase 3 |
| Finger | 79 | TCP | None | None | Phase 3 |
| WebSocket | 80/443 | TCP/TLS | Varies | None | Phase 3 |

### 8.2 File Transfer Protocol Details

| Protocol | Max Speed | Error Detect | Batch | Auto-Start | Resume |
|---|---|---|---|---|---|
| Xmodem | ~1KB/s | Checksum/CRC | No | No | No |
| Xmodem-1K | ~2KB/s | CRC-16 | No | No | No |
| Ymodem | Line speed | CRC-16 | Yes | No | No |
| Ymodem-G | Line speed | CRC-16 | Yes | No | No |
| Zmodem | Line speed | CRC-32 | Yes | Yes | Yes |
| Kermit | Configurable | CRC | Yes | No | Yes |

---

## 9. UI/UX Specification

### 9.1 Layout Principles

1. **Keyboard first.** Every action has a keyboard shortcut. Mouse optional.
2. **F-key toolbar always visible** (toggleable). New users can discover features.
3. **Status bar** always shows: connection status, protocol, latency, transfer activity.
4. **No modal dialogs** unless absolutely necessary. Use inline panels and overlays.
5. **Esc key** always backs out of any panel without losing session focus.

### 9.2 Main Layout (Phase 2+)

```
┌──────────────────────────────────────────────────────────────────┐
│ Waystone Comm                                           v0.3.0        │
├──────────────────────────────────────────────────────────────────┤
│ [SSH: web-01 ✓] [Telnet: lvl29 ●] [Serial: ttyUSB0] [+New Tab]  │
├──────────────┬───────────────────────────────────────────────────┤
│ DIRECTORY    │  web-01 — SSH — 10.0.1.10                         │
│ ──────────── │ ──────────────────────────────────────────────── │
│ 📁 Prod      │                                                   │
│  ► web-01 ✓  │  user@web-01:~$ systemctl status nginx           │
│  ► web-02    │  ● nginx.service - A high performance web server  │
│  ► db-01     │    Loaded: loaded (/lib/systemd/...)              │
│ 📁 BBS       │    Active: active (running) since Mon 2026...     │
│  ► lvl29 ●   │                                                   │
│  ► throwback │  user@web-01:~$ _                                 │
│ 📁 Serial    │                                                   │
│  ► router    │                                                   │
│  ► arduino   │                                                   │
├──────────────┴───────────────────────────────────────────────────┤
│ SSH:web-01  Connected  10.0.1.10:22  Lat:12ms  Log:ON  AI:Ready  │
├──────────────────────────────────────────────────────────────────┤
│ F1:Help  F2:Dir  F3:Log  F5:AI  F6:Send  F7:Recv  F9:Cfg F10:X  │
└──────────────────────────────────────────────────────────────────┘
```

### 9.3 Color Themes

Default color roles (overridable by themes):

| Role | Default | Description |
|---|---|---|
| Background | #0d0d0d | Main terminal background |
| Foreground | #e0e0e0 | Terminal text |
| UI Background | #1a1a2e | Panels and sidebars |
| UI Foreground | #a0a0c0 | Panel text |
| Accent | #00ff88 | Active elements, cursor |
| Warning | #ffaa00 | Warnings, unread indicators |
| Error | #ff4444 | Errors, disconnected state |
| Success | #44ff88 | Connected, transfer complete |

### 9.4 Accessibility

- All colors configurable for colorblind-friendly themes
- Screen reader hints in TUI (where Ratatui supports it)
- High-contrast theme built-in
- Font size adjustable (for Tauri GUI wrapper)

---

## 10. Data Models

### 10.1 DirectoryEntry

```toml
[[entry]]
id = "uuid-here"
name = "web-01"
group = "Production"
tags = ["nginx", "ubuntu", "prod"]
protocol = "ssh"

[entry.connection]
host = "10.0.1.10"
port = 22
username = "deploy"
auth_method = "key"
credential_id = "cred-uuid"  # Reference to credential manager
known_host_fingerprint = "SHA256:..."
keepalive = 30
compression = true
jump_host = ""  # optional ProxyJump

[entry.terminal]
emulation = "xterm-256color"
encoding = "utf-8"
columns = 220
rows = 50
scrollback = 10000

[entry.scripts]
on_connect = "scripts/web-prod-login.rhai"
on_disconnect = ""
on_data = ""

[entry.logging]
enabled = true
path = "logs/web-01/"
format = "timestamped"
scrub_credentials = true

[entry.ui]
color = "#00ff88"
icon = "server"
notes = "Primary nginx web server. sudo password in vault."
```

### 10.2 Session Log (SQLite)

```sql
CREATE TABLE session_logs (
    id          TEXT PRIMARY KEY,
    entry_id    TEXT NOT NULL,
    started_at  DATETIME NOT NULL,
    ended_at    DATETIME,
    protocol    TEXT NOT NULL,
    host        TEXT,
    bytes_sent  INTEGER DEFAULT 0,
    bytes_recv  INTEGER DEFAULT 0,
    log_path    TEXT
);

CREATE TABLE connection_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id    TEXT NOT NULL,
    connected_at DATETIME NOT NULL,
    duration_s  INTEGER,
    outcome     TEXT  -- connected | refused | timeout | error
);
```

### 10.3 Credential Storage

Credentials are NEVER stored in the main config file. They are stored in:
1. OS keychain (preferred): `libsecret` (Linux), `Keychain` (macOS), `Credential Manager` (Windows)
2. Encrypted SQLite fallback (master-password protected, AES-256-GCM)

```rust
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub kind: CredentialKind,  // Password | PrivateKey | Certificate | Token
    pub username: Option<String>,
    pub secret: SecretString,   // zeroizes on drop
}
```

---

## 11. Scripting Engine Spec

### 11.1 Engine Selection

| Engine | Language | Use Case | Default |
|---|---|---|---|
| Rhai | Rhai (Rust-like) | Fast, embedded, safe | Yes |
| Python | Python 3.x | Power users, existing scripts | Optional |

Rhai is the default because it is sandboxed by default, has no external dependencies, and integrates natively with Rust async.

### 11.2 Script File Format

```
~/.config/waystone-comm/scripts/
├── global/
│   ├── auto-reconnect.rhai
│   └── log-monitor.rhai
├── entries/
│   ├── web-01-login.rhai
│   └── router-setup.rhai
└── library/
    ├── common.rhai       # Shared functions
    └── protocols.rhai    # Protocol helpers
```

### 11.3 Example Scripts

**Auto-login with 2FA prompt:**
```javascript
fn on_connect(s) {
    s.wait_for("login:", 10);
    s.send(s.credential("username") + "\n");
    s.wait_for("Password:", 5);
    s.send(s.credential("password") + "\n");
    // Handle 2FA if present
    let result = s.wait_for_any(["Verification code:", "$"], 10);
    if result == 0 {
        let code = s.prompt("Enter 2FA code:");
        s.send(code + "\n");
        s.wait_for("$", 10);
    }
    s.log("Login complete at " + timestamp());
}
```

**Serial device monitor with alerting:**
```javascript
fn on_data(s, data) {
    if data.contains("ERROR") || data.contains("FAULT") {
        s.notify("⚠️ Alert on " + s.entry_name() + ": " + data);
        s.log("[ALERT] " + data);
    }
}
```

---

## 12. AI Feature Spec

### 12.1 API Integration

```rust
pub struct AIClient {
    api_key: SecretString,
    model: String,
    base_url: String,   // supports custom/local endpoints
}

impl AIClient {
    pub async fn complete(&self, req: AIRequest) -> Result<AIResponse>;
    pub async fn stream(&self, req: AIRequest) -> Result<impl Stream<Item = String>>;
}
```

### 12.2 Script Generation Prompt Template

```
System: You are an expert in Waystone Comm terminal automation scripting using the Rhai language.
The user is connected to {entry_name} via {protocol} at {host}.

Available session API: {api_reference}

Generate only valid Rhai script code. Include error handling.
Do not use external libraries. Do not perform destructive actions without confirmation.

Recent session output (last 50 lines):
{session_output}

User request: {user_request}
```

### 12.3 Log Analysis Prompt Template

```
System: You are analyzing a terminal session log. Identify:
1. What commands were run and their outcomes
2. Any errors, warnings, or anomalies
3. Configuration changes made
4. Suggestions for improvement

Session: {entry_name} ({protocol}) — {duration}
Log excerpt:
{log_content}
```

---

## 13. Testing Strategy

### 13.1 Unit Tests

- Protocol parsers: Telnet option negotiation, terminal escape sequences, URL handoff detection
- Terminal emulator: vttest compliance suite
- Script engine: all built-in functions, error handling, sandboxing
- Dialing directory: CRUD, import/export, search

### 13.2 Integration Tests

- Full SSH connection lifecycle (use local `sshd` in test container)
- Serial loopback testing (virtual serial pair via `socat`)
- File transfer: all protocols, including mid-transfer interruption and resume
- Script execution: on_connect, on_data, on_disconnect hooks

### 13.3 End-to-End Tests

- Connect → authenticate → run command → disconnect via each protocol
- Full Zmodem transfer with large file
- Dialing directory: import PuTTY sessions → connect via imported entry

### 13.4 Performance Targets

- Terminal rendering: < 5ms latency between input and screen update
- SSH throughput: > 50 MB/s on loopback
- Startup time: < 200ms to TUI ready
- Memory: < 50MB base, < 10MB per additional session

---

## 14. File & Directory Structure

```
waystone-comm/
├── Cargo.toml                  # Workspace manifest
├── Cargo.lock
├── README.md
├── MASTERPLAN.md               # This file
├── CLAUDE.md                   # AI assistant build guide
├── LICENSE
│
├── crates/
│   ├── waystone-comm-core/          # Core library (no UI)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── connection/     # Connection trait + session manager
│   │   │   ├── protocols/      # One module per protocol
│   │   │   │   ├── ssh.rs
│   │   │   │   ├── telnet.rs
│   │   │   │   ├── serial.rs
│   │   │   │   ├── raw.rs
│   │   │   │   ├── gemini.rs
│   │   │   │   └── ...
│   │   │   ├── terminal/       # Terminal emulator
│   │   │   ├── transfer/       # File transfer protocols
│   │   │   ├── scripting/      # Rhai + Python engines
│   │   │   ├── directory/      # Dialing directory, credentials
│   │   │   ├── logging/        # Session logging
│   │   │   └── ai/             # AI client
│   │   └── Cargo.toml
│   │
│   ├── waystone-comm-tui/           # Ratatui TUI application
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   ├── app.rs          # App state machine
│   │   │   ├── ui/             # All UI components
│   │   │   │   ├── terminal_pane.rs
│   │   │   │   ├── directory_panel.rs
│   │   │   │   ├── tab_bar.rs
│   │   │   │   ├── status_bar.rs
│   │   │   │   ├── fkey_bar.rs
│   │   │   │   ├── ai_panel.rs
│   │   │   │   └── transfer_dialog.rs
│   │   │   └── events.rs
│   │   └── Cargo.toml
│   │
│   └── waystone-comm-gui/           # Tauri GUI wrapper (Phase 5)
│       └── ...
│
├── tests/                      # Integration tests
├── benches/                    # Performance benchmarks
├── docs/                       # mdBook documentation source
└── scripts/                    # Dev tooling scripts
```

---

## 15. Dependencies & Licenses

### 15.1 Core Dependencies

| Crate | Version | License | Purpose |
|---|---|---|---|
| `tokio` | 1.x | MIT | Async runtime |
| `ratatui` | 0.26+ | MIT | TUI framework |
| `russh` | 0.44+ | Apache-2.0 | SSH v2 |
| `serialport` | 4.x | MPL-2.0 | Serial ports |
| `rhai` | 1.x | MIT/Apache | Scripting |
| `sqlx` | 0.7+ | MIT/Apache | SQLite |
| `serde` | 1.x | MIT/Apache | Serialization |
| `toml` | 0.8+ | MIT/Apache | Config parsing |
| `uuid` | 1.x | MIT/Apache | UUIDs |
| `reqwest` | 0.12+ | MIT/Apache | HTTP/AI API |
| `keyring` | 2.x | MIT/Apache | OS keychain |
| `zeroize` | 1.x | MIT/Apache | Secure memory |
| `regex` | 1.x | MIT/Apache | Pattern matching |
| `chrono` | 0.4+ | MIT/Apache | Timestamps |
| `crossterm` | 0.27+ | MIT | Terminal I/O |

### 15.2 Optional Dependencies

| Crate | Purpose | Feature Flag |
|---|---|---|
| `pyo3` | Python scripting | `python-scripting` |
| `tauri` | GUI wrapper | `gui` |
| `ollama-rs` | Local AI | `local-ai` |

### 15.3 License

Waystone Comm is released under the **MIT License**.

All dependencies are MIT, Apache-2.0, or MPL-2.0 compatible with MIT distribution.

---

*End of MASTERPLAN.md*
*Last updated: 2026 — Waystone Comm Project*
