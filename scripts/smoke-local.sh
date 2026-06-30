#!/usr/bin/env bash
set -euo pipefail

cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings

cargo run -p waystone-comm-tui -- --help >/dev/null
cargo run -p waystone-comm-tui -- connect --help >/dev/null
cargo run -p waystone-comm-tui -- connect ssh --help >/dev/null
cargo run -p waystone-comm-tui -- connect telnet --help >/dev/null
cargo run -p waystone-comm-tui -- connect serial --help >/dev/null
cargo run -p waystone-comm-tui -- connect raw --help >/dev/null
cargo run -p waystone-comm-tui -- replay --help >/dev/null

cargo run -p waystone-comm-tui -- connect ssh nobody@example.invalid --emulation ansi-bbs --help >/dev/null
cargo run -p waystone-comm-tui -- connect telnet example.invalid:23 --emulation ansi-bbs --help >/dev/null
cargo run -p waystone-comm-tui -- connect serial /dev/null --emulation ansi-bbs --help >/dev/null
cargo run -p waystone-comm-tui -- connect raw example.invalid:23 --emulation ansi-bbs --help >/dev/null

echo "Local smoke checks passed."
