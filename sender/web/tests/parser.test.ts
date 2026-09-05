import { describe, expect, it } from "vitest";
import {
  arcCentreFromR,
  initialParserState,
  parseLine,
  parseProgramText,
  stripComments,
  tokenizeLine,
} from "../src/core/mock/parser";
import type { Diagnostic } from "../src/core/types";

function motionsOf(text: string) {
  return parseProgramText(text);
}

describe("comment stripping and tokenizing", () => {
  it("strips parenthesised and ; comments", () => {
    expect(stripComments("G1 X5 ( face ) Z-2 ; trailing").trim()).toBe("G1 X5          Z-2");
  });

  it("skips block-delete and % lines", () => {
    const diags: Diagnostic[] = [];
    expect(tokenizeLine("/ G1 X5", 0, diags)).toEqual([]);
    expect(tokenizeLine("%", 0, diags)).toEqual([]);
    expect(diags).toEqual([]);
  });

  it("reports a word without a number", () => {
    const diags: Diagnostic[] = [];
    tokenizeLine("G1 X Z-2", 0, diags);
    expect(diags.some((d) => d.severity === "error" && d.message.includes("'X'"))).toBe(true);
  });
});

describe("motion parsing", () => {
  it("halves X in default diameter mode", () => {
    const p = motionsOf("G1 X10 Z-5 F100");
    expect(p.motions).toHaveLength(1);
    expect(p.motions[0]).toMatchObject({ kind: "feed", x1: 5, z1: -5 });
  });

  it("keeps X as radius in G8 mode", () => {
    const p = motionsOf("G8\nG1 X10 Z-5 F100");
    expect(p.motions[0]!.x1).toBe(10);
  });

  it("handles G91 incremental and U/W words", () => {
    const p = motionsOf("G0 X10 Z-2\nG91 G1 Z-3 F50\nG90\nG1 U2 W-1");
    // X10 dia -> radius 5; then incremental Z-3 from -2.
    expect(p.motions[1]).toMatchObject({ x0: 5, z0: -2, x1: 5, z1: -5 });
    // U2 diameter-convention incremental -> radius +1; W-1 -> z -6.
    expect(p.motions[2]).toMatchObject({ x1: 6, z1: -6 });
  });

  it("scales G20 inch input to mm", () => {
    const p = motionsOf("G20\nG0 X1 Z-1");
    expect(p.motions[0]!.x1).toBeCloseTo(12.7, 9); // 1 inch dia -> 12.7 mm radius
    expect(p.motions[0]!.z1).toBeCloseTo(-25.4, 9);
  });

  it("errors on a feed move with no F word", () => {
    const p = motionsOf("G1 X10 Z-5");
    expect(p.diagnostics.some((d) => d.severity === "error" && /no F word/.test(d.message))).toBe(
      true,
    );
  });

  it("errors on an axis word with no motion mode", () => {
    const p = motionsOf("X10 Z-5");
    expect(p.motions).toHaveLength(0);
    expect(p.diagnostics.some((d) => d.severity === "error")).toBe(true);
  });

  it("warns on unsupported codes instead of failing", () => {
    const p = motionsOf("G70 X10\nM99");
    expect(p.diagnostics.filter((d) => d.severity === "warning").length).toBeGreaterThanOrEqual(2);
  });

  it("warns when threading above 400 rpm", () => {
    const p = motionsOf("S800 M3\nG33 Z-10 K1.5 F1");
    expect(p.diagnostics.some((d) => /400 rpm/.test(d.message))).toBe(true);
    expect(p.motions[0]!.kind).toBe("thread");
  });

  it("parses EMCO-style T0101 as station 1", () => {
    const state = initialParserState();
    const diags: Diagnostic[] = [];
    parseLine(tokenizeLine("T0101", 0, diags), 0, state, diags);
    expect(state.tool).toBe(1);
  });

  it("tracks modal groups", () => {
    const p = motionsOf("G21 G90 G1 X4 Z-1 F100");
    // Final state is embedded implicitly: reparse via initial state.
    const state = initialParserState();
    const diags: Diagnostic[] = [];
    parseLine(tokenizeLine("G21 G90 G1 X4 Z-1 F100", 0, diags), 0, state, diags);
    expect(state.modal).toContain("G1");
    expect(state.modal).toContain("G21");
    expect(state.modal).toContain("G90");
    expect(p.blockLines).toEqual([0]);
  });
});

describe("arcs", () => {
  it("builds an arc centre from I/K offsets (radius frame)", () => {
    const p = motionsOf("G0 X0 Z0\nG2 X10 Z-5 I5 K0 F100");
    const arc = p.motions[1]!;
    expect(arc.kind).toBe("arc");
    expect(arc.cx).toBe(5); // start x(0) + I5
    expect(arc.cz).toBe(0);
    expect(arc.ccw).toBe(false);
  });

  it("rejects arcs whose endpoints are not equidistant from the centre", () => {
    const p = motionsOf("G0 X0 Z0\nG2 X10 Z-20 I1 K0 F100");
    expect(p.diagnostics.some((d) => /equidistant/.test(d.message))).toBe(true);
  });

  it("computes a centre from R that is equidistant from both endpoints", () => {
    const c = arcCentreFromR(0, 0, 5, -5, 5, true);
    expect(c).not.toBeNull();
    const r0 = Math.hypot(0 - c!.cx, 0 - c!.cz);
    const r1 = Math.hypot(5 - c!.cx, -5 - c!.cz);
    expect(r0).toBeCloseTo(5, 9);
    expect(r1).toBeCloseTo(5, 9);
  });

  it("returns null when R cannot span the chord", () => {
    expect(arcCentreFromR(0, 0, 20, 0, 5, true)).toBeNull();
  });

  it("errors when an arc has neither I/K nor a reachable R", () => {
    const p = motionsOf("G0 X0 Z0\nG3 X40 Z0 R1 F100");
    expect(p.diagnostics.some((d) => /I\/K or a reachable R/.test(d.message))).toBe(true);
  });
});
