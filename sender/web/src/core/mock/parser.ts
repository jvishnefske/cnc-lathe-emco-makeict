/**
 * Small G-code parser for the mock core. Not a full dialect — enough of
 * RS-274 to develop and demo the UI: G0/G1/G2/G3, G33 threading, G20/G21,
 * G90/G91, G7/G8 (diameter/radius), F/S/T, common M codes.
 *
 * All output geometry is in millimetres with X as RADIUS, per INTERFACE.md.
 * EMCO-style programs are written in diameter; diameter mode (G7) is the
 * default and X words are halved during parse.
 */

import type { Diagnostic, SegmentKind } from "../types";

export type Motion = {
  kind: SegmentKind;
  x0: number;
  z0: number;
  x1: number;
  z1: number;
  cx?: number;
  cz?: number;
  ccw?: boolean;
  line: number;
  feedMmMin?: number;
  tool?: number;
};

export type ParsedProgram = {
  lines: string[];
  motions: Motion[];
  diagnostics: Diagnostic[];
  /** Lines that contain something worth sending to the machine. */
  blockLines: number[];
};

type Word = { letter: string; value: number; column: number };

const SUPPORTED_G = new Set([0, 1, 2, 3, 4, 7, 8, 18, 20, 21, 28, 33, 40, 90, 91, 94, 95, 96, 97]);
const SUPPORTED_M = new Set([0, 1, 2, 3, 4, 5, 6, 8, 9, 30]);

export function stripComments(line: string): string {
  let out = "";
  let depth = 0;
  for (let i = 0; i < line.length; i++) {
    const c = line[i]!;
    if (c === "(") {
      depth++;
      out += " ";
    } else if (c === ")") {
      if (depth > 0) depth--;
      out += " ";
    } else if (depth === 0 && c === ";") {
      break;
    } else {
      out += depth === 0 ? c : " ";
    }
  }
  return out;
}

export function tokenizeLine(
  line: string,
  lineNo: number,
  diagnostics: Diagnostic[],
): Word[] {
  const text = stripComments(line);
  const words: Word[] = [];
  const re = /[A-Za-z][ \t]*([+-]?(?:\d+\.?\d*|\.\d+))?/g;
  let i = 0;
  while (i < text.length) {
    const c = text[i]!;
    if (c === " " || c === "\t" || c === "\r") {
      i++;
      continue;
    }
    if (c === "%" || c === "/") {
      // program marker / block delete — skip rest of line
      return [];
    }
    if (/[A-Za-z]/.test(c)) {
      re.lastIndex = i;
      const m = re.exec(text);
      if (m && m.index === i) {
        if (m[1] === undefined) {
          diagnostics.push({
            line: lineNo,
            column: i,
            severity: "error",
            message: `word '${c.toUpperCase()}' has no number`,
          });
          i++;
          continue;
        }
        words.push({ letter: c.toUpperCase(), value: parseFloat(m[1]), column: i });
        i = re.lastIndex;
        continue;
      }
    }
    diagnostics.push({
      line: lineNo,
      column: i,
      severity: "error",
      message: `unexpected character '${c}'`,
    });
    i++;
  }
  return words;
}

export type ParserState = {
  x: number; // radius mm
  z: number;
  unitScale: number; // 1 for G21, 25.4 for G20
  absolute: boolean; // G90
  diameterMode: boolean; // G7 (default on a lathe)
  motionModal: number; // 0,1,2,3,33; -1 none yet
  feedMmMin: number;
  tool: number;
  spindleRpm: number;
  spindleOn: boolean;
  modal: string[];
};

export function initialParserState(x = 0, z = 0): ParserState {
  return {
    x,
    z,
    unitScale: 1,
    absolute: true,
    diameterMode: true,
    motionModal: -1,
    feedMmMin: 0,
    tool: 0,
    spindleRpm: 0,
    spindleOn: false,
    modal: ["G21", "G90", "G7", "G94"],
  };
}

function setModal(state: ParserState, group: string[], code: string): void {
  state.modal = state.modal.filter((m) => !group.includes(m));
  state.modal.push(code);
}

/** Parse one line's words into zero or more motions, mutating `state`. */
export function parseLine(
  words: Word[],
  lineNo: number,
  state: ParserState,
  diagnostics: Diagnostic[],
): Motion[] {
  const motions: Motion[] = [];
  let sawAxisWord = false;
  let targetX = state.x;
  let targetZ = state.z;
  let iOfs: number | undefined;
  let kOfs: number | undefined;
  let rArc: number | undefined;
  let lineMotion = -1; // motion G given on this line, else modal

  for (const w of words) {
    const v = w.value;
    switch (w.letter) {
      case "N":
        break; // line numbers ignored
      case "G": {
        const g = Math.round(v * 10) / 10;
        if (!SUPPORTED_G.has(g)) {
          diagnostics.push({
            line: lineNo,
            column: w.column,
            severity: "warning",
            message: `G${g} not supported by the simulator; ignored`,
          });
          break;
        }
        if (g === 0 || g === 1 || g === 2 || g === 3 || g === 33) {
          lineMotion = g;
          state.motionModal = g;
          setModal(state, ["G0", "G1", "G2", "G3", "G33"], `G${g}`);
        } else if (g === 20) {
          state.unitScale = 25.4;
          setModal(state, ["G20", "G21"], "G20");
        } else if (g === 21) {
          state.unitScale = 1;
          setModal(state, ["G20", "G21"], "G21");
        } else if (g === 90) {
          state.absolute = true;
          setModal(state, ["G90", "G91"], "G90");
        } else if (g === 91) {
          state.absolute = false;
          setModal(state, ["G90", "G91"], "G91");
        } else if (g === 7) {
          state.diameterMode = true;
          setModal(state, ["G7", "G8"], "G7");
        } else if (g === 8) {
          state.diameterMode = false;
          setModal(state, ["G7", "G8"], "G8");
        } else if (g === 94 || g === 95) {
          setModal(state, ["G94", "G95"], `G${g}`);
          if (g === 95) {
            diagnostics.push({
              line: lineNo,
              column: w.column,
              severity: "warning",
              message: "G95 feed-per-rev: simulator treats F as mm/min",
            });
          }
        } else if (g === 96 || g === 97) {
          setModal(state, ["G96", "G97"], `G${g}`);
        }
        // G4, G18, G28, G40 accepted silently
        break;
      }
      case "M": {
        const m = Math.round(v);
        if (!SUPPORTED_M.has(m)) {
          diagnostics.push({
            line: lineNo,
            column: w.column,
            severity: "warning",
            message: `M${m} not supported by the simulator; ignored`,
          });
          break;
        }
        if (m === 3 || m === 4) state.spindleOn = true;
        if (m === 5 || m === 2 || m === 30) state.spindleOn = false;
        break;
      }
      case "X": {
        const raw = v * state.unitScale;
        const r = state.diameterMode ? raw / 2 : raw;
        targetX = state.absolute ? r : state.x + r;
        sawAxisWord = true;
        break;
      }
      case "U": {
        // incremental X (diameter convention like X)
        const raw = v * state.unitScale;
        targetX = state.x + (state.diameterMode ? raw / 2 : raw);
        sawAxisWord = true;
        break;
      }
      case "Z": {
        const raw = v * state.unitScale;
        targetZ = state.absolute ? raw : state.z + raw;
        sawAxisWord = true;
        break;
      }
      case "W": {
        targetZ = state.z + v * state.unitScale;
        sawAxisWord = true;
        break;
      }
      case "I":
        iOfs = v * state.unitScale; // radius offset from start
        break;
      case "K":
        kOfs = v * state.unitScale;
        break;
      case "R":
        rArc = v * state.unitScale;
        break;
      case "F":
        state.feedMmMin = v * state.unitScale;
        break;
      case "S":
        state.spindleRpm = v;
        break;
      case "T": {
        // EMCO-style T0101 selects station 1; plain T1 also works.
        const t = Math.round(v);
        state.tool = t >= 100 ? Math.floor(t / 100) : t;
        break;
      }
      case "P":
      case "Q":
      case "D":
      case "L":
      case "O":
        break; // accepted, unused
      default:
        diagnostics.push({
          line: lineNo,
          column: w.column,
          severity: "error",
          message: `unknown word '${w.letter}'`,
        });
    }
  }

  if (!sawAxisWord) return motions;
  const motion = lineMotion >= 0 ? lineMotion : state.motionModal;
  if (motion < 0) {
    diagnostics.push({
      line: lineNo,
      column: 0,
      severity: "error",
      message: "axis word with no motion mode (no G0/G1/G2/G3 in effect)",
    });
    return motions;
  }
  if (motion === 33 && state.spindleRpm > 400) {
    diagnostics.push({
      line: lineNo,
      column: 0,
      severity: "warning",
      message: "threading on the PC Turn 50 is limited to 400 rpm",
    });
  }
  if ((motion === 1 || motion === 2 || motion === 3) && state.feedMmMin <= 0) {
    diagnostics.push({
      line: lineNo,
      column: 0,
      severity: "error",
      message: "feed move with no F word in effect",
    });
  }

  const base = {
    x0: state.x,
    z0: state.z,
    x1: targetX,
    z1: targetZ,
    line: lineNo,
    feedMmMin: motion === 0 ? undefined : state.feedMmMin,
    tool: state.tool || undefined,
  };

  if (motion === 0) {
    motions.push({ kind: "rapid", ...base });
  } else if (motion === 1) {
    motions.push({ kind: "feed", ...base });
  } else if (motion === 33) {
    motions.push({ kind: "thread", ...base });
  } else {
    const ccw = motion === 3;
    let cx: number | undefined;
    let cz: number | undefined;
    if (iOfs !== undefined || kOfs !== undefined) {
      cx = state.x + (iOfs ?? 0);
      cz = state.z + (kOfs ?? 0);
    } else if (rArc !== undefined) {
      const c = arcCentreFromR(state.x, state.z, targetX, targetZ, rArc, ccw);
      if (c) {
        cx = c.cx;
        cz = c.cz;
      }
    }
    if (cx === undefined || cz === undefined) {
      diagnostics.push({
        line: lineNo,
        column: 0,
        severity: "error",
        message: "arc needs I/K or a reachable R",
      });
    } else {
      const r0 = Math.hypot(state.x - cx, state.z - cz);
      const r1 = Math.hypot(targetX - cx, targetZ - cz);
      if (Math.abs(r0 - r1) > 0.01 + 0.001 * r0) {
        diagnostics.push({
          line: lineNo,
          column: 0,
          severity: "error",
          message: `arc endpoints not equidistant from centre (${r0.toFixed(3)} vs ${r1.toFixed(3)} mm)`,
        });
      }
      motions.push({ kind: "arc", ...base, cx, cz, ccw });
    }
  }

  state.x = targetX;
  state.z = targetZ;
  return motions;
}

/** Centre of an arc defined by endpoints and radius (G2/G3 R-form). */
export function arcCentreFromR(
  x0: number,
  z0: number,
  x1: number,
  z1: number,
  r: number,
  ccw: boolean,
): { cx: number; cz: number } | null {
  const dx = x1 - x0;
  const dz = z1 - z0;
  const d = Math.hypot(dx, dz);
  if (d < 1e-9 || d > 2 * Math.abs(r) + 1e-9) return null;
  const h = Math.sqrt(Math.max(0, r * r - (d * d) / 4));
  // Perpendicular to the chord; sign picks the minor arc for +R (per RS-274),
  // flipped for cw vs ccw and for negative R (major arc).
  let sign = ccw ? 1 : -1;
  if (r < 0) sign = -sign;
  const mx = (x0 + x1) / 2;
  const mz = (z0 + z1) / 2;
  // In the (z, x) plane treat z like the horizontal axis and x vertical.
  const ux = -dz / d;
  const uz = dx / d;
  return { cx: mx + sign * h * ux, cz: mz + sign * h * uz };
}

export function parseProgramText(text: string): ParsedProgram {
  const lines = text.split(/\r\n|\r|\n/);
  const diagnostics: Diagnostic[] = [];
  const motions: Motion[] = [];
  const blockLines: number[] = [];
  const state = initialParserState();
  for (let i = 0; i < lines.length; i++) {
    const words = tokenizeLine(lines[i]!, i, diagnostics);
    if (words.length === 0) continue;
    blockLines.push(i);
    motions.push(...parseLine(words, i, state, diagnostics));
  }
  return { lines, motions, diagnostics, blockLines };
}
