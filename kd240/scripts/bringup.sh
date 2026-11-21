#!/usr/bin/env bash
# KD240 bring-up, step by step (kd240/BRINGUP.md). Run from anywhere in the
# repository, on the laptop that builds and deploys the board.
#
# Every step is CHECK-ONLY by default: it reads the board's state over SSH
# (and the laptop's tools) and prints what --apply would do. Nothing is
# built, written, deployed or installed without --apply. Some steps have
# nothing safe to automate and stay manual even with --apply; they say so.
#
#   kd240/scripts/bringup.sh                    check every step
#   kd240/scripts/bringup.sh --step 6           check one step
#   kd240/scripts/bringup.sh --from 3 --apply   carry out steps 3 to 8
#   kd240/scripts/bringup.sh --list             the steps
#
# Nothing here moves the real lathe: the KD240 driver has no outputs, the
# RS-485 port is opened read-only, and the bench test (step 5) refuses to
# command anything unless the physical node runs the `sim` driver.

set -euo pipefail

HELP="usage: bringup.sh [options]

  --host HOST           the board [kd240.local]
  --user USER           its SSH user [admin]
  --apply               carry the steps out (default: check only)
  --step N              only step N (1-8)
  --from N              steps N to 8
  --list                list the steps and exit
  --device DEV          step 1 --apply: also write the image to this whole
                        card (just flash, which asks before writing)
  --key FILE            steps 2/3: an mwctl key file (claims the nodes, signs)
  --pkcs11 LABEL        steps 2/3: a PKCS#11 key for mwctl instead
  --tunnel-port PORT    step 3: local end of the SSH tunnel to the board's
                        loopback zenohd [17447]
  --cyclictest-s N      step 6 --apply: run length, seconds [300]
  --cert FILE           step 8 --apply: the board's signed certificate
  --ca FILE             step 8 --apply: the Zenoh CA (edge-ca ca --zenoh)
  --no-load             step 6: run cyclictest without the stress-ng load
  -h, --help            this text
"

STEPS=(
    ""
    "Flash and boot"
    "Claim and trust"
    "Sign and install the physical and process payloads (mwctl over Zenoh)"
    "Enable services.emco-mesh.ui and .fleetInterlock"
    "Bench test: the UI drives the simulated lathe through the payloads"
    "cyclictest under load (pass: max <= 246 us, no overflow)"
    "RS-485 DE stays low; the GPIO inputs"
    "Enroll the uplink certificate (emco-mesh-enroll; services.emco-mesh.uplink)"
)

host=kd240.local
user="admin"
apply=""
first=1
last=8
device=""
key=""
pkcs11=""
tunnel_port=17447
ct_seconds=300
ct_load=yes
cert=""
ca=""

while [ $# -gt 0 ]; do
    case "$1" in
        --host) host=$2; shift 2 ;;
        --user) user=$2; shift 2 ;;
        --apply) apply=yes; shift ;;
        --step) first=$2; last=$2; shift 2 ;;
        --from) first=$2; last=8; shift 2 ;;
        --device) device=$2; shift 2 ;;
        --key) key=$2; shift 2 ;;
        --pkcs11) pkcs11=$2; shift 2 ;;
        --tunnel-port) tunnel_port=$2; shift 2 ;;
        --cyclictest-s) ct_seconds=$2; shift 2 ;;
        --no-load) ct_load=""; shift ;;
        --cert) cert=$2; shift 2 ;;
        --ca) ca=$2; shift 2 ;;
        --list)
            for i in 1 2 3 4 5 6 7 8; do printf '%d. %s\n' "$i" "${STEPS[$i]}"; done
            exit 0 ;;
        -h|--help) printf '%s' "$HELP"; exit 0 ;;
        *) printf 'unknown option: %s\n\n%s' "$1" "$HELP" >&2; exit 2 ;;
    esac
done

case "$first$last" in
    *[!0-9]*) echo "--step/--from take a number 1-8" >&2; exit 2 ;;
esac
if [ "$first" -lt 1 ] || [ "$last" -gt 8 ] || [ "$first" -gt "$last" ]; then
    echo "--step/--from take a number 1-8" >&2; exit 2
fi
if [ -n "$key" ] && [ -n "$pkcs11" ]; then
    echo "--key and --pkcs11: give one" >&2; exit 2
fi
# --host and --user go to ssh and scp: a name, never an option or a shell word.
valid_name() { case "$1" in "" | -* | *[!A-Za-z0-9._-]*) return 1 ;; esac; }
if ! valid_name "$host"; then
    echo "--host must be letters, digits, . _ - (not starting with -): '$host'" >&2; exit 2
fi
if ! valid_name "$user"; then
    echo "--user must be letters, digits, . _ - (not starting with -): '$user'" >&2; exit 2
fi
if [ -n "$cert$ca" ] && { [ -z "$cert" ] || [ -z "$ca" ]; }; then
    echo "--cert and --ca go together" >&2; exit 2
fi
for f in "$cert" "$ca"; do
    if [ -n "$f" ] && [ ! -r "$f" ]; then echo "$f: no such file" >&2; exit 2; fi
done

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target="$user@$host"
base="http://$host:8080"

# ------------------------------------------------------------ output

say()  { printf '    %s\n' "$*"; }
ok()   { printf '  ok    %s\n' "$*"; }
bad()  { printf '  FAIL  %s\n' "$*"; }
gap()  { printf '  GAP   %s\n' "$*"; }
todo() { printf '  TODO  %s\n' "$*"; }
# What --apply would run, or runs.
would() {
    if [ -n "$apply" ]; then printf '  run   %s\n' "$*"; else printf '  would %s\n' "$*"; fi
}

# ------------------------------------------------------------ helpers

# On the board, non-interactive; stdin closed.
on_board() {
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$target" "$@" </dev/null
}

have() { command -v "$1" >/dev/null 2>&1; }

board_up() {
    if on_board true 2>/dev/null; then
        ok "ssh $target answers"
        return 0
    fi
    bad "ssh $target does not answer (BatchMode, key only; nix/authorized_keys)"
    return 1
}

# A unit's Environment= line, on the board.
unit_env() {
    on_board systemctl show "$1" -p Environment --value 2>/dev/null || true
}

# The node ids emco-mesh-ids wrote (UNIT=, PROCESS=): 16 lowercase hex
# digits each, or neither is used (they end up in local file names).
read_ids() {
    UNIT=""; PROCESS=""
    local ids u p
    ids=$(on_board cat /run/emco-mesh/ids 2>/dev/null) || return 1
    u=$(printf '%s\n' "$ids" | sed -n 's/^UNIT=//p' | head -1)
    p=$(printf '%s\n' "$ids" | sed -n 's/^PROCESS=//p' | head -1)
    local id
    for id in "$u" "$p"; do
        case "$id" in
            [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) ;;
            *) bad "/run/emco-mesh/ids: UNIT and PROCESS must be 16 lowercase hex digits (UNIT='$u' PROCESS='$p')"; return 1 ;;
        esac
    done
    UNIT=$u; PROCESS=$p
}

# mwctl's key options, as an array.
key_args() {
    KEYARGS=()
    if [ -n "$key" ]; then KEYARGS=(--key "$key"); fi
    if [ -n "$pkcs11" ]; then KEYARGS=(--pkcs11 "$pkcs11"); fi
}

# GET a JSON path from the lathe service, e.g. state_field transport.
state_field() {
    curl -fsS --max-time 5 "$base/api/state" 2>/dev/null | python3 -c '
import json, sys
v = json.load(sys.stdin)
for k in sys.argv[1].split("."):
    v = v.get(k) if isinstance(v, dict) else None
print("" if v is None else (v if isinstance(v, str) else json.dumps(v)))
' "$1" || true
}

# ------------------------------------------------------------ step 1

step1() {
    say "build: nix build .#nixosConfigurations.kd240.config.system.build.sdImage (just image)"
    say "flash: just flash /dev/sdX (asks before writing; kd240/justfile)"
    if [ -n "$apply" ]; then
        would "just -f $root/kd240/justfile image"
        just -f "$root/kd240/justfile" image || { bad "image build failed"; return 1; }
        if [ -n "$device" ]; then
            would "just -f $root/kd240/justfile flash $device"
            just -f "$root/kd240/justfile" flash "$device" || { bad "flash failed or declined"; return 1; }
            todo "card into the KD240, Ethernet in, power on; then run this step again without --apply"
            return 0
        fi
        say "no --device: image built, nothing written"
    else
        would "just image$( [ -n "$device" ] && printf ' && just flash %s' "$device")"
    fi

    board_up || return 1
    local r=0 v failed
    v=$(on_board uname -v)
    case "$v" in
        *PREEMPT_RT*) ok "kernel: $v" ;;
        *) bad "kernel is not PREEMPT_RT: $v"; r=1 ;;
    esac
    say "system: $(on_board readlink /run/current-system)"
    failed=$(on_board systemctl --failed --no-legend --plain 2>/dev/null | awk '{print $1}' | paste -sd' ' -)
    if [ -z "$failed" ]; then ok "no failed units"; else bad "failed units: $failed"; r=1; fi
    for u in zenohd emco-mesh-ids mw-posix-node mw-posix-node-process emco-mesh-announce emco-kd240; do
        if on_board systemctl is-active --quiet "$u"; then ok "$u active"; else bad "$u not active"; r=1; fi
    done
    if read_ids; then ok "node ids: unit $UNIT, process $PROCESS"; else bad "/run/emco-mesh/ids missing"; r=1; fi
    local listen
    listen=$(on_board ss -Hltn 'sport = :7447' 2>/dev/null | awk '{print $4}') || listen=""
    if [ -z "$listen" ]; then
        bad "zenohd is not listening on 127.0.0.1:7447"; r=1
    elif printf '%s\n' "$listen" | grep -qvx '127.0.0.1:7447'; then
        bad "port 7447 is not loopback only: $(printf '%s' "$listen" | paste -sd' ' -)"; r=1
    else
        ok "zenohd listens on 127.0.0.1:7447 only"
    fi
    if have avahi-browse; then
        if avahi-browse -rtp _emco-lathe._tcp 2>/dev/null | grep -q '^='; then
            ok "_emco-lathe._tcp is announced"
        else
            bad "_emco-lathe._tcp not seen on the LAN"; r=1
        fi
    else
        say "(no avahi-browse here: _emco-lathe._tcp not checked)"
    fi
    return "$r"
}

# ------------------------------------------------------------ step 2

step2() {
    local r=0
    say "two separate trusts (BRINGUP.md, step 2):"
    say "  a. each mw-posix-node pins its owner's key, over its claim link:"
    say "     kd240/scripts/claim.sh <unit|process> --host $host (--key K | --pkcs11 L)"
    say "  b. the fleet agent obeys orders signed by trustedKeys (emco-fleet pubkey / keygen)"
    board_up || return 1

    # a. A claim never passes gw/sys or Zenoh: it goes over the node's local
    # serial link, which the claim link (services.emco-mesh.claimLink) makes
    # a PTY in the node's unit, with a root-only socket beside it.
    key_args
    local w u env sock links=0
    for w in unit process; do
        u=mw-posix-node
        [ "$w" = process ] && u=mw-posix-node-process
        env=$(unit_env "$u")
        sock=/run/emco-mesh-claim-$w/sock
        if ! printf '%s' "$env" | grep -q "MW_POSIX_NODE_LOCAL_SERIAL=/run/emco-mesh-claim-$w/tty"; then
            gap "$u: no claim link (services.emco-mesh.claimLink; on once the middleware pin opens a PTY: flake.nix ptyLocalSerial)"
            continue
        fi
        if on_board sudo test -S "$sock"; then
            ok "$u: claim link $sock"
            links=$((links + 1))
        else
            bad "$u: MW_POSIX_NODE_LOCAL_SERIAL is set but $sock is missing (journalctl -u $u: emco-mesh-claim-link)"
            r=1
        fi
    done
    if [ "$links" -eq 0 ] && [ "$r" -eq 0 ]; then r=2; fi
    if [ "$links" -gt 0 ]; then
        if ! have mwctl || ! have socat; then
            bad "claim.sh needs mwctl and socat here"; r=1
        else
            for w in unit process; do
                on_board sudo test -S "/run/emco-mesh-claim-$w/sock" || continue
                local info state
                info=$("$root/kd240/scripts/claim.sh" "$w" --host "$host" --user "$user" --info 2>&1) || {
                    bad "$w node: trust info over the claim link failed:"
                    printf '%s\n' "$info" | sed 's/^/        /'
                    r=1; continue
                }
                state=$(printf '%s\n' "$info" | sed -n 's/^trust: *//p' | head -1)
                case "$state" in
                    unclaimed*)
                        say "$w node: unclaimed"
                        would "kd240/scripts/claim.sh $w --host $host --user $user ${KEYARGS[*]:-<--key FILE | --pkcs11 LABEL>}"
                        if [ -n "$apply" ]; then
                            if [ ${#KEYARGS[@]} -eq 0 ]; then
                                bad "--apply needs --key FILE or --pkcs11 LABEL to claim"; r=1
                            elif "$root/kd240/scripts/claim.sh" "$w" --host "$host" --user "$user" "${KEYARGS[@]}"; then
                                ok "$w node claimed"
                            else
                                bad "$w node: claim failed"; r=1
                            fi
                        elif [ "$r" -eq 0 ]; then
                            r=1
                        fi ;;
                    claimed*) ok "$w node: $state" ;;
                    *) bad "$w node: no trust state in: $info"; r=1 ;;
                esac
            done
        fi
    fi

    # b. The fleet agent and its trusted keys (nix/fleet.nix): only on the
    # A/B layout (nixosConfigurations.kd240-ab).
    if on_board systemctl cat emco-fleet-agent.service >/dev/null 2>&1; then
        local keys
        keys=$(on_board systemctl show emco-fleet-agent -p ExecStart --value 2>/dev/null \
            | grep -o -- '--trusted-key [^ ;]*' | cut -d' ' -f2 | paste -sd' ' -)
        if [ -n "$keys" ]; then ok "fleet agent trusts: $keys"; else bad "fleet agent has no --trusted-key"; r=1; fi
        if on_board sudo test -s /var/lib/emco-fleet-agent/token; then
            ok "fleet device token present"
        else
            bad "/var/lib/emco-fleet-agent/token missing (emco-fleet token --device kd240, line 2)"; r=1
        fi
    else
        gap "no emco-fleet-agent on the board: it exists only on nixosConfigurations.kd240-ab (not booted yet)"
        [ "$r" -eq 0 ] && r=2
    fi
    if [ -s "$HOME/.config/emco-fleet/orders.pub" ]; then
        ok "orders.pub: $(cat "$HOME/.config/emco-fleet/orders.pub")"
    else
        say "no ~/.config/emco-fleet/orders.pub here (fleet/README.md, \"One-time setup\")"
    fi
    if [ -n "$apply" ]; then
        todo "manual: key generation and trustedKeys are the owner's choice (card or software key); see BRINGUP.md"
    fi
    return "$r"
}

# ------------------------------------------------------------ step 3

physical_wasm="$root/cnc/target/wasm32-unknown-unknown/release/cnc_physical.wasm"
process_wasm="$root/cnc/target/wasm32-unknown-unknown/release/cnc_process.wasm"
# cnc_model::IoMap::binding_specs() for the default map (the simulator's
# point names; model/fixtures/pc_turn_50.json "io"). `queue` binds to the
# motion point too.
physical_binding=(
    x_pos=driver:x_pos z_pos=driver:z_pos x_home=driver:x_home z_home=driver:z_home
    spindle_rpm=driver:spindle_rpm spindle_actual=driver:spindle_actual
    turret_station=driver:turret_station turret_ready=driver:turret_ready
    estop=driver:estop motion=driver:motion queue=driver:motion
)

tunnel_pid=""
close_tunnel() {
    if [ -n "$tunnel_pid" ]; then kill "$tunnel_pid" 2>/dev/null || true; tunnel_pid=""; fi
}
trap close_tunnel EXIT

open_tunnel() {
    if (exec 3<>"/dev/tcp/127.0.0.1/$tunnel_port") 2>/dev/null; then
        bad "127.0.0.1:$tunnel_port is already in use (a leftover tunnel?): pick --tunnel-port"
        return 1
    fi
    ssh -o BatchMode=yes -o ExitOnForwardFailure=yes -N \
        -L "127.0.0.1:$tunnel_port:127.0.0.1:7447" "$target" </dev/null &
    tunnel_pid=$!
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        if (exec 3<>"/dev/tcp/127.0.0.1/$tunnel_port") 2>/dev/null && kill -0 "$tunnel_pid" 2>/dev/null; then
            return 0
        fi
        kill -0 "$tunnel_pid" 2>/dev/null || break
        sleep 0.5
    done
    close_tunnel
    return 1
}

step3() {
    local r=0 zen="tcp/127.0.0.1:$tunnel_port"
    key_args

    # The payloads (cnc/README.md).
    for p in physical process; do
        local f="$physical_wasm"
        [ "$p" = process ] && f="$process_wasm"
        if [ -s "$f" ]; then
            ok "$p payload: $f ($(wc -c <"$f") bytes)"
        else
            would "(cd cnc && cargo build --release --target wasm32-unknown-unknown -p cnc-$p)"
            if [ -n "$apply" ]; then
                (cd "$root/cnc" && cargo build --release --target wasm32-unknown-unknown -p "cnc-$p") \
                    || { bad "cnc-$p build failed"; return 1; }
            else
                r=1
            fi
        fi
    done

    # mwctl with the zenoh feature (not a flake output; built from the pin).
    if have mwctl; then
        local out
        out=$(timeout 5 mwctl --zenoh tcp/127.0.0.1:9 --node 0000000000000001 info 2>&1 || true)
        case "$out" in
            *"built without Zenoh"*) bad "mwctl here was built without --features zenoh"; r=1 ;;
            *) ok "mwctl: $(command -v mwctl)" ;;
        esac
    else
        bad "no mwctl on PATH"
        say "  cargo install --locked --git https://github.com/jvishnefske/middleware \\"
        say "    --rev 514f9747c114f70058361db13365f5efd806775e --features zenoh mwctl"
        r=1
    fi
    if [ ${#KEYARGS[@]} -eq 0 ]; then
        say "no --key/--pkcs11: over Zenoh mwctl refuses upload, bind and start unsigned"
        [ -n "$apply" ] && { bad "--apply needs --key FILE or --pkcs11 LABEL"; return 1; }
    fi

    board_up || return 1
    read_ids || { bad "/run/emco-mesh/ids missing (step 1)"; return 1; }
    ok "unit (physical) $UNIT, process $PROCESS"

    local m=(mwctl --zenoh "$zen")
    would "ssh -N -L 127.0.0.1:$tunnel_port:127.0.0.1:7447 $target   (the board's zenohd is loopback only)"
    would "${m[*]} --node $UNIT info"
    would "${m[*]} --node $UNIT upload $physical_wasm ${KEYARGS[*]:-<KEY>}"
    would "${m[*]} --node $UNIT bind $physical_wasm ${physical_binding[*]} ${KEYARGS[*]:-<KEY>}"
    would "${m[*]} --node $UNIT start ${KEYARGS[*]:-<KEY>}"
    would "${m[*]} --node $PROCESS upload $process_wasm ${KEYARGS[*]:-<KEY>}"
    would "${m[*]} --node $PROCESS start ${KEYARGS[*]:-<KEY>}"
    say "  (the physical binding names the sim driver's points: it starts only with driver = \"sim\")"

    [ "$r" -eq 0 ] || return "$r"
    if [ -z "$apply" ]; then
        gap "posix-node serves no sys keys over Zenoh until W4a: expect info to fail"
        return 2
    fi

    open_tunnel || { bad "the SSH tunnel to the board's zenohd did not open"; return 1; }
    ok "tunnel open on 127.0.0.1:$tunnel_port"
    if ! "${m[@]}" --node "$UNIT" info; then
        gap "no answer on @mw/node/$UNIT/sys/info: posix-node serves no sys keys over Zenoh (W4a)"
        close_tunnel
        return 2
    fi
    if ! { "${m[@]}" --node "$UNIT" upload "$physical_wasm" "${KEYARGS[@]}" \
        && "${m[@]}" --node "$UNIT" bind "$physical_wasm" "${physical_binding[@]}" "${KEYARGS[@]}" \
        && "${m[@]}" --node "$UNIT" start "${KEYARGS[@]}" \
        && "${m[@]}" --node "$PROCESS" upload "$process_wasm" "${KEYARGS[@]}" \
        && "${m[@]}" --node "$PROCESS" start "${KEYARGS[@]}"; }; then
        bad "install failed (above)"; close_tunnel; return 1
    fi
    "${m[@]}" --node "$UNIT" info || r=1
    "${m[@]}" --node "$PROCESS" info || r=1
    close_tunnel
    return "$r"
}

# ------------------------------------------------------------ step 4

step4() {
    local r=0 ui fl
    say "kd240/nix/configuration.nix, services.emco-mesh:"
    say "    ui = true;              # emco-kd240 --mesh: the UI drives the payloads"
    say "    fleetInterlock = true;  # the fleet agent's reboot interlock also reads the process state"
    say "only once both payloads run (step 3): with none, the UI shows disconnected and the"
    say "interlock (fail closed) defers every reboot"
    ui=$(cd "$root" && nix eval --raw .#nixosConfigurations.kd240.config.services.emco-mesh.ui \
        --apply 'b: if b then "true" else "false"' 2>/dev/null || echo "?")
    fl=$(cd "$root" && nix eval --raw .#nixosConfigurations.kd240.config.services.emco-mesh.fleetInterlock \
        --apply 'b: if b then "true" else "false"' 2>/dev/null || echo "?")
    say "in this checkout: ui = $ui, fleetInterlock = $fl"
    say "fleetInterlock acts only where services.emco-fleet-agent exists (kd240-ab); on kd240 it is a no-op"

    if board_up; then
        if on_board systemctl show emco-kd240 -p ExecStart --value 2>/dev/null | grep -q -- '--mesh'; then
            ok "emco-kd240 runs with --mesh"
            local tr
            tr=$(state_field transport)
            if [ "$tr" = mesh ]; then ok "GET /api/state: transport mesh"; else bad "GET /api/state: transport ${tr:-no answer} (want mesh)"; r=1; fi
        else
            say "emco-kd240 runs on its virtual machine (no --mesh)"; r=1
        fi
        if on_board systemctl show emco-fleet-agent -p EnvironmentFiles --value 2>/dev/null | grep -q fleet.env; then
            ok "fleet agent reads /run/emco-mesh/fleet.env"
        fi
    else
        r=1
    fi

    if [ "$ui" != true ]; then
        would "edit kd240/nix/configuration.nix as above, then: just -f kd240/justfile deploy $target"
        [ -n "$apply" ] && todo "edit the configuration first; this script does not change it"
        return 1
    fi
    would "just -f kd240/justfile deploy $target"
    if [ -n "$apply" ]; then
        just -f "$root/kd240/justfile" deploy "$target" || { bad "deploy failed"; return 1; }
        say "deployed; checking again"
        apply="" step4
        return $?
    fi
    return "$r"
}

# ------------------------------------------------------------ step 5

step5() {
    local r=0
    say "needs: services.emco-mesh.driver = \"sim\" and ui = true deployed, payloads running (step 3)"
    board_up || return 1
    local drv
    drv=$(unit_env mw-posix-node | grep -o 'MW_POSIX_NODE_DRIVER=[a-z0-9]*' | cut -d= -f2) || drv=
    if [ "$drv" = sim ]; then ok "physical node driver: sim"; else bad "physical node driver: ${drv:-?} (want sim for the bench test)"; r=1; fi
    if on_board journalctl -b -u mw-posix-node --no-pager -o cat 2>/dev/null | grep -q 'driver sim'; then
        ok "mw-posix-node logged its sim driver"
    fi
    local tr ps run
    tr=$(state_field transport)
    ps=$(state_field mesh.processState)
    run=$(state_field session.run)
    if [ "$tr" = mesh ]; then ok "GET $base/api/state: transport mesh, run $run"; else bad "transport: ${tr:-no answer} (want mesh: step 4)"; r=1; fi
    if [ -n "$ps" ] && [ "$ps" != null ]; then ok "process state heard: $ps"; else bad "no process state (payload not running?)"; r=1; fi

    would "POST $base/api/command {\"kind\":\"home\"} with the board's token, wait for homed"
    say "then by hand, in the UI at $base/: load a program, Run, Hold, Resume, Abort"
    [ -n "$apply" ] || return "$r"
    if [ "$drv" != sim ] || [ "$r" -ne 0 ]; then bad "not commanding: the bench test needs the sim driver and a live process model"; return 1; fi
    local token i homed
    token=$(on_board sudo cat /var/lib/emco-kd240/token) || { bad "cannot read the token"; return 1; }
    curl -fsS --max-time 5 -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
        -d '{"kind":"home"}' "$base/api/command" >/dev/null || { bad "home refused"; return 1; }
    for i in $(seq 1 60); do
        homed=$(state_field session.dro.homed)
        if [ "$homed" = true ]; then ok "homed after ${i}s"; return 0; fi
        sleep 1
    done
    bad "not homed after 60 s (run: $(state_field session.run))"
    return 1
}

# ------------------------------------------------------------ step 6

CT_MAX_US=246  # doc 65 S1: the 3-hour stress-ng baseline's worst case
step6() {
    local cmd="cyclictest -m -S -p 90 -i 1000 -h 1000 -q -D $ct_seconds"
    local load="stress-ng --cpu 4 --io 2 --vm 2 --vm-bytes 128M --timeout $((ct_seconds + 10))s"
    say "pass: every core's max <= ${CT_MAX_US} us (doc 65 S1 baseline) and no overflow (> 1000 us)"
    board_up || return 1
    local r=0 v
    v=$(on_board uname -v)
    case "$v" in *PREEMPT_RT*) ok "PREEMPT_RT kernel" ;; *) bad "not a PREEMPT_RT kernel: $v"; r=1 ;; esac
    for t in cyclictest stress-ng; do
        if on_board command -v "$t" >/dev/null; then ok "$t on the board"; else bad "no $t on the board"; r=1; fi
    done
    if [ -n "$ct_load" ]; then would "sudo $load &  sudo $cmd"; else would "sudo $cmd"; fi
    [ -n "$apply" ] || return "$r"
    [ "$r" -eq 0 ] || return "$r"

    say "running for ${ct_seconds}s ..."
    local out
    if [ -n "$ct_load" ]; then
        out=$(on_board "sudo $load >/dev/null 2>&1 & sudo $cmd; wait") || { bad "cyclictest failed"; return 1; }
    else
        out=$(on_board "sudo $cmd") || { bad "cyclictest failed"; return 1; }
    fi
    local maxes overflows worst=0 m
    maxes=$(printf '%s\n' "$out" | sed -n 's/^# Max Latencies: *//p')
    overflows=$(printf '%s\n' "$out" | sed -n 's/^# Histogram Overflows: *//p')
    [ -n "$maxes" ] || { bad "no '# Max Latencies' line in the output"; printf '%s\n' "$out" | tail -20; return 1; }
    for m in $maxes; do m=$((10#$m)); [ "$m" -gt "$worst" ] && worst=$m; done
    say "max per core: $maxes; overflows: $overflows"
    for m in $overflows; do [ "$((10#$m))" -eq 0 ] || { bad "overflows (> 1000 us): $overflows"; r=1; break; }; done
    if [ "$worst" -le "$CT_MAX_US" ]; then ok "worst ${worst} us <= ${CT_MAX_US} us"; else bad "worst ${worst} us > ${CT_MAX_US} us"; r=1; fi
    return "$r"
}

# ------------------------------------------------------------ step 7

step7() {
    local r=0
    board_up || return 1
    say "RS-485: PS UART0, /dev/ttyPS0, RS-485 mode from the device tree, DE on rts-gpios (README, \"RS-485\")"
    local of=/sys/class/tty/ttyPS0/device/of_node
    if on_board test -e "$of/linux,rs485-enabled-at-boot-time"; then
        ok "ttyPS0's node has linux,rs485-enabled-at-boot-time"
    else
        bad "ttyPS0's device-tree node has no linux,rs485-enabled-at-boot-time"; r=1
    fi
    if on_board test -e "$of/rts-gpios"; then ok "ttyPS0's node has rts-gpios (DE)"; else bad "no rts-gpios on ttyPS0's node"; r=1; fi
    local de
    de=$(on_board sudo cat /sys/kernel/debug/gpio 2>/dev/null | grep -i 'rts' || true)
    if [ -n "$de" ]; then
        say "debugfs: $de"
        case "$de" in
            *" out lo"*) ok "DE line reads low" ;;
            *) bad "DE line does not read 'out lo'"; r=1 ;;
        esac
    else
        say "(DE line not found by name in /sys/kernel/debug/gpio; check by meter)"
    fi
    local holders
    holders=$(on_board "sudo find /proc/[0-9]*/fd -lname /dev/ttyPS0 2>/dev/null | cut -d/ -f3 | sort -u | xargs -r ps -o comm= -p" | paste -sd" " - || true)
    say "ttyPS0 open by: ${holders:-nothing}"
    todo "by hand: meter DE (or an analyser on A/B) while a capture runs from the UI; it must stay low"

    say "GPIO inputs (kd240 driver: MW_POSIX_NODE_KD240_INPUTS, point=chip:offset[:low])"
    say "chips: $(on_board 'ls /dev/gpiochip* 2>/dev/null' | paste -sd' ' -)"
    local inputs
    inputs=$(unit_env mw-posix-node | grep -o 'MW_POSIX_NODE_KD240_INPUTS=[^ ]*' || true)
    if [ -n "$inputs" ]; then
        ok "$inputs"
    else
        gap "services.emco-mesh.kd240Inputs is empty: the driver has only pl_present (no pin map yet)"
        [ "$r" -eq 0 ] && r=2
    fi
    if [ -z "$(on_board systemctl show mw-posix-node -p SupplementaryGroups --value 2>/dev/null)" ]; then
        gap "mw-posix-node has no supplementary group for /dev/gpiochip* and PrivateDevices=yes (mesh.nix: not set up)"
        [ "$r" -eq 0 ] && r=2
    fi
    local drvlog
    drvlog=$(on_board journalctl -b -u mw-posix-node --no-pager -o cat 2>/dev/null | grep 'driver ' | tail -1 || true)
    [ -n "$drvlog" ] && say "node: $drvlog"
    say "line names and consumers: ssh $target nix shell nixpkgs#libgpiod -c gpioinfo (not in the image)"
    [ -n "$apply" ] && todo "manual: toggle each switch and watch app/unit/<node>/cm (no tool in the repo shows it yet)"
    return "$r"
}

# ------------------------------------------------------------ step 8

step8() {
    local r=0
    say "services.emco-mesh.uplink: a second zenohd (emco-mesh-uplink) linking the board's router"
    say "to the cloud mesh over mTLS, with a certificate from emco-mesh-enroll (BRINGUP.md step 8)"
    if have edge-ca; then ok "edge-ca here"; else say "no edge-ca here: the certificate is signed on the CA host (W3b)"; fi
    board_up || return 1
    read_ids || { bad "/run/emco-mesh/ids missing (step 1)"; return 1; }
    local id=$UNIT csr="$PWD/kd240-$UNIT.csr"
    if ! on_board command -v emco-mesh-enroll >/dev/null 2>&1; then
        gap "no emco-mesh-enroll on the board: services.emco-mesh.uplink.enable is off (nix/configuration.nix)"
        say "  set it, deploy (just deploy), then run this step again"
        return 2
    fi
    ok "emco-mesh-enroll on the board"
    local status
    status=$(on_board sudo emco-mesh-enroll status 2>&1 || true)
    printf '%s\n' "$status" | sed 's/^/        /'
    local enrolled=""
    if on_board sudo emco-mesh-enroll check >/dev/null 2>&1; then
        ok "the certificate passes emco-mesh-enroll check (kd240-$id)"
        enrolled=yes
    else
        say "not enrolled yet, or the certificate fails its check:"
        on_board sudo emco-mesh-enroll check 2>&1 | sed 's/^/        /' || true
    fi
    if on_board systemctl is-active --quiet emco-mesh-uplink; then
        ok "emco-mesh-uplink active"
    else
        if [ -n "$enrolled" ]; then bad "emco-mesh-uplink not active (journalctl -u emco-mesh-uplink)"; r=1
        else say "emco-mesh-uplink waits for a certificate (its ExecStartPre is the check)"; fi
    fi
    # zenohd's own connections (the fleet agent may use port 443 too).
    local links
    links=$(on_board "sudo ss -Htnp state established '( dport = :443 )'" 2>/dev/null | grep '"zenohd"' || true)
    if [ -n "$links" ]; then
        ok "the uplink's zenohd is connected: $(printf '%s' "$links" | awk '{print $4}' | paste -sd' ' -)"
    elif [ -n "$enrolled" ]; then
        bad "no established connection to port 443 (DNS for mesh.updates.<site>, the router, its ACL's CN list)"; r=1
    fi

    say "the hand-off (the key never leaves the board; --apply runs the board's parts):"
    say "  ssh $target sudo emco-mesh-enroll csr > $csr"
    say "  edge-ca issue --ou kd240 $id --csr kd240-$id.csr --out kd240-$id.crt   # on the CA host"
    say "  edge-ca ca --zenoh > zenoh-ca.pem                                       # on the CA host"
    say "  scp kd240-$id.crt zenoh-ca.pem $target:/tmp/"
    say "  ssh $target sudo emco-mesh-enroll install /tmp/kd240-$id.crt /tmp/zenoh-ca.pem"
    say "and add \"kd240-$id\" to the kd240 subject's cert_common_names in the mesh router's ACL (MESH.md)"
    if [ -n "$cert" ]; then would "scp $cert $ca $target:/tmp/ && ssh $target sudo emco-mesh-enroll install …"
    elif [ -z "$enrolled" ]; then would "ssh $target sudo emco-mesh-enroll csr > $csr"; fi

    if [ -z "$apply" ]; then
        [ -n "$enrolled" ] || [ "$r" -ne 0 ] || r=2
        return "$r"
    fi
    if [ -n "$cert" ]; then
        local base cab
        base=$(basename "$cert")
        cab=$(basename "$ca")
        scp -q -o BatchMode=yes "$cert" "$ca" "$target:/tmp/" || { bad "scp failed"; return 1; }
        # ssh hands the board's shell one string: quote each name for it.
        if on_board sudo emco-mesh-enroll install "$(printf '%q' "/tmp/$base")" "$(printf '%q' "/tmp/$cab")"; then
            ok "installed $base; emco-mesh-uplink restarted"
            apply="" cert="" ca="" step8
            return $?
        fi
        bad "emco-mesh-enroll install refused it (above)"
        return 1
    fi
    if [ -n "$enrolled" ]; then return "$r"; fi
    if on_board sudo emco-mesh-enroll csr >"$csr" 2>"$csr.log"; then
        ok "CSR for kd240-$id in $csr (the key stays on the board)"
    else
        cat "$csr.log" >&2; bad "emco-mesh-enroll csr failed"; return 1
    fi
    rm -f "$csr.log"
    gap "blocked: needs signing on the CA host, then: bringup.sh --step 8 --apply --cert kd240-$id.crt --ca zenoh-ca.pem"
    return 2
}

# ------------------------------------------------------------ run

declare -a results=()
for n in $(seq "$first" "$last"); do
    printf '\n== %d. %s%s\n' "$n" "${STEPS[$n]}" "$( [ -n "$apply" ] && printf ' [apply]' )"
    rc=0
    case "$n" in
        1) step1 || rc=$? ;;
        2) step2 || rc=$? ;;
        3) step3 || rc=$? ;;
        4) step4 || rc=$? ;;
        5) step5 || rc=$? ;;
        6) step6 || rc=$? ;;
        7) step7 || rc=$? ;;
        8) step8 || rc=$? ;;
    esac
    case "$rc" in
        0) results+=("$n pass") ;;
        2) results+=("$n blocked (gap)") ;;
        *) results+=("$n FAIL") ;;
    esac
done

printf '\n== summary (%s)\n' "$( [ -n "$apply" ] && echo apply || echo check only)"
status=0
for line in "${results[@]}"; do
    printf '  %s\n' "$line"
    case "$line" in *FAIL*) status=1 ;; esac
done
exit "$status"
