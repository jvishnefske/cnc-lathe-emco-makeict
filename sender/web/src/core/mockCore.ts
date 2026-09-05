/**
 * Pure-TypeScript implementation of CoreApi (see api.ts / INTERFACE.md).
 *
 * This is not a toy shim: it parses a useful subset of G-code, generates a
 * backplot, and simulates both sides of the serial conversation, so the whole
 * UI can be developed, demoed and used for operator training in a plain
 * browser with no WASM build and no hardware. The wire dialect implemented
 * here mirrors the core's `PlainGcode` guess: CR-terminated text lines, one
 * "ok" per completed block, "ALARM:<code>:<message>" for faults.
 */

import type { CoreApi, ProgramApi, SessionApi, VirtualMachineApi } from "./api";
import type {
  Backplot,
  Command,
  Diagnostic,
  Dro,
  FaultKind,
  MachineProfile,
  RunState,
  SessionEvent,
  SessionState,
  Step,
  StockSpec,
} from "./types";
import {
  initialParserState,
  parseLine,
  parseProgramText,
  tokenizeLine,
  type ParsedProgram,
  type ParserState,
} from "./mock/parser";
import { buildBackplot } from "./mock/backplot";
import { MotionSim } from "./mock/motionSim";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

// ---------------------------------------------------------------------------
// Program

export class MockProgram implements ProgramApi {
  readonly parsed: ParsedProgram;

  constructor(text: string) {
    this.parsed = parseProgramText(text);
  }

  lineCount(): number {
    return this.parsed.lines.length;
  }

  diagnostics(): Diagnostic[] {
    return this.parsed.diagnostics;
  }

  backplot(stock: StockSpec): Backplot {
    return buildBackplot(this.parsed.motions, stock);
  }

  line(index: number): string {
    return this.parsed.lines[index] ?? "";
  }
}

// ---------------------------------------------------------------------------
// VirtualMachine — the fake AC95 control box on the far end of the wire.

export class MockVirtualMachine implements VirtualMachineApi {
  private rxBuf = "";
  private sim: MotionSim;
  private parser: ParserState;
  private lastMs: number | null = null;
  private lineCounter = 0;
  /** vm line tag -> needs motion completion before ack */
  private pendingMotionAcks = new Set<number>();
  private out: number[] = [];
  private alarmed = false;
  private faultQueue: FaultKind[] = [];
  private homed = false;

  constructor(private profile: MachineProfile) {
    this.sim = new MotionSim(profile);
    this.parser = initialParserState();
  }

  injectFault(kind: FaultKind): void {
    this.faultQueue.push(kind);
  }

  private emit(text: string): void {
    for (const b of encoder.encode(text)) this.out.push(b);
  }

  step(nowMs: number, hostTx: Uint8Array): Uint8Array {
    const dt = this.lastMs === null ? 0 : Math.max(0, nowMs - this.lastMs);
    this.lastMs = nowMs;

    for (const fault of this.faultQueue.splice(0)) {
      this.alarmed = true;
      this.sim.paused = true;
      const msg: Record<FaultKind, [string, string]> = {
        estop: ["ESTOP", "emergency stop pressed"],
        limit_x: ["LIMX", "X axis limit switch"],
        limit_z: ["LIMZ", "Z axis limit switch"],
        turret_jam: ["TURRET", "turret failed to index"],
      };
      const [code, text] = msg[fault];
      this.emit(`ALARM:${code}:${text}\r\n`);
    }

    if (hostTx.length > 0) this.rxBuf += decoder.decode(hostTx);
    let idx: number;
    while ((idx = this.rxBuf.search(/[\r\n]/)) >= 0) {
      const lineText = this.rxBuf.slice(0, idx);
      this.rxBuf = this.rxBuf.slice(idx + 1);
      this.handleLine(lineText);
    }
    // A bare '!' (abort) may arrive without a terminator.
    if (this.rxBuf.includes("!")) {
      this.rxBuf = this.rxBuf.replace(/!/g, "");
      this.handleAbort();
    }

    if (!this.alarmed) {
      const doneLines = this.sim.advance(dt);
      for (const tag of doneLines) {
        if (this.pendingMotionAcks.delete(tag)) this.emit("ok\r\n");
      }
    }

    const bytes = new Uint8Array(this.out);
    this.out = [];
    return bytes;
  }

  private handleAbort(): void {
    this.sim.clear();
    this.pendingMotionAcks.clear();
    this.alarmed = false;
    this.sim.paused = false;
  }

  private handleLine(text: string): void {
    const trimmed = text.trim();
    if (trimmed === "") return;
    if (trimmed === "!") {
      this.handleAbort();
      return;
    }
    if (this.alarmed) {
      this.emit("ALARM:LOCKED:machine in alarm, send ! to reset\r\n");
      return;
    }
    if (/^G28\b/i.test(trimmed) || /^\$H/i.test(trimmed)) {
      this.homed = true;
    }
    const tag = this.lineCounter++;
    const diags: Diagnostic[] = [];
    const words = tokenizeLine(trimmed, tag, diags);
    if (diags.some((d) => d.severity === "error")) {
      this.emit(`ALARM:PARSE:${diags[0]!.message}\r\n`);
      this.alarmed = true;
      this.sim.paused = true;
      return;
    }
    const motions = parseLine(words, tag, this.parser, diags);
    // Enforce soft limits like a real control would.
    for (const m of motions) {
      if (
        m.x1 < this.profile.xMinMm - 1e-6 ||
        m.x1 > this.profile.xMaxMm + 1e-6 ||
        m.z1 < this.profile.zMinMm - 1e-6 ||
        m.z1 > this.profile.zMaxMm + 1e-6
      ) {
        this.emit(`ALARM:LIMIT:move on line exceeds travel\r\n`);
        this.alarmed = true;
        this.sim.paused = true;
        return;
      }
    }
    if (motions.length > 0) {
      this.sim.enqueue(motions);
      this.pendingMotionAcks.add(tag);
    } else {
      this.emit("ok\r\n");
    }
  }

  dro(): Dro {
    const dtg = this.sim.dtg();
    return {
      xRadiusMm: this.sim.pos.x,
      xDiameterMm: this.sim.pos.x * 2,
      zMm: this.sim.pos.z,
      xWorkMm: this.sim.pos.x,
      zWorkMm: this.sim.pos.z,
      xDtgMm: dtg.x,
      zDtgMm: dtg.z,
      homed: this.homed,
    };
  }
}

// ---------------------------------------------------------------------------
// Session — protocol driver and single source of UI state.

type SentBlock = { line: number; text: string; hasMotion: boolean };

export class MockSession implements SessionApi {
  private profile: MachineProfile;
  private program: MockProgram | null = null;
  private blockQueue: number[] = []; // source lines still to send
  private live: ParserState; // modal/positional state as blocks are sent
  private sim: MotionSim; // local motion model for the DRO
  private sentQueue: SentBlock[] = [];
  /** "ok" responses received but not yet matched to a completed block.
   * Persisted across ticks: the wire ack and the local motion can finish on
   * different frames (e.g. during feed hold, or when the machine acks a
   * non-motion block instantly while our simulated motion is still going). */
  private acks = 0;
  private rxBuf = "";
  private lastMs: number | null = null;
  private run: RunState = "disconnected";
  private alarms: { code: string; message: string }[] = [];
  private singleBlock = false;
  private dryRun = false;
  private homed = false;
  private workOffset = { x: 0, z: 0 };
  private commands: Command[] = [];
  private mdiText: string[] = [];
  private programActive = false;

  constructor(profile: MachineProfile) {
    this.profile = profile;
    this.live = initialParserState();
    this.sim = new MotionSim(profile);
  }

  loadProgram(program: ProgramApi): void {
    if (!(program instanceof MockProgram)) {
      throw new Error("mock Session can only load mock Programs");
    }
    this.program = program;
    this.blockQueue = [];
    this.programActive = false;
    if (this.run === "disconnected") this.run = "idle";
  }

  command(cmd: Command): void {
    this.commands.push(cmd);
  }

  state(): SessionState {
    const dtg = this.sim.dtg();
    const dro: Dro = {
      xRadiusMm: this.sim.pos.x,
      xDiameterMm: this.sim.pos.x * 2,
      zMm: this.sim.pos.z,
      xWorkMm: this.sim.pos.x - this.workOffset.x,
      zWorkMm: this.sim.pos.z - this.workOffset.z,
      xDtgMm: dtg.x,
      zDtgMm: dtg.z,
      homed: this.homed,
    };
    let currentLine = -1;
    if (this.programActive) {
      currentLine = this.sentQueue[0]?.line ?? this.blockQueue[0] ?? -1;
    }
    return {
      run: this.run,
      dro,
      currentLine,
      tool: this.live.tool,
      spindleRpm: this.live.spindleOn ? this.live.spindleRpm : 0,
      spindleOn: this.live.spindleOn,
      feedMmMin: this.live.feedMmMin,
      modal: [...this.live.modal].sort(),
      alarms: [...this.alarms],
      singleBlock: this.singleBlock,
      dryRun: this.dryRun,
    };
  }

  tick(nowMs: number, rx: Uint8Array): Step {
    const dt = this.lastMs === null ? 0 : Math.max(0, nowMs - this.lastMs);
    this.lastMs = nowMs;
    const events: SessionEvent[] = [];
    const tx: number[] = [];

    if (rx.length > 0) {
      events.push({ kind: "raw", dir: "rx", bytes: rx.slice(), atMs: nowMs });
      this.rxBuf += decoder.decode(rx);
    }

    for (const cmd of this.commands.splice(0)) {
      this.applyCommand(cmd, nowMs, tx, events);
    }

    // Consume machine responses.
    let idx: number;
    while ((idx = this.rxBuf.search(/[\r\n]/)) >= 0) {
      const lineText = this.rxBuf.slice(0, idx).trim();
      this.rxBuf = this.rxBuf.slice(idx + 1);
      if (lineText === "") continue;
      if (lineText === "ok") {
        this.acks++;
      } else if (lineText.startsWith("ALARM:")) {
        const parts = lineText.split(":");
        const code = parts[1] ?? "ALARM";
        const message = parts.slice(2).join(":") || "machine alarm";
        this.alarms.push({ code, message });
        events.push({ kind: "alarm", code, message, atMs: nowMs });
        this.setRun("alarm", nowMs, events);
        this.sim.paused = true;
      }
    }

    // Advance the local motion model.
    if (this.run !== "alarm" && this.run !== "hold") {
      this.sim.advance(dt);
    }

    // Retire acknowledged blocks. In dry-run there is no wire, so a block is
    // "acked" as soon as its motion completes locally.
    while (this.sentQueue.length > 0) {
      const head = this.sentQueue[0]!;
      const motionDone = !head.hasMotion || this.sim.idle;
      const wireAck = this.dryRun ? motionDone : this.acks > 0;
      if (!(wireAck && motionDone)) break;
      if (!this.dryRun) this.acks--;
      this.sentQueue.shift();
      events.push({ kind: "acked", line: head.line, atMs: nowMs });
      if (this.programActive && this.singleBlock && this.blockQueue.length > 0 && this.run === "running") {
        this.setRun("hold", nowMs, events);
      }
    }

    // Homing completes when its motion does.
    if (this.run === "homing" && this.sim.idle && this.sentQueue.length === 0) {
      this.homed = true;
      this.setRun("idle", nowMs, events);
    }

    // Feed the wire: at most one outstanding block.
    if (this.run === "running" && this.sentQueue.length === 0) {
      const next = this.blockQueue.shift();
      if (next !== undefined) {
        this.sendLine(this.program!.line(next), next, nowMs, tx, events);
      } else if (this.sim.idle) {
        this.programActive = false;
        this.setRun("idle", nowMs, events);
      }
    } else if (this.run === "idle" && !this.programActive && this.sentQueue.length === 0) {
      // MDI / jog issue lines while idle.
      const text = this.mdiText.shift();
      if (text !== undefined) this.sendLine(text, -1, nowMs, tx, events);
    }

    const txBytes = new Uint8Array(tx);
    if (txBytes.length > 0) {
      events.push({ kind: "raw", dir: "tx", bytes: txBytes.slice(), atMs: nowMs });
    }
    return { tx: txBytes, events };
  }

  private setRun(to: RunState, atMs: number, events: SessionEvent[]): void {
    if (this.run === to) return;
    events.push({ kind: "state", from: this.run, to, atMs });
    this.run = to;
  }

  private sendLine(
    text: string,
    sourceLine: number,
    nowMs: number,
    tx: number[],
    events: SessionEvent[],
  ): void {
    const diags: Diagnostic[] = [];
    const words = tokenizeLine(text, Math.max(sourceLine, 0), diags);
    const motions = parseLine(words, Math.max(sourceLine, 0), this.live, diags);
    this.sim.enqueue(motions);
    this.sentQueue.push({ line: sourceLine, text, hasMotion: motions.length > 0 });
    if (!this.dryRun) {
      for (const b of encoder.encode(text.trim() + "\r")) tx.push(b);
    }
    events.push({ kind: "sent", line: sourceLine, text: text.trim(), atMs: nowMs });
  }

  private applyCommand(
    cmd: Command,
    nowMs: number,
    tx: number[],
    events: SessionEvent[],
  ): void {
    switch (cmd.kind) {
      case "run": {
        if (!this.program) break;
        if (this.run !== "idle") break;
        this.blockQueue = [...this.program.parsed.blockLines];
        this.programActive = true;
        // Re-seed the live parser at the current physical position.
        this.live = initialParserState(this.sim.pos.x, this.sim.pos.z);
        this.setRun("running", nowMs, events);
        break;
      }
      case "hold": {
        if (this.run === "running" || this.run === "homing") {
          this.sim.paused = true;
          this.setRun("hold", nowMs, events);
        }
        break;
      }
      case "resume": {
        if (this.run === "hold") {
          this.sim.paused = false;
          this.setRun(this.programActive || !this.sim.idle ? "running" : "idle", nowMs, events);
        }
        break;
      }
      case "abort": {
        tx.push(0x21); // '!' — best-effort stop; ADVISORY, not a safety function
        tx.push(0x0d);
        this.sim.clear();
        this.sim.paused = false;
        this.sentQueue = [];
        this.blockQueue = [];
        this.mdiText = [];
        this.programActive = false;
        this.alarms = [];
        this.acks = 0; // stale acks must not leak into the next run
        this.setRun("idle", nowMs, events);
        break;
      }
      case "singleBlock":
        this.singleBlock = cmd.on;
        break;
      case "dryRun":
        if (this.run === "idle") this.dryRun = cmd.on;
        break;
      case "jog": {
        if (this.run !== "idle" || !this.sim.idle) break;
        const target = {
          x: clamp(
            this.sim.pos.x + (cmd.axis === "x" ? cmd.deltaMm : 0),
            this.profile.xMinMm,
            this.profile.xMaxMm,
          ),
          z: clamp(
            this.sim.pos.z + (cmd.axis === "z" ? cmd.deltaMm : 0),
            this.profile.zMinMm,
            this.profile.zMaxMm,
          ),
        };
        const feed = Math.min(cmd.feedMmMin, this.profile.maxFeedMmMin);
        // One self-contained line; X is emitted as diameter (G7 default).
        const xWord = (target.x * 2).toFixed(3);
        const zWord = target.z.toFixed(3);
        this.mdiText.push(`G90 G1 X${xWord} Z${zWord} F${feed.toFixed(0)}`);
        break;
      }
      case "mdi": {
        if (this.run !== "idle") break;
        for (const line of cmd.text.split(/\r\n|\r|\n/)) {
          if (line.trim() !== "") this.mdiText.push(line);
        }
        break;
      }
      case "setWorkOffset": {
        // Touch-off semantics: make the current position read `valueMm`.
        if (cmd.axis === "x") this.workOffset.x = this.sim.pos.x - cmd.valueMm;
        else this.workOffset.z = this.sim.pos.z - cmd.valueMm;
        break;
      }
      case "home": {
        if (this.run !== "idle" && this.run !== "disconnected") break;
        this.setRun("homing", nowMs, events);
        this.live = initialParserState(this.sim.pos.x, this.sim.pos.z);
        this.sim.enqueue([
          {
            kind: "rapid",
            x0: this.sim.pos.x,
            z0: this.sim.pos.z,
            x1: this.profile.xMaxMm,
            z1: this.profile.zMaxMm,
            line: -1,
          },
        ]);
        this.live.x = this.profile.xMaxMm;
        this.live.z = this.profile.zMaxMm;
        if (!this.dryRun) {
          for (const b of encoder.encode("G28\r")) tx.push(b);
        }
        this.sentQueue.push({ line: -1, text: "G28", hasMotion: true });
        break;
      }
    }
  }

  /** Called by the UI when a transport connects/disconnects. */
  setConnected(connected: boolean): void {
    if (connected && this.run === "disconnected") this.run = "idle";
    if (!connected) this.run = "disconnected";
  }
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

// ---------------------------------------------------------------------------

export class MockCore implements CoreApi {
  readonly name = "mock" as const;

  parseProgram(text: string): ProgramApi {
    return new MockProgram(text);
  }

  createSession(profile: MachineProfile): SessionApi {
    return new MockSession(profile);
  }

  createVirtualMachine(profile: MachineProfile): VirtualMachineApi {
    return new MockVirtualMachine(profile);
  }
}
