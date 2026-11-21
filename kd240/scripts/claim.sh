#!/usr/bin/env bash
# claim.sh: claim one of the KD240's payload nodes with your mwctl key, over
# its claim link (services.emco-mesh.claimLink, BRINGUP.md step 2). Run on
# the laptop, once per node:
#
#   kd240/scripts/claim.sh unit    --host kd240.local --pkcs11 mw-release
#   kd240/scripts/claim.sh process --host kd240.local --key owner.key
#   kd240/scripts/claim.sh unit --info            # trust info only
#
# A claim never travels over Zenoh: it goes over the node's local serial
# link. On the board that link is a PTY made, and always drained, by a root
# unit per node (emco-mesh-claim-link-<node>), with a socket beside it that
# only root can use (/run/emco-mesh-claim-<node>/sock: root 0600, and the
# daemon checks each client's uid).
# This script makes a PTY here with socat, joined over `ssh … sudo socat` to
# that socket, and runs mwctl on it:
#
#   mwctl --port <pty> trust info                      unclaimed?
#   mwctl --port <pty> trust claim --key K | --pkcs11 L
#   mwctl --port <pty> trust info                      now claimed
#
# Afterwards the node takes signed installs over Zenoh (bring-up step 3).
# A node that is already claimed is left alone.
#
# Needs here: mwctl, socat, ssh (a key for the board's admin user, which
# has passwordless sudo). On the board: socat (in the image with the
# claim link). MWCTL and SOCAT override the commands.
set -euo pipefail

HELP="usage: claim.sh <unit|process> [options]

  --host HOST      the board [kd240.local]
  --user USER      its SSH user [admin]
  --key FILE       claim with this mwctl key file
  --pkcs11 LABEL   claim with this PKCS#11 key (mwctl key new --pkcs11 LABEL)
  --info           only print the node's trust info
  --socket PATH    a claim-link socket on this machine instead of ssh
                   (on the board itself, or for tests)
  -h, --help       this text
"

mwctl=${MWCTL:-mwctl}
socat=${SOCAT:-socat}
which=""
host=kd240.local
user="admin"
key=""
info="" out=""
pkcs11=""
info_only=""
socket=""

while [ $# -gt 0 ]; do
    case "$1" in
        unit | process) which=$1; shift ;;
        --host) host=${2?--host needs a host}; shift 2 ;;
        --user) user=${2?--user needs a user}; shift 2 ;;
        --key) key=${2:?--key needs a file}; shift 2 ;;
        --pkcs11) pkcs11=${2:?--pkcs11 needs a label}; shift 2 ;;
        --info) info_only=yes; shift ;;
        --socket) socket=${2:?--socket needs a path}; shift 2 ;;
        -h | --help) printf '%s' "$HELP"; exit 0 ;;
        *) printf 'claim.sh: unknown argument %s\n\n%s' "$1" "$HELP" >&2; exit 2 ;;
    esac
done
die() { printf 'claim.sh: %s\n' "$*" >&2; exit 1; }
# --host and --user go to ssh: a name, never an option or a shell word.
valid_name() { case "$1" in "" | -* | *[!A-Za-z0-9._-]*) return 1 ;; esac; }
for v in "host:$host" "user:$user"; do
    if ! valid_name "${v#*:}"; then
        printf 'claim.sh: --%s must be letters, digits, . _ - (not starting with -): %s\n' "${v%%:*}" "'${v#*:}'" >&2
        exit 2
    fi
done
[ -n "$which" ] || { printf 'claim.sh: which node: unit or process\n\n%s' "$HELP" >&2; exit 2; }
if [ -n "$key" ] && [ -n "$pkcs11" ]; then die "--key and --pkcs11: give one"; fi
if [ -z "$info_only" ] && [ -z "$key$pkcs11" ]; then die "give --key FILE or --pkcs11 LABEL (or --info)"; fi
if [ -n "$key" ] && [ ! -r "$key" ]; then die "$key: no such key file"; fi
for t in "$mwctl" "$socat"; do command -v "$t" >/dev/null || die "$t not found"; done

remote_sock=/run/emco-mesh-claim-$which/sock
# socat reads `:` and `,` in an EXEC address as its own separators, hence \:.
if [ -n "$socket" ]; then
    case "$socket" in *[:,]*) die "--socket: a path without : or , please" ;; esac
    link=(EXEC:"$socat - UNIX-CONNECT\:$socket")
    where=$socket
else
    command -v ssh >/dev/null || die "ssh not found"
    link=(EXEC:"ssh -T -o BatchMode=yes -o ConnectTimeout=5 $user@$host sudo socat - UNIX-CONNECT\:$remote_sock")
    where="$user@$host:$remote_sock"
fi

tmp=$(mktemp -d)
pid=""
cleanup() {
    if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
    rm -rf "$tmp"
}
trap cleanup EXIT
port=$tmp/$which.tty

"$socat" PTY,link="$port",rawer,echo=0 "${link[@]}" 2>"$tmp/socat.log" &
pid=$!
for _ in $(seq 50); do
    [ -e "$port" ] && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
done
if [ ! -e "$port" ] || ! kill -0 "$pid" 2>/dev/null; then
    cat "$tmp/socat.log" >&2
    die "no link to $where (is the claim link on? BRINGUP.md step 2)"
fi

# mwctl RESULT-VAR ARGS...: mwctl on the PTY; its output in RESULT-VAR.
run_mwctl() {
    local __var=$1 __out __rc=0
    shift
    __out=$("$mwctl" --port "$port" --timeout 3000 "$@" 2>&1) || __rc=$?
    printf -v "$__var" '%s' "$__out"
    if [ "$__rc" -ne 0 ]; then
        printf '%s\n' "$__out" >&2
        case "$__out" in
            *"Not a typewriter"*)
                echo "claim.sh: this mwctl cannot open a PTY: it deasserts RTS/DTR and fails with ENOTTY" >&2
                echo "  (middleware tools/mwctl/src/port.rs; needs the same fix as the gateway's open_serial, bc80998)" >&2 ;;
        esac
        if ! kill -0 "$pid" 2>/dev/null; then cat "$tmp/socat.log" >&2; fi
        return "$__rc"
    fi
}

echo "== $which node, over $where"
run_mwctl info trust info || die "trust info failed"
printf '%s\n' "$info"
[ -z "$info_only" ] || exit 0

state=$(printf '%s\n' "$info" | sed -n 's/^trust: *//p' | head -1)
case "$state" in
    unclaimed*) ;;
    "claimed, re-key window open"*) echo "re-key window open: claiming again" ;;
    claimed*)
        echo "already claimed: left alone (a new claim needs the re-key window, or a reset of its state)"
        exit 0 ;;
    *) die "unexpected trust info (no 'trust:' line)" ;;
esac

if [ -n "$key" ]; then signer=(--key "$key"); else signer=(--pkcs11 "$pkcs11"); fi
run_mwctl out trust claim "${signer[@]}" || die "trust claim failed"
printf '%s\n' "$out"
run_mwctl info trust info || die "trust info after the claim failed"
printf '%s\n' "$info"
case "$(printf '%s\n' "$info" | sed -n 's/^trust: *//p' | head -1)" in
    claimed*) echo "== $which node claimed" ;;
    *) die "the node does not report itself claimed" ;;
esac
