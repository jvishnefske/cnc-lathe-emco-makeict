/** Backplot generation for the mock core: motions -> Segment[] + bounds. */

import type { Backplot, Segment, StockSpec } from "../types";
import type { Motion } from "./parser";

const TAU = Math.PI * 2;

/** Start/end angles (radians, atan2(x-cx, z-cz) convention) and swept angle. */
export function arcAngles(m: Motion): { a0: number; a1: number; sweep: number; r: number } {
  const cx = m.cx ?? 0;
  const cz = m.cz ?? 0;
  const a0 = Math.atan2(m.x0 - cx, m.z0 - cz);
  const a1 = Math.atan2(m.x1 - cx, m.z1 - cz);
  const r = Math.hypot(m.x0 - cx, m.z0 - cz);
  let sweep: number;
  if (m.ccw) {
    sweep = a1 - a0;
    while (sweep <= 1e-12) sweep += TAU;
  } else {
    sweep = a1 - a0;
    while (sweep >= -1e-12) sweep -= TAU;
  }
  return { a0, a1, sweep, r };
}

export function arcPoint(m: Motion, t: number): { x: number; z: number } {
  const { a0, sweep, r } = arcAngles(m);
  const a = a0 + sweep * t;
  return { x: (m.cx ?? 0) + r * Math.sin(a), z: (m.cz ?? 0) + r * Math.cos(a) };
}

export function motionLength(m: Motion): number {
  if (m.kind === "arc") {
    const { sweep, r } = arcAngles(m);
    return Math.abs(sweep) * r;
  }
  return Math.hypot(m.x1 - m.x0, m.z1 - m.z0);
}

function arcBounds(m: Motion): { xMin: number; xMax: number; zMin: number; zMax: number } {
  const { a0, sweep, r } = arcAngles(m);
  let xMin = Math.min(m.x0, m.x1);
  let xMax = Math.max(m.x0, m.x1);
  let zMin = Math.min(m.z0, m.z1);
  let zMax = Math.max(m.z0, m.z1);
  // Axis-extreme angles in our atan2(x, z) frame: 0 => +z, PI/2 => +x, etc.
  for (let k = -4; k <= 4; k++) {
    const a = (k * Math.PI) / 2;
    const t = (a - a0) / sweep;
    if (t > 1e-9 && t < 1 - 1e-9) {
      const x = (m.cx ?? 0) + r * Math.sin(a);
      const z = (m.cz ?? 0) + r * Math.cos(a);
      xMin = Math.min(xMin, x);
      xMax = Math.max(xMax, x);
      zMin = Math.min(zMin, z);
      zMax = Math.max(zMax, z);
    }
  }
  return { xMin, xMax, zMin, zMax };
}

export function buildBackplot(motions: Motion[], stock: StockSpec | null): Backplot {
  const segments: Segment[] = motions.map((m) => {
    const s: Segment = {
      kind: m.kind,
      x0: m.x0,
      z0: m.z0,
      x1: m.x1,
      z1: m.z1,
      line: m.line,
    };
    if (m.kind === "arc") {
      s.cx = m.cx;
      s.cz = m.cz;
      s.ccw = m.ccw;
    }
    if (m.feedMmMin !== undefined) s.feedMmMin = m.feedMmMin;
    if (m.tool !== undefined) s.tool = m.tool;
    return s;
  });

  let xMin = Infinity;
  let xMax = -Infinity;
  let zMin = Infinity;
  let zMax = -Infinity;
  for (const m of motions) {
    if (m.kind === "arc") {
      const b = arcBounds(m);
      xMin = Math.min(xMin, b.xMin);
      xMax = Math.max(xMax, b.xMax);
      zMin = Math.min(zMin, b.zMin);
      zMax = Math.max(zMax, b.zMax);
    } else {
      xMin = Math.min(xMin, m.x0, m.x1);
      xMax = Math.max(xMax, m.x0, m.x1);
      zMin = Math.min(zMin, m.z0, m.z1);
      zMax = Math.max(zMax, m.z0, m.z1);
    }
  }
  if (stock) {
    xMin = Math.min(xMin, 0);
    xMax = Math.max(xMax, stock.diameterMm / 2);
    zMin = Math.min(zMin, stock.zFaceMm - stock.lengthMm);
    zMax = Math.max(zMax, stock.zFaceMm);
  }
  if (!isFinite(xMin)) {
    xMin = 0;
    xMax = 1;
    zMin = -1;
    zMax = 0;
  }
  return { segments, bounds: { xMin, xMax, zMin, zMax } };
}
