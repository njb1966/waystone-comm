# Waystone Comm Beta Handoff

Last updated: 2026-06-30

## Current State

Waystone Comm is at `v0.3.0-rc.2`, intended for a small real-world beta with
experienced BBS/terminal users.

The GitHub default branch is:

```text
main
```

The current release is:

```text
https://github.com/njb1966/waystone-comm/releases/tag/v0.3.0-rc.2
```

The Waystone Comm repository was re-imported under the new name. Cut new RC tags
from `main`; do not reuse local tags from the old pre-rename history.

## Verified Before Beta

Release gates passed for `v0.3.0-rc.2` before beta publication:

- `cargo fmt --check`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `bash scripts/smoke-local.sh`
- `cargo build --release`

Live validation before beta included:

- Mystic A-Net over SSH with `ansi-bbs`
- Dead Parrot BBS over SSH with `xterm-256color`
- Retroboard over Telnet
- GameSrv/LoRD ANSI-BBS rendering
- Zmodem upload/download workflows
- The Bottomless Abyss SSH BBS upload testing

## Beta Goal

Do not expand feature scope during this beta unless tester feedback exposes a
blocker. The goal is to validate stability, BBS compatibility, and confusing
workflows before cutting `v0.3.0`.

Target testers:

- 3-5 experienced BBS or terminal users
- Linux users comfortable running a release tarball or building from source
- People who actively use SSH, Telnet, ANSI BBSes, and classic transfers

## Recruiting Blurb

```markdown
I'm looking for **3-5 experienced beta testers** for **Waystone Comm**, a keyboard-driven communications terminal for Linux inspired by ProComm Plus and built for people who still connect to BBSes and other remote terminal systems.

I'm specifically looking for testers who actively use SSH, Telnet, ANSI BBSes, and classic file transfers, not people who are only curious to try it for a few minutes. This is pre-1.0 release-candidate software, so the goal is real-world compatibility testing and useful bug reports.

If you're interested, download the latest release from GitHub:

https://github.com/njb1966/waystone-comm/releases

Please work through the included smoke test checklist and test:

- SSH and Telnet logins to one or more BBSes
- ANSI rendering, including ANSI art and doors
- Zmodem uploads and downloads
- Dialing directory create/edit/group workflows
- Saved credentials and entry scripts
- Logging and history features

When reporting results, please include:

- BBS or host name
- Protocol used, such as SSH or Telnet
- Terminal emulation used, such as `ansi-bbs` or `xterm-256color`
- Terminal/window size
- What you expected to happen
- What actually happened
- Screenshot or terminal trace for rendering or transfer issues, if possible

Real-world testing across different systems is exactly what Waystone Comm needs at this stage. Detailed feedback is much more useful than simply reporting that something works or doesn't.
```

## Feedback To Collect

For every report, capture:

- Tester name/contact
- Waystone Comm version or commit
- Install method: release tarball or source build
- Linux distribution and terminal emulator
- Terminal/window size
- BBS or host name
- Protocol: SSH, Telnet, Serial, or Raw TCP
- Emulation: `ansi-bbs`, `xterm-256color`, `vt100`, or `vt220`
- Exact steps to reproduce
- Expected result
- Actual result
- Screenshot, trace, or log if available

For transfer bugs, ask for:

- Transfer direction: upload or download
- Protocol selected on the BBS
- File size and file type
- Whether a small text file works
- `WAYSTONE_COMM_TRANSFER_DEBUG=/tmp/waystone-comm-zmodem.trace waystone-comm` trace when possible

For rendering bugs, ask for:

- Whether the same host works in another terminal/client
- Whether switching between `ansi-bbs` and `xterm-256color` changes behavior
- Raw capture when possible:

```bash
waystone-comm connect ssh HOST --emulation ansi-bbs --raw-capture /tmp/waystone-comm.raw
waystone-comm replay /tmp/waystone-comm.raw --emulation ansi-bbs
```

## Triage Rules

Treat as release blockers:

- Panic/crash
- Terminal left in broken local state after exit
- Common SSH/Telnet BBS cannot connect
- ANSI-BBS rendering regression on known-good hosts
- Zmodem upload/download consistently corrupts files or hangs
- Credential secret exposure in normal logs
- Directory or credential data loss

Treat as high priority:

- Confusing script workflow
- Unclear transfer error messages
- Key binding discoverability problems
- BBS-specific rendering issues with reproducible raw capture
- Log/history failures that are visible but recoverable

Defer until after `v0.3.0`:

- New protocols
- GUI wrapper
- Packaging beyond the tarball
- AI features
- Theming beyond small ProComm-style polish
- Large refactors

## Resume Checklist

When beta feedback comes in:

1. Review each report and classify it as blocker, high priority, normal, or
   deferred.
2. Reproduce locally where possible.
3. For terminal/rendering bugs, request or capture raw session bytes and add a
   focused replay test if practical.
4. For transfer bugs, inspect the transfer trace before changing protocol code.
5. Fix blockers first.
6. Run the local release gates before any new RC:

   ```bash
   cargo fmt --check
   cargo test --workspace
   cargo clippy --workspace -- -D warnings
   bash scripts/smoke-local.sh
   cargo build --release
   ```

7. If code changes are needed after `v0.3.0-rc.2`, cut a new RC rather than
   moving an existing RC tag.

## Notes For Future Us

- The repo history was rewritten on 2026-06-30 to remove old AI co-author
  trailers. Avoid reintroducing co-author trailers in future commits.
- GitHub may cache contributor displays after history rewrites. The contributors
  API reported only `njb1966` after the cleanup.
- The repo history still contains old large `target/` artifacts. Cleaning that
  would require a separate, deliberate history rewrite and should not be mixed
  with beta bug fixing.
