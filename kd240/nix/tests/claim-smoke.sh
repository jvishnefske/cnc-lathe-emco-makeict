#!/usr/bin/env bash
# Smoke test of the claim path without a board: claim.sh --socket, socat's
# PTY, socat to the claim link's socket, claim-link.py and its PTY, with a
# stand-in node and a stand-in mwctl at the two ends (claim_fakes.py).
# Real mwctl cannot open a PTY yet (its port.rs fails on ENOTTY), and the
# pinned posix-node cannot either, so neither is used here.
#
#   kd240/nix/tests/claim-smoke.sh kd240/scripts/claim-link.py kd240/scripts/claim.sh
#
# Needs python3, socat, bash.
# shellcheck disable=SC2015
set -euo pipefail
daemon=${1:?usage: claim-smoke.sh CLAIM-LINK.PY CLAIM.SH}
claim=${2:?usage: claim-smoke.sh CLAIM-LINK.PY CLAIM.SH}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fakes=$here/claim_fakes.py

work=$(mktemp -d)
pids=()
cleanup() {
    for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT
fails=0
ok()   { echo "ok    $*"; }
fail() { echo "FAIL  $*"; fails=$((fails + 1)); }

dir=$work/emco-mesh-claim-unit
mkdir "$dir"
RUNTIME_DIRECTORY=$dir python3 -I "$daemon" 2>"$work/daemon.log" &
pids+=("$!")
for _ in $(seq 50); do [ -S "$dir/sock" ] && [ -e "$dir/tty" ] && break; sleep 0.1; done
[ -S "$dir/sock" ] && ok "the claim link is up" || { fail "the claim link did not come up"; cat "$work/daemon.log"; exit 1; }
python3 -I "$fakes" node "$dir/tty" "$work/claimed" &
pids+=("$!")
sleep 0.3

printf '#!/bin/sh\nexec python3 -I %s mwctl "$@"\n' "$fakes" >"$work/mwctl"
chmod +x "$work/mwctl"
echo k >"$work/owner.key"
export MWCTL=$work/mwctl

out=$("$claim" unit --socket "$dir/sock" --info 2>&1) && rc=0 || rc=$?
[ "$rc" -eq 0 ] && grep -q "trust: *unclaimed" <<<"$out" && ok "--info: the node reports unclaimed" \
    || fail "--info (rc $rc): $out"

out=$("$claim" unit --socket "$dir/sock" --key "$work/owner.key" 2>&1) && rc=0 || rc=$?
[ "$rc" -eq 0 ] && grep -q "== unit node claimed" <<<"$out" && ok "claim: the node is claimed" \
    || fail "claim (rc $rc): $out"
[ -s "$work/claimed" ] && ok "the claim reached the node" || fail "the node never saw the claim"

out=$("$claim" unit --socket "$dir/sock" --key "$work/owner.key" 2>&1) && rc=0 || rc=$?
[ "$rc" -eq 0 ] && grep -q "already claimed" <<<"$out" && ok "again: already claimed, left alone" \
    || fail "second claim (rc $rc): $out"

out=$("$claim" unit --socket "$dir/sock" 2>&1) && rc=0 || rc=$?
[ "$rc" -ne 0 ] && grep -q "give --key FILE or --pkcs11 LABEL" <<<"$out" && ok "no key: refused" \
    || fail "no key (rc $rc): $out"

out=$("$claim" unit --socket "$work/nope.sock" --info 2>&1) && rc=0 || rc=$?
[ "$rc" -ne 0 ] && grep -qi "no link\|trust info failed" <<<"$out" && ok "no claim link: a clear failure" \
    || fail "no claim link (rc $rc): $out"

# mwctl as it is today, on a PTY.
printf '#!/bin/sh\necho "mwctl: serial port: Not a typewriter" >&2; exit 1\n' >"$work/mwctl-enotty"
chmod +x "$work/mwctl-enotty"
out=$(MWCTL=$work/mwctl-enotty "$claim" unit --socket "$dir/sock" --info 2>&1) && rc=0 || rc=$?
[ "$rc" -ne 0 ] && grep -q "cannot open a PTY" <<<"$out" && ok "an mwctl that cannot open a PTY: says so" \
    || fail "ENOTTY hint (rc $rc): $out"

# The node kept being drained throughout: the daemon is still up.
kill -0 "${pids[0]}" && ok "the claim link is still up" || fail "the claim link died"
echo "$fails failure(s)"
[ "$fails" -eq 0 ]
