/** Machine state: run state, turret, spindle, feed, modal codes, alarms. */

import type { SessionState } from "../core/types";
import { RUN_STATE_CLASS, RUN_STATE_LABEL } from "./format";
import { formatFeed } from "../units";

export class StatePanel {
  readonly el: HTMLElement;
  private badge: HTMLElement;
  private tool: HTMLElement;
  private spindle: HTMLElement;
  private feed: HTMLElement;
  private modal: HTMLElement;
  private alarms: HTMLElement;
  private flags: HTMLElement;

  constructor() {
    this.el = document.createElement("section");
    this.el.className = "panel state-panel";
    this.el.innerHTML = `
      <header class="panel-title">Machine</header>
      <div class="state-badge st-off" data-e="badge">DISCONNECTED</div>
      <dl class="state-list">
        <dt>Turret</dt><dd data-e="tool">—</dd>
        <dt>Spindle</dt><dd data-e="spindle">off</dd>
        <dt>Feed</dt><dd data-e="feed">—</dd>
        <dt>Modal</dt><dd class="modal-codes" data-e="modal">—</dd>
        <dt>Flags</dt><dd data-e="flags">—</dd>
      </dl>
      <ul class="alarm-list" data-e="alarms"></ul>`;
    const q = (k: string) => this.el.querySelector<HTMLElement>(`[data-e="${k}"]`)!;
    this.badge = q("badge");
    this.tool = q("tool");
    this.spindle = q("spindle");
    this.feed = q("feed");
    this.modal = q("modal");
    this.alarms = q("alarms");
    this.flags = q("flags");
  }

  update(s: SessionState): void {
    const label = RUN_STATE_LABEL[s.run];
    if (this.badge.textContent !== label) {
      this.badge.textContent = label;
      this.badge.className = `state-badge ${RUN_STATE_CLASS[s.run]}`;
    }
    setText(this.tool, s.tool > 0 ? `T${s.tool}` : "—");
    setText(
      this.spindle,
      s.spindleOn ? `ON · ${s.spindleRpm.toFixed(0)} rpm` : "off",
    );
    setText(this.feed, s.feedMmMin > 0 ? formatFeed(s.feedMmMin) : "—");
    setText(this.modal, s.modal.length > 0 ? s.modal.join(" ") : "—");
    const flags = [s.singleBlock ? "SINGLE BLOCK" : "", s.dryRun ? "DRY RUN" : ""]
      .filter(Boolean)
      .join(" · ");
    setText(this.flags, flags || "—");

    const alarmText = s.alarms.map((a) => `${a.code}: ${a.message}`).join("\n");
    if (this.alarms.dataset.prev !== alarmText) {
      this.alarms.dataset.prev = alarmText;
      this.alarms.innerHTML = "";
      for (const a of s.alarms) {
        const li = document.createElement("li");
        li.textContent = `${a.code} — ${a.message}`;
        this.alarms.appendChild(li);
      }
    }
  }
}

function setText(el: HTMLElement, text: string): void {
  if (el.textContent !== text) el.textContent = text;
}
