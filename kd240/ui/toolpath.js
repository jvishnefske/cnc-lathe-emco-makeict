// The toolpath view: sender/web's toolpath.ts and plotMath.ts in plain JS.
// Lathe convention: Z horizontal (negative toward the chuck, on the left),
// X vertical as radius, profile mirrored about the centreline. Drag pans,
// the wheel zooms, a double click fits. The filled marker is the session's
// predicted position; a hollow red marker shows the simulator's true
// position when the two differ (after a stall).

const COLORS = {
  bg: "#10151a",
  grid: "#1c242c",
  gridLabel: "#3d4b58",
  centreline: "#4a5a68",
  rapidPending: "rgba(122, 143, 163, 0.55)",
  feedPending: "#3f7fbf",
  threadPending: "#9a6fd0",
  rapidDone: "rgba(180, 200, 150, 0.5)",
  feedDone: "#5ad06e",
  threadDone: "#c79af0",
  mirrorAlpha: 0.35,
  tool: "#ffb020",
  toolLine: "rgba(255, 176, 32, 0.55)",
  truth: "#ff4136",
};

export function worldToScreen(z, x, view, w, h) {
  return { sx: w / 2 + (z - view.zCenter) * view.pxPerMm, sy: h / 2 - (x - view.xCenter) * view.pxPerMm };
}

export function screenToWorld(sx, sy, view, w, h) {
  return { z: view.zCenter + (sx - w / 2) / view.pxPerMm, x: view.xCenter - (sy - h / 2) / view.pxPerMm };
}

export function fitView(bounds, w, h, margin = 0.08) {
  const zSpan = Math.max(bounds.zMax - bounds.zMin, 1e-6);
  const xTop = Math.max(Math.abs(bounds.xMax), Math.abs(bounds.xMin), 1e-6);
  const pxPerMm = Math.min((w * (1 - 2 * margin)) / zSpan, (h * (1 - 2 * margin)) / (2 * xTop));
  return { pxPerMm: Math.max(pxPerMm, 1e-6), zCenter: (bounds.zMin + bounds.zMax) / 2, xCenter: 0 };
}

export function zoomAt(view, factor, sx, sy, w, h) {
  const before = screenToWorld(sx, sy, view, w, h);
  const pxPerMm = Math.min(5000, Math.max(0.01, view.pxPerMm * factor));
  const scaled = { ...view, pxPerMm };
  const after = screenToWorld(sx, sy, scaled, w, h);
  return { pxPerMm, zCenter: scaled.zCenter + (before.z - after.z), xCenter: scaled.xCenter + (before.x - after.x) };
}

export function tessellateArc(seg, pxPerMm) {
  const cx = seg.cx ?? 0;
  const cz = seg.cz ?? 0;
  const a0 = Math.atan2(seg.x0 - cx, seg.z0 - cz);
  const a1 = Math.atan2(seg.x1 - cx, seg.z1 - cz);
  const r = Math.hypot(seg.x0 - cx, seg.z0 - cz);
  const TAU = Math.PI * 2;
  let sweep = a1 - a0;
  if (seg.ccw) while (sweep <= 1e-12) sweep += TAU;
  else while (sweep >= -1e-12) sweep -= TAU;
  const maxErr = 0.25 / Math.max(pxPerMm, 1e-6);
  let dTheta = Math.PI / 8;
  if (r > 1e-9 && maxErr < r) dTheta = 2 * Math.acos(Math.max(-1, Math.min(1, 1 - maxErr / r)));
  const n = Math.min(1024, Math.max(2, Math.ceil(Math.abs(sweep) / Math.max(dTheta, 1e-4))));
  const pts = [];
  for (let i = 0; i <= n; i++) {
    const a = a0 + (sweep * i) / n;
    pts.push({ z: cz + r * Math.cos(a), x: cx + r * Math.sin(a) });
  }
  pts[0] = { z: seg.z0, x: seg.x0 };
  pts[n] = { z: seg.z1, x: seg.x1 };
  return pts;
}

export function gridPitchMm(pxPerMm, targetPx = 80) {
  const raw = targetPx / Math.max(pxPerMm, 1e-9);
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  for (const m of [1, 2, 5, 10]) if (mag * m >= raw) return mag * m;
  return mag * 10;
}

const fmt = (v) =>
  Math.abs(v) >= 100 ? v.toFixed(0) : Math.abs(v) >= 1 ? String(+v.toFixed(1)) : String(+v.toFixed(2));

export class ToolpathView {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.view = { pxPerMm: 4, zCenter: -40, xCenter: 0 };
    this.backplot = null;
    this.dro = null;
    this.truth = null;
    this.currentLine = -1;
    this.executedThrough = -1;
    this.needFit = true;
    let drag = null;
    canvas.addEventListener("pointerdown", (e) => {
      drag = { x: e.offsetX, y: e.offsetY };
      canvas.setPointerCapture(e.pointerId);
    });
    canvas.addEventListener("pointermove", (e) => {
      if (!drag) return;
      const dx = e.offsetX - drag.x;
      const dy = e.offsetY - drag.y;
      drag = { x: e.offsetX, y: e.offsetY };
      this.view = {
        ...this.view,
        zCenter: this.view.zCenter - dx / this.view.pxPerMm,
        xCenter: this.view.xCenter + dy / this.view.pxPerMm,
      };
    });
    const end = () => (drag = null);
    canvas.addEventListener("pointerup", end);
    canvas.addEventListener("pointercancel", end);
    canvas.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        this.view = zoomAt(this.view, Math.pow(1.0015, -e.deltaY), e.offsetX, e.offsetY, ...this.size());
      },
      { passive: false },
    );
    canvas.addEventListener("dblclick", () => this.fit());
  }

  size() {
    return [this.canvas.clientWidth, this.canvas.clientHeight];
  }

  setBackplot(bp) {
    this.backplot = bp;
    this.executedThrough = -1;
    this.needFit = true;
  }

  update(dro, truth, currentLine, running) {
    this.dro = dro;
    this.truth = truth;
    this.currentLine = currentLine;
    if (running && currentLine >= 0) {
      this.executedThrough = Math.max(this.executedThrough, currentLine - 1);
    } else if (!running && currentLine < 0 && this.executedThrough >= 0 && this.backplot) {
      const maxLine = this.backplot.segments.reduce((a, s) => Math.max(a, s.line), -1);
      if (this.executedThrough >= maxLine - 1) this.executedThrough = maxLine;
    }
  }

  fit() {
    const [w, h] = this.size();
    if (!w || !h) return;
    const b = this.backplot?.bounds;
    const ok = b && Number.isFinite(b.zMin) && Number.isFinite(b.zMax) && b.zMax >= b.zMin;
    this.view = fitView(ok ? b : { xMin: 0, xMax: 25, zMin: -80, zMax: 5 }, w, h);
    this.needFit = false;
  }

  render() {
    const dpr = globalThis.devicePixelRatio || 1;
    const [w, h] = this.size();
    if (!w || !h) return;
    if (this.canvas.width !== Math.round(w * dpr) || this.canvas.height !== Math.round(h * dpr)) {
      this.canvas.width = Math.round(w * dpr);
      this.canvas.height = Math.round(h * dpr);
    }
    if (this.needFit) this.fit();
    const ctx = this.ctx;
    ctx.save();
    ctx.scale(dpr, dpr);
    ctx.fillStyle = COLORS.bg;
    ctx.fillRect(0, 0, w, h);
    this.drawGrid(ctx, w, h);
    this.drawCentreline(ctx, w, h);
    if (this.backplot) {
      this.drawSegments(ctx, w, h, true);
      this.drawSegments(ctx, w, h, false);
    }
    this.drawTruth(ctx, w, h);
    this.drawTool(ctx, w, h);
    this.drawScale(ctx, h);
    ctx.restore();
  }

  s(z, x, w, h) {
    return worldToScreen(z, x, this.view, w, h);
  }

  drawGrid(ctx, w, h) {
    const pitch = gridPitchMm(this.view.pxPerMm);
    const tl = screenToWorld(0, 0, this.view, w, h);
    const br = screenToWorld(w, h, this.view, w, h);
    ctx.strokeStyle = COLORS.grid;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let z = Math.ceil(tl.z / pitch) * pitch; z <= br.z; z += pitch) {
      const { sx } = this.s(z, 0, w, h);
      ctx.moveTo(sx, 0);
      ctx.lineTo(sx, h);
    }
    for (let x = Math.ceil(br.x / pitch) * pitch; x <= tl.x; x += pitch) {
      const { sy } = this.s(0, x, w, h);
      ctx.moveTo(0, sy);
      ctx.lineTo(w, sy);
    }
    ctx.stroke();
    ctx.fillStyle = COLORS.gridLabel;
    ctx.font = "10px ui-monospace, monospace";
    for (let z = Math.ceil(tl.z / pitch) * pitch; z <= br.z; z += pitch) {
      ctx.fillText(`Z${fmt(z)}`, this.s(z, 0, w, h).sx + 3, h - 6);
    }
  }

  drawCentreline(ctx, w, h) {
    const { sy } = this.s(0, 0, w, h);
    if (sy < -2 || sy > h + 2) return;
    ctx.strokeStyle = COLORS.centreline;
    ctx.setLineDash([14, 4, 3, 4]);
    ctx.beginPath();
    ctx.moveTo(0, sy);
    ctx.lineTo(w, sy);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = COLORS.gridLabel;
    ctx.fillText("℄", w - 16, sy - 4);
  }

  drawSegments(ctx, w, h, mirrored) {
    ctx.save();
    if (mirrored) ctx.globalAlpha = COLORS.mirrorAlpha;
    const flip = mirrored ? -1 : 1;
    for (const seg of this.backplot.segments) {
      const done = seg.line <= this.executedThrough;
      const active = seg.line === this.currentLine;
      let color = done ? COLORS.feedDone : COLORS.feedPending;
      let dash = [];
      if (seg.kind === "rapid") {
        color = done ? COLORS.rapidDone : COLORS.rapidPending;
        dash = [5, 5];
      } else if (seg.kind === "thread") {
        color = done ? COLORS.threadDone : COLORS.threadPending;
        dash = [8, 2, 2, 2];
      }
      ctx.strokeStyle = color;
      ctx.lineWidth = seg.kind === "rapid" ? 1 : active ? 2.5 : 1.75;
      ctx.setLineDash(dash);
      ctx.beginPath();
      const pts =
        seg.kind === "arc"
          ? tessellateArc(seg, this.view.pxPerMm)
          : [
              { z: seg.z0, x: seg.x0 },
              { z: seg.z1, x: seg.x1 },
            ];
      pts.forEach((p, i) => {
        const q = this.s(p.z, flip * p.x, w, h);
        if (i === 0) ctx.moveTo(q.sx, q.sy);
        else ctx.lineTo(q.sx, q.sy);
      });
      ctx.stroke();
    }
    ctx.setLineDash([]);
    ctx.restore();
  }

  drawTruth(ctx, w, h) {
    if (!this.truth || !this.dro) return;
    if (Math.hypot(this.truth.xRadiusMm - this.dro.xRadiusMm, this.truth.zMm - this.dro.zMm) < 0.01) return;
    const p = this.s(this.truth.zMm, this.truth.xRadiusMm, w, h);
    ctx.strokeStyle = COLORS.truth;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(p.sx, p.sy);
    ctx.lineTo(p.sx + 6, p.sy - 10);
    ctx.lineTo(p.sx - 6, p.sy - 10);
    ctx.closePath();
    ctx.stroke();
  }

  drawTool(ctx, w, h) {
    if (!this.dro) return;
    const p = this.s(this.dro.zMm, this.dro.xRadiusMm, w, h);
    const cl = this.s(this.dro.zMm, 0, w, h);
    ctx.strokeStyle = COLORS.toolLine;
    ctx.lineWidth = 1;
    ctx.setLineDash([2, 4]);
    ctx.beginPath();
    ctx.moveTo(p.sx, p.sy);
    ctx.lineTo(cl.sx, cl.sy);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = COLORS.tool;
    ctx.beginPath();
    ctx.moveTo(p.sx, p.sy);
    ctx.lineTo(p.sx + 6, p.sy - 10);
    ctx.lineTo(p.sx - 6, p.sy - 10);
    ctx.closePath();
    ctx.fill();
  }

  drawScale(ctx, h) {
    const pitch = gridPitchMm(this.view.pxPerMm);
    const px = pitch * this.view.pxPerMm;
    const y = h - 14;
    ctx.strokeStyle = "#8aa0b4";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(12, y);
    ctx.lineTo(12 + px, y);
    ctx.moveTo(12, y - 4);
    ctx.lineTo(12, y + 4);
    ctx.moveTo(12 + px, y - 4);
    ctx.lineTo(12 + px, y + 4);
    ctx.stroke();
    ctx.fillStyle = "#8aa0b4";
    ctx.font = "11px ui-monospace, monospace";
    ctx.fillText(`${fmt(pitch)} mm`, 16, y - 6);
  }
}
