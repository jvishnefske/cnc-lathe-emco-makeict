# cnc/: the lathe as S88 models on middleware

The lathe split the ISA-88 way, as WASM payloads that run on
`mw-posix-node` (the middleware's Linux payload host):

* a **physical model** (`physical/`): the lathe *unit* and its equipment
  modules (motion with X and Z, the spindle, the turret, the safety chain),
  built for **one machine** (its `PhysicalConfig`). It serves the unit's
  equipment phases and publishes every control module's state. Its machine
  channels bind to the node's control-module points (ABI 8), which a native
  driver owns: the simulator, the KD240's inputs, later LinuxCNC.
* a **process model** (`process/`): the CNC business logic, separate from
  any machine: operator commands, the reference gate, the screen's
  preflight (`emco-rules`), and G-code (the browser sender's own
  interpreter) compiled into the unit's phase commands and motion segments.
  It learns the machine from the unit it is assigned (`app/unit/<node>/info`),
  so **one build runs on every physical model**: `hil/tests/portable.rs`
  runs the same process `.wasm`, byte for byte, against two physical builds
  for two different machines and checks that they agree.

```
 process payload        app/unit/<node>/phase/<em>.<phase>    (recipe-core commands, published)
   │                    app/unit/<node>/motion/segments        (machine-coordinate segments)
   ▼                    ◄── .../phase/<em>.<phase>/state, app/unit/<node>/cm
 physical payload       machine channels: x_pos z_pos x_home z_home spindle_rpm spindle_actual
   │                                      turret_station turret_ready estop motion queue
   ▼  drv/<point>
 mw-posix-node driver   sim | kd240 (inputs only) | linuxcnc (later)
```

**Nothing here moves the real lathe.** The physical model never generates
steps: motion leaves it as whole segments, and a driver runs them in its
own time base. The KD240 driver has no outputs at all.

**Not yet authenticated: who may command a unit.** The physical model
takes phase commands and segments from any node that can publish on its
keys; a payload is not told who sent a publication. Before a driver with
outputs exists, commands must come only from the process model it is
assigned: the runtime can already do this for a channel bound as a
`Remote` target (only that node's messages are delivered), which needs
the command keys declared as channels (more than the 16 a payload has), or
an ABI change that hands `on_message` the sender. What the physical model
does check: limits and travel, the threading limit and feed of a
synchronised move, one motion phase at a time, the e-stop (which stops the
spindle explicitly), and a reference before any move.

| | |
|---|---|
| `model/` (`cnc-model`) | `PhysicalConfig` (the physical half of the sender's `MachineProfile`, which embeds it, plus the reference and the I/O map), the integer `Limits`, the control-module traits, the wire (keys, `CmState`, segment batches). `fixtures/pc_turn_50.json` is the PC Turn 50, written by its test and checked by the browser sender's |
| `physical/` (`cnc-physical`) | the physical payload (about 26 KB): `lathe.rs` the pure model, `payload.rs` the ABI glue, `build.rs` bakes the machine's config in (`CNC_PHYSICAL_CONFIG=<json>`, default the PC Turn 50) |
| `process/` (`cnc-process`) | the process payload (about 190 KB; a `std` crate on wasm32, as the sender's core and the screen's rules are): `compile.rs` G-code to operations, `process.rs` the pure executor, `payload.rs` the ABI glue |
| `hil/` (`cnc-hil`) | host-in-the-loop tests: `build.rs` builds the payloads for wasm32 (the physical one twice, for the PC Turn 50 and `model/fixtures/alt_lathe.json`); the tests install, bind and start them in mw-posix-node's own controller with its simulator, and carry publications between nodes as Zenoh would |

Everything is a publication under `app/` (`model/src/wire.rs`): a gateway
carries only `app/**` between a node and the cluster, and payloads cannot
make requests. A command is acknowledged by the phase report naming its id,
as recipe-core allows.

```sh
cd cnc
cargo test                     # all of it; hil builds the payloads first (a few minutes the first time)
cargo build --release --target wasm32-unknown-unknown -p cnc-physical
cargo build --release --target wasm32-unknown-unknown -p cnc-process  # separately: the physical
                               # payload uses the SDK's panic handler, the process one std's
CNC_WRITE_FIXTURES=1 cargo test -p cnc-model   # after changing pc_turn_50()
```

The process model talks to its unit only through publications: operator
commands on `app/proc/<node>/cmd` (assign a unit, reference, run a program
with the fitted belt, hold, resume, stop, clear), programs in chunks on
`app/proc/<node>/program`, its state on `app/proc/<node>/state` (on
every change and once a second, naming the unit it drives). The codecs
are `cnc_model::proc`, so a tool that drives the process model (the
KD240 service, the fleet agent) shares them without the payload's
dependencies. Programs
are held in memory for now; storing them in the node's kv needs a kv
import the ABI does not have yet.

Middleware comes as git dependencies pinned to a commit of
github.com/jvishnefske/middleware (stack/posix-node); `cnc/Cargo.toml`
shows how to build against a local checkout instead. A Linux node gives a
payload 16 subscriptions and 20 M instructions per call (a process
model's preflight and compile run in one), where a microcontroller gives 8
and 1 M.
