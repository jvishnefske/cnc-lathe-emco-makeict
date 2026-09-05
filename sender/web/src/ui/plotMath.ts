/**
 * Pure geometry for the toolpath view — kept free of DOM/canvas so it can be
 * unit tested. Screen convention (lathe): Z runs horizontally with negative
 * toward the chuck (left), X is radius and runs vertically (up = +X). The
 * profile is mirrored about the centreline, so the "plot plane" vertical
 * coordinate is signed radius.
 */

import type { Backplot, Segment } from "../core/types";

export type View = {
  pxPerMm: number;
  zCenter: number; // world z at canvas centre
  xCenter: number; // signed radius at canvas centre (0 = centreline centred)
};

export type ScreenPoint = { sx: number; sy: number };

export function worldToScreen(
  z: number,
  x: number,
  view: View,
  canvasW: number,
  canvasH: number,
): ScreenPoint {
  return {
    sx: canvasW / 2 + (z - view.zCenter) * view.pxPerMm,
    sy: canvasH / 2 - (x - view.xCenter) * view.pxPerMm,
  };
}

export function screenToWorld(
  sx: number,
  sy: number,
  view: View,
  canvasW: number,
  canvasH: number,
): { z: number; x: number } {
  return {
    z: view.zCenter + (sx - canvasW / 2) / view.pxPerMm,
    x: view.xCenter - (sy - canvasH / 2) / view.pxPerMm,
  };
}

/**
 * Fit the mirrored envelope (|x| up to xMax, z range as-is) into the canvas
 * with a margin fraction. Centres the centreline vertically.
 */
export function fitView(
  bounds: Backplot["bounds"],
  canvasW: number,
  canvasH: number,
  marginFrac = 0.08,
): View {
  const zSpan = Math.max(bounds.zMax - bounds.zMin, 1e-6);
  const xTop = Math.max(Math.abs(bounds.xMax), Math.abs(bounds.xMin), 1e-6);
  const xSpan = 2 * xTop; // mirrored about the centreline
  const usableW = canvasW * (1 - 2 * marginFrac);
  const usableH = canvasH * (1 - 2 * marginFrac);
  const pxPerMm = Math.min(usableW / zSpan, usableH / xSpan);
  return {
    pxPerMm: Math.max(pxPerMm, 1e-6),
    zCenter: (bounds.zMin + bounds.zMax) / 2,
    xCenter: 0,
  };
}

/** Zoom about a screen point, keeping the world point under the cursor fixed. */
export function zoomAt(
  view: View,
  factor: number,
  sx: number,
  sy: number,
  canvasW: number,
  canvasH: number,
): View {
  const before = screenToWorld(sx, sy, view, canvasW, canvasH);
  const pxPerMm = clamp(view.pxPerMm * factor, 0.01, 5000);
  const scaled: View = { ...view, pxPerMm };
  const after = screenToWorld(sx, sy, scaled, canvasW, canvasH);
  return {
    pxPerMm,
    zCenter: scaled.zCenter + (before.z - after.z),
    xCenter: scaled.xCenter + (before.x - after.x),
  };
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

export function arcAnglesOf(seg: Segment): { a0: number; sweep: number; r: number } {
  const cx = seg.cx ?? 0;
  const cz = seg.cz ?? 0;
  const a0 = Math.atan2(seg.x0 - cx, seg.z0 - cz);
  const a1 = Math.atan2(seg.x1 - cx, seg.z1 - cz);
  const r = Math.hypot(seg.x0 - cx, seg.z0 - cz);
  let sweep = a1 - a0;
  const TAU = Math.PI * 2;
  if (seg.ccw) {
    while (sweep <= 1e-12) sweep += TAU;
  } else {
    while (sweep >= -1e-12) sweep -= TAU;
  }
  return { a0, sweep, r };
}

/**
 * Flatten an arc segment into a polyline whose chord error stays below
 * ~0.25 px at the given zoom. Density therefore follows the current zoom,
 * as required by INTERFACE.md.
 */
export function tessellateArc(seg: Segment, pxPerMm: number): { z: number; x: number }[] {
  const { a0, sweep, r } = arcAnglesOf(seg);
  const cx = seg.cx ?? 0;
  const cz = seg.cz ?? 0;
  const maxErrMm = 0.25 / Math.max(pxPerMm, 1e-6);
  let dTheta = Math.PI / 8;
  if (r > 1e-9 && maxErrMm < r) {
    dTheta = 2 * Math.acos(Math.max(-1, Math.min(1, 1 - maxErrMm / r)));
  }
  const n = Math.min(1024, Math.max(2, Math.ceil(Math.abs(sweep) / Math.max(dTheta, 1e-4))));
  const pts: { z: number; x: number }[] = [];
  for (let i = 0; i <= n; i++) {
    const a = a0 + (sweep * i) / n;
    pts.push({ z: cz + r * Math.cos(a), x: cx + r * Math.sin(a) });
  }
  // Pin exact endpoints.
  pts[0] = { z: seg.z0, x: seg.x0 };
  pts[n] = { z: seg.z1, x: seg.x1 };
  return pts;
}

/** Mirror a plot-plane point about the lathe centreline. */
export function mirrorPoint(p: { z: number; x: number }): { z: number; x: number } {
  return { z: p.z, x: -p.x };
}

/** Nice round grid pitch (in mm) giving roughly `targetPx` between lines. */
export function gridPitchMm(pxPerMm: number, targetPx = 80): number {
  const raw = targetPx / Math.max(pxPerMm, 1e-9);
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  for (const m of [1, 2, 5, 10]) {
    if (mag * m >= raw) return mag * m;
  }
  return mag * 10;
}
