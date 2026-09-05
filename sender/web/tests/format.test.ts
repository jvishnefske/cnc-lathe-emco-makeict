import { describe, expect, it } from "vitest";
import {
  bytesToAscii,
  bytesToHex,
  consoleExportText,
  controlAvailability,
  formatConsoleLine,
  RUN_STATE_LABEL,
  type ConsoleEntry,
} from "../src/ui/format";
import type { SessionState } from "../src/core/types";

const enc = new TextEncoder();

describe("byte rendering", () => {
  it("renders hex pairs", () => {
    expect(bytesToHex(new Uint8Array([0x47, 0x31, 0x0d]))).toBe("47 31 0d");
  });

  it("renders printable ascii and names control bytes", () => {
    expect(bytesToAscii(enc.encode("G1 X5\r"))).toBe("G1 X5⟨CR⟩");
    expect(bytesToAscii(new Uint8Array([0x13, 0x11]))).toBe("⟨XOFF⟩⟨XON⟩");
    expect(bytesToAscii(new Uint8Array([0x81]))).toBe("⟨x81⟩");
  });
});

describe("console lines and export", () => {
  const entries: ConsoleEntry[] = [
    { atMs: 1234.5, kind: "tx", bytes: enc.encode("G0 X1\r") },
    { atMs: 1300, kind: "rx", bytes: enc.encode("ok\r\n") },
    { atMs: 1400, kind: "note", text: "connected" },
  ];

  it("formats a line with timestamp and direction", () => {
    const line = formatConsoleLine(entries[0]!, "ascii");
    expect(line).toContain("TX");
    expect(line).toContain("G0 X1⟨CR⟩");
    expect(line).toMatch(/^\s*1\.23[45]/);
  });

  it("switches to hex mode", () => {
    expect(formatConsoleLine(entries[1]!, "hex")).toContain("6f 6b 0d 0a");
  });

  it("exports a complete capture: timestamp, direction, hex AND ascii", () => {
    const text = consoleExportText(entries, "capture header");
    expect(text).toContain("# capture header");
    const txLine = text.split("\n").find((l) => l.includes("TX"))!;
    expect(txLine).toContain("47 30 20 58 31 0d"); // hex
    expect(txLine).toContain("|G0 X1⟨CR⟩|"); // ascii
    expect(text).toContain("connected");
    expect(text.endsWith("\n")).toBe(true);
  });
});

describe("controlAvailability", () => {
  const base: SessionState = {
    run: "idle",
    dro: {
      xRadiusMm: 0, xDiameterMm: 0, zMm: 0, xWorkMm: 0, zWorkMm: 0,
      xDtgMm: 0, zDtgMm: 0, homed: false,
    },
    currentLine: -1,
    tool: 0,
    spindleRpm: 0,
    spindleOn: false,
    feedMmMin: 0,
    modal: [],
    alarms: [],
    singleBlock: false,
    dryRun: false,
  };

  it("enables run only when connected, idle, with a program", () => {
    expect(controlAvailability(base, true, true).run).toBe(true);
    expect(controlAvailability(base, false, true).run).toBe(false);
    expect(controlAvailability(base, true, false).run).toBe(false);
    expect(controlAvailability({ ...base, run: "running" }, true, true).run).toBe(false);
  });

  it("enables hold while running and resume while held", () => {
    expect(controlAvailability({ ...base, run: "running" }, true, true).hold).toBe(true);
    expect(controlAvailability({ ...base, run: "hold" }, true, true).resume).toBe(true);
    expect(controlAvailability(base, true, true).hold).toBe(false);
    expect(controlAvailability(base, true, true).resume).toBe(false);
  });

  it("gates jog/mdi/home on an idle connected machine", () => {
    const a = controlAvailability(base, true, false);
    expect(a.jog).toBe(true);
    expect(a.mdi).toBe(true);
    expect(a.home).toBe(true);
    const b = controlAvailability({ ...base, run: "alarm" }, true, false);
    expect(b.jog).toBe(false);
  });

  it("labels every run state", () => {
    for (const key of ["disconnected", "idle", "running", "hold", "alarm", "homing", "toolchange"] as const) {
      expect(RUN_STATE_LABEL[key]).toBeTruthy();
    }
  });
});
