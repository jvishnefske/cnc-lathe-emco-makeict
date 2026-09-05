/**
 * Operator controls: Run / Feed Hold / Resume / Single Block / Dry Run,
 * Home (reference), X/Z jog with step selector, MDI line, and a large
 * always-visible E-STOP.
 *
 * The E-STOP here sends whatever stop the dialect offers and drops motion —
 * it is ADVISORY, not a safety function, and the panel says so in plain
 * text. The real stop is the mains switch with a person at the machine.
 */

import type { Command, SessionState } from "../core/types";
import { controlAvailability } from "./format";

export type ControlsCallbacks = {
  onCommand: (cmd: Command) => void;
};

const JOG_STEPS_MM = [0.01, 0.1, 1, 10] as const;
const JOG_FEED_MM_MIN = 300;

export class ControlsPanel {
  readonly el: HTMLElement;
  private buttons: Record<string, HTMLButtonElement> = {};
  private stepSel: HTMLSelectElement;
  private mdiInput: HTMLInputElement;
  private singleBlockCb: HTMLInputElement;
  private dryRunCb: HTMLInputElement;

  constructor(private cb: ControlsCallbacks) {
    this.el = document.createElement("section");
    this.el.className = "panel controls";
    this.el.innerHTML = `
      <div class="estop-wrap">
        <button class="estop" data-b="estop" title="Software stop — advisory only">E-STOP</button>
        <p class="estop-note">Software stop over the serial link — <strong>advisory, not a
        safety function</strong>. The real stop is the mains switch on the control box,
        with a person standing at the machine.</p>
      </div>
      <div class="run-row">
        <button class="btn btn-run" data-b="run">▶ Run</button>
        <button class="btn" data-b="hold">⏸ Hold</button>
        <button class="btn" data-b="resume">⏵ Resume</button>
        <button class="btn btn-home" data-b="home">⌂ Home / Reference</button>
      </div>
      <div class="mode-row">
        <label class="toggle"><input type="checkbox" data-b="single"> Single block</label>
        <label class="toggle"><input type="checkbox" data-b="dry"> Dry run (no bytes sent)</label>
      </div>
      <div class="jog-block">
        <div class="jog-grid">
          <span></span>
          <button class="btn jog" data-b="jog-x-plus" title="X+ (larger diameter)">X+</button>
          <span></span>
          <button class="btn jog" data-b="jog-z-minus" title="Z− (toward chuck)">Z−</button>
          <span class="jog-centre">jog</span>
          <button class="btn jog" data-b="jog-z-plus" title="Z+ (toward tailstock)">Z+</button>
          <span></span>
          <button class="btn jog" data-b="jog-x-minus" title="X− (toward centreline)">X−</button>
          <span></span>
        </div>
        <label class="jog-step">step
          <select data-b="step">
            ${JOG_STEPS_MM.map((s) => `<option value="${s}" ${s === 1 ? "selected" : ""}>${s} mm</option>`).join("")}
          </select>
        </label>
      </div>
      <div class="mdi-row">
        <label class="mdi-label">MDI</label>
        <input class="mdi-input" data-b="mdi" type="text" spellcheck="false"
               placeholder="G1 X10 Z-5 F100 ⏎">
      </div>`;

    const q = <T extends HTMLElement>(k: string) => this.el.querySelector<T>(`[data-b="${k}"]`)!;
    for (const k of ["estop", "run", "hold", "resume", "home",
      "jog-x-plus", "jog-x-minus", "jog-z-plus", "jog-z-minus"]) {
      this.buttons[k] = q<HTMLButtonElement>(k);
    }
    this.stepSel = q<HTMLSelectElement>("step");
    this.mdiInput = q<HTMLInputElement>("mdi");
    this.singleBlockCb = q<HTMLInputElement>("single");
    this.dryRunCb = q<HTMLInputElement>("dry");

    this.buttons["estop"]!.addEventListener("click", () => cb.onCommand({ kind: "abort" }));
    this.buttons["run"]!.addEventListener("click", () => cb.onCommand({ kind: "run" }));
    this.buttons["hold"]!.addEventListener("click", () => cb.onCommand({ kind: "hold" }));
    this.buttons["resume"]!.addEventListener("click", () => cb.onCommand({ kind: "resume" }));
    this.buttons["home"]!.addEventListener("click", () => cb.onCommand({ kind: "home" }));
    this.singleBlockCb.addEventListener("change", () =>
      cb.onCommand({ kind: "singleBlock", on: this.singleBlockCb.checked }));
    this.dryRunCb.addEventListener("change", () =>
      cb.onCommand({ kind: "dryRun", on: this.dryRunCb.checked }));

    const jog = (axis: "x" | "z", sign: 1 | -1) => {
      const step = parseFloat(this.stepSel.value);
      this.cb.onCommand({
        kind: "jog",
        axis,
        deltaMm: sign * step,
        feedMmMin: JOG_FEED_MM_MIN,
      });
    };
    this.buttons["jog-x-plus"]!.addEventListener("click", () => jog("x", 1));
    this.buttons["jog-x-minus"]!.addEventListener("click", () => jog("x", -1));
    this.buttons["jog-z-plus"]!.addEventListener("click", () => jog("z", 1));
    this.buttons["jog-z-minus"]!.addEventListener("click", () => jog("z", -1));

    this.mdiInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        const text = this.mdiInput.value.trim();
        if (text !== "") {
          this.cb.onCommand({ kind: "mdi", text });
          this.mdiInput.value = "";
        }
      }
    });
  }

  /** Enable/disable buttons to match the session state. E-STOP is never disabled. */
  update(s: SessionState, connected: boolean, hasProgram: boolean): void {
    const a = controlAvailability(s, connected, hasProgram);
    this.setEnabled("run", a.run);
    this.setEnabled("hold", a.hold);
    this.setEnabled("resume", a.resume);
    this.setEnabled("home", a.home);
    for (const k of ["jog-x-plus", "jog-x-minus", "jog-z-plus", "jog-z-minus"]) {
      this.setEnabled(k, a.jog);
    }
    this.mdiInput.disabled = !a.mdi;
    if (this.singleBlockCb.checked !== s.singleBlock) this.singleBlockCb.checked = s.singleBlock;
    if (this.dryRunCb.checked !== s.dryRun) this.dryRunCb.checked = s.dryRun;
    this.buttons["home"]!.classList.toggle("home-needed", a.home && !s.dro.homed);
  }

  private setEnabled(key: string, on: boolean): void {
    const b = this.buttons[key];
    if (b && b.disabled === on) b.disabled = !on;
  }
}
