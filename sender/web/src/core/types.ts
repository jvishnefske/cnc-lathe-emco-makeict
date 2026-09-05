/**
 * Types mirroring sender/INTERFACE.md — the contract with the Rust core.
 * If a shape here disagrees with INTERFACE.md, INTERFACE.md wins.
 *
 * Units: millimetres, mm/min, degrees. `x` is always RADIUS.
 * `z` is negative toward the chuck. Time is f64 ms from performance.now().
 */

export type Diagnostic = {
  line: number; // 0-based
  column: number; // 0-based
  severity: "error" | "warning";
  message: string;
};

export type StockSpec = {
  diameterMm: number;
  lengthMm: number;
  zFaceMm: number; // z of the free end of the stock, usually 0
};

export type SegmentKind = "rapid" | "feed" | "arc" | "thread";

export type Segment = {
  kind: SegmentKind;
  x0: number; // radius, mm
  z0: number;
  x1: number;
  z1: number;
  cx?: number; // arc centre (absolute), radius frame
  cz?: number;
  ccw?: boolean;
  line: number; // 0-based source line that produced this move
  feedMmMin?: number;
  tool?: number;
};

export type Backplot = {
  segments: Segment[];
  bounds: { xMin: number; xMax: number; zMin: number; zMax: number };
};

export type RunState =
  | "disconnected"
  | "idle"
  | "running"
  | "hold"
  | "alarm"
  | "homing"
  | "toolchange";

export type Dro = {
  xRadiusMm: number;
  xDiameterMm: number;
  zMm: number;
  xWorkMm: number; // work-offset-relative, radius for X
  zWorkMm: number;
  xDtgMm: number; // distance to go on the current move
  zDtgMm: number;
  homed: boolean;
};

export type SessionState = {
  run: RunState;
  dro: Dro;
  currentLine: number; // -1 when not running
  tool: number; // active turret station, 0 = unknown
  spindleRpm: number;
  spindleOn: boolean;
  feedMmMin: number;
  modal: string[]; // e.g. ["G21","G90","G95","G01"]
  alarms: { code: string; message: string }[];
  singleBlock: boolean;
  dryRun: boolean;
};

export type SessionEvent =
  | { kind: "sent"; line: number; text: string; atMs: number }
  | { kind: "acked"; line: number; atMs: number }
  | { kind: "alarm"; code: string; message: string; atMs: number }
  | { kind: "state"; from: RunState; to: RunState; atMs: number }
  | { kind: "raw"; dir: "tx" | "rx"; bytes: Uint8Array; atMs: number };

export type Step = {
  tx: Uint8Array; // write these bytes to the transport
  events: SessionEvent[];
};

export type Command =
  | { kind: "run" }
  | { kind: "hold" }
  | { kind: "resume" }
  | { kind: "abort" } // best-effort stop; advisory, NOT a safety function
  | { kind: "singleBlock"; on: boolean }
  | { kind: "dryRun"; on: boolean }
  | { kind: "jog"; axis: "x" | "z"; deltaMm: number; feedMmMin: number }
  | { kind: "mdi"; text: string }
  | { kind: "setWorkOffset"; axis: "x" | "z"; valueMm: number }
  | { kind: "home" };

export type FaultKind = "estop" | "limit_x" | "limit_z" | "turret_jam";

export type MachineProfile = {
  name: string;
  xMinMm: number;
  xMaxMm: number; // radius
  zMinMm: number;
  zMaxMm: number;
  rapidMmMin: number;
  maxFeedMmMin: number;
  accelMmSec2: number;
  spindleMinRpm: number;
  spindleMaxRpm: number;
  turretStations: number;
  turretIndexSeconds: number;
  serial: {
    baud: number;
    dataBits: 7 | 8;
    stopBits: 1 | 2;
    parity: "none" | "even" | "odd";
    flowControl: "none" | "hardware";
  };
  dialect: "ac95" | "plain-gcode";
};
