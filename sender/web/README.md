# EMCO PC Turn 50 — browser G-code sender

A browser UI for sending G-code to the MakeICT EMCO PC Turn 50 lathe, with a
live toolpath backplot, DRO, serial console and a fully simulated machine for
training. The heavy logic (parsing, geometry, protocol, machine state) lives
in `sender/core` (Rust → WASM); this app is the I/O and rendering shell around
it, per the contract in `sender/INTERFACE.md`.

## Run it

Node is provided by the repo's nix dev shell:

```sh
cd <repo-root>
nix develop --command bash -c "cd sender/web && npm ci && npm run dev"
```

Then open the printed URL (usually <http://localhost:5173>). Other scripts:

```sh
npm run build   # typecheck + production build into dist/
npm test        # vitest, headless
```

No hardware is needed: the default transport is **Simulated machine**, an
in-process model of the control box. Click Connect, load a program (or the
built-in Demo), Home, Run.

## Cores: WASM and mock

- **WASM core** — the real thing, built from `sender/core` with
  `wasm-pack build --target web` (output in `sender/core/pkg`). Loaded
  automatically when present.
- **Mock core** — a pure-TypeScript stand-in (`src/core/mock*`) that parses a
  useful G-code subset, backplots, and simulates both ends of the wire. It is
  what makes the app usable for operator training with no Rust toolchain at
  all, and it backs the unit tests.

Force a core with `?core=wasm` or `?core=mock` in the URL; the active core is
shown in the connection bar.

## Browser requirements

- Any modern browser runs the app in **Simulated machine** mode.
- **Web Serial** (real hardware) is Chromium-only (Chrome/Edge) and only on
  `localhost` or HTTPS. The UI says so and disables the option elsewhere —
  Firefox and Safari do not implement Web Serial.
- The optional webcam pane uses `getUserMedia` (same localhost/HTTPS rule).
  The video never leaves the page.

## Talking to the real machine: RS-485, not RS-232

The machine's rear DB9 (control-box connector `X300`) is an **RS-485**
differential bus — pin 2 GND, pin 3 DATA−, pin 8 DATA+ (see
`docs/_machine-facts.md`). To reach it you need a **USB-RS485 adapter**
(half-duplex, A/B terminals). A USB-RS232 cable is the **wrong electrical
standard** for this port: it will not work and risks damaging the control.

Beyond the electrical layer, the wire **protocol is not yet known** — the
original DOS PC drove this bus through a memory-mapped ISA card, not a UART.
The Web Serial transport therefore exists mainly as a capture tool for the
protocol reverse-engineering effort: the serial console records every byte
with timestamps, in hex and ASCII, and its **Export capture** button writes a
complete transcript for analysis. The UART settings in the connection bar are
adjustable precisely because nobody knows the right ones yet.

## Safety

- The on-screen **E-STOP** (and the abort command behind it) sends a stop
  request over the serial link. That is **advisory, not a safety function**:
  software, a USB cable and a browser are all failure points. The real
  emergency stop is the mains switch on the control box, with a person
  standing at the machine.
- The machine has **no absolute position memory and no end-of-travel limit
  switches**. Every power-off loses the reference; the UI shows a NOT
  REFERENCED banner until you home, and homing should always be the first
  action after connecting. An unreferenced machine can be crashed into its
  mechanical stops by an otherwise-correct program.
- Nothing in this app supports unattended or remote operation, on purpose.

## Layout

- **Connection bar** — transport picker (simulated by default), UART settings,
  fault injection (training), link status, active core.
- **Toolpath view** (the centrepiece) — lathe convention: Z horizontal with
  negative to the left toward the chuck, X vertical as radius, profile
  mirrored about the centreline, stock silhouette and chuck drawn behind.
  Drag to pan, wheel to zoom, double-click or **Fit** to fit. Executed path is
  drawn distinct from pending; the tool marker tracks the DRO. Arcs are
  tessellated to the current zoom.
- **DRO** — X as diameter and radius, Z; machine vs work coordinates and
  distance-to-go; mm primary with inch alongside; REFERENCED/NOT REFERENCED.
- **Program pane** — file picker or drag-drop, line numbers, current line
  highlighted, diagnostics inline on the offending line; stock size entry.
- **Controls** — Run / Hold / Resume, Single block, Dry run, Home, X/Z jog
  with step selector, MDI line, and the E-STOP with its safety note.
- **State panel** — run state, turret station (6 stations: 3 external,
  3 internal), spindle, feed, modal codes, alarms.
- **Serial console** — timestamped tx/rx transcript, hex/ASCII toggle,
  complete export (the protocol-discovery capture tool).
- **Webcam** — optional, collapsible, local-only.

## Tests

`npm test` runs Vitest headless over the pure logic: the mock parser
(diameter/radius modes, units, arcs incl. the R-form), backplot geometry and
arc-extrema bounds, toolpath view math (fit, zoom-about-cursor, zoom-dependent
arc tessellation, mirroring), unit conversion, the transport contract against
fakes, session control flow (run / hold / resume / abort / single block / dry
run / homing / jog / MDI / work offsets / alarms), and the console capture
format. Rendering itself is not pixel-tested.
