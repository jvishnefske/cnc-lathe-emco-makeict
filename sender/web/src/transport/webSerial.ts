/**
 * WebSerialTransport — real hardware over the Web Serial API.
 * Chromium-only, and only on localhost or HTTPS; callers must check
 * webSerialSupported() and explain, not fail silently.
 */

import type { SerialSettings, Transport } from "./transport";
import { concatBytes } from "./transport";

export function webSerialSupported(): boolean {
  return typeof navigator !== "undefined" && !!navigator.serial;
}

export class WebSerialTransport implements Transport {
  readonly kind = "webserial" as const;
  private port: SerialPort | null = null;
  private reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  private writer: WritableStreamDefaultWriter<Uint8Array> | null = null;
  private rxChunks: Uint8Array[] = [];
  private _connected = false;
  private closing = false;
  onDisconnect?: () => void;

  get connected(): boolean {
    return this._connected;
  }

  /** Human-readable name of the picked port, for the connection bar. */
  get portLabel(): string {
    if (!this.port) return "";
    const info = this.port.getInfo();
    if (info.usbVendorId !== undefined) {
      const v = info.usbVendorId.toString(16).padStart(4, "0");
      const p = (info.usbProductId ?? 0).toString(16).padStart(4, "0");
      return `USB ${v}:${p}`;
    }
    return "Port picked";
  }

  /** Must be called from a user gesture; shows the browser's port picker. */
  async pickPort(): Promise<void> {
    if (!navigator.serial) {
      throw new Error("Web Serial is not available in this browser");
    }
    this.port = await navigator.serial.requestPort();
  }

  async connect(settings: SerialSettings): Promise<void> {
    if (!navigator.serial) {
      throw new Error(
        "Web Serial is not available. Use Chrome/Edge/Chromium on localhost or HTTPS, " +
          "or switch to the simulated machine.",
      );
    }
    if (!this.port) {
      const granted = await navigator.serial.getPorts();
      if (granted.length === 1) this.port = granted[0]!;
      else await this.pickPort();
    }
    const port = this.port!;
    await port.open({
      baudRate: settings.baud,
      dataBits: settings.dataBits,
      stopBits: settings.stopBits,
      parity: settings.parity,
      flowControl: settings.flowControl,
    });
    this._connected = true;
    this.closing = false;
    this.writer = port.writable!.getWriter();
    void this.readLoop(port);
    port.addEventListener("disconnect", this.handleUnplug);
  }

  private handleUnplug = (): void => {
    this._connected = false;
    this.onDisconnect?.();
  };

  private async readLoop(port: SerialPort): Promise<void> {
    while (port.readable && !this.closing) {
      this.reader = port.readable.getReader();
      try {
        for (;;) {
          const { value, done } = await this.reader.read();
          if (done) break;
          if (value && value.length > 0) this.rxChunks.push(value);
        }
      } catch {
        break; // fatal stream error (device gone)
      } finally {
        this.reader.releaseLock();
        this.reader = null;
      }
    }
    if (!this.closing && this._connected) {
      this._connected = false;
      this.onDisconnect?.();
    }
  }

  async disconnect(): Promise<void> {
    this.closing = true;
    this._connected = false;
    try {
      await this.reader?.cancel();
    } catch {
      /* already gone */
    }
    try {
      this.writer?.releaseLock();
    } catch {
      /* already gone */
    }
    this.writer = null;
    if (this.port) {
      this.port.removeEventListener("disconnect", this.handleUnplug);
      try {
        await this.port.close();
      } catch {
        /* already gone */
      }
    }
  }

  send(bytes: Uint8Array): void {
    if (!this._connected || !this.writer || bytes.length === 0) return;
    void this.writer.write(bytes).catch(() => {
      this._connected = false;
      this.onDisconnect?.();
    });
  }

  drain(_nowMs: number): Uint8Array {
    return concatBytes(this.rxChunks.splice(0));
  }
}
