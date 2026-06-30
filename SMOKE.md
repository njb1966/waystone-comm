# Waystone Comm Smoke Checklist

Use this checklist after recovery work or before cutting a commit. The local
script covers build/test/CLI parser checks; live connection checks require real
hosts or devices.

## Local Smoke

```bash
bash scripts/smoke-local.sh
```

This verifies:

- `cargo build --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- CLI help for top-level and protocol-specific commands
- CLI help for raw capture replay
- `--emulation ansi-bbs` is accepted by SSH/Telnet/Serial/Raw command parsers

## BBS SSH Smoke

Use a known Mystic/ANSI BBS account.

```bash
cargo run -p waystone-comm-tui -- connect ssh mystic-anet.online --legacy-ssh --ask-password --emulation ansi-bbs
cargo run -p waystone-comm-tui -- connect ssh bbs.deadparrotbbs.com:4000 --emulation xterm-256color
```

Expected:

- Login ANSI art should draw without scrolling over itself.
- CP437 systems should render line/block characters and ANSI colors.
- UTF-8 ANSI systems should preserve Unicode box/block glyphs and ANSI colors.
- Backspace should correct login or editor typos.
- The connection should not print literal CSI fragments such as `?[0;1;34m`.

Encoding note:

- Use `ansi-bbs` for CP437 BBS art, such as many Mystic-style ANSI systems.
- Use `xterm-256color` for UTF-8 ANSI systems. Dead Parrot BBS is a verified
  example.
- Mojibake like `ΓöîΓöÇ` usually means a UTF-8 BBS is incorrectly configured as
  `ansi-bbs`.

## Raw Capture Replay Smoke

Capture exact bytes from a live session, then replay them without reconnecting:

```bash
cargo run -p waystone-comm-tui -- connect ssh bbs.deadparrotbbs.com:4000 --emulation xterm-256color --raw-capture /tmp/waystone-comm-deadparrot.raw
cargo run -p waystone-comm-tui -- replay /tmp/waystone-comm-deadparrot.raw --emulation xterm-256color
```

Expected:

- Replay renders through the same emulator and terminal pane as a live session.
- The replay screen can be exited with `Ctrl+Q` or `Ctrl+C`.
- Dead Parrot's UTF-8 block logo renders with gray shadow and red/white glyphs.

## Live SSH Smoke

Use a disposable test host if possible.

```bash
cargo run -p waystone-comm-tui -- connect ssh user@example.test --identity ~/.ssh/id_ed25519
cargo run -p waystone-comm-tui -- connect ssh user@example.test --identity ~/.ssh/id_ed25519 --emulation ansi-bbs
```

Expected:

- First connection records a host-bound entry in `~/.config/waystone-comm/known_hosts`.
- Reconnecting to the same host succeeds.
- A changed host key for the same host/port is rejected.
- Resizing the terminal updates the remote PTY.

## Credential SSH Smoke

In the TUI:

1. Press `F5`.
2. Create an SSH key credential.
3. Copy the public key into the remote account's `authorized_keys`.
4. Create a new SSH directory entry.
5. Press `F5` from the directory entry form and select the SSH key credential.
6. Connect from the directory.
7. Reopen `F5`, select the SSH key credential, and press `Enter` or `V`.

Expected:

- The entry authenticates using the generated credential-backed private key.
- The directory file stores only the credential UUID, not secret material.
- The credential picker fills the entry's credential UUID field without manual copy/paste.
- The credential panel can show the public key again after creation.
- Deleting the credential from the panel removes it from persisted storage.

For SSH password auth from a directory entry, create a password credential in
`F5`, select it from the directory entry form's `F5` picker, and leave
private-key fields unset.
Password credentials are used for SSH password/keyboard-interactive auth.

## Directory Entry Smoke

In the TUI dialing directory:

1. Press `N`.
2. Create an SSH entry with `ansi-bbs` emulation and `Legacy SSH` set to `yes`.
3. Save it, then select it and press `E`.
4. Change the host, port, username, credential UUID, or legacy setting and save again.
5. Reopen the edit form and confirm the values persisted.

Expected:

- New entries save to `~/.config/waystone-comm/directory.toml`.
- Edited entries keep the same UUID/history identity.
- `Legacy SSH` writes `connection.extra.legacy_ssh = "true"` only when enabled.
- The sidebar shows emulation, credential UUID, and legacy SSH state.

## ANSI/CP437 Telnet Smoke

Use a known ANSI BBS or a local Telnet test service that emits CP437 bytes.

```bash
cargo run -p waystone-comm-tui -- connect telnet bbs.example.test:23 --emulation ansi-bbs
```

Expected:

- CP437 box drawing and block characters render as Unicode line/block glyphs.
- ANSI color and cursor movement continue to work.
- Resizing sends Telnet NAWS to capable hosts.

## Zmodem Receive Smoke

Use a BBS download area with a small test file.

Expected:

- Zmodem auto-detect starts after the remote prints `rz` or begins `**\x18B0`.
- The transfer completes without CRC retry loops or timeout.
- The file appears in `~/Downloads` when a desktop Downloads directory exists,
  otherwise in the home directory.
- Remote DOS paths such as `C:\WWIV\DLOADS\DOORS\pw_152d.zip` save as the
  basename `pw_152d.zip`.
- The resulting file opens or passes a basic archive check such as:

```bash
unzip -t ~/Downloads/pw_152d.zip
```

If a transfer fails, rerun with byte tracing enabled and inspect the trace:

```bash
WAYSTONE_COMM_TRANSFER_DEBUG=/tmp/waystone-comm-zmodem.trace cargo run -p waystone-comm-tui
```

Zmodem receive is currently live-validated against multiple BBSes. Upload/send
via F6 or Alt+U is live-validated against Retroboard/WWIV over Telnet and The
Bottomless Abyss over SSH.

## Zmodem Upload Smoke

Use a BBS upload area with a small local test file.

Expected:

- The remote BBS enters Zmodem receive mode and emits `ZRINIT`, commonly shown
  as `**B0100000027fed4`.
- Pressing `F6` or `Alt+U` opens the send-file prompt.
- Entering a valid local path starts the send and completes without timeout.
- The status area reports `[transfer complete: <bytes> bytes]`.
- The BBS reports that the file uploaded and the uploaded file appears in the
  target directory.

If a transfer fails, rerun with byte tracing enabled and inspect the trace:

```bash
WAYSTONE_COMM_TRANSFER_DEBUG=/tmp/waystone-comm-zmodem-upload.trace cargo run -p waystone-comm-tui
```

## Serial Smoke

Use a real serial device.

```bash
cargo run -p waystone-comm-tui -- connect serial /dev/ttyUSB0 --baud 9600
```

Expected:

- Input/output works with the configured baud/parity/stop bits.
- Serial remains a no-op for terminal resize, as expected.

## Production Release Checklist

Use this before tagging or publishing a build.

Required local checks:

- `cargo fmt --check`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `bash scripts/smoke-local.sh`
- `cargo build --release`

Required TUI workflow checks:

- Directory new/edit/delete, including default SSH/xterm/legacy fields.
- `F5` credential picker from a directory entry form fills the Credential UUID field.
- Credential create/view/delete reports persistence failures instead of silently changing the list.
- Key mapping edits either persist or show a local save warning.
- Session logging failures show a local warning while the session continues.
- History/session startup persistence failures are visible through warnings or stderr.
- Procomm-style cyan chrome is visible on directory, session, tab, F-key, and script panels.

Required live checks:

- At least one SSH ANSI-BBS connection.
- At least one SSH `xterm-256color` connection.
- At least one Telnet ANSI-BBS connection.
- Zmodem receive with a small known-good file.
- Zmodem upload with a small known-good file over Telnet or SSH.
- Raw capture replay of one captured session.

Release notes must include:

- Verified live BBSes and protocols.
- Any known transfer limitations.
- Config/data paths touched by the release.
- MSRV if it changed. Current MSRV is Rust 1.85.

Packaging stance for Phase 2:

- Source builds and `cargo build --release` are the supported production path.
- `.deb`, AppImage, Homebrew, and GUI wrapper packaging remain Phase 5 work unless
  explicitly pulled forward.
