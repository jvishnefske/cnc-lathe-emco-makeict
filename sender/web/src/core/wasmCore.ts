/**
 * Adapter that puts the real Rust/WASM core (sender/core, built with
 * wasm-pack into sender/core/pkg) behind the CoreApi interface.
 *
 * The import below is a lazy dynamic import; if it fails (pkg missing or
 * incompatible), loadWasmCore rejects and the caller falls back to the mock
 * core. Nothing else in the app may import from core/pkg — this file is the
 * only place that path appears.
 */

import type { CoreApi, ProgramApi, SessionApi, VirtualMachineApi } from "./api";
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

// Shape of the wasm-bindgen module per INTERFACE.md.
type WasmModule = {
  default?: (input?: unknown) => Promise<unknown>; // wasm-pack web-target init
  Program: {
    parse(text: string): WasmProgram;
  };
  Session: new (profile: unknown) => WasmSession;
  VirtualMachine: new (profile: unknown) => WasmVm;
};
type WasmProgram = {
  line_count(): number;
  diagnostics(): unknown;
  backplot(stock: unknown): unknown;
  line(index: number): string;
};
type WasmSession = {
  load_program(program: WasmProgram): void;
  tick(nowMs: number, rx: Uint8Array): unknown;
  command(cmd: unknown): void;
  state(): unknown;
};
type WasmVm = {
  step(nowMs: number, hostTx: Uint8Array): Uint8Array;
  dro(): unknown;
  inject_fault(kind: string): void;
};

class WasmProgramAdapter implements ProgramApi {
  constructor(readonly raw: WasmProgram) {}
  lineCount(): number {
    return this.raw.line_count();
  }
  diagnostics(): Diagnostic[] {
    return this.raw.diagnostics() as Diagnostic[];
  }
  backplot(stock: StockSpec): Backplot {
    return this.raw.backplot(stock) as Backplot;
  }
  line(index: number): string {
    return this.raw.line(index);
  }
}

class WasmSessionAdapter implements SessionApi {
  constructor(private inner: WasmSession) {}
  loadProgram(program: ProgramApi): void {
    const raw = program.raw as WasmProgram | undefined;
    if (!raw) throw new Error("wasm Session can only load wasm Programs");
    this.inner.load_program(raw);
  }
  tick(nowMs: number, rx: Uint8Array): Step {
    const step = this.inner.tick(nowMs, rx) as Step;
    // wasm-bindgen serialises byte arrays as plain arrays through JsValue;
    // normalise so callers always see Uint8Array.
    if (!(step.tx instanceof Uint8Array)) step.tx = new Uint8Array(step.tx as Iterable<number>);
    for (const ev of step.events) {
      if (ev.kind === "raw" && !(ev.bytes instanceof Uint8Array)) {
        ev.bytes = new Uint8Array(ev.bytes as Iterable<number>);
      }
    }
    return step;
  }
  command(cmd: Command): void {
    this.inner.command(cmd);
  }
  state(): SessionState {
    return this.inner.state() as SessionState;
  }
  setConnected(_connected: boolean): void {
    // Not yet in the contract; the wasm core tracks link state itself.
  }
}

class WasmVmAdapter implements VirtualMachineApi {
  constructor(private inner: WasmVm) {}
  step(nowMs: number, hostTx: Uint8Array): Uint8Array {
    const out = this.inner.step(nowMs, hostTx);
    return out instanceof Uint8Array ? out : new Uint8Array(out as Iterable<number>);
  }
  dro(): Dro {
    return this.inner.dro() as Dro;
  }
  injectFault(kind: FaultKind): void {
    this.inner.inject_fault(kind);
  }
}

class WasmCore implements CoreApi {
  readonly name = "wasm" as const;
  constructor(private mod: WasmModule) {}
  parseProgram(text: string): ProgramApi {
    return new WasmProgramAdapter(this.mod.Program.parse(text));
  }
  createSession(profile: MachineProfile): SessionApi {
    return new WasmSessionAdapter(new this.mod.Session(profile));
  }
  createVirtualMachine(profile: MachineProfile): VirtualMachineApi {
    return new WasmVmAdapter(new this.mod.VirtualMachine(profile));
  }
}

/** The one place the core/pkg path exists. Rejects when the wasm build is absent. */
export async function loadWasmCore(): Promise<CoreApi> {
  // A real (non-@vite-ignore) dynamic import so Vite bundles the wasm-bindgen
  // glue and emits the .wasm as an asset; still lazy, so the mock path never
  // pays for it. `wasm-pack build --target web` in sender/core produces it.
  const mod = (await import("../../../core/pkg/emco_sender_core.js")) as unknown as WasmModule;
  if (typeof mod.default === "function") {
    await mod.default(); // web-target init: fetch + instantiate the .wasm
  }
  if (!mod.Program || !mod.Session || !mod.VirtualMachine) {
    throw new Error("core/pkg loaded but does not export Program/Session/VirtualMachine");
  }
  return new WasmCore(mod);
}
