#!/usr/bin/env bash
# ok and fail only echo, so `cond && ok … || fail …` is if-then-else here.
# shellcheck disable=SC2015
# Level 3 of the uplink's checks (W4b): the board's two routers and a mesh
# router, as real zenohd processes on this machine, with throwaway certs.
#
#   A<n>  the board's local zenohd: the golden config (zenohd-local.golden.json),
#         its port moved
#   B<n>  emco-mesh-uplink: the generated config (kd240-uplink-config's
#         uplink.json), pointed at A<n> and at C, its TLS files here
#   C     the mesh router: an mTLS listener, and either no ACL (B's ACL
#         alone is under test) or the mesh router's own (W3c's, as in
#         homelab-flux mesh/zenohd.json5, or that file itself: CLOUD=)
#
# Boards: 1 and 2 enrolled, 3 with a cert from another CA, 4 with none.
# The behaviour cases are uplink_chain.py's. The B's start before C, so
# the first answer also shows B's retry; then C restarts and B must link
# again by itself.
#
#   PY=venv/bin/python kd240/nix/tests/uplink-chain.sh
#
# Needs: zenohd (ZENOHD=, or on PATH), a Python with eclipse-zenoh of the
# same version (PY=; python3 -m venv V && V/bin/pip install
# eclipse-zenoh==1.10.1), openssl, jq, nix (for uplink.json, or UPLINK=).
# KEEP=1 keeps the work dir; WORK=DIR puts it there (absent or empty).
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../../.." && pwd)
zenohd=${ZENOHD:-$(command -v zenohd || true)}
py=${PY:?set PY to a python with eclipse-zenoh (see the header)}
[ -x "$zenohd" ] || { echo "zenohd not found: set ZENOHD" >&2; exit 2; }
for t in openssl jq; do command -v $t >/dev/null || { echo "$t not found" >&2; exit 2; }; done
uplink=${UPLINK:-$(nix build --no-link --print-out-paths "$repo#checks.x86_64-linux.kd240-uplink-config")/uplink.json}
golden=$here/zenohd-local.golden.json
cloud_file=${CLOUD:-}

created=0
if [ -n "${WORK:-}" ]; then
    work=$WORK
    if [ -e "$work" ]; then
        [ -d "$work" ] && [ -z "$(ls -A "$work")" ] || { echo "WORK=$work is not an empty directory" >&2; exit 2; }
    else
        mkdir -p "$work"; created=1
    fi
else
    work=$(mktemp -d); created=1
fi
chmod 700 "$work"
declare -A pid=()
cleanup() {
    for p in "${pid[@]}"; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    if [ "${KEEP:-}" = 1 ]; then echo "kept $work"
    elif [ "$created" = 1 ]; then rm -rf "$work"
    else find "$work" -mindepth 1 -delete; fi
}
trap cleanup EXIT
fails=0
ok()   { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; fails=$((fails + 1)); }

N1=00000000000000b1 N2=00000000000000b2 N3=00000000000000b3 N4=00000000000000b4
PA1=17470 PA2=17471 PA3=17472 PA4=17473 PC=17474 PWS=17475

# ---- certificates ---------------------------------------------------------
c=$work/certs
mkdir -p "$c"
ca() {
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 \
        -subj "/CN=$1" -keyout "$c/$1.key" -out "$c/$1.crt" \
        -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
}
leaf() { # leaf CA CN EKU [SAN]: CN as edge-ca issues it
    {
        echo "basicConstraints=critical,CA:FALSE"
        echo "keyUsage=critical,digitalSignature"
        echo "extendedKeyUsage=$3"
        if [ -n "${4:-}" ]; then echo "subjectAltName=$4"; fi
    } >"$c/$2.ext"
    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/O=vishnefske-edge/OU=${2%%-*}/CN=$2" \
        -keyout "$c/$2.key" -out "$c/$2.csr" 2>/dev/null
    openssl x509 -req -in "$c/$2.csr" -CA "$c/$1.crt" -CAkey "$c/$1.key" -CAcreateserial -days 2 \
        -extfile "$c/$2.ext" -out "$c/$2.crt" 2>/dev/null
}
ca zenoh-ca
ca other-ca
leaf zenoh-ca server serverAuth DNS:localhost
for cn in "kd240-$N1" "kd240-$N2" "kd240-$N4" tool-t1 observer-t1; do leaf zenoh-ca "$cn" clientAuth; done
leaf other-ca "kd240-$N3" clientAuth

# The board's TLS dir as emco-mesh-enroll leaves it: ca, cert, key.
tlsdir() { # tlsdir N [CN]: CN's cert and key with the Zenoh CA; no CN: empty
    local d=$work/tls-$1
    mkdir -p "$d"
    if [ -n "${2:-}" ]; then
        cp "$c/zenoh-ca.crt" "$d/ca"; cp "$c/$2.crt" "$d/cert"; cp "$c/$2.key" "$d/key"
    fi
    printf '%s' "$d"
}

# ---- configs -----------------------------------------------------------------
for n in 1 2 3 4; do
    port=$((PA1 + n - 1))
    jq -c --arg e "tcp/127.0.0.1:$port" '.listen.endpoints = [$e]' "$golden" >"$work/a$n.json"
    # Only the port differs from the golden config.
    diff <(jq -S 'del(.listen)' "$golden") <(jq -S 'del(.listen)' "$work/a$n.json") >/dev/null \
        || { echo "A$n differs from the golden config" >&2; exit 1; }
done
b_conf() { # b_conf N TLS-DIR
    jq --arg a "tcp/127.0.0.1:$((PA1 + $1 - 1))" --arg c "tls/localhost:$PC" --arg d "$2" '
      .connect.endpoints = [$a, $c]
      | .transport.link.tls.root_ca_certificate = "\($d)/ca"
      | .transport.link.tls.connect_certificate = "\($d)/cert"
      | .transport.link.tls.connect_private_key = "\($d)/key"' "$uplink" >"$work/b$1.json"
}
b_conf 1 "$(tlsdir 1 "kd240-$N1")"
b_conf 2 "$(tlsdir 2 "kd240-$N2")"
b_conf 3 "$(tlsdir 3 "kd240-$N3")"
b_conf 4 "$(tlsdir 4)"
# B is the generated config, but for those endpoints and paths.
for n in 1 2 3 4; do
    diff <(jq -S 'del(.connect.endpoints, .transport.link.tls)' "$uplink") \
         <(jq -S 'del(.connect.endpoints, .transport.link.tls)' "$work/b$n.json") >/dev/null \
        || { echo "B$n differs from uplink.json beyond endpoints and paths" >&2; exit 1; }
done

# C: the mesh router. Its ACL follows W3c's for kd240 devices and tools,
# plus a test observer that may subscribe to and receive app/**, so that a
# put the ACLs wrongly let out is seen.
cloud_conf() { # cloud_conf none|on > file
    jq -n --arg d "$work/tls-c" --arg l "tls/127.0.0.1:$PC" --arg mode "$1" \
        --arg k1 "kd240-$N1" --arg k2 "kd240-$N2" --arg k3 "kd240-$N3" --arg k4 "kd240-$N4" '
    def sys($k): "@mw/node/*/sys/" + $k;
    {
      mode: "router",
      listen: { endpoints: [$l] },
      scouting: { multicast: { enabled: false }, gossip: { enabled: false } },
      plugins_loading: { enabled: false },
      transport: { link: { tls: {
        root_ca_certificate: "\($d)/ca", listen_certificate: "\($d)/cert", listen_private_key: "\($d)/key",
        enable_mtls: true, close_link_on_expiration: true } } },
      access_control: (if $mode == "none" then { enabled: false } else {
        enabled: true, default_permission: "deny",
        rules: [
          { id: "deny-unit-writes", permission: "deny", flows: ["ingress", "egress"], messages: ["put", "delete"], key_exprs: ["app/unit/**"] },
          { id: "deny-claim", permission: "deny", flows: ["ingress", "egress"], messages: ["query", "put"], key_exprs: [sys("trust/claim")] },
          { id: "kd-announce", permission: "allow", flows: ["ingress"], messages: ["declare_queryable"], key_exprs: [sys("**")] },
          { id: "kd-queries-in", permission: "allow", flows: ["egress"], messages: ["query"], key_exprs: [sys("**")] },
          { id: "kd-replies", permission: "allow", flows: ["ingress"], messages: ["reply"], key_exprs: [sys("**")] },
          { id: "kd-publish", permission: "allow", flows: ["ingress"], messages: ["put"], key_exprs: ["app/proc/*/state", "app/telemetry/**"] },
          { id: "kd-subscribers", permission: "allow", flows: ["egress"], messages: ["declare_subscriber"], key_exprs: ["app/proc/*/state", "app/telemetry/**"] },
          { id: "tool-query", permission: "allow", flows: ["ingress"], messages: ["query"], key_exprs: [
              "info", "board", "manifest", "manifest/cm", "peers", "trust/info", "payload/status", "payload/binding",
              "loader/begin", "loader/chunk", "loader/commit", "payload/start", "payload/stop", "payload/bind",
              "fw/begin", "fw/chunk", "fw/commit", "fw/reboot", "fw/confirm", "fw/abort" | sys(.)] },
          { id: "asker-see", permission: "allow", flows: ["egress"], messages: ["declare_queryable", "reply"], key_exprs: [sys("**")] },
          { id: "tool-subscribe", permission: "allow", flows: ["ingress"], messages: ["declare_subscriber"], key_exprs: ["app/proc/*/state", "app/telemetry/**"] },
          { id: "tool-receive", permission: "allow", flows: ["egress"], messages: ["put"], key_exprs: ["app/proc/*/state", "app/telemetry/**"] },
          { id: "test-observe-sub", permission: "allow", flows: ["ingress"], messages: ["declare_subscriber"], key_exprs: ["app/**"] },
          { id: "test-observe-recv", permission: "allow", flows: ["egress"], messages: ["put"], key_exprs: ["app/**"] }
        ],
        subjects: [
          { id: "kd240", link_protocols: ["tls"], cert_common_names: [$k1, $k2, $k3, $k4] },
          { id: "tool", link_protocols: ["tls"], cert_common_names: ["tool-t1"] },
          { id: "observer", link_protocols: ["tls"], cert_common_names: ["observer-t1"] }
        ],
        policies: [
          { id: "p-kd240", subjects: ["kd240"], rules: ["kd-announce", "kd-queries-in", "kd-replies", "kd-publish", "kd-subscribers", "deny-unit-writes", "deny-claim"] },
          { id: "p-tool", subjects: ["tool"], rules: ["tool-query", "asker-see", "tool-subscribe", "tool-receive", "deny-unit-writes", "deny-claim"] },
          { id: "p-observer", subjects: ["observer"], rules: ["test-observe-sub", "test-observe-recv"] }
        ]
      } end)
    }'
}
mkdir -p "$work/tls-c"
cp "$c/zenoh-ca.crt" "$work/tls-c/ca"; cp "$c/server.crt" "$work/tls-c/cert"; cp "$c/server.key" "$work/tls-c/key"
cloud_conf none >"$work/c-none.json"
cloud_conf on >"$work/c-on.json"
if [ -n "$cloud_file" ]; then
    # The mesh router's own file, rewritten as its check.sh does: files and
    # ports here, the test CNs in its lists, the observer added.
    sed -e "s#/etc/mesh/tls#$work/tls-c#g" -e "s#0.0.0.0:7447#127.0.0.1:$PC#" -e "s#0.0.0.0:8000#127.0.0.1:$PWS#" \
        -e "s#\\(id: \"kd240\",.*cert_common_names: \\[\\)#\\1 \"kd240-$N1\", \"kd240-$N2\", \"kd240-$N3\", \"kd240-$N4\",#" \
        -e 's#\(id: "tool",.*cert_common_names: \[\)#\1 "tool-t1",#' \
        -e 's#^    rules: \[$#&\n      { id: "test-observe-sub", permission: "allow", flows: ["ingress"], messages: ["declare_subscriber"], key_exprs: ["app/**"] },\n      { id: "test-observe-recv", permission: "allow", flows: ["egress"], messages: ["put"], key_exprs: ["app/**"] },#' \
        -e 's#^    subjects: \[$#&\n      { id: "test-observer", link_protocols: ["tls"], cert_common_names: ["observer-t1"] },#' \
        -e 's#^    policies: \[$#&\n      { id: "p-test-observer", subjects: ["test-observer"], rules: ["test-observe-sub", "test-observe-recv"] },#' \
        "$cloud_file" >"$work/c-file.json5"
    [ "$(grep -c 'test-observe\|kd240-0000000000000\|tool-t1' "$work/c-file.json5")" -ge 6 ] \
        || { echo "CLOUD=$cloud_file: could not add the test CNs and observer" >&2; exit 2; }
fi

# ---- processes -----------------------------------------------------------------
listening() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
start() { # start NAME CONFIG
    RUST_LOG=${RUST_LOG:-info} "$zenohd" --config "$2" >>"$work/$1.log" 2>&1 &
    pid[$1]=$!
}
stop() { kill "${pid[$1]}" 2>/dev/null || true; wait "${pid[$1]}" 2>/dev/null || true; unset "pid[$1]"; }
alive() { kill -0 "${pid[$1]}" 2>/dev/null; }

"$zenohd" --version 2>/dev/null | grep -o 'zenohd v[0-9.]*' | tail -1 || true
for n in 1 2 3 4; do start "a$n" "$work/a$n.json"; done
for n in 1 2 3 4; do for _ in $(seq 50); do listening $((PA1 + n - 1)) && break; sleep 0.1; done; done
for n in 1 2 3 4; do start "b$n" "$work/b$n.json"; done
sleep 2

# ---- before C exists ---------------------------------------------------------
for n in 1 2 3; do
    if alive "b$n"; then ok "B$n runs with the mesh router down (retrying)"; else fail "B$n exited:"; tail -5 "$work/b$n.log"; fi
done
lst=""
for n in 1 2 3; do lst+=$(ss -Hltnp 2>/dev/null | grep "pid=${pid[b$n]}," || true); done
[ -z "$lst" ] && ok "the B's listen on nothing" || fail "a B listens: $lst"
grep -qi "access control not enabled" "$work/b1.log" && fail "B1's ACL is not enabled" || ok "B1's ACL is enabled"

run_cases() { # run_cases LABEL C-CONFIG
    local label=$1 conf=$2
    echo "--- $label"
    [ -n "${pid[c]:-}" ] && stop c
    start c "$conf"
    for _ in $(seq 50); do listening $PC && break; sleep 0.1; done
    alive c || { fail "$label: C exited:"; tail -20 "$work/c.log"; return; }
    local args=(--certs "$c" --a1 "tcp/127.0.0.1:$PA1" --a2 "tcp/127.0.0.1:$PA2" --a3 "tcp/127.0.0.1:$PA3"
                --a4 "tcp/127.0.0.1:$PA4" --c "tls/localhost:$PC")
    "$py" -I "$here/uplink_chain.py" --case main "${args[@]}" && ok "$label: main cases" || fail "$label: main cases"
    "$py" -I "$here/uplink_chain.py" --case other-ca "${args[@]}" && ok "$label: other CA" || fail "$label: other CA"
    "$py" -I "$here/uplink_chain.py" --case no-cert "${args[@]}" && ok "$label: no cert" || fail "$label: no cert"
    stop c
    sleep 1
    start c "$conf"
    for _ in $(seq 50); do listening $PC && break; sleep 0.1; done
    "$py" -I "$here/uplink_chain.py" --case reconnect "${args[@]}" && ok "$label: B reconnects after C restarts" \
        || fail "$label: reconnect"
}

run_cases "C without an ACL: B's ACL alone" "$work/c-none.json"
run_cases "C with the mesh router's ACL (W3c's rules)" "$work/c-on.json"
[ -n "$cloud_file" ] && run_cases "C on $cloud_file itself" "$work/c-file.json5"

# ---- afterwards ------------------------------------------------------------------
for n in 1 2 3 4; do alive "a$n" && ok "A$n still up" || fail "A$n died"; done
alive b1 && ok "B1 still up" || fail "B1 died"
if grep -qiE "unknown ?ca|invalid peer certificate|UnknownIssuer|BadCertificate" "$work/b3.log" "$work/c.log" 2>/dev/null; then
    ok "B3's other-CA cert is refused in the TLS handshake"
else
    fail "no TLS refusal of B3's cert in the logs"; grep -i -m3 'tls\|cert' "$work/b3.log" || true
fi
if alive b4; then echo "      B4 (no cert files) keeps running and retrying:"; else echo "      B4 (no cert files) exited:"; fi
grep -i -m2 "cert\|key\|tls" "$work/b4.log" | cut -c1-220 | sed 's/^/        /' || true
n=$(grep -c "is unauthorized to" "$work/b1.log" || true)
echo "B1 logged $n denials (\"is unauthorized to\"; per-key cache entries included)"
echo "$fails failure(s)"
[ "$fails" -eq 0 ]
