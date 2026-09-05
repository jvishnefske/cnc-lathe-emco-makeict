/**
 * Transport — the one interface between the app and any byte pipe.
 * Implementations: WebSerialTransport (real hardware over Web Serial) and
 * LoopbackTransport (the core's VirtualMachine, no hardware).
 */

import type { MachineProfile } from "../core/types";

export type SerialSettings = MachineProfile["serial"];

export interface Transport {
  readonly kind: "webserial" | "loopback";
  readonly connected: boolean;
  /** Open the link. May prompt the user (Web Serial port picker). */
  connect(settings: SerialSettings): Promise<void>;
  disconnect(): Promise<void>;
  /** Queue bytes for the machine. Fire and forget. */
  send(bytes: Uint8Array): void;
  /**
   * Bytes received since the last drain. Called once per animation frame;
   * the loopback transport also advances its VirtualMachine here.
   */
  drain(nowMs: number): Uint8Array;
  /** Invoked when the link drops out from under us (cable pulled, etc.). */
  onDisconnect?: () => void;
}

/** Concatenate byte chunks. */
export function concatBytes(chunks: Uint8Array[]): Uint8Array {
  if (chunks.length === 0) return new Uint8Array(0);
  if (chunks.length === 1) return chunks[0]!;
  let total = 0;
  for (const c of chunks) total += c.length;
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.length;
  }
  return out;
}
