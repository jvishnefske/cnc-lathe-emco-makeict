/**
 * Toolpath view — the centrepiece. Canvas 2D, lathe convention:
 * Z horizontal (negative toward the chuck, on the left), X vertical as
 * radius, profile mirrored about the centreline, stock silhouette behind.
 * Pan (drag), zoom (wheel), fit-to-view. Executed path drawn distinct from
 * pending; live tool marker; centreline and chuck face indicated.
 */

import type { Backplot, Dro, Segment, StockSpec } from "../core/types";
import {
  fitView,
  gridPitchMm,
  screenToWorld,
  tessellateArc,
  worldToScreen,
  zoomAt,
  type View,
} from "./plotMath";

const COLORS = {
  bg: "#10151a",
  grid: "#1c242c",
  gridLabel: "#3d4b58",
  stockFill: "rgba(120, 132, 144, 0.13)",
  stockEdge: "rgba(148, 163, 178, 0.45)",
  centreline: "#4a5a68",
  chuck: "#2a333d",
  chuckEdge: "#55636f",
  rapidPending: "rgba(122, 143, 163, 0.55)",
  feedPending: "#3f7fbf",
  threadPending: "#9a6fd0",
  rapidDone: "rgba(180, 200, 150, 0.5)",
  feedDone: "#5ad06e",
  threadDone: "#c79af0",
  mirrorAlpha: 0.35,
  tool: "#ffb020",
  toolLine: "rgba(255, 176, 32, 0.55)",
};

export class ToolpathView {
  private ctx: CanvasRenderingContext2D;
  private view: View = { pxPerMm: 4, zCenter: -40, xCenter: 0 };
  private backplot: Backplot | null = null;
  private stock: StockSpec | null = null;
  private dro: Dro | null = null;
  private currentLine = -1;
  private executedThrough = -1; // highest fully-executed source line
  private dragging = false;
  private lastPointer = { x: 0, y: 0 };
  private needFit = true;

  constructor(private canvas: HTMLCanvasElement) {
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no 2d context");
    this.ctx = ctx;

    canvas.addEventListener("pointerdown", (e) => {
      this.dragging = true;
      this.lastPointer = { x: e.offsetX, y: e.offsetY };
      canvas.setPointerCapture(e.pointerId);
    });
    canvas.addEventListener("pointermove", (e) => {
      if (!this.dragging) return;
      const dx = e.offsetX - this.lastPointer.x;
      const dy = e.offsetY - this.lastPointer.y;
      this.lastPointer = { x: e.offsetX, y: e.offsetY };
      this.view = {
        ...this.view,
        zCenter: this.view.zCenter - dx / this.view.pxPerMm,
        xCenter: this.view.xCenter + dy / this.view.pxPerMm,
      };
    });
    const endDrag = () => (this.dragging = false);
    canvas.addEventListener("pointerup", endDrag);
    canvas.addEventListener("pointercancel", endDrag);
    canvas.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const factor = Math.pow(1.0015, -e.deltaY);
        const { w, h } = this.cssSize();
        this.view = zoomAt(this.view, factor, e.offsetX, e.offsetY, w, h);
      },
      { passive: false },
    );
    canvas.addEventListener("dblclick", () => this.fit());
  }

  private cssSize(): { w: number; h: number } {
    return { w: this.canvas.clientWidth, h: this.canvas.clientHeight };
  }

  setBackplot(bp: Backplot | null, stock: StockSpec | null): void {
    this.backplot = bp;
    this.stock = stock;
    this.executedThrough = -1;
    this.needFit = true;
  }

  resetProgress(): void {
    this.executedThrough = -1;
  }

  update(dro: Dro | null, currentLine: number, running: boolean): void {
    this.dro = dro;
    this.currentLine = currentLine;
    if (running && currentLine >= 0) {
      this.executedThrough = Math.max(this.executedThrough, currentLine - 1);
    } else if (!running && currentLine < 0 && this.executedThrough >= 0 && this.backplot) {
      // Program finished: mark everything executed.
      const maxLine = this.backplot.segments.reduce((a, s) => Math.max(a, s.line), -1);
      if (this.executedThrough >= maxLine - 1) this.executedThrough = maxLine;
    }
  }

  fit(): void {
    const { w, h } = this.cssSize();
    if (w === 0 || h === 0) return;
    const bounds = this.backplot?.bounds ?? {
      xMin: 0,
      xMax: this.stock ? this.stock.diameterMm / 2 : 25,
      zMin: this.stock ? this.stock.zFaceMm - this.stock.lengthMm : -80,
      zMax: this.stock ? this.stock.zFaceMm : 5,
    };
    this.view = fitView(bounds, w, h);
    this.needFit = false;
  }

  /** Draw one frame. Call from the rAF loop. */
  render(): void {
    const dpr = window.devicePixelRatio || 1;
    const { w, h } = this.cssSize();
    if (w === 0 || h === 0) return;
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
    this.drawStock(ctx, w, h);
    this.drawCentreline(ctx, w, h);
    if (this.backplot) {
      this.drawSegments(ctx, w, h, true); // mirrored, faint
      this.drawSegments(ctx, w, h, false);
    }
    this.drawTool(ctx, w, h);
    this.drawScale(ctx, w, h);
    ctx.restore();
  }

  private toScreen(z: number, x: number, w: number, h: number) {
    return worldToScreen(z, x, this.view, w, h);
  }

  private drawGrid(ctx: CanvasRenderingContext2D, w: number, h: number): void {
    const pitch = gridPitchMm(this.view.pxPerMm);
    const tl = screenToWorld(0, 0, this.view, w, h);
    const br = screenToWorld(w, h, this.view, w, h);
    ctx.strokeStyle = COLORS.grid;
    ctx.lineWidth = 1;
    ctx.fillStyle = COLORS.gridLabel;
    ctx.font = "10px ui-monospace, monospace";
    ctx.beginPath();
    for (let z = Math.ceil(tl.z / pitch) * pitch; z <= br.z; z += pitch) {
      const { sx } = this.toScreen(z, 0, w, h);
      ctx.moveTo(sx, 0);
      ctx.lineTo(sx, h);
    }
    for (let x = Math.ceil(br.x / pitch) * pitch; x <= tl.x; x += pitch) {
      const { sy } = this.toScreen(0, x, w, h);
      ctx.moveTo(0, sy);
      ctx.lineTo(w, sy);
    }
    ctx.stroke();
    for (let z = Math.ceil(tl.z / pitch) * pitch; z <= br.z; z += pitch) {
      const { sx } = this.toScreen(z, 0, w, h);
      ctx.fillText(`Z${fmt(z)}`, sx + 3, h - 6);
    }
  }

  private drawStock(ctx: CanvasRenderingContext2D, w: number, h: number): void {
    if (!this.stock) return;
    const r = this.stock.diameterMm / 2;
    const zFace = this.stock.zFaceMm;
    const zBack = zFace - this.stock.lengthMm;
    const a = this.toScreen(zBack, r, w, h);
    const b = this.toScreen(zFace, -r, w, h);
    ctx.fillStyle = COLORS.stockFill;
    ctx.strokeStyle = COLORS.stockEdge;
    ctx.lineWidth = 1;
    ctx.fillRect(a.sx, a.sy, b.sx - a.sx, b.sy - a.sy);
    ctx.strokeRect(a.sx, a.sy, b.sx - a.sx, b.sy - a.sy);

    // Chuck: hatched block just behind the stock (more negative z).
    const chuckW = Math.max(18, this.view.pxPerMm * 8);
    ctx.fillStyle = COLORS.chuck;
    const c0 = this.toScreen(zBack, r * 1.35, w, h);
    const c1 = this.toScreen(zBack, -r * 1.35, w, h);
    ctx.fillRect(c0.sx - chuckW, c0.sy, chuckW, c1.sy - c0.sy);
    ctx.strokeStyle = COLORS.chuckEdge;
    ctx.strokeRect(c0.sx - chuckW, c0.sy, chuckW, c1.sy - c0.sy);
    ctx.save();
    ctx.beginPath();
    ctx.rect(c0.sx - chuckW, c0.sy, chuckW, c1.sy - c0.sy);
    ctx.clip();
    ctx.strokeStyle = COLORS.chuckEdge;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let i = -1; i < (c1.sy - c0.sy) / 6 + 4; i++) {
      ctx.moveTo(c0.sx - chuckW, c0.sy + i * 6);
      ctx.lineTo(c0.sx, c0.sy + i * 6 - chuckW);
    }
    ctx.stroke();
    ctx.restore();
    ctx.fillStyle = COLORS.gridLabel;
    ctx.font = "10px ui-monospace, monospace";
    ctx.fillText("CHUCK", c0.sx - chuckW + 2, c0.sy - 4);
  }

  private drawCentreline(ctx: CanvasRenderingContext2D, w: number, h: number): void {
    const { sy } = this.toScreen(0, 0, w, h);
    if (sy < -2 || sy > h + 2) return;
    ctx.strokeStyle = COLORS.centreline;
    ctx.lineWidth = 1;
    ctx.setLineDash([14, 4, 3, 4]); // dash-dot: machinist's centreline
    ctx.beginPath();
    ctx.moveTo(0, sy);
    ctx.lineTo(w, sy);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = COLORS.gridLabel;
    ctx.font = "10px ui-monospace, monospace";
    ctx.fillText("℄", w - 16, sy - 4); // ℄ centreline symbol
  }

  private segmentDone(seg: Segment): boolean {
    return seg.line <= this.executedThrough;
  }

  private drawSegments(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    mirrored: boolean,
  ): void {
    if (!this.backplot) return;
    ctx.save();
    if (mirrored) ctx.globalAlpha = COLORS.mirrorAlpha;
    const flip = mirrored ? -1 : 1;
    for (const seg of this.backplot.segments) {
      const done = this.segmentDone(seg);
      const active = seg.line === this.currentLine;
      let color: string;
      let dash: number[] = [];
      switch (seg.kind) {
        case "rapid":
          color = done ? COLORS.rapidDone : COLORS.rapidPending;
          dash = [5, 5];
          break;
        case "thread":
          color = done ? COLORS.threadDone : COLORS.threadPending;
          dash = [8, 2, 2, 2];
          break;
        default:
          color = done ? COLORS.feedDone : COLORS.feedPending;
      }
      ctx.strokeStyle = color;
      ctx.lineWidth = seg.kind === "rapid" ? 1 : active ? 2.5 : 1.75;
      ctx.setLineDash(dash);
      ctx.beginPath();
      if (seg.kind === "arc") {
        const pts = tessellateArc(seg, this.view.pxPerMm);
        pts.forEach((p, i) => {
          const s = this.toScreen(p.z, flip * p.x, w, h);
          if (i === 0) ctx.moveTo(s.sx, s.sy);
          else ctx.lineTo(s.sx, s.sy);
        });
      } else {
        const a = this.toScreen(seg.z0, flip * seg.x0, w, h);
        const b = this.toScreen(seg.z1, flip * seg.x1, w, h);
        ctx.moveTo(a.sx, a.sy);
        ctx.lineTo(b.sx, b.sy);
      }
      ctx.stroke();
    }
    ctx.setLineDash([]);
    ctx.restore();
  }

  private drawTool(ctx: CanvasRenderingContext2D, w: number, h: number): void {
    if (!this.dro) return;
    const p = this.toScreen(this.dro.zMm, this.dro.xRadiusMm, w, h);
    // Faint droplines to the axes.
    ctx.strokeStyle = COLORS.toolLine;
    ctx.lineWidth = 1;
    ctx.setLineDash([2, 4]);
    const cl = this.toScreen(this.dro.zMm, 0, w, h);
    ctx.beginPath();
    ctx.moveTo(p.sx, p.sy);
    ctx.lineTo(cl.sx, cl.sy);
    ctx.stroke();
    ctx.setLineDash([]);
    // Tool marker: point-down triangle like a turning tool tip.
    ctx.fillStyle = COLORS.tool;
    ctx.beginPath();
    ctx.moveTo(p.sx, p.sy);
    ctx.lineTo(p.sx + 6, p.sy - 10);
    ctx.lineTo(p.sx - 6, p.sy - 10);
    ctx.closePath();
    ctx.fill();
    ctx.strokeStyle = COLORS.bg;
    ctx.stroke();
  }

  private drawScale(ctx: CanvasRenderingContext2D, _w: number, h: number): void {
    const pitch = gridPitchMm(this.view.pxPerMm);
    const px = pitch * this.view.pxPerMm;
    const x0 = 12;
    const y0 = h - 14;
    ctx.strokeStyle = "#8aa0b4";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(x0, y0);
    ctx.lineTo(x0 + px, y0);
    ctx.moveTo(x0, y0 - 4);
    ctx.lineTo(x0, y0 + 4);
    ctx.moveTo(x0 + px, y0 - 4);
    ctx.lineTo(x0 + px, y0 + 4);
    ctx.stroke();
    ctx.fillStyle = "#8aa0b4";
    ctx.font = "11px ui-monospace, monospace";
    ctx.fillText(`${fmt(pitch)} mm`, x0 + 4, y0 - 6);
  }
}

function fmt(v: number): string {
  return Math.abs(v) >= 100 ? v.toFixed(0) : Math.abs(v) >= 1 ? String(+v.toFixed(1)) : String(+v.toFixed(2));
}
