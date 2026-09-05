# emco-sender-core

The pure, synchronous Rust core of the browser G-code sender for the EMCO
PC Turn 50 lathe. It owns parsing, backplot geometry, protocol framing,
machine simulation and session state. It performs **no I/O and never reads a
clock** — the TypeScript UI (`sender/web`) feeds it bytes and timestamps and
renders what comes out. The contract between the two sides is
[`sender/INTERFACE.md`](../INTERFACE.md).

> **The AC95 wire protocol is UNVERIFIED.** Everything this crate assumes
> about framing, acknowledgement, flow control and error text is a community
> best guess, kept behind the `Dialect` trait and marked with
> `TODO(protocol)` / "UNVERIFIED" comments. Real findings land in
> `docs/30-serial-protocol.md` and get folded into the `Ac95` dialect one
> seam at a time. The same goes for every number in the default machine
> profile (travels, rapids, accel, spindle range): placeholders until someone
> measures the actual lathe.

## Module map

| Module | What it owns |
|---|---|
| `gcode` | Lexer + modal interpreter for the Fanuc-0T-style subset: word-address blocks, `/` block skip, `( )` and `;` comments, motion group, G20/G21, G90/G91, G94/G95, diameter/radius mode (G07/G08, **diameter is the default**), G54–G59, `T`/`S`/`M03..M05`/coolant/`M30`. Problems become `Diagnostic`s; parsing never panics and never stops at a bad line. **X is radius internally**; diameter programs are converted during interpretation. |
| `geom` | Turns blocks into backplot `Segment`s (rapid/feed/arc/thread), with arcs in both I/K and R form (negative R = major arc; full circles rejected), G33 as a thread move, G76 simplified to a single thread pass (warned). Bounds include exact arc extrema, not just endpoints. |
| `dialect` | The `Dialect` trait plus `PlainGcode` (CR-terminated lines, `ok`/`error`/`alarm:` lines or raw ACK/NAK bytes, XON/XOFF + hardware-CTS gating, one block outstanding) and `Ac95` (currently a delegating wrapper with `TODO(protocol)` seams). |
| `machine` | `VirtualMachine`: consumes the same bytes as the real control, moves X/Z with trapezoidal velocity profiles under the profile's rapid/feed/accel limits, indexes a turret with a delay, tracks the spindle, enforces soft limits (including arc sweep extents), and emits alarms. `inject_fault("estop" \| "limit_x" \| "limit_z" \| "turret_jam")` for training scenarios. |
| `session` | `Session`: the run state machine (`disconnected`/`idle`/`running`/`hold`/`alarm`/`homing`/`toolchange`), block queueing gated on `Dialect::wants_next_block`, current-line tracking, predicted DRO, jog/MDI/single-block/dry-run commands, and `Event` emission. `tick(now_ms, rx)` is the only pump; time always comes from the caller. |
| `profile` | `MachineProfile` (de)serialization and the **UNVERIFIED** PC Turn 50 default. |
| `wasm` | Thin `wasm-bindgen` wrappers (`Program`, `VirtualMachine`, `Session`), compiled only on `wasm32` targets. |

Because the wire protocol has no confirmed position feedback, the session's
DRO is a *prediction*: it interprets each block it sends with the same modal
machinery as the parser, interpolates along the in-flight move, and snaps to
the block's target on acknowledgement. The `VirtualMachine` keeps its own
ground truth, and the end-to-end tests assert the two agree.

## Testing

```sh
cargo test
```

That is the whole story — native, no browser, no wasm toolchain, no nix. The
suite covers the lexer (including malformed input), modal transitions,
diameter-vs-radius handling, arc geometry and bounds, dialect framing and
flow control, the virtual machine's motion/turret/fault behaviour, the
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
  `valueMm` afterwards).
- `Event::raw` carries bytes as a plain number array over the JS boundary,
  not a `Uint8Array`.
- `Session.set_cts(bool)` is an extra export so the UI can report the
  hardware CTS line when the transport exposes it.

## Safety

Nothing in this crate is a safety function. `abort` is advisory — it sends
whatever stop the dialect offers and drops the link. The real stop is the
mains switch on the control box with a person standing at the machine. Do
not build unattended or remote operation on top of this.
