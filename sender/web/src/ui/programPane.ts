/**
 * Program pane: load via file picker or drag-drop, line numbers, current line
 * highlighted and auto-scrolled, diagnostics inline on the offending line.
 */

import type { Diagnostic } from "../core/types";

export type ProgramCallbacks = {
  onProgramText: (name: string, text: string) => void;
  onStockChange: (diameterMm: number, lengthMm: number) => void;
};

export class ProgramPane {
  readonly el: HTMLElement;
  private listEl: HTMLElement;
  private nameEl: HTMLElement;
  private currentLine = -1;
  private lineEls: HTMLElement[] = [];

  constructor(stock: { diameterMm: number; lengthMm: number }, private cb: ProgramCallbacks) {
    this.el = document.createElement("section");
    this.el.className = "panel program-pane";
    this.el.innerHTML = `
      <header class="panel-title">Program
        <span class="program-name" data-e="name">none</span>
        <button class="btn btn-small" data-e="open">Open…</button>
        <input type="file" data-e="file" accept=".nc,.ngc,.gcode,.tap,.txt,.cnc,.min" hidden>
      </header>
      <div class="stock-row">
        stock ⌀<input data-e="dia" type="number" min="1.5" max="14" step="0.5" value="${stock.diameterMm}"> ×
        <input data-e="len" type="number" min="5" max="200" step="5" value="${stock.lengthMm}"> mm
      </div>
      <div class="program-drop" data-e="drop">
        <ol class="program-lines" data-e="lines"><li class="program-empty">Drop a G-code file here, or Open…</li></ol>
      </div>`;
    const q = <T extends HTMLElement>(k: string) => this.el.querySelector<T>(`[data-e="${k}"]`)!;
    this.listEl = q("lines");
    this.nameEl = q("name");
    const fileInput = q<HTMLInputElement>("file");
    q("open").addEventListener("click", () => fileInput.click());
    fileInput.addEventListener("change", () => {
      const f = fileInput.files?.[0];
      if (f) void this.readFile(f);
      fileInput.value = "";
    });
    const drop = q("drop");
    drop.addEventListener("dragover", (e) => {
      e.preventDefault();
      drop.classList.add("dragging");
    });
    drop.addEventListener("dragleave", () => drop.classList.remove("dragging"));
    drop.addEventListener("drop", (e) => {
      e.preventDefault();
      drop.classList.remove("dragging");
      const f = e.dataTransfer?.files?.[0];
      if (f) void this.readFile(f);
    });
    const diaEl = q<HTMLInputElement>("dia");
    const lenEl = q<HTMLInputElement>("len");
    const stockChanged = () =>
      this.cb.onStockChange(parseFloat(diaEl.value) || 14, parseFloat(lenEl.value) || 60);
    diaEl.addEventListener("change", stockChanged);
    lenEl.addEventListener("change", stockChanged);
  }

  private async readFile(f: File): Promise<void> {
    const text = await f.text();
    this.cb.onProgramText(f.name, text);
  }

  setProgram(name: string, lines: string[], diagnostics: Diagnostic[]): void {
    this.nameEl.textContent = name;
    this.listEl.innerHTML = "";
    this.lineEls = [];
    this.currentLine = -1;
    const byLine = new Map<number, Diagnostic[]>();
    for (const d of diagnostics) {
      const arr = byLine.get(d.line) ?? [];
      arr.push(d);
      byLine.set(d.line, arr);
    }
    lines.forEach((text, i) => {
      const li = document.createElement("li");
      li.className = "program-line";
      const num = document.createElement("span");
      num.className = "line-num";
      num.textContent = String(i + 1);
      const body = document.createElement("span");
      body.className = "line-text";
      body.textContent = text || " ";
      li.append(num, body);
      const diags = byLine.get(i);
      if (diags) {
        li.classList.add(diags.some((d) => d.severity === "error") ? "has-error" : "has-warning");
        for (const d of diags) {
          const dEl = document.createElement("span");
          dEl.className = `line-diag diag-${d.severity}`;
          dEl.textContent = `${d.severity === "error" ? "✕" : "▲"} ${d.message}`;
          li.appendChild(dEl);
        }
      }
      this.listEl.appendChild(li);
      this.lineEls.push(li);
    });
  }

  setCurrentLine(line: number): void {
    if (line === this.currentLine) return;
    const prev = this.lineEls[this.currentLine];
    if (prev) prev.classList.remove("current");
    this.currentLine = line;
    const cur = this.lineEls[line];
    if (cur) {
      cur.classList.add("current");
      cur.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
  }
}
