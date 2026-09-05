# Sender interface contract

This file is the coordination point between `sender/core` (Rust → WASM) and `sender/web`
(TypeScript UI). **Both sides are written against this document, in parallel.** If you need to
change a signature, change it here first and say so in your report — do not diverge silently.

Design rule: the core is pure and synchronous. It owns parsing, geometry, protocol framing and
machine state. It performs no I/O and knows nothing about serial ports, timers or the DOM. The UI
owns all I/O, feeds bytes in, and renders what comes out. This is what lets the whole app run
against a simulated machine with no hardware, and lets the core be tested with `cargo test` and no
browser.

```
      ┌──────────────── sender/web (TypeScript) ────────────────┐
      │  Web Serial port  ──rx bytes──►                         │
      │        ▲                        Session (WASM)          │
      │        └──tx bytes───────────                           │
      │  Canvas renderer  ◄──state + backplot──                 │
      └─────────────────────────────────────────────────────────┘
                                  │
                   swap transport for VirtualMachine (WASM)
                   to run with no hardware attached
```

## Units and conventions

- All lengths in the core are **millimetres**, all feeds in **mm/min**, all angles in degrees.
  Inch display is a UI-side conversion only.
- `x` in every geometry and DRO structure is **radius**, matching LinuxCNC's internal convention.
  Diameter display is a UI-side conversion (`diameter = 2 * x`). Programs written in diameter mode
  are converted during parse; `Dro` reports both so the UI never has to guess.
- `z` is negative toward the chuck, positive toward the tailstock, matching lathe convention.
- Time is passed in as `f64` milliseconds from the UI (`performance.now()`). The core never reads a
  clock.

## Core exports (`wasm-bindgen`)

### `Program`

```rust
#[wasm_bindgen]
impl Program {
    /// Parse G-code text. Never throws — syntax problems come back as diagnostics.
    pub fn parse(text: &str) -> Program;

    pub fn line_count(&self) -> u32;

    /// -> Diagnostic[]
    pub fn diagnostics(&self) -> JsValue;

    /// -> Backplot
    pub fn backplot(&self, stock: JsValue /* StockSpec */) -> JsValue;

    /// Source text of one 0-based line, for the program pane.
    pub fn line(&self, index: u32) -> String;
}
```

```ts
type Diagnostic = {
  line: number;        // 0-based
  column: number;      // 0-based
  severity: "error" | "warning";
  message: string;
};

type StockSpec = {
  diameterMm: number;
  lengthMm: number;
  zFaceMm: number;     // z of the free end of the stock, usually 0
};

type Backplot = {
  segments: Segment[];
  bounds: { xMin: number; xMax: number; zMin: number; zMax: number };
};

type Segment = {
  kind: "rapid" | "feed" | "arc" | "thread";
  x0: number; z0: number;   // radius, mm
  x1: number; z1: number;
  // For "arc" only; absolute centre in the same frame.
  cx?: number; cz?: number;
  ccw?: boolean;
  line: number;             // 0-based source line that produced this move
  feedMmMin?: number;
  tool?: number;
};
```

Arcs are emitted as a single `arc` segment, not pre-flattened — the renderer decides tessellation
density from the current zoom. `bounds` already accounts for arc extrema.

### `VirtualMachine`

An in-process model of the EMCO control box (axis-controller board `Y4A012000`): it consumes the
same byte protocol as the development dialect and moves a simulated axis pair. This is what
"Simulated machine" mode talks to, and what the core's own tests run against.

```rust
#[wasm_bindgen]
impl VirtualMachine {
    pub fn new(profile: JsValue /* MachineProfile */) -> VirtualMachine;

    /// Advance the simulation to `now_ms` and consume bytes the host sent.
    /// Returns bytes the machine emits back.
    pub fn step(&mut self, now_ms: f64, host_tx: &[u8]) -> Vec<u8>;

    /// -> Dro, for the simulator's own truth (the UI normally reads Session state instead)
    pub fn dro(&self) -> JsValue;

    /// Inject a fault for training scenarios: "estop" | "limit_x" | "limit_z" | "turret_jam"
    pub fn inject_fault(&mut self, kind: &str);
}
```

### `Session`

The protocol driver and single source of UI state. Transport-agnostic: it emits bytes to send and
consumes bytes received, whether those came from a real port or from `VirtualMachine`.

```rust
#[wasm_bindgen]
impl Session {
    pub fn new(profile: JsValue /* MachineProfile */) -> Session;

    pub fn load_program(&mut self, program: &Program);

    /// The pump. Call from a rAF or interval loop.
    /// `rx` is whatever arrived from the transport since the last call (may be empty).
    /// -> Step
    pub fn tick(&mut self, now_ms: f64, rx: &[u8]) -> JsValue;

    /// Queue an operator command. -> void, effects appear on the next tick.
    pub fn command(&mut self, cmd: JsValue /* Command */);

    /// -> SessionState
    pub fn state(&self) -> JsValue;
}
```

```ts
type Step = {
  tx: Uint8Array;      // write these bytes to the transport
  events: Event[];     // things that happened, for the log pane
};

type Event =
  | { kind: "sent";   line: number; text: string; atMs: number }
  | { kind: "acked";  line: number; atMs: number }
  | { kind: "alarm";  code: string; message: string; atMs: number }
  | { kind: "state";  from: RunState; to: RunState; atMs: number }
  | { kind: "raw";    dir: "tx" | "rx"; bytes: Uint8Array; atMs: number };

type RunState =
  | "disconnected" | "idle" | "running" | "hold" | "alarm" | "homing" | "toolchange";

type Dro = {
  xRadiusMm: number; xDiameterMm: number; zMm: number;
  xWorkMm: number;   zWorkMm: number;      // work-offset-relative, radius for X
  xDtgMm: number;    zDtgMm: number;       // distance to go on the current move
  homed: boolean;
};

type SessionState = {
  run: RunState;
  dro: Dro;
  currentLine: number;        // -1 when not running
  tool: number;               // active turret station, 0 = unknown
  spindleRpm: number;
  spindleOn: boolean;
  feedMmMin: number;
  modal: string[];            // e.g. ["G21","G90","G95","G01"]
  alarms: { code: string; message: string }[];
  singleBlock: boolean;
  dryRun: boolean;
};

type Command =
  | { kind: "run" }
  | { kind: "hold" }
  | { kind: "resume" }
  | { kind: "abort" }            // best-effort stop; see the safety note below
  | { kind: "singleBlock"; on: boolean }
  | { kind: "dryRun"; on: boolean }
  | { kind: "jog"; axis: "x" | "z"; deltaMm: number; feedMmMin: number }
  | { kind: "mdi"; text: string }
  | { kind: "setWorkOffset"; axis: "x" | "z"; valueMm: number }
  | { kind: "home" };
```

### `MachineProfile`

Every machine-specific number lives here rather than being baked into code. The values below are
the **real ones**, read off EMCO's own manuals and schematics — see `docs/_machine-facts.md`. They
have not been re-measured at the machine, so treat them as documented-but-unconfirmed rather than
as guesses.

```ts
type MachineProfile = {
  name: string;
  xMinMm: number; xMaxMm: number;       // radius; real machine 0 .. 48
  zMinMm: number; zMaxMm: number;       // real machine 228 usable (275 mechanical)
  rapidMmMin: number;                   // 750
  maxFeedMmMin: number;                 // 750 normal, 1500 absolute max
  resolutionMm: number;                 // 0.001
  accelMmSec2: number;
  spindleMinRpm: number; spindleMaxRpm: number;   // 130 .. 3000
  spindleBeltSteps: { minRpm: number; maxRpm: number }[];  // 130-1300, 200-2000, 300-3000
  threadingMaxRpm: number;              // 400
  turretStations: number;               // 6 — three external, three internal. NOT 8.
  turretIndexSeconds: number;
  hasEndOfTravelLimits: boolean;        // false — travel ends at mechanical stops
  requiresReferenceAfterPowerOn: boolean; // true — no absolute position memory
  link: {
    kind: "rs485";                      // NOT rs232 — see below
    baud: number;                       // unknown; the wire framing is not yet characterised
  };
  dialect: "emco" | "plain-gcode";      // see docs/30-serial-protocol.md
};
```

The belt steps are changed by physically moving a belt, so `spindleBeltSteps` is something the
operator tells the software, not something the machine reports.

## The link is RS-485, and its protocol is unknown

Read `docs/_machine-facts.md` before making any assumption about the wire.

- The machine's DB9 (control-box connector `X300`) is **RS-485**: pin 2 GND, pin 3 DATA−,
  pin 8 DATA+. A three-wire differential multidrop bus. **Not RS-232.** Connecting an RS-232
  adapter to it is electrically wrong and risks damage.
- The original DOS PC drove it through a proprietary EMCO `Y4A013000` ISA card that is
  **memory-mapped** (a 4 KB dual-port RAM window at CC000–CFFFF), **not a UART**. So there is no
  known byte-stream protocol, and the "plain G-code with CTS handshaking" story from the community
  record is wrong for this machine.
- Consequence: `PlainGcode` is a **development and training fiction**, not the machine protocol. It
  is what `VirtualMachine` speaks so the whole app is testable and useful today. Keep it, and keep
  it clearly labelled as such.
- The real dialect stays a stub until protocol discovery produces something. Do not let assumed
  framing leak out of the `Dialect` trait into `Session`, the backplot or the UI.

## Protocol boundary

The wire format is **not yet confirmed** (see `docs/30-serial-protocol.md`). Put every assumption
about framing, handshaking, acknowledgement and error text behind one Rust trait:

```rust
pub trait Dialect {
    fn encode_block(&self, block: &Block) -> Vec<u8>;
    fn feed_rx(&mut self, bytes: &[u8]) -> Vec<DialectEvent>;
    fn wants_next_block(&self) -> bool;
}
```

Ship two implementations:

- **`PlainGcode`** — line-oriented text, CR terminated, XON/XOFF and CTS gating. This is a
  **development and training fiction**, not the machine protocol. It is what `VirtualMachine`
  speaks, which is what makes the whole app testable and useful today.
- **`EmcoStub`** — the real dialect, deliberately still a stub. It delegates to `PlainGcode` with
  `TODO(protocol)` seams where discovery findings will land.

Do not name anything `Ac95`: there is no such board in this machine. The community's
"AC95 / `Y4B081000`" is a part number from a different EMCO model; this machine's brain is the
axis-controller board `Y4A012000`.

Swapping dialects must not require touching the UI, the backplot or `Session`'s state machine.

## Safety

`{ kind: "abort" }` sends whatever stop the dialect offers and drops the link. **This is advisory,
not a safety function.** The real stop is the mains switch on the control box with a person
standing at the machine. The UI must say so next to the button, and nothing in this app may
encourage unattended or remote operation. Do not add a "run when I'm away" feature.
