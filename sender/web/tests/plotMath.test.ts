import { describe, expect, it } from "vitest";
import {
  fitView,
  gridPitchMm,
  mirrorPoint,
  screenToWorld,
  tessellateArc,
  worldToScreen,
  zoomAt,
} from "../src/ui/plotMath";
import type { Segment } from "../src/core/types";

const W = 800;
const H = 600;

describe("world/screen mapping", () => {
  it("round-trips through worldToScreen and screenToWorld", () => {
    const view = { pxPerMm: 7, zCenter: -30, xCenter: 2 };
    const s = worldToScreen(-12.5, 4.25, view, W, H);
    const w = screenToWorld(s.sx, s.sy, view, W, H);
    expect(w.z).toBeCloseTo(-12.5, 9);
    expect(w.x).toBeCloseTo(4.25, 9);
  });

  it("puts +x up (smaller sy) and -z left (smaller sx)", () => {
    const view = { pxPerMm: 10, zCenter: 0, xCenter: 0 };
    const up = worldToScreen(0, 5, view, W, H);
    const left = worldToScreen(-5, 0, view, W, H);
    const centre = worldToScreen(0, 0, view, W, H);
    expect(up.sy).toBeLessThan(centre.sy);
    expect(left.sx).toBeLessThan(centre.sx);
  });
});

describe("fitView", () => {
  it("fits the mirrored envelope and centres the centreline", () => {
    const bounds = { xMin: 0, xMax: 10, zMin: -50, zMax: 0 };
    const view = fitView(bounds, W, H);
    expect(view.xCenter).toBe(0);
    expect(view.zCenter).toBe(-25);
    // Mirrored profile spans 2*xMax = 20 mm vertically.
    const usableH = H * (1 - 2 * 0.08);
    const usableW = W * (1 - 2 * 0.08);
    expect(view.pxPerMm).toBeCloseTo(Math.min(usableW / 50, usableH / 20), 9);
    // Whole envelope fits inside the canvas.
    for (const [z, x] of [
      [-50, 10],
      [0, -10],
    ] as const) {
      const s = worldToScreen(z, x, view, W, H);
      expect(s.sx).toBeGreaterThanOrEqual(0);
      expect(s.sx).toBeLessThanOrEqual(W);
      expect(s.sy).toBeGreaterThanOrEqual(0);
      expect(s.sy).toBeLessThanOrEqual(H);
    }
  });
});

describe("zoomAt", () => {
  it("keeps the world point under the cursor fixed", () => {
    const view = { pxPerMm: 5, zCenter: -20, xCenter: 0 };
    const cursor = { sx: 123, sy: 456 };
    const before = screenToWorld(cursor.sx, cursor.sy, view, W, H);
    const zoomed = zoomAt(view, 1.8, cursor.sx, cursor.sy, W, H);
    const after = screenToWorld(cursor.sx, cursor.sy, zoomed, W, H);
    expect(after.z).toBeCloseTo(before.z, 9);
    expect(after.x).toBeCloseTo(before.x, 9);
    expect(zoomed.pxPerMm).toBeCloseTo(9, 9);
  });

  it("clamps the zoom range", () => {
    const view = { pxPerMm: 5, zCenter: 0, xCenter: 0 };
    expect(zoomAt(view, 1e9, 0, 0, W, H).pxPerMm).toBeLessThanOrEqual(5000);
    expect(zoomAt(view, 1e-9, 0, 0, W, H).pxPerMm).toBeGreaterThanOrEqual(0.01);
  });
});

describe("tessellateArc", () => {
  const arc: Segment = {
    kind: "arc",
    x0: 0,
    z0: 0,
    x1: 0,
    z1: -10,
    cx: 0,
    cz: -5,
    ccw: true,
    line: 0,
  };

  it("pins exact endpoints", () => {
    const pts = tessellateArc(arc, 4);
    expect(pts[0]).toEqual({ z: 0, x: 0 });
    expect(pts[pts.length - 1]).toEqual({ z: -10, x: 0 });
  });

  it("keeps every point on the circle", () => {
    for (const p of tessellateArc(arc, 10)) {
      expect(Math.hypot(p.x - 0, p.z - -5)).toBeCloseTo(5, 6);
    }
  });

  it("uses more points when zoomed in", () => {
    const coarse = tessellateArc(arc, 1).length;
    const fine = tessellateArc(arc, 100).length;
    expect(fine).toBeGreaterThan(coarse);
  });

  it("keeps chord error under ~0.25 px at the given zoom", () => {
    for (const pxPerMm of [2, 20, 200]) {
      const pts = tessellateArc(arc, pxPerMm);
      for (let i = 1; i < pts.length; i++) {
        const mid = { x: (pts[i - 1]!.x + pts[i]!.x) / 2, z: (pts[i - 1]!.z + pts[i]!.z) / 2 };
        const sagitta = 5 - Math.hypot(mid.x - 0, mid.z - -5);
        expect(sagitta * pxPerMm).toBeLessThanOrEqual(0.3); // px
      }
    }
  });
});

describe("helpers", () => {
  it("mirrors about the centreline", () => {
    expect(mirrorPoint({ z: -3, x: 7 })).toEqual({ z: -3, x: -7 });
  });

  it("picks 1/2/5-series grid pitches", () => {
    for (const pxPerMm of [0.05, 0.7, 3, 12, 160]) {
      const pitch = gridPitchMm(pxPerMm);
      const mantissa = pitch / Math.pow(10, Math.floor(Math.log10(pitch)));
      expect([1, 2, 5]).toContainEqual(+mantissa.toFixed(6) % 10 === 0 ? 1 : +mantissa.toFixed(6));
      // Roughly the target spacing.
      expect(pitch * pxPerMm).toBeGreaterThanOrEqual(40);
      expect(pitch * pxPerMm).toBeLessThanOrEqual(801);
    }
  });
});
