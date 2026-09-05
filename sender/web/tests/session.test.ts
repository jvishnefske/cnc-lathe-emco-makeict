/**
 * Control-flow tests for the mock core's Session/VirtualMachine pair, driven
 * exactly like the app's main loop: drain transport → tick → send tx.
 */

import { describe, expect, it } from "vitest";
import { MockCore } from "../src/core/mockCore";
import type { SessionApi } from "../src/core/api";
import type { SessionEvent, SessionState } from "../src/core/types";
import { DEFAULT_PROFILE } from "../src/profile";
import { DEFAULT_SERIAL_SETTINGS } from "../src/transport/transport";
import { LoopbackTransport } from "../src/transport/loopback";

type Rig = {
  core: MockCore;
  session: SessionApi;
  transport: LoopbackTransport;
  vm: ReturnType<MockCore["createVirtualMachine"]>;
  now: number;
  events: SessionEvent[];
  txBytes: number;
  pump: (opts?: { untilMs?: number; until?: (s: SessionState) => boolean }) => SessionState;
};

async function makeRig(connect = true): Promise<Rig> {
  const core = new MockCore();
  const session = core.createSession(DEFAULT_PROFILE);
  const vm = core.createVirtualMachine(DEFAULT_PROFILE);
  const transport = new LoopbackTransport(vm);
  if (connect) {
    await transport.connect(DEFAULT_SERIAL_SETTINGS);
    session.setConnected?.(true);
  }
  const rig: Rig = {
    core,
    session,
    transport,
    vm,
    now: 0,
    events: [],
    txBytes: 0,
    pump: (opts = {}) => {
      const deadline = rig.now + (opts.untilMs ?? 30000);
      const dt = 50;
      for (;;) {
        rig.now += dt;
        const rx = transport.connected ? transport.drain(rig.now) : new Uint8Array(0);
        const step = session.tick(rig.now, rx);
        rig.events.push(...step.events);
        rig.txBytes += step.tx.length;
        if (step.tx.length > 0 && transport.connected) transport.send(step.tx);
        const s = session.state();
        if (opts.until?.(s)) return s;
        if (rig.now >= deadline) return s;
      }
    },
  };
  return rig;
}

const PROGRAM = "G0 X10 Z-5\nG1 X4 Z-10 F200\n";

describe("connection state", () => {
  it("starts disconnected and becomes idle on connect", async () => {
    const core = new MockCore();
    const session = core.createSession(DEFAULT_PROFILE);
    expect(session.state().run).toBe("disconnected");
    session.setConnected?.(true);
    expect(session.state().run).toBe("idle");
    session.setConnected?.(false);
    expect(session.state().run).toBe("disconnected");
  });
});

describe("program run", () => {
  it("runs a program to completion against the virtual machine", async () => {
    const rig = await makeRig();
    rig.session.loadProgram(rig.core.parseProgram(PROGRAM));
    rig.session.command({ kind: "run" });
    let s = rig.pump({ until: (st) => st.run === "running" });
    expect(s.run).toBe("running");
    expect(s.currentLine).toBeGreaterThanOrEqual(0);
    s = rig.pump({ until: (st) => st.run === "idle" });
    expect(s.run).toBe("idle");
    expect(s.currentLine).toBe(-1);
    expect(s.dro.xRadiusMm).toBeCloseTo(2, 3); // X4 diameter -> 2 mm radius
    expect(s.dro.zMm).toBeCloseTo(-10, 3);
    // The wire really carried bytes and blocks were acked.
    expect(rig.events.some((e) => e.kind === "sent" && e.line === 0)).toBe(true);
    expect(rig.events.some((e) => e.kind === "acked" && e.line === 1)).toBe(true);
    expect(rig.events.some((e) => e.kind === "raw" && e.dir === "tx")).toBe(true);
    expect(rig.events.some((e) => e.kind === "raw" && e.dir === "rx")).toBe(true);
  });

  it("feed hold freezes motion and resume finishes the program", async () => {
    const rig = await makeRig();
    rig.session.loadProgram(rig.core.parseProgram(PROGRAM));
    rig.session.command({ kind: "run" });
    rig.pump({ until: (st) => st.run === "running" });
    rig.pump({ untilMs: 200 });
    rig.session.command({ kind: "hold" });
    let s = rig.pump({ until: (st) => st.run === "hold" });
    const frozen = { x: s.dro.xRadiusMm, z: s.dro.zMm };
    s = rig.pump({ untilMs: 500 });
    expect(s.dro.xRadiusMm).toBe(frozen.x);
    expect(s.dro.zMm).toBe(frozen.z);
    rig.session.command({ kind: "resume" });
    s = rig.pump({ until: (st) => st.run === "idle" });
    expect(s.dro.zMm).toBeCloseTo(-10, 3);
  });

  it("abort stops mid-program and returns to idle", async () => {
    const rig = await makeRig();
    rig.session.loadProgram(rig.core.parseProgram(PROGRAM));
    rig.session.command({ kind: "run" });
    rig.pump({ until: (st) => st.run === "running" });
    rig.pump({ untilMs: 200 });
    rig.session.command({ kind: "abort" });
    const s = rig.pump({ until: (st) => st.run === "idle" });
    expect(s.currentLine).toBe(-1);
    expect(s.dro.zMm).toBeGreaterThan(-10); // never reached the end point
    // Stays idle: nothing keeps running afterwards.
    const later = rig.pump({ untilMs: 500 });
    expect(later.dro.zMm).toBe(s.dro.zMm);
  });

  it("single block holds after each block", async () => {
    const rig = await makeRig();
    rig.session.loadProgram(rig.core.parseProgram(PROGRAM));
    rig.session.command({ kind: "singleBlock", on: true });
    rig.session.command({ kind: "run" });
    let s = rig.pump({ until: (st) => st.run === "hold" });
    expect(s.run).toBe("hold");
    expect(s.dro.zMm).toBeCloseTo(-5, 3); // exactly one block executed
    rig.session.command({ kind: "resume" });
    s = rig.pump({ until: (st) => st.run === "idle" });
    expect(s.dro.zMm).toBeCloseTo(-10, 3);
  });

  it("dry run completes with no transport and no bytes on the wire", async () => {
    const rig = await makeRig(false); // never connected
    rig.session.loadProgram(rig.core.parseProgram(PROGRAM)); // load flips mock to idle
    rig.session.command({ kind: "dryRun", on: true });
    rig.session.command({ kind: "run" });
    const s = rig.pump({ until: (st) => st.run === "idle" && st.currentLine === -1 });
    expect(s.dryRun).toBe(true);
    expect(s.dro.zMm).toBeCloseTo(-10, 3);
    expect(rig.txBytes).toBe(0);
  });
});

describe("homing", () => {
  it("moves to the reference and sets homed", async () => {
    const rig = await makeRig();
    expect(rig.session.state().dro.homed).toBe(false);
    rig.session.command({ kind: "home" });
    let s = rig.pump({ until: (st) => st.run === "homing" });
    expect(s.run).toBe("homing");
    s = rig.pump({ until: (st) => st.dro.homed });
    expect(s.dro.homed).toBe(true);
    expect(s.run).toBe("idle");
    expect(s.dro.xRadiusMm).toBeCloseTo(DEFAULT_PROFILE.xMaxMm, 3);
    expect(s.dro.zMm).toBeCloseTo(DEFAULT_PROFILE.zMaxMm, 3);
  });
});

describe("jog / MDI / work offsets", () => {
  it("jogs X by the requested step", async () => {
    const rig = await makeRig();
    rig.session.command({ kind: "jog", axis: "x", deltaMm: 1, feedMmMin: 300 });
    const s = rig.pump({
      until: (st) => Math.abs(st.dro.xRadiusMm - 1) < 1e-6 && st.run === "idle",
    });
    expect(s.dro.xRadiusMm).toBeCloseTo(1, 6);
    expect(s.dro.zMm).toBeCloseTo(0, 6);
  });

  it("clamps jog targets to the travel limits", async () => {
    const rig = await makeRig();
    rig.session.command({ kind: "jog", axis: "z", deltaMm: 500, feedMmMin: 300 });
    const s = rig.pump({ untilMs: 2000 });
    expect(s.dro.zMm).toBeLessThanOrEqual(DEFAULT_PROFILE.zMaxMm + 1e-6);
  });

  it("executes an MDI line", async () => {
    const rig = await makeRig();
    rig.session.command({ kind: "mdi", text: "G1 X10 Z-2 F400" });
    const s = rig.pump({
      until: (st) => Math.abs(st.dro.zMm - -2) < 1e-6,
    });
    expect(s.dro.xRadiusMm).toBeCloseTo(5, 6);
  });

  it("touch-off sets the work coordinate without moving", async () => {
    const rig = await makeRig();
    rig.session.command({ kind: "mdi", text: "G1 X10 Z-2 F400" });
    rig.pump({ until: (st) => Math.abs(st.dro.zMm - -2) < 1e-6 });
    rig.session.command({ kind: "setWorkOffset", axis: "z", valueMm: 0 });
    const s = rig.pump({ untilMs: 100 });
    expect(s.dro.zWorkMm).toBeCloseTo(0, 9);
    expect(s.dro.zMm).toBeCloseTo(-2, 6); // machine coordinate unchanged
  });
});

describe("alarms", () => {
  it("raises an alarm when the virtual machine faults", async () => {
    const rig = await makeRig();
    rig.vm.injectFault("estop");
    const s = rig.pump({ until: (st) => st.run === "alarm" });
    expect(s.alarms.some((a) => a.code === "ESTOP")).toBe(true);
    expect(rig.events.some((e) => e.kind === "alarm" && e.code === "ESTOP")).toBe(true);
  });

  it("alarms on a program that exceeds the machine travel", async () => {
    const rig = await makeRig();
    rig.session.loadProgram(rig.core.parseProgram("G0 X10 Z-5\nG0 Z-300\n"));
    rig.session.command({ kind: "run" });
    const s = rig.pump({ until: (st) => st.run === "alarm" });
    expect(s.alarms.some((a) => a.code === "LIMIT")).toBe(true);
  });

  it("abort resets the alarm and returns to idle", async () => {
    const rig = await makeRig();
    rig.vm.injectFault("estop");
    rig.pump({ until: (st) => st.run === "alarm" });
    rig.session.command({ kind: "abort" });
    const s = rig.pump({ until: (st) => st.run === "idle" });
    expect(s.alarms).toHaveLength(0);
  });
});
