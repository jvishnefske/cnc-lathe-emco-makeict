#!/usr/bin/env bash
# ok and fail only echo, so `cond && ok … || fail …` is if-then-else here.
# shellcheck disable=SC2015
# Smoke test of bringup.sh's argument and board-input handling, with no
# board: ssh and scp are stand-ins (on PATH) that answer from a temp dir
# and record what they were asked to run.
#
#   kd240/nix/tests/bringup-smoke.sh kd240/scripts/bringup.sh kd240/scripts/emco-mesh-enroll
#
# Needs bash, coreutils, openssl.
set -euo pipefail
bringup=${1:?usage: bringup-smoke.sh BRINGUP.SH EMCO-MESH-ENROLL}
enroll=${2:?usage: bringup-smoke.sh BRINGUP.SH EMCO-MESH-ENROLL}
bringup=$(realpath "$bringup") enroll=$(realpath "$enroll")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fails=0
ok()   { echo "ok    $*"; }
fail() { echo "FAIL  $*"; fails=$((fails + 1)); }

mkdir -p "$work/bin" "$work/tmp"
# The stand-ins' interpreter is this bash (the Nix sandbox has no /usr/bin/env).
bash=$(command -v bash)
printf 'UNIT=0123456789abcdef\nPROCESS=1111111111111111\n' >"$work/ids"
# ssh: options and the host dropped, the command looked up; every command
# is logged one per line.
cat >"$work/bin/ssh" <<EOS
#!$bash
while [ \$# -gt 0 ]; do case "\$1" in -o) shift 2;; -*) shift;; *@*) shift; break;; *) break;; esac; done
cmd="\$*"
printf '%s\n' "\$cmd" >>"$work/ssh.log"
export EMCO_ENROLL_DIR=$work/tls EMCO_MESH_IDS=$work/ids
case "\$cmd" in
  true) exit 0;;
  "cat /run/emco-mesh/ids") cat "$work/ids";;
  "command -v emco-mesh-enroll") exit 0;;
  "sudo emco-mesh-enroll install "*) eval "set -- \${cmd#sudo emco-mesh-enroll install }"; printf '%s\n' "\$@" >"$work/install-args"; exit 1;;
  "sudo emco-mesh-enroll "*) exec "$enroll" \${cmd#sudo emco-mesh-enroll };;
  *) exit 1;;
esac
EOS
cat >"$work/bin/scp" <<EOS
#!$bash
printf '%s\n' "\$*" >>"$work/scp.log"
EOS
chmod +x "$work/bin/ssh" "$work/bin/scp"
export PATH=$work/bin:$PATH
cd "$work"

# ---- --host and --user: names only, refused before any ssh
for bad in "-oProxyCommand=x" "a b" "h;id" "" "user@h" "x/../y"; do
    : >"$work/ssh.log"
    out=$("$bringup" --step 8 --host "$bad" 2>&1) && rc=0 || rc=$?
    [ "$rc" -eq 2 ] && grep -q "host" <<<"$out" && [ ! -s "$work/ssh.log" ] \
        && ok "--host '$bad' refused, nothing run" || fail "--host '$bad' (rc $rc): $out"
    out=$("$bringup" --step 8 --user "$bad" 2>&1) && rc=0 || rc=$?
    [ "$rc" -eq 2 ] && grep -q "user" <<<"$out" && [ ! -s "$work/ssh.log" ] \
        && ok "--user '$bad' refused, nothing run" || fail "--user '$bad' (rc $rc): $out"
done

# ---- the board's ids: 16 hex digits, or nothing is built from them
out=$("$bringup" --step 8 --host fake 2>&1) && rc=0 || rc=$?
grep -q "kd240-0123456789abcdef.csr" <<<"$out" && ok "good ids: the CSR path names the unit id" \
    || fail "good ids (rc $rc): $out"
for bad in "../../etc/x" "0123456789ABCDEF" "0123" "0123456789abcdef0"; do
    printf 'UNIT=%s\nPROCESS=1111111111111111\n' "$bad" >"$work/ids"
    out=$("$bringup" --step 8 --host fake --apply 2>&1) && rc=0 || rc=$?
    [ "$rc" -eq 1 ] && grep -q "ids" <<<"$out" && [ -z "$(find "$work" -maxdepth 3 -name '*.csr')" ] \
        && ok "UNIT='$bad' refused, no file made" || fail "UNIT='$bad' (rc $rc): $out"
done
printf 'UNIT=0123456789abcdef\nPROCESS=../x\n' >"$work/ids"
out=$("$bringup" --step 8 --host fake 2>&1) && rc=0 || rc=$?
[ "$rc" -eq 1 ] && grep -q "ids" <<<"$out" && ok "PROCESS='../x' refused" || fail "PROCESS='../x' (rc $rc): $out"
printf 'UNIT=0123456789abcdef\nPROCESS=1111111111111111\n' >"$work/ids"

# ---- --cert with spaces and shell characters reaches the board as one word
# shellcheck disable=SC2016  # literal on purpose: it must never run
odd='my cert;$(touch pwned).crt'
cp /dev/null "$work/$odd"; cp /dev/null "$work/ca.pem"
"$bringup" --step 8 --host fake --apply --cert "$work/$odd" --ca "$work/ca.pem" >/dev/null 2>&1 || true
if [ -f "$work/install-args" ] && [ "$(head -1 "$work/install-args")" = "/tmp/$odd" ] && [ ! -e "$work/pwned" ]; then
    ok "the cert's name is quoted on the board's side"
else
    fail "cert name quoting: $(cat "$work/install-args" 2>/dev/null || echo 'no install')"
fi

echo "$fails failure(s)"
[ "$fails" -eq 0 ]
