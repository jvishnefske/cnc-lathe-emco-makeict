#!/usr/bin/env bash
# Everything about the bridge that can be checked without a board.
#
#   ./firmware/check.sh
#
# What it does NOT check is stated in firmware/README.md under "Not verified":
# USB enumeration, timestamp accuracy, and DE turnaround all need hardware,
# and no amount of green here says anything about them.
set -euo pipefail

cd "$(dirname "$0")/.."
root="$PWD"
elf="firmware/rs485-bridge/target/thumbv7em-none-eabihf/release/emco-rs485-bridge"

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# The same two checks the pre-commit hook runs, on the same crates. A hook is
# skippable with --no-verify and lives on one machine; this is the gate that
# actually holds, and having both means the hook is a fast local echo of CI
# rather than a separate opinion.
#
# `sender/core` is deliberately absent: it has never been through rustfmt and
# has ~112 differences waiting, and reformatting it belongs in its own commit
# rather than in the firmware's gate.
step "firmware: rustfmt"
for crate in firmware/bridge-core firmware/rs485-bridge; do
    (cd "$crate" && cargo fmt --all -- --check)
done
echo "ok: both firmware crates are formatted"

step "firmware: clippy, with warnings as errors"
(cd firmware/bridge-core && cargo clippy --all-targets -- -D warnings)
# No --all-targets for the bare-metal crate: it pulls in test harnesses that
# want a `test` crate the target has no copy of, and the failure reads like a
# broken toolchain. Both feature sets, because the listen-only interlock only
# means something if the other one compiles too.
(cd firmware/rs485-bridge && cargo clippy -- -D warnings)
(cd firmware/rs485-bridge && cargo clippy --no-default-features \
    --features board-f407-discovery -- -D warnings)

step "bridge-core: the segmentation, the framing and the accounting"
(cd firmware/bridge-core && cargo test)

step "bridge-core: it builds for the part"
(cd firmware/bridge-core && cargo build --release --target thumbv7em-none-eabihf)

step "rs485-bridge: listen-only (the default, and what gets flashed)"
(cd firmware/rs485-bridge && cargo build --release)
python3 tools/image-size.py "$elf" --min-bytes 16384

# The interlock, checked rather than described. `listen-only` is meant to
# remove the transmit path from the BINARY, not to disable it at runtime, and
# a claim like that is worth a grep: with debug symbols kept, the task's name
# is in the ELF if and only if it was compiled.
step "rs485-bridge: the listen-only build contains no transmit path"
if grep -qa 'emco_rs485_bridge::transmit' "$elf"; then
    echo "FAIL: the listen-only build carries the transmit task." >&2
    echo "The whole basis of the interlock is that this code is absent." >&2
    exit 1
fi
echo "ok: no transmit symbol in the listen-only image"

step "rs485-bridge: the transmitting build, which is a deliberate act"
(cd firmware/rs485-bridge && cargo build --release --no-default-features \
    --features board-f407-discovery)
python3 tools/image-size.py "$elf" --min-bytes 16384
if ! grep -qa 'emco_rs485_bridge::transmit' "$elf"; then
    echo "FAIL: the transmitting build has no transmit task either." >&2
    echo "Then the check above proves nothing about the listen-only one." >&2
    exit 1
fi
echo "ok: the transmit symbol is present, so its absence above means something"

# Put the default back, so the tree is left holding the image somebody would
# actually flash rather than whichever one was built last.
(cd firmware/rs485-bridge && cargo build --release >/dev/null)

step "host: the decoder, and the corpus the Rust encoder wrote"
python3 tools/test_emco_bridge.py

step "host: the simulator end to end, through the real decoder"
python3 tools/virtual-bridge.py --stdout --polls 40 > "${TMPDIR:-/tmp}/bridge-capture.bin"
python3 tools/emco-capture.py --replay "${TMPDIR:-/tmp}/bridge-capture.bin" >/dev/null

printf '\n\033[1mAll checks that do not need a board have passed.\033[0m\n'
printf 'Untested until a board answers: USB enumeration, timestamp accuracy,\n'
printf 'DE turnaround, and every pin in firmware/rs485-bridge/src/board/.\n'
