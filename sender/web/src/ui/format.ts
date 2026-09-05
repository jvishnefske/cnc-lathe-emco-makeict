/**
 * Pure formatting/rendering logic for the panels — kept out of the DOM code
 * so it can be unit tested.
 */

import type { RunState, SessionState } from "../core/types";

export type ConsoleEntry = {
  atMs: number;
  kind: "tx" | "rx" | "note";
  bytes?: Uint8Array;
  text?: string;
};

export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(" ");
}

/** Printable ASCII stays; control bytes shown as ⟨CR⟩-style tokens. */
export function bytesToAscii(bytes: Uint8Array): string {
  const names: Record<number, string> = {
    0x00: "NUL", 0x02: "STX", 0x03: "ETX", 0x04: "EOT", 0x05: "ENQ",
    0x06: "ACK", 0x0a: "LF", 0x0d: "CR", 0x11: "XON", 0x13: "XOFF",
    0x15: "NAK", 0x18: "CAN", 0x1b: "ESC",
  };
  let out = "";
  for (const b of bytes) {
    if (b >= 0x20 && b < 0x7f) out += String.fromCharCode(b);
    else out += `⟨${names[b] ?? "x" + b.toString(16).padStart(2, "0").toUpperCase()}⟩`;
  }
  return out;
}

export function formatTimestamp(atMs: number): string {
  return (atMs / 1000).toFixed(3).padStart(9, " ");
}

export function formatConsoleLine(e: ConsoleEntry, mode: "ascii" | "hex"): string {
  const dir = e.kind === "tx" ? "TX" : e.kind === "rx" ? "RX" : "--";
  const body =
    e.kind === "note"
      ? (e.text ?? "")
      : mode === "hex"
        ? bytesToHex(e.bytes ?? new Uint8Array())
        : bytesToAscii(e.bytes ?? new Uint8Array());
  return `${formatTimestamp(e.atMs)}  ${dir}  ${body}`;
}

/**
 * Full transcript for export: every entry with timestamp, direction, hex AND
 * ascii — complete enough to reverse-engineer a protocol from.
 */
export function consoleExportText(entries: ConsoleEntry[], header = ""): string {
  const lines: string[] = [];
  if (header) lines.push(`# ${header}`);
  lines.push("#  seconds  dir  hex  |ascii|");
  for (const e of entries) {
    if (e.kind === "note") {
      lines.push(`${formatTimestamp(e.atMs)}  --   ${e.text ?? ""}`);
    } else {
      const b = e.bytes ?? new Uint8Array();
      lines.push(
        `${formatTimestamp(e.atMs)}  ${e.kind.toUpperCase()}   ${bytesToHex(b)}  |${bytesToAscii(b)}|`,
      );
    }
  }
  return lines.join("\n") + "\n";
}

export const RUN_STATE_LABEL: Record<RunState, string> = {
  disconnected: "DISCONNECTED",
  idle: "IDLE",
  running: "RUNNING",
  hold: "FEED HOLD",
  alarm: "ALARM",
  homing: "HOMING",
  toolchange: "TOOL CHANGE",
};

export const RUN_STATE_CLASS: Record<RunState, string> = {
  disconnected: "st-off",
  idle: "st-idle",
  running: "st-run",
  hold: "st-hold",
  alarm: "st-alarm",
  homing: "st-run",
  toolchange: "st-run",
};

/** Which control buttons are enabled in a given state. */
export function controlAvailability(s: SessionState, connected: boolean, hasProgram: boolean) {
  return {
    run: connected && hasProgram && s.run === "idle",
    hold: s.run === "running" || s.run === "homing",
    resume: s.run === "hold",
    abort: connected && s.run !== "disconnected",
    jog: connected && s.run === "idle",
    mdi: connected && s.run === "idle",
    home: connected && s.run === "idle",
  };
}
