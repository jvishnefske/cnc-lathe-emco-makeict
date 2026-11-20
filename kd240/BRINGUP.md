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
`--pkcs11 LABEL` (the mwctl signer for steps 2 and 3), `--tunnel-port`
(step 3, default 17447), `--cyclictest-s N` and `--no-load` (step 6).

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
start/stop. The middleware README ("Linux payload host") says to claim
over the node's local port, as `mwctl --port <local port> trust claim
--key …`. A claim never passes `gw/sys` or Zenoh (mwctl refuses
`trust claim` over `--zenoh`).

* **Gap:** neither node has a local port. `MW_POSIX_NODE_LOCAL_SERIAL` is
  set by neither `mesh.nix` nor the posix-node module, so the board
  offers no way to claim them. Both nodes run **unclaimed**. An unclaimed
  node accepts any commit (trust on first use).
* The script checks both units and `/var/lib/mw-robot/robot.env` for the
  variable. Even when it is set, the claim stays a manual step on the
  board. The script doesn't forward a serial port.

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

## 8. Enroll the uplink certificate (depends on W4b)

**Nothing for this step exists in the repository yet.** W4b adds
`services.emco-mesh.uplink` and `emco-mesh-enroll`. The planned flow:

```sh
ssh admin@kd240.local sudo emco-mesh-enroll         # key + CSR on the board (the key never leaves it)
edge-ca issue <node-id> --ou kd240 --csr <file>     # on the CA host (W3b, nixos-x86-squashfs; TPM)
# copy the certificate back to /var/lib/emco-mesh/tls on the board
```

* **Checks:**
  * `emco-mesh-enroll` on the board.
  * `edge-ca` on this machine.
  * What `/var/lib/emco-mesh/tls` holds.
* While W4b and W3b are missing, this step reports **blocked (gap)**, not
  FAIL. `--apply` only prints the three commands, because W4b fixes the
  file names.

## Gaps, in one list

| Step | What the bring-up needs | Where it would come from |
|---|---|---|
| 1 | a freshly written image booted once (the board's card was converted in place) | a bench run |
| 2 | a local port on each node, to claim it (`MW_POSIX_NODE_LOCAL_SERIAL`) | `nix/mesh.nix` |
| 2 | the fleet agent on the default configuration, or the A/B card booted | `nix/ab.nix` on a board |
| 3 | posix-node's `sys/*` keys over Zenoh | W4a (middleware) |
| 3 | an `mwctl` package (`--features zenoh`) from the flake | middleware flake / lathe flake |
| 3 | the signed control target rule for posix-node over Zenoh | W4a |
| 3, 7 | a KD240 IoMap and pin map (the physical payload binds only to `sim`) | doc 65 §1–2, `cnc/model` |
| 4 | `fleetInterlock` doing anything on `kd240` (needs the agent) | as step 2 |
| 7 | GPIO device access for `mw-posix-node` (group, udev rule) | `nix/mesh.nix` |
| 7 | a tool to watch `app/unit/<node>/cm` | — |
| 8 | `emco-mesh-enroll`, `services.emco-mesh.uplink` | W4b |
| 8 | `edge-ca issue --ou kd240 --csr` | W3b |
