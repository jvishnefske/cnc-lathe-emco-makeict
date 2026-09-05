/**
 * Connection bar: transport picker, port/serial settings, connect/disconnect,
 * link status, core badge.
 *
 * "Simulated machine" is the DEFAULT. The real-hardware path is experimental:
 * the PC Turn 50's rear DB9 (X300) is an RS-485 multidrop bus and its wire
 * protocol is not yet known. The UI says so rather than pretending.
 */

import type { SerialSettings } from "../transport/transport";
import { webSerialSupported } from "../transport/webSerial";

export type TransportChoice = "loopback" | "webserial";

export type ConnectionCallbacks = {
  onConnect: (choice: TransportChoice, settings: SerialSettings) => Promise<void>;
  onDisconnect: () => Promise<void>;
  onPickPort: () => Promise<string>;
  onInjectFault: (kind: string) => void;
};

export class ConnectionBar {
  readonly el: HTMLElement;
  private transportSel: HTMLSelectElement;
  private connectBtn: HTMLButtonElement;
  private portBtn: HTMLButtonElement;
  private statusDot: HTMLElement;
  private statusText: HTMLElement;
  private serialFields: HTMLElement;
  private warn: HTMLElement;
  private faultSel: HTMLSelectElement;
  private coreBadge: HTMLElement;
  private connected = false;
  private busy = false;

  constructor(
    private defaults: SerialSettings,
    private cb: ConnectionCallbacks,
  ) {
    this.el = document.createElement("header");
    this.el.className = "conn-bar";
    const serialOk = webSerialSupported();
    this.el.innerHTML = `
      <span class="app-name">EMCO PC Turn 50 · sender</span>
      <label>Transport
        <select data-e="transport">
          <option value="loopback" selected>Simulated machine</option>
          <option value="webserial" ${serialOk ? "" : "disabled"}>
            Web Serial — EXPERIMENTAL${serialOk ? "" : " (unavailable)"}</option>
        </select>
      </label>
      <span class="serial-fields" data-e="serial" hidden>
        <button data-e="port" class="btn">Pick port…</button>
        <label>Baud <select data-e="baud">
          ${[1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200]
            .map((b) => `<option ${b === defaults.baud ? "selected" : ""}>${b}</option>`)
            .join("")}
        </select></label>
        <label>Data <select data-e="databits">
          <option ${defaults.dataBits === 7 ? "selected" : ""}>7</option>
          <option ${defaults.dataBits === 8 ? "selected" : ""}>8</option>
        </select></label>
        <label>Parity <select data-e="parity">
          ${["none", "even", "odd"]
            .map((p) => `<option ${p === defaults.parity ? "selected" : ""}>${p}</option>`)
            .join("")}
        </select></label>
        <label>Stop <select data-e="stopbits">
          <option ${defaults.stopBits === 1 ? "selected" : ""}>1</option>
          <option ${defaults.stopBits === 2 ? "selected" : ""}>2</option>
        </select></label>
        <label>Flow <select data-e="flow">
          <option ${defaults.flowControl === "none" ? "selected" : ""}>none</option>
          <option ${defaults.flowControl === "hardware" ? "selected" : ""}>hardware</option>
        </select></label>
      </span>
      <label class="fault-field" data-e="faultwrap">Inject fault
        <select data-e="fault">
          <option value="">—</option>
          <option value="estop">E-stop</option>
          <option value="limit_x">X limit</option>
          <option value="limit_z">Z limit</option>
          <option value="turret_jam">Turret jam</option>
        </select>
      </label>
      <button data-e="connect" class="btn btn-primary">Connect</button>
      <span class="link-status"><span class="dot" data-e="dot"></span><span data-e="status">disconnected</span></span>
      <span class="core-badge" data-e="core">core: …</span>
      <div class="conn-warn" data-e="warn" hidden></div>`;

    const q = <T extends HTMLElement>(k: string) =>
      this.el.querySelector<T>(`[data-e="${k}"]`)!;
    this.transportSel = q<HTMLSelectElement>("transport");
    this.connectBtn = q<HTMLButtonElement>("connect");
    this.portBtn = q<HTMLButtonElement>("port");
    this.statusDot = q("dot");
    this.statusText = q("status");
    this.serialFields = q("serial");
    this.warn = q("warn");
    this.faultSel = q<HTMLSelectElement>("fault");
    this.coreBadge = q("core");

    this.transportSel.addEventListener("change", () => this.refreshTransportUi());
    this.portBtn.addEventListener("click", () => {
      void this.cb.onPickPort().then(
        (name) => (this.portBtn.textContent = name || "Port picked"),
        () => undefined,
      );
    });
    this.connectBtn.addEventListener("click", () => void this.toggle());
    this.faultSel.addEventListener("change", () => {
      if (this.faultSel.value) {
        this.cb.onInjectFault(this.faultSel.value);
        this.faultSel.value = "";
      }
    });
    this.refreshTransportUi();
  }

  private refreshTransportUi(): void {
    const choice = this.transportSel.value as TransportChoice;
    this.serialFields.hidden = choice !== "webserial";
    this.el.querySelector<HTMLElement>('[data-e="faultwrap"]')!.hidden = choice !== "loopback";
    if (choice === "webserial") {
      if (!webSerialSupported()) {
        this.showWarn(
          "Web Serial is not available here. It needs a Chromium browser (Chrome/Edge) " +
            "on localhost or HTTPS. Use the simulated machine instead.",
        );
      } else {
        this.showWarn(
          "EXPERIMENTAL — the PC Turn 50's rear DB9 (X300) is an RS-485 bus " +
            "(pin 2 GND, 3 DATA−, 8 DATA+) and its wire protocol is not yet known. " +
            "Use a half-duplex USB-RS485 (A/B) adapter ONLY. A USB-RS232 cable is " +
            "electrically wrong and can damage the control. This mode exists to capture " +
            "traffic for protocol discovery, not to run parts.",
        );
      }
    } else {
      this.hideWarn();
    }
  }

  private showWarn(text: string): void {
    this.warn.textContent = text;
    this.warn.hidden = false;
  }

  private hideWarn(): void {
    this.warn.hidden = true;
  }

  private settings(): SerialSettings {
    const q = <T extends HTMLElement>(k: string) =>
      this.el.querySelector<T>(`[data-e="${k}"]`)!;
    return {
      baud: parseInt(q<HTMLSelectElement>("baud").value, 10),
      dataBits: parseInt(q<HTMLSelectElement>("databits").value, 10) as 7 | 8,
      stopBits: parseInt(q<HTMLSelectElement>("stopbits").value, 10) as 1 | 2,
      parity: q<HTMLSelectElement>("parity").value as SerialSettings["parity"],
      flowControl: q<HTMLSelectElement>("flow").value as SerialSettings["flowControl"],
    };
  }

  private async toggle(): Promise<void> {
    if (this.busy) return;
    this.busy = true;
    this.connectBtn.disabled = true;
    try {
      if (this.connected) {
        await this.cb.onDisconnect();
        this.setConnected(false);
      } else {
        const choice = this.transportSel.value as TransportChoice;
        await this.cb.onConnect(choice, this.settings());
        this.setConnected(true, choice);
      }
    } catch (err) {
      this.showWarn(err instanceof Error ? err.message : String(err));
    } finally {
      this.busy = false;
      this.connectBtn.disabled = false;
    }
  }

  setConnected(connected: boolean, choice?: TransportChoice): void {
    this.connected = connected;
    this.connectBtn.textContent = connected ? "Disconnect" : "Connect";
    this.connectBtn.classList.toggle("btn-primary", !connected);
    this.statusDot.classList.toggle("on", connected);
    this.statusText.textContent = connected
      ? choice === "webserial"
        ? "linked (serial)"
        : "linked (simulated)"
      : "disconnected";
    this.transportSel.disabled = connected;
  }

  setCoreNote(note: string): void {
    this.coreBadge.textContent = `core: ${note}`;
  }

  get transport(): TransportChoice {
    return this.transportSel.value as TransportChoice;
  }
}
