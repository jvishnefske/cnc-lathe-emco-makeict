/** DRO — big, monospace, readable from a step back. mm primary, inch alongside. */

import type { Dro } from "../core/types";
import { formatInch, formatMm } from "../units";

export class DroPanel {
  readonly el: HTMLElement;
  private fields: Record<string, HTMLElement> = {};

  constructor() {
    this.el = document.createElement("section");
    this.el.className = "panel dro";
    this.el.innerHTML = `
      <header class="panel-title">DRO <span class="dro-homed" data-f="homed">NOT REFERENCED</span></header>
      <div class="dro-grid">
        <div class="dro-axis dro-primary">
          <span class="dro-label">X⌀</span>
          <span class="dro-value" data-f="xdia">0.000</span>
          <span class="dro-inch" data-f="xdia-in">0.0000"</span>
        </div>
        <div class="dro-axis">
          <span class="dro-label">Xr</span>
          <span class="dro-value dro-small" data-f="xrad">0.000</span>
          <span class="dro-inch" data-f="xrad-in">0.0000"</span>
        </div>
        <div class="dro-axis dro-primary">
          <span class="dro-label">Z</span>
          <span class="dro-value" data-f="z">0.000</span>
          <span class="dro-inch" data-f="z-in">0.0000"</span>
        </div>
      </div>
      <div class="dro-sub">
        <div><span class="dro-subtitle">work</span>
          X⌀ <span data-f="wx">0.000</span> · Z <span data-f="wz">0.000</span></div>
        <div><span class="dro-subtitle">dtg</span>
          X <span data-f="dtgx">0.000</span> · Z <span data-f="dtgz">0.000</span></div>
      </div>`;
    this.el.querySelectorAll<HTMLElement>("[data-f]").forEach((n) => {
      this.fields[n.dataset.f!] = n;
    });
  }

  private set(f: string, text: string): void {
    const n = this.fields[f];
    if (n && n.textContent !== text) n.textContent = text;
  }

  update(dro: Dro): void {
    this.set("xdia", formatMm(dro.xDiameterMm));
    this.set("xdia-in", formatInch(dro.xDiameterMm) + '"');
    this.set("xrad", formatMm(dro.xRadiusMm));
    this.set("xrad-in", formatInch(dro.xRadiusMm) + '"');
    this.set("z", formatMm(dro.zMm));
    this.set("z-in", formatInch(dro.zMm) + '"');
    this.set("wx", formatMm(dro.xWorkMm * 2));
    this.set("wz", formatMm(dro.zWorkMm));
    this.set("dtgx", formatMm(dro.xDtgMm));
    this.set("dtgz", formatMm(dro.zDtgMm));
    const homedEl = this.fields["homed"];
    if (homedEl) {
      const t = dro.homed ? "REFERENCED" : "NOT REFERENCED";
      if (homedEl.textContent !== t) homedEl.textContent = t;
      homedEl.classList.toggle("ok", dro.homed);
    }
  }
}
