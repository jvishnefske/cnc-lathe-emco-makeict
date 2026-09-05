import { describe, expect, it } from "vitest";
import { arcAngles, arcPoint, buildBackplot, motionLength } from "../src/core/mock/backplot";
import type { Motion } from "../src/core/mock/parser";

const line = (x0: number, z0: number, x1: number, z1: number): Motion => ({
  kind: "feed",
  x0,
  z0,
  x1,
  z1,
  line: 0,
});

// CCW half-circle from (x0,z0)=(0,0) to (0,-10) about centre (0,-5), r=5.
// Its extreme bulges to x=+5 at z=-5.
const halfArc: Motion = {
  kind: "arc",
  x0: 0,
  z0: 0,
  x1: 0,
  z1: -10,
  cx: 0,
  cz: -5,
  ccw: true,
  line: 3,
};

describe("arc geometry", () => {
  it("computes sweep direction and radius", () => {
    const { sweep, r } = arcAngles(halfArc);
    expect(r).toBeCloseTo(5, 9);
    expect(sweep).toBeCloseTo(Math.PI, 9);
  });

  it("interpolates points along the arc", () => {
    const mid = arcPoint(halfArc, 0.5);
    expect(mid.x).toBeCloseTo(5, 6);
    expect(mid.z).toBeCloseTo(-5, 6);
    const end = arcPoint(halfArc, 1);
    expect(end.x).toBeCloseTo(0, 6);
    expect(end.z).toBeCloseTo(-10, 6);
  });

  it("measures arc length as r * sweep", () => {
    expect(motionLength(halfArc)).toBeCloseTo(5 * Math.PI, 9);
    expect(motionLength(line(0, 0, 3, -4))).toBeCloseTo(5, 9);
  });
});

describe("buildBackplot", () => {
  it("copies segments and computes straight-line bounds", () => {
    const bp = buildBackplot([line(0, 0, 5, -20)], null);
    expect(bp.segments).toHaveLength(1);
    expect(bp.bounds).toEqual({ xMin: 0, xMax: 5, zMin: -20, zMax: 0 });
  });

  it("includes arc extrema in bounds, not just endpoints", () => {
    const bp = buildBackplot([halfArc], null);
    // Endpoints only would give xMax = 0; the bulge reaches x = 5.
    expect(bp.bounds.xMax).toBeCloseTo(5, 6);
    expect(bp.bounds.zMin).toBeCloseTo(-10, 6);
  });

  it("grows bounds to cover the stock silhouette", () => {
    const bp = buildBackplot([line(0, 0, 1, -1)], {
      diameterMm: 14,
      lengthMm: 60,
      zFaceMm: 0,
    });
    expect(bp.bounds.xMax).toBe(7);
    expect(bp.bounds.zMin).toBe(-60);
    expect(bp.bounds.zMax).toBe(0);
  });

  it("carries arc metadata onto segments", () => {
    const bp = buildBackplot([halfArc], null);
    expect(bp.segments[0]).toMatchObject({ kind: "arc", cx: 0, cz: -5, ccw: true, line: 3 });
  });

  it("survives an empty program", () => {
    const bp = buildBackplot([], null);
    expect(bp.segments).toHaveLength(0);
    expect(Number.isFinite(bp.bounds.xMin)).toBe(true);
  });
});
