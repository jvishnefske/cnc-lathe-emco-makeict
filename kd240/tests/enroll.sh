#!/usr/bin/env bash
# `cond && ok … || fail …` throughout: ok only echoes, so fail runs exactly
# when the condition is false.
# shellcheck disable=SC2015
# Tests for kd240/scripts/emco-mesh-enroll (W4b), with a throwaway CA that
# signs as `edge-ca issue --ou kd240 <id>` does: CN kd240-<id>, OU kd240,
# EKU clientAuth, from the board's CSR. No root, no board, no network: the
# script runs on a temp dir (EMCO_ENROLL_DIR) with a fake ids file
# (EMCO_MESH_IDS) and machine id (EMCO_MACHINE_ID).
#
#   kd240/tests/enroll.sh kd240/scripts/emco-mesh-enroll
#   nix build .#checks.x86_64-linux.kd240-enroll -L    (the same, packaged)
#
# Needs openssl 3.4 or later (x509 -not_before/-not_after), bash, coreutils.
set -euo pipefail

enroll=${1:?usage: enroll.sh PATH-TO-emco-mesh-enroll}
[ -x "$enroll" ] || { echo "not executable: $enroll" >&2; exit 2; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fails=0
ok()   { echo "ok    $*"; }
fail() { echo "FAIL  $*"; fails=$((fails + 1)); }

ID=0123456789abcdef
OTHER=fedcba9876543210
export EMCO_ENROLL_DIR=$work/tls
export EMCO_MESH_IDS=$work/ids
export EMCO_MACHINE_ID=$work/machine-id
printf 'UNIT=%s\nPROCESS=%s\n' "$ID" 1111111111111111 >"$EMCO_MESH_IDS"
printf '%s\n' 0123456789abcdef0123456789abcdef >"$EMCO_MACHINE_ID"
dir=$EMCO_ENROLL_DIR

# run ARGS...: the script's status in $rc, its stdout in $out, stderr in $err.
run() {
    rc=0
    "$enroll" "$@" >"$work/out" 2>"$work/err" || rc=$?
    out=$(cat "$work/out"); err=$(cat "$work/err")
}
# expect_fail WHAT PATTERN ARGS...: non-zero, stderr matching PATTERN.
expect_fail() {
    local what=$1 pat=$2; shift 2
    run "$@"
    if [ "$rc" -ne 0 ] && grep -qiE "$pat" <<<"$err"; then
        ok "$what"
    else
        fail "$what (rc $rc, stderr: $(head -3 <<<"$err" | paste -sd'|' -))"
    fi
}
sum() { sha256sum "$1" | cut -d' ' -f1; }

# ---- CAs and signing ------------------------------------------------------
c=$work/ca
mkdir -p "$c"
ca() {
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 \
        -subj "/CN=$1" -keyout "$c/$1.key" -out "$c/$1.crt" \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
}
ca zenoh-ca
ca other-ca
printf '%s\n' "basicConstraints=critical,CA:FALSE" "keyUsage=critical,digitalSignature" \
    "extendedKeyUsage=clientAuth" >"$c/leaf.ext"
# sign CSR OUT [ca] [cn] [x509 date args...]: as edge-ca does, the subject
# is the CA's choice and only the CSR's public key is kept.
sign() {
    local csr=$1 crt=$2 ca=${3:-zenoh-ca} cn=${4:-kd240-$ID}
    shift 4 2>/dev/null || shift $#
    local dates=("$@")
    [ ${#dates[@]} -gt 0 ] || dates=(-days 365)
    openssl x509 -req -in "$csr" -CA "$c/$ca.crt" -CAkey "$c/$ca.key" -CAcreateserial \
        -set_subject "/O=vishnefske-edge/OU=kd240/CN=$cn" -extfile "$c/leaf.ext" \
        "${dates[@]}" -out "$crt" 2>/dev/null
}
ts() { date -u -d "$1" +%Y%m%d%H%M%SZ; }

# ---- usage ----------------------------------------------------------------
run
[ "$rc" -eq 2 ] && ok "no command: usage, exit 2" || fail "no command: exit $rc"
run frobnicate
[ "$rc" -eq 2 ] && ok "unknown command: exit 2" || fail "unknown command: exit $rc"

# ---- check before anything ------------------------------------------------
expect_fail "check with nothing enrolled fails" "no key" check

# ---- csr --------------------------------------------------------------------
run csr
if [ "$rc" -eq 0 ]; then ok "csr exits 0"; else fail "csr: rc $rc: $err"; fi
printf '%s\n' "$out" >"$work/board.csr"
if openssl req -in "$work/board.csr" -noout -verify >/dev/null 2>&1; then
    ok "csr prints a CSR, self-signed by the key"
else
    fail "csr's stdout is not a CSR"
fi
subj=$(openssl req -in "$work/board.csr" -noout -subject -nameopt RFC2253 2>/dev/null || true)
grep -q "CN=kd240-$ID" <<<"$subj" && ok "CSR subject CN kd240-<unit id> ($subj)" || fail "CSR subject: $subj"
cmp -s "$work/board.csr" "$dir/csr.pem" && ok "the CSR is kept in csr.pem" || fail "csr.pem differs from stdout"
head -1 "$dir/key" | grep -q -- "-----BEGIN PRIVATE KEY-----" && ok "key is PKCS#8 PEM" || fail "key is not PKCS#8"
openssl pkey -in "$dir/key" -noout -text 2>/dev/null | grep -qE "prime256v1|P-256" \
    && ok "key is EC P-256" || fail "key is not P-256"
[ "$(stat -c %a "$dir/key")" = 640 ] && ok "key mode 0640" || fail "key mode $(stat -c %a "$dir/key")"
[ "$(stat -c %a "$dir")" = 750 ] && ok "dir mode 0750" || fail "dir mode $(stat -c %a "$dir")"
grep -q "edge-ca issue --ou kd240 $ID --csr kd240-$ID.csr --out kd240-$ID.crt" <<<"$err" \
    && ok "csr prints the edge-ca command" || fail "csr's next steps: $err"
grep -q "emco-mesh-enroll install /tmp/kd240-$ID.crt /tmp/zenoh-ca.pem" <<<"$err" \
    && ok "csr prints the install command" || fail "csr's install step: $err"

key1=$(sum "$dir/key")
run csr
[ "$rc" -eq 0 ] && [ "$(sum "$dir/key")" = "$key1" ] && ok "csr again keeps the key" || fail "csr again changed the key (rc $rc)"
printf '%s\n' "$out" >"$work/board2.csr"
diff <(openssl req -in "$work/board.csr" -noout -pubkey) <(openssl req -in "$work/board2.csr" -noout -pubkey) >/dev/null \
    && ok "the second CSR is for the same key" || fail "the second CSR has another key"

run csr --id "$OTHER"
grep -q "CN=kd240-$OTHER" <<<"$(openssl req -noout -subject -nameopt RFC2253 2>/dev/null <<<"$out")" \
    && ok "csr --id names that id" || fail "csr --id"
expect_fail "csr --id rejects a malformed id" "id" csr --id NOT-HEX

# ---- install: rejections first, with nothing installed -------------------
sign "$work/board.csr" "$work/good.crt"
cp "$c/zenoh-ca.crt" "$work/ca.pem"

openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=x" \
    -keyout "$work/stranger.key" -out "$work/stranger.csr" 2>/dev/null
sign "$work/stranger.csr" "$work/wrongkey.crt"
expect_fail "install: a cert for another key" "key" install "$work/wrongkey.crt" "$work/ca.pem" --no-restart
sign "$work/board.csr" "$work/otherca.crt" other-ca
expect_fail "install: a cert from another CA" "CA|chain|verify" install "$work/otherca.crt" "$work/ca.pem" --no-restart
sign "$work/board.csr" "$work/expired.crt" zenoh-ca "kd240-$ID" -not_before "$(ts '-10 days')" -not_after "$(ts '-1 day')"
expect_fail "install: an expired cert" "expired" install "$work/expired.crt" "$work/ca.pem" --no-restart
sign "$work/board.csr" "$work/future.crt" zenoh-ca "kd240-$ID" -not_before "$(ts '+2 days')" -not_after "$(ts '+30 days')"
expect_fail "install: a not-yet-valid cert says to check the clock" "not valid until.*clock" \
    install "$work/future.crt" "$work/ca.pem" --no-restart
sign "$work/board.csr" "$work/wrongcn.crt" zenoh-ca "kd240-$OTHER"
expect_fail "install: a cert for another board's CN" "kd240-$OTHER.*kd240-$ID" \
    install "$work/wrongcn.crt" "$work/ca.pem" --no-restart
printf 'not a certificate\n' >"$work/junk.crt"
expect_fail "install: not a certificate" "not a PEM certificate" install "$work/junk.crt" "$work/ca.pem" --no-restart
expect_fail "install: the CA file missing" "no such|missing|not found" install "$work/good.crt" "$work/nope.pem" --no-restart
expect_fail "install: no arguments" "usage" install
if [ ! -e "$dir/cert" ] && [ ! -e "$dir/ca" ]; then
    ok "rejected installs left nothing behind"
else
    fail "a rejected install wrote $(ls "$dir")"
fi
[ -z "$(find "$dir" -name '.install*')" ] && ok "no temp files left" || fail "temp files left in $dir"

# ---- install: the happy path ---------------------------------------------
run install "$work/good.crt" "$work/ca.pem" --no-restart
if [ "$rc" -eq 0 ]; then ok "install a good cert"; else fail "install a good cert: rc $rc: $err"; fi
cmp -s "$dir/cert" "$work/good.crt" && cmp -s "$dir/ca" "$work/ca.pem" && ok "cert and ca in place" || fail "cert/ca not in place"
[ "$(stat -c %a "$dir/cert")" = 644 ] && [ "$(stat -c %a "$dir/ca")" = 644 ] && ok "cert and ca mode 0644" \
    || fail "modes: cert $(stat -c %a "$dir/cert") ca $(stat -c %a "$dir/ca")"
run check
[ "$rc" -eq 0 ] && ok "check passes after install" || fail "check after install: rc $rc: $err"
run status
grep -q "kd240-$ID" <<<"$out$err" && ok "status shows the CN" || fail "status: $out $err"

# A rejected install keeps what is there.
expect_fail "install over a good one: another CA" "CA|chain|verify" install "$work/otherca.crt" "$work/ca.pem" --no-restart
cmp -s "$dir/cert" "$work/good.crt" && ok "a rejected install kept the good cert" || fail "a rejected install replaced the cert"

# ---- check ---------------------------------------------------------------
sign "$work/board.csr" "$work/soon.crt" zenoh-ca "kd240-$ID" -not_before "$(ts '-1 day')" -not_after "$(ts '+10 days')"
run install "$work/soon.crt" "$work/ca.pem" --no-restart
run check
if [ "$rc" -eq 0 ] && grep -qi "expires in" <<<"$err"; then ok "check warns under 30 days, exit 0"; else fail "check near expiry: rc $rc: $err"; fi
run install "$work/good.crt" "$work/ca.pem" --no-restart

printf 'UNIT=%s\n' "$OTHER" >"$EMCO_MESH_IDS"
expect_fail "check: the unit id changed (reflash)" "kd240-$ID.*kd240-$OTHER" check
printf 'UNIT=%s\n' "$ID" >"$EMCO_MESH_IDS"

mv "$dir/cert" "$work/saved.crt"
expect_fail "check: cert missing" "no certificate" check
mv "$work/saved.crt" "$dir/cert"
mv "$dir/ca" "$work/saved.ca"
expect_fail "check: ca missing" "no CA" check
mv "$work/saved.ca" "$dir/ca"
cp "$work/expired.crt" "$dir/cert"
expect_fail "check: an expired cert" "expired" check
cp "$work/future.crt" "$dir/cert"
expect_fail "check: a not-yet-valid cert says to check the clock" "clock" check
cp "$work/good.crt" "$dir/cert"
run check
[ "$rc" -eq 0 ] && ok "check passes again" || fail "check: rc $rc: $err"
run check --dir "$work/empty"
[ "$rc" -ne 0 ] && ok "check --dir names another dir" || fail "check --dir ignored"

# ---- the id from the machine id, when /run/emco-mesh/ids is missing -------
rm "$EMCO_MESH_IDS"
want=$(printf '%s\0%s' mw-pi-node/node-id/v1 0123456789abcdef0123456789abcdef | sha256sum | cut -c1-16)
run csr
grep -q "CN=kd240-$want" <<<"$(openssl req -noout -subject -nameopt RFC2253 2>/dev/null <<<"$out")" \
    && ok "with no ids file, the id is mesh.nix's from the machine id ($want)" || fail "machine-id fallback: $err"
printf 'UNIT=%s\n' "$ID" >"$EMCO_MESH_IDS"

# ---- rotate -----------------------------------------------------------------
key1=$(sum "$dir/key")
run csr --rotate
if [ "$rc" -eq 0 ] && [ "$(sum "$dir/key")" != "$key1" ] && [ "$(sum "$dir/key.old")" = "$key1" ]; then
    ok "csr --rotate: a new key, the old one in key.old"
else
    fail "csr --rotate (rc $rc)"
fi
[ "$(stat -c %a "$dir/key")" = 640 ] && ok "rotated key mode 0640" || fail "rotated key mode"
expect_fail "after rotate, the old cert no longer matches the key" "key" check

# ---- root -------------------------------------------------------------------
if [ "$(id -u)" -ne 0 ]; then
    rc=0
    env -u EMCO_ENROLL_DIR "$enroll" csr >/dev/null 2>"$work/err" || rc=$?
    if [ "$rc" -ne 0 ] && grep -qi root "$work/err"; then ok "csr on the real dir needs root"; else fail "csr as a user (rc $rc)"; fi
fi

echo "$fails failure(s)"
[ "$fails" -eq 0 ]
