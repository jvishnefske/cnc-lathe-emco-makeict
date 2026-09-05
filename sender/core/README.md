# emco-sender-core

The pure, synchronous Rust core of the browser G-code sender for the EMCO
PC Turn 50 lathe. It owns parsing, backplot geometry, protocol framing,
machine simulation and session state. It performs **no I/O and never reads a
clock** — the TypeScript UI (`sender/web`) feeds it bytes and timestamps and
renders what comes out. The contract between the two sides is
[`sender/INTERFACE.md`](../INTERFACE.md).

> **The real machine's wire protocol is UNKNOWN.** The PC link is **RS-485**
> (control-box connector `X300`: pin 2 GND, pin 3 DATA−, pin 8 DATA+), driven
> on the original DOS PC by a proprietary memory-mapped ISA card
> (`Y4A013000`) — **not a UART** — so there is no known byte-stream framing
> at all. There is also no "AC95" board in this machine; the brain is the
> axis-controller / CPU board `Y4A012000`. Consequences here:
> `PlainGcode` is a **development and training fiction** spoken by the
> `VirtualMachine`, and `EmcoStub` is an explicit stub with `TODO(protocol)`
> seams awaiting discovery findings (`docs/30-serial-protocol.md`). All
> framing assumptions live behind the `Dialect` trait; none leak into
> `Session`, the geometry, or the public state types.
> See `docs/_machine-facts.md` for the consolidated machine facts.

The default `MachineProfile` carries the real numbers from EMCO's manuals
(48 mm of X travel as a **radius**, 228 mm of Z travel running negative from
the reference at X48/Z0, rapid 750 mm/min, feed 0–750 mm/min, 0.001 mm
resolution, spindle 130–3000 rpm in three belt steps, threading ≤400 rpm,
6-station turret), with the same ±0.5 mm working margins as the LinuxCNC
simulator config (`linuxcnc/sim/emco-pcturn50/`). They are documented but
**not re-measured at the machine**; acceleration and turret index time are
genuinely UNVERIFIED guesses.

**Work offsets:** both `Session` and `VirtualMachine` default G54 to
X0 / **Z −50** — "a piece of stock whose face was touched off 50 mm in from
the reference point" — matching the LinuxCNC sim's parameter file (var 5223),
so the sender, the LinuxCNC sim and the example programs in `gcode/` all run
the same files. `G53` moves are machine coordinates (and the diameter-mode
trap applies to them too: fully retracted is `G53 X96` in diameter mode, not
`X48`). Touch-off (`setWorkOffset`) is sent as a `G10 L2` block through the
normal pipeline, so the machine's offset and the session's prediction always
move together.

## Module map

| Module | What it owns |
|---|---|
| `gcode` | Lexer + modal interpreter for the Fanuc-0T-style subset: word-address blocks, `/` block skip, `( )` and `;` comments, motion group, G20/G21, G90/G91, G94/G95, diameter/radius mode (G07/G08, **diameter is the default**), work offsets with `G53` machine-coordinate moves and `G10 L2` offset setting, G54–G59, `T`/`S`/`M03..M05`/coolant/`M30`. Problems become `Diagnostic`s; parsing never panics and never stops at a bad line. **X is radius internally**; diameter programs are converted during interpretation. Warns when threading above the 400 rpm sync limit. |
| `geom` | Turns blocks into backplot `Segment`s (rapid/feed/arc/thread), with arcs in both I/K and R form (negative R = major arc; full circles rejected), G33 as a thread move, G76/G78 threading cycles simplified to a single thread pass (warned). Bounds include exact arc extrema, not just endpoints. |
| `dialect` | The `Dialect` trait plus `PlainGcode` (the development fiction: CR-terminated lines, `ok`/`error`/`alarm:` lines or raw ACK/NAK bytes, XON/XOFF + CTS gating, one block outstanding) and `EmcoStub` (the real machine's dialect — an explicit stub delegating to `PlainGcode` until protocol discovery lands). |
| `machine` | `VirtualMachine`: consumes the development byte protocol and behaves like the real control. **Starts un-referenced** (no absolute position memory); homing references off the simulated per-axis sensors at X48/Z0. **No end-of-travel limit switches by default**: overtravel pins the slide at the mechanical stop while the control counts on — a silent stall with lost position — until re-referenced. Set `hasEndOfTravelLimits: true` (retrofit) for clean soft-limit rejection instead. Trapezoidal velocity profiles, turret index delay, spindle state, threading rejected above `threadingMaxRpm` (alarm 8010). `inject_fault("estop" \| "limit_x" \| "limit_z" \| "turret_jam")` for training scenarios. |
| `session` | `Session`: the run state machine (`disconnected`/`idle`/`running`/`hold`/`alarm`/`homing`/`toolchange`), block queueing gated on `Dialect::wants_next_block`, current-line tracking, predicted DRO, jog/MDI/single-block/dry-run commands, `Event` emission. Includes a **sender-side soft-limit net**: a move whose swept **machine-coordinate** extent (after work offsets) leaves the profile travel is never transmitted (the machine itself has no limit switches to catch it). `tick(now_ms, rx)` is the only pump; time always comes from the caller. |
| `profile` | `MachineProfile` (de)serialization with the documented PC Turn 50 defaults, `spindleBeltSteps` (operator-declared — belts are moved by hand), `link: {kind: "rs485", baud}`. |
| `wasm` | Thin `wasm-bindgen` wrappers (`Program`, `VirtualMachine`, `Session`), compiled only on `wasm32` targets. |

Because the wire protocol has no confirmed position feedback, the session's
DRO is a *prediction*: it interprets each block it sends with the same modal
machinery as the parser, interpolates along the in-flight move, and snaps to
the block's target on acknowledgement. The `VirtualMachine` keeps its own
ground truth — including believed-vs-physical divergence after a stall — and
the end-to-end tests assert the two agree on clean runs.

## Testing

```sh
cargo test
```

That is the whole story — native, no browser, no wasm toolchain, no nix. The
suite covers the lexer (including malformed input), modal transitions,
diameter-vs-radius handling, arc geometry and bounds, dialect framing and
flow control, the virtual machine's motion/turret/stall/fault behaviour, the
session state machine, and an end-to-end run of a demo program through
`Session` + `VirtualMachine` in a lockstep loop.

## Building for wasm

```sh
# from the repo root, inside the nix dev shell
nix develop --command wasm-pack build sender/core --target web
```

or, to just type-check the wasm layer without wasm-pack:

```sh
cargo build --target wasm32-unknown-unknown
```

`wasm-pack` emits the JS/TS bindings package into `sender/core/pkg/` for
`sender/web` to import. The `wasm` module (and its wasm-bindgen dependency)
is gated on `target_arch = "wasm32"`, so native builds never touch it.

## Known deviations from INTERFACE.md

- `Dialect` has four extra methods beyond the contract's three
  (`note_block_sent`, `set_cts`, `control_bytes`, `reset`) — needed to drive
  flow control and realtime actions through the same swap-out seam.
- `Session` moves `disconnected → idle` on its first `tick`: to a
  transport-agnostic core, "being pumped" is what connected means.
- `setWorkOffset` uses touch-off semantics (current position reads
  `valueMm` afterwards) and is realised as a `G10 L2` block through the
  normal send/ack pipeline, only accepted while idle.
- `Event::raw` carries bytes as a plain number array over the JS boundary,
  not a `Uint8Array`.
- `Session.set_cts(bool)` is an extra export so the UI can report a CTS
  line if a transport has one (the fiction dialect uses it; the real RS-485
  link has no such signal).

## Safety

Nothing in this crate is a safety function. `abort` is advisory — it sends
whatever stop the dialect offers and drops the link. The real stop is the
mains switch on the control box with a person standing at the machine. Do
not build unattended or remote operation on top of this.
