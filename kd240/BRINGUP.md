# KD240 bring-up

The steps that take a fresh card to a KD240 that hosts the lathe's
payloads, with what each step checks and what the repository does not
provide yet. `scripts/bringup.sh` runs them. By default every step only
checks: it reads the board over SSH and prints what `--apply` would run.

```sh
kd240/scripts/bringup.sh --list                # the steps
kd240/scripts/bringup.sh                       # check all eight
kd240/scripts/bringup.sh --step 6 --apply      # run one step
kd240/scripts/bringup.sh --from 3 --apply --pkcs11 mw-release
kd240/scripts/bringup.sh --host 192.168.1.134  # no mDNS resolver here
```

Options: `--host` (default `kd240.local`), `--user` (default `admin`),
`--device /dev/sdX` (step 1 writes the card), `--key FILE` or
`--pkcs11 LABEL` (the mwctl key that claims the nodes in step 2 and signs
in step 3), `--tunnel-port` (step 3, default 17447), `--cyclictest-s N`
and `--no-load` (step 6), `--cert FILE --ca FILE` (step 8).

Each step ends as **pass**, **FAIL**, or **blocked (gap)**. A gap is
something the repository doesn't provide yet. The exit status is 1 if any
step failed. A gap alone doesn't fail the run.

> **Nothing here moves the real lathe** ([README](README.md)). The KD240
> driver has no outputs. The RS-485 port is opened read-only. The bench
> test (step 5) refuses to send a command unless the physical node runs
> the `sim` driver.

## 1. Flash and boot

* **Build:** `just image`, which is `nix build
  .#nixosConfigurations.kd240.config.system.build.sdImage` (`justfile`,
  `nix/configuration.nix`). Build on an x86-64 host, because the kernel is
  cross-compiled ([README](README.md), "The SD card").
* **Flash:** `just flash /dev/sdX`. It asks before writing.
* **What `--apply` does:** builds the image, then runs `just flash` only
  if `--device` is given.
* **Checks:**
  * SSH answers (key only, `nix/authorized_keys`).
  * `uname -v` says `PREEMPT_RT`.
  * There are no failed units.
  * `zenohd`, `emco-mesh-ids`, `mw-posix-node`, `mw-posix-node-process`,
    `emco-mesh-announce` and `emco-kd240` are active.
  * `/run/emco-mesh/ids` names both node ids.
  * zenohd listens on `127.0.0.1:7447` only.
  * With `avahi-browse` on the laptop, `_emco-lathe._tcp` is announced.
* An image written fresh from this configuration has not been booted yet.
  The card in the board was converted in place (README).

## 2. Claim and trust

These are two separate trusts:

**a. Each node's owner key.** Every `mw-posix-node` pins one public key
(`sys/trust/claim`). After that it takes only signed commits and signed
start/stop. A claim never passes `gw/sys` or Zenoh (mwctl refuses
`trust claim` over `--zenoh`). It goes over the node's local serial port
(`MW_POSIX_NODE_LOCAL_SERIAL`), with the owner's mwctl key: a key file
(`--key`) or a PKCS#11 key such as the TPM's P-256 (`--pkcs11`).

**The claim link** (`services.emco-mesh.claimLink`, `nix/mesh.nix`) is that
port on the KD240:

* Each node has a root unit of its own, `emco-mesh-claim-link-unit` or
  `emco-mesh-claim-link-process`, running `scripts/claim-link.py`. It makes
  a PTY and hands the PTY's slave to the node's uid (0600), at
  `/run/emco-mesh-claim-unit/tty` or `/run/emco-mesh-claim-process/tty`.
  systemd binds the host's `/dev/pts` into the node's `PrivateDevices`
  `/dev` and allows PTY devices there, so the node opens it by that path
  and keeps `PrivateDevices`. The node unit wants that unit, starts after
  it and restarts with it (a new PTY).
* The daemon always drains the PTY, because the node writes every
  publication to its local link as well.
* It forwards to a socket beside the PTY, `/run/emco-mesh-claim-<node>/sock`,
  that only root can use: root:root 0600 in a root 0755 directory, and the
  daemon also checks every client's uid (`SO_PEERCRED`) and closes anyone
  but root.
* **Shared uid (known gap, pre-existing).** Both nodes run as one uid,
  `mw-posix-node` (the process node's unit copies the unit node's, state
  directories included). So either node can open the other's PTY slave,
  which is the node's own end: it could read what root sends that node and
  answer in its place. Neither can reach the other's socket, where the
  claim's Origin::Serial authority comes from.
* `kd240-claim-link-vm` checks this under a real systemd (a VM of the
  board's release, the real `mesh.nix` and posix-node module, a stand-in
  node): root reaches the node; the node's uid and another user cannot
  connect, and with the socket's mode opened up the daemon still refuses
  the node's uid.

`scripts/claim.sh` claims a node from the laptop. socat makes a PTY on the
laptop, joined over `ssh admin@kd240.local sudo socat -
UNIX-CONNECT:…/sock` to that socket, and mwctl runs on it:

```sh
kd240/scripts/claim.sh unit    --host kd240.local --pkcs11 mw-release
kd240/scripts/claim.sh process --host kd240.local --pkcs11 mw-release
kd240/scripts/claim.sh unit --info        # trust info only
```

It runs `trust info`, then `trust claim`, then `trust info` again. A node
that is already claimed is left alone. It needs mwctl, socat and ssh on the
laptop. On the board, socat comes with the claim link.

* **Checks:**
  * Each unit's `MW_POSIX_NODE_LOCAL_SERIAL` and the socket beside it.
  * Each node's trust state, from `claim.sh --info`.
* **What `--apply` does:** with `--key` or `--pkcs11`, it runs `claim.sh`
  for each node that reports unclaimed.
* **Gaps:** two middleware fixes, neither in the lathe's pin (514f974):
  * **The node can't open a PTY.** posix-node's `open_serial` deasserts RTS
    and DTR, which a PTY refuses with ENOTTY, and that error is fatal. A
    node on the claim link would exit and restart for ever. Middleware
    `bc80998` (branch `net/w4b0-zenohd-static-pty`, not pushed yet) ignores
    ENOTTY there. So the board keeps the claim link **off**:
    `claimLink.enable = kd240Mesh.ptyLocalSerial`, and `flake.nix` sets
    that to `false`. Once the pin includes `bc80998`, set it to `true` and
    deploy.
  * **mwctl can't open a PTY either.** `tools/mwctl/src/port.rs` has the
    same RTS/DTR calls and fails with `serial port: Not a typewriter` on
    socat's PTY. This was checked with the W4a and `stack/posix-node`
    builds. It needs the same fix as `bc80998`. `claim.sh` names this
    error when it hits it.
* Until then the step reports **blocked (gap)**, and both nodes run
  **unclaimed**. An unclaimed node accepts any commit locally (trust on
  first use). Over Zenoh it refuses `begin`, `commit`, `start` and `bind`
  (W4a), so remote installs wait for the claim.
* `kd240-claim-link` tests the daemon and `claim.sh` without a board, with
  stand-ins for the node and mwctl (`nix/tests/claim_link.py`,
  `nix/tests/claim-smoke.sh`).

**b. The fleet's order key (M1, fleet on middleware trust).** The fleet
agent obeys orders signed by a key in `trustedKeys`, using the same key
types and verifier as middleware payloads ([fleet/README](fleet/README.md),
"One-time setup" and "The board"):

```sh
emco-fleet pubkey --pkcs11 emco-fleet > orders.pub    # card key (mwctl key new --pkcs11 emco-fleet)
emco-fleet keygen --name emco-fleet-1 --out orders.sec > orders.pub   # or a software key
emco-fleet token --device kd240 > kd240.token-pair    # line 2 goes to the board
```

Then set `services.emco-fleet-agent.trustedKeys = [ "<orders.pub>" ]`
and put the token in `/var/lib/emco-fleet-agent/token` (mode 0600).

* **Checks:**
  * The agent's `--trusted-key` arguments.
  * Its token file.
  * `~/.config/emco-fleet/orders.pub` on the laptop.
* **Gap:** the agent exists only on the A/B layout
  (`nixosConfigurations.kd240-ab`, `nix/ab.nix` imports `nix/fleet.nix`).
  That layout has never booted on a board. The default `kd240`
  configuration has no agent.
* `--apply` does nothing here. Choosing a card key or a software key is
  the owner's call.

## 3. Sign and install the payloads

The payloads are not in the image. A fresh card runs both nodes empty
(`nix/mesh.nix`). Build them ([cnc/README](../cnc/README.md)):

```sh
cd cnc
cargo build --release --target wasm32-unknown-unknown -p cnc-physical
cargo build --release --target wasm32-unknown-unknown -p cnc-process   # separately
```

zenohd on the board is loopback-only. So mwctl reaches it through an SSH
tunnel, and each node is addressed by its id from `/run/emco-mesh/ids`:

```sh
ssh -N -L 127.0.0.1:17447:127.0.0.1:7447 admin@kd240.local &
M="mwctl --zenoh tcp/127.0.0.1:17447"
$M --node $UNIT info
$M --node $UNIT upload cnc/target/wasm32-unknown-unknown/release/cnc_physical.wasm --pkcs11 LABEL
$M --node $UNIT bind   cnc/target/wasm32-unknown-unknown/release/cnc_physical.wasm \
    x_pos=driver:x_pos z_pos=driver:z_pos x_home=driver:x_home z_home=driver:z_home \
    spindle_rpm=driver:spindle_rpm spindle_actual=driver:spindle_actual \
    turret_station=driver:turret_station turret_ready=driver:turret_ready \
    estop=driver:estop motion=driver:motion queue=driver:motion --pkcs11 LABEL
$M --node $UNIT start --pkcs11 LABEL
$M --node $PROCESS upload cnc/target/wasm32-unknown-unknown/release/cnc_process.wasm --pkcs11 LABEL
$M --node $PROCESS start --pkcs11 LABEL
```

The binding is `cnc_model::IoMap::binding_specs()` for the default map,
which uses the simulator's point names (`cnc/model/src/config.rs`,
`model/fixtures/pc_turn_50.json`). The process payload takes no binding
(`cnc/hil/tests/portable.rs`).

* **What `--apply` does:**
  * Builds any payload that is missing.
  * Opens the tunnel.
  * Asks the unit node for `info`, then runs the commands above.
  * Needs `--key` or `--pkcs11`, because over Zenoh mwctl refuses unsigned
    upload, bind and start.
* **Gaps:**
  * **posix-node serves no `sys/*` keys over Zenoh** at the pinned
    middleware (514f974). Its sys requests go through `gw/sys` (gRPC with a
    SPIFFE principal, `MW_GATEWAY_UPDATERS`, `mw-update`) or its local
    port, and the KD240 sets up neither. So `info` gets no answer, and the
    script stops there and reports the gap. This needs **W4a**.
  * **mwctl is not a flake output.** Build it from the pin with
    `cargo install --locked --git https://github.com/jvishnefske/middleware
    --rev 514f9747c114f70058361db13365f5efd806775e --features zenoh
    mwctl`.
  * **The signed control target.** mwctl over Zenoh signs start/stop for
    the RT1050's address rule (`02` plus the node id's last five bytes)
    unless `--target` is given. `gw/sys` uses the node id's first six
    bytes. W4a decides which rule posix-node uses. Until then the right
    `--target` is unknown.
  * **The physical payload only binds against `sim`.** The binding names
    the simulator's points. The KD240 driver has only `pl_present` plus the
    configured inputs, so with `driver = "kd240"` the bind cannot resolve.
    Use step 5's `sim` driver until a KD240 IoMap exists.
  * mwctl's help is ambiguous about `bind` over Zenoh. Its option text says
    a key is "required over Zenoh", but bind is not in the module's list of
    Zenoh commands. Check this when W4a lands.

## 4. Enable `ui` and `fleetInterlock`

Only once both payloads run. With no payload the UI shows `disconnected`,
and the interlock fails closed and defers every reboot (`nix/mesh.nix`).
In `nix/configuration.nix`:

```nix
  services.emco-mesh = {
    enable = true;
    posixNodePackage = kd240Mesh.posixNode;
    zenohdPackage = kd240Mesh.zenohd;
    driver = "kd240";
    ui = true;              # emco-kd240 --mesh: the UI drives the payloads
    fleetInterlock = true;  # the fleet agent's reboot interlock also needs the process state
  };
```

Then run `just deploy` (`nixos-rebuild switch --flake ..#kd240
--target-host admin@kd240.local --sudo`).

* **Checks:**
  * The two options as this checkout evaluates them (`nix eval`).
  * Whether `emco-kd240` runs with `--mesh`, and `GET /api/state` has
    `transport: "mesh"`.
  * Whether the fleet agent reads `/run/emco-mesh/fleet.env`.
* **What `--apply` does:** the script never edits the configuration. It
  deploys only if `ui` already evaluates to true.
* **Note:** `fleetInterlock` acts only where `services.emco-fleet-agent`
  exists, that is on `kd240-ab`. On `kd240` it does nothing.

## 5. Bench test: the UI drives the simulated lathe

The M6 bench test sets `services.emco-mesh.driver = "sim"` (a simulated
PC Turn 50 in `mw-posix-node`; middleware `posix-node/src/driver/sim.rs`)
with `ui = true`, deploys, and installs the payloads (step 3). The UI at
`http://kd240.local:8080/` then drives the process model, which drives
the physical model, which drives the simulator. The commands it handles
are Home, Run (with the configured belt), Hold, Resume and Abort. Jog,
MDI, touch-off, single block and dry run are refused with an alarm
([README](README.md), "On the payloads").

* **Checks:**
  * The physical node's `MW_POSIX_NODE_DRIVER` is `sim`, and its journal
    says `driver sim`.
  * `GET /api/state` has `transport: "mesh"` and a `mesh.processState`.
* **What `--apply` does:** only with `sim` and a live process state, it
  sends `POST /api/command {"kind":"home"}` with the board's token
  (`/var/lib/emco-kd240/token`) and waits up to 60 s for
  `session.dro.homed`.
* **By hand:** load a program in the UI, then Run, Hold, Resume, Abort.
* This test has not run on the board yet. It needs W4a (step 3) first.

## 6. cyclictest

Doc 65 S1 measured the PREEMPT_RT kernel at a 1 ms period on all four
A53 cores:

```sh
stress-ng --cpu 4 --io 2 --vm 2 --vm-bytes 128M &
cyclictest -m -S -p 90 -i 1000 -h 1000 -D 300
```

The worst case was 158 µs over 5 minutes and **246 µs over 3 hours**
under that load, with no overflows (none over 1 ms). The pass threshold
here is **every core's max ≤ 246 µs and zero histogram overflows**. That
is the 3-hour baseline, a quarter of doc 60 R8's 1 ms servo budget.

* **Checks:** a PREEMPT_RT kernel, and `cyclictest` and `stress-ng` on the
  board (`rt-tests` and `stress-ng` are in the image).
* **What `--apply` does:** runs the load and `cyclictest … -q -D
  <--cyclictest-s>` (default 300 s), parses `# Max Latencies` and
  `# Histogram Overflows`, and compares them with the threshold.
  `--no-load` runs without the load.
* A 5-minute run under load should pass easily. The 246 µs figure is a
  3-hour worst case. Not measured yet: the real system's load (USB,
  Ethernet, SD, the PL's interrupts) and an isolated RT core (doc 65).

## 7. RS-485 DE stays low, and the GPIO inputs

**RS-485.** PS UART0 is `/dev/ttyPS0`. The device tree runs it in RS-485
mode with DE on `rts-gpios`, so Linux asserts DE only while transmitting.
The service opens the port read-only (`nix/configuration.nix`, README).
The repository doesn't name the DE GPIO line. It comes from the mainline
device tree (`zynqmp-smk-k24-revA-sck-kd-g-revA.dtb`).

* **Checks:**
  * `ttyPS0`'s device-tree node has `linux,rs485-enabled-at-boot-time`
    and `rts-gpios`.
  * The DE line in `/sys/kernel/debug/gpio` reads `out lo`, if debugfs
    names it.
  * Which processes hold `/dev/ttyPS0`.
* **By hand (required before attaching to `X300`):** meter DE, or watch
  A/B with an analyser, while a capture runs from the UI. It must stay
  low ([README](README.md), "What the capture is and is not").

**GPIO inputs.** The physical node's `kd240` driver reads switches on
GPIO lines from `services.emco-mesh.kd240Inputs`
(`MW_POSIX_NODE_KD240_INPUTS`, `point=chip:offset[:low]`, for example
`x_home=gpiochip2:12:low`). It also reads `pl_present`, a UIO device named
`emco*`. It has no outputs.

* **Checks:**
  * The `/dev/gpiochip*` devices.
  * The configured inputs.
  * The node's supplementary groups.
  * The node's `driver …` log line.
* **Gaps:**
  * **No pin map.** `kd240Inputs` is empty, and the repository names no
    real lines (the option's example is illustrative). The switches' wiring
    comes from doc 65 §1–2, which is still on paper.
  * **No device access.** `mw-posix-node` runs with `PrivateDevices` and
    no supplementary group, and there is no udev rule for
    `/dev/gpiochip*`. `mesh.nix` says this is "not set up here yet".
  * **No tool shows the control-module state** (`app/unit/<node>/cm`) to
    watch a switch toggle. `libgpiod`'s `gpioinfo` is not in the image
    (`nix shell nixpkgs#libgpiod -c gpioinfo` on the board).

## 8. Enroll the uplink certificate

**The uplink** (`services.emco-mesh.uplink`, `nix/mesh.nix`) is a second
zenohd, `emco-mesh-uplink`. It listens on nothing and links the board's
loopback router to the cloud mesh router at
`tls/mesh.updates.vishnefske.com:443`, over mTLS.

* **The local zenohd is unchanged** whether the uplink is on or off: the
  same unit and the same config, byte for byte. A missing or bad
  certificate stops only the uplink.
* **Its ACL is default deny.**
  * In from the mesh: queries for the payloads' `info`, `trust/info`,
    `manifest`, `manifest/cm`, `payload/{binding,start,stop,bind}` and
    `loader/{begin,chunk,commit}`, and subscriptions to `app/proc/*/state`.
  * Out to the mesh: the payloads' `@mw/node/*/sys/**` queryables, their
    replies, and the process state.
  * Never: `app/unit/**` in either direction, `trust/claim`, `board`,
    `peers`, `fw/*`, `log`, `status`, or the board's other local
    declarations.
* **It is off by default.** To turn it on, in `nix/configuration.nix`:

```nix
  services.emco-mesh.uplink.enable = true;
```

Then `just deploy`. That adds the unit, its user, `/var/lib/emco-mesh/tls`
(`ca`, `cert`, `key`; root:emco-mesh-uplink 0750) and `emco-mesh-enroll`.

* **The clock.** The KD240 has no RTC. `systemd-time-wait-sync` holds
  `time-sync.target` for up to 5 minutes, and the uplink starts after it.
* **Its start check.** The uplink's `ExecStartPre` is `emco-mesh-enroll
  check`. With no certificate, or a bad one, the uplink does not start, and
  it retries every 2 minutes.
* **Expiry.** A daily timer (`emco-mesh-uplink-expiry`) runs the same
  check. It stops the uplink once the certificate fails it, because a TLS
  session would otherwise outlive the certificate.

**The certificate.** The board's identity is its **unit node id** (the
`UNIT=` of `/run/emco-mesh/ids`, which `sys/info` and mDNS also show), with
CN `kd240-<unit id>`. The key is made on the board and never leaves it:

```sh
ssh admin@kd240.local sudo emco-mesh-enroll csr > kd240-<id>.csr   # key (P-256, once) + CSR
# on the CA host (W3b, edge-ca with the Zenoh CA):
edge-ca issue --ou kd240 <id> --csr kd240-<id>.csr --out kd240-<id>.crt
edge-ca ca --zenoh > zenoh-ca.pem
scp kd240-<id>.crt zenoh-ca.pem admin@kd240.local:/tmp/
ssh admin@kd240.local sudo emco-mesh-enroll install /tmp/kd240-<id>.crt /tmp/zenoh-ca.pem
```

Then add `"kd240-<id>"` to the `kd240` subject's `cert_common_names` in the
mesh router's `zenohd.json5` (homelab-flux, `MESH.md`).

**What `emco-mesh-enroll install` checks.** It refuses any certificate
that:

* is not for this board's key;
* doesn't chain to the CA for client use;
* is expired;
* is not valid yet (the message says to check the clock); or
* has a CN other than `kd240-<unit id>`.

A refused install changes nothing. A reflashed card gets a new machine id,
and so a new unit id, so its old certificate is reported as another
board's. `emco-mesh-enroll status` shows the certificate, the unit and the
uplink zenohd's connection. `csr --rotate` makes a new key and keeps the
old one as `key.old`.

* **Checks:**
  * `emco-mesh-enroll` on the board.
  * `emco-mesh-enroll status` and `check`.
  * `emco-mesh-uplink` active.
  * The uplink zenohd's established connection to port 443.
  * Whether `edge-ca` is on this machine.
* **What `--apply` does:**
  * With `--cert FILE --ca FILE`, it copies both to the board, installs
    them and checks again.
  * Otherwise, if not yet enrolled, it fetches a CSR (`emco-mesh-enroll
    csr`, which keeps an existing key) into `./kd240-<id>.csr`, and reports
    **blocked** until the certificate is signed on the CA host.
* **Gaps:**
  * The mesh router (W3c) must be deployed, with DNS for
    `mesh.updates.<site>`, and the CN must be enrolled in its ACL.
  * The board's zenohd is nixpkgs' 1.10.0 until the middleware pin has
    `f70ea67` (W4b-0). After that it is middleware's static 1.10.1 cross
    build (`flake.nix`, `kd240Mesh.zenohd`; `zenohdSource` says which).
* **Tested without a board:**
  * `kd240-uplink-eval`: the generated configs, the unit and the ACL.
  * `kd240-uplink-config`: a real zenohd runs the config.
  * `kd240-enroll`: the script against a throwaway CA.
  * `nix/tests/uplink-chain.sh`: a real chain of three zenohd.

## Gaps, in one list

| Step | What the bring-up needs | Where it would come from |
|---|---|---|
| 1 | a freshly written image booted once (the board's card was converted in place) | a bench run |
| 2 | a posix-node that opens a PTY as its local port (the claim link stays off until then) | middleware `bc80998` (W4b-0), then `ptyLocalSerial = true` in `flake.nix` |
| 2 | an mwctl that opens a PTY (`--port` on claim.sh's socat PTY) | middleware, `tools/mwctl/src/port.rs` (as `bc80998`) |
| 2 | the fleet agent on the default configuration, or the A/B card booted | `nix/ab.nix` on a board |
| 2 | one uid per node: both run as `mw-posix-node` (pre-existing; either can open the other's claim PTY slave and state directory, not its claim socket) | `nix/mesh.nix` (a `User=` for the process node) |
| 3 | posix-node's `sys/*` keys over Zenoh | W4a (middleware) |
| 3 | an `mwctl` package (`--features zenoh`) from the flake | middleware flake / lathe flake |
| 3 | the signed control target rule for posix-node over Zenoh | W4a |
| 3, 7 | a KD240 IoMap and pin map (the physical payload binds only to `sim`) | doc 65 §1–2, `cnc/model` |
| 4 | `fleetInterlock` doing anything on `kd240` (needs the agent) | as step 2 |
| 7 | GPIO device access for `mw-posix-node` (group, udev rule) | `nix/mesh.nix` |
| 7 | a tool to watch `app/unit/<node>/cm` | — |
| 8 | the mesh router deployed, DNS for `mesh.updates.<site>`, the board's CN in its ACL | W3c (homelab-flux) |
| 8 | middleware's static zenohd 1.10.1 on the board (nixpkgs' 1.10.0 until then) | middleware `f70ea67` (W4b-0) |
| 8 | `edge-ca issue --ou kd240 --csr` | W3b |
