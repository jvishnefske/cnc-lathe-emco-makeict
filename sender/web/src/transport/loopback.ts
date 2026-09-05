/**
 * LoopbackTransport — wires the session to the core's VirtualMachine instead
 * of a serial port. This is "Simulated machine" mode: the whole protocol
 * conversation happens in-process, so the app works in any browser with no
 * hardware attached. Also the training mode.
 */

import type { VirtualMachineApi } from "../core/api";
import type { SerialSettings, Transport } from "./transport";
import { concatBytes } from "./transport";

export class LoopbackTransport implements Transport {
  readonly kind = "loopback" as const;
  private _connected = false;
  private toMachine: Uint8Array[] = [];
  onDisconnect?: () => void;

  constructor(private vm: VirtualMachineApi) {}

  get connected(): boolean {
    return this._connected;
  }

  async connect(_settings: SerialSettings): Promise<void> {
    this._connected = true;
  }

  async disconnect(): Promise<void> {
    this._connected = false;
  }

  send(bytes: Uint8Array): void {
    if (!this._connected || bytes.length === 0) return;
    this.toMachine.push(bytes);
  }

  drain(nowMs: number): Uint8Array {
    if (!this._connected) return new Uint8Array(0);
    const tx = concatBytes(this.toMachine.splice(0));
    return this.vm.step(nowMs, tx);
  }

  /** Expose the VM for fault-injection UI (training scenarios). */
  get machine(): VirtualMachineApi {
    return this.vm;
  }
}
