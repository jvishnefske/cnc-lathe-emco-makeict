/**
 * CoreApi — the TypeScript face of the Rust/WASM core described in
 * sender/INTERFACE.md. Two implementations exist:
 *
 *  - wasmCore.ts  wraps the real wasm-bindgen module (sender/core/pkg)
 *  - mockCore.ts  a pure-TypeScript stand-in, good enough to develop,
 *    demo and train against with no WASM build and no hardware
 *
 * Everything above this layer (UI, transports, main loop) talks only to
 * these interfaces and must not care which implementation is live.
 */

import type {
  Backplot,
  Command,
  Diagnostic,
  Dro,
  FaultKind,
  MachineProfile,
  SessionState,
  Step,
  StockSpec,
} from "./types";

export interface ProgramApi {
  lineCount(): number;
  diagnostics(): Diagnostic[];
  backplot(stock: StockSpec): Backplot;
  line(index: number): string;
  /** Implementation-private handle (the wasm Program object). Do not touch. */
  readonly raw?: unknown;
}

export interface SessionApi {
  loadProgram(program: ProgramApi): void;
  /**
   * NOT in INTERFACE.md (yet): lets the UI tell the core when a transport
   * came up or went away, so RunState can leave/enter "disconnected".
   * Optional — the wasm adapter no-ops it until the contract grows one.
   */
  setConnected?(connected: boolean): void;
  /** The pump: advance to nowMs, consume received bytes, get bytes to send. */
  tick(nowMs: number, rx: Uint8Array): Step;
  command(cmd: Command): void;
  state(): SessionState;
}

export interface VirtualMachineApi {
  /** Advance the simulation and consume host->machine bytes; returns machine->host bytes. */
  step(nowMs: number, hostTx: Uint8Array): Uint8Array;
  dro(): Dro;
  injectFault(kind: FaultKind): void;
}

export interface CoreApi {
  readonly name: "wasm" | "mock";
  parseProgram(text: string): ProgramApi;
  createSession(profile: MachineProfile): SessionApi;
  createVirtualMachine(profile: MachineProfile): VirtualMachineApi;
}
