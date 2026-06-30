# Waystone Comm Release Notes

## v0.3.0-rc.1 - 2026-06-30

This is the first Phase 2 release candidate. It is intended for real BBS
testing from a source or tarball build before cutting `v0.3.0`.

### Highlights

- SSH, Telnet, Serial, and Raw TCP connection support.
- Tabbed TUI with dialing directory, grouped entries, credential picker, and
  ProComm-style cyan chrome.
- ANSI-BBS mode for CP437 BBS art, capped 80-column rendering, BBS-style ANSI
  colors, cursor reports, and LoRD/GameSrv validation.
- Zmodem receive and upload/send live-validated against multiple BBSes,
  including Retroboard/WWIV over Telnet and The Bottomless Abyss over SSH.
- Encrypted credential storage with password and SSH key credentials.
- Rhai entry scripts with in-app editing, login template support, runtime
  status, and credential log redaction.
- Session logging, history, and log viewer filters.

### Validation

Local release gates for this RC:

- `cargo fmt --check`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `bash scripts/smoke-local.sh`
- `cargo build --release`

Live smoke coverage includes Mystic A-Net over SSH, Dead Parrot BBS over SSH,
Retroboard over Telnet, GameSrv/LoRD ANSI-BBS rendering, and The Bottomless
Abyss SSH BBS upload testing.

### Known Scope

- This RC ships as a Linux x86_64 tarball artifact, not a `.deb`, AppImage, or
  Homebrew package.
- Gemini, Gopher, IRC, NNTP, Mosh, SFTP browser, AI features, GUI wrapper,
  plugins, and packaging remain deferred.
