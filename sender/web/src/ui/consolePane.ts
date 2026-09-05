/**
 * Serial console: timestamped tx/rx transcript, hex or ASCII, exportable.
 * This is the capture tool for reverse-engineering the X300 RS-485 protocol,
 * so the export contains exact timestamps, direction, raw hex AND ascii.
 */

import { consoleExportText, formatConsoleLine, type ConsoleEntry } from "./format";

const MAX_RENDERED = 500;

export class ConsolePane {
  readonly el: HTMLElement;
  private listEl: HTMLElement;
  private entries: ConsoleEntry[] = [];
  private mode: "ascii" | "hex" = "ascii";
  private autoScroll = true;
  private dirty = false;

  constructor() {
    this.el = document.createElement("section");
    this.el.className = "panel console-pane";
    this.el.innerHTML = `
      <header class="panel-title">Serial console
        <span class="console-tools">
          <label class="toggle"><input type="checkbox" data-e="hex"> hex</label>
          <label class="toggle"><input type="checkbox" data-e="scroll" checked> follow</label>
          <button class="btn btn-small" data-e="export">Export capture</button>
          <button class="btn btn-small" data-e="clear">Clear</button>
        </span>
      </header>
      <pre class="console-list" data-e="list"></pre>`;
    const q = <T extends HTMLElement>(k: string) => this.el.querySelector<T>(`[data-e="${k}"]`)!;
    this.listEl = q("list");
    q<HTMLInputElement>("hex").addEventListener("change", (e) => {
      this.mode = (e.target as HTMLInputElement).checked ? "hex" : "ascii";
      this.rerender();
    });
    q<HTMLInputElement>("scroll").addEventListener("change", (e) => {
      this.autoScroll = (e.target as HTMLInputElement).checked;
    });
    q("export").addEventListener("click", () => this.export());
    q("clear").addEventListener("click", () => {
      this.entries = [];
      this.rerender();
    });
  }

  push(entry: ConsoleEntry): void {
    this.entries.push(entry);
    this.dirty = true;
  }

  note(atMs: number, text: string): void {
    this.push({ atMs, kind: "note", text });
  }

  /** Called once per frame; only touches the DOM when something changed. */
  flush(): void {
    if (!this.dirty) return;
    this.dirty = false;
    this.rerender();
  }

  private rerender(): void {
    const shown = this.entries.slice(-MAX_RENDERED);
    this.listEl.textContent = shown.map((e) => formatConsoleLine(e, this.mode)).join("\n");
    if (this.autoScroll) this.listEl.scrollTop = this.listEl.scrollHeight;
  }

  private export(): void {
    const stamp = new Date().toISOString().replace(/[:.]/g, "-");
    const header =
      `EMCO PC Turn 50 serial capture — exported ${new Date().toISOString()} — ` +
      `timestamps are seconds since page load (performance.now)`;
    const text = consoleExportText(this.entries, header);
    const blob = new Blob([text], { type: "text/plain" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `emco-capture-${stamp}.txt`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  }
}
