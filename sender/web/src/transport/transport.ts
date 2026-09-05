/**
 * Transport — the one interface between the app and any byte pipe.
 * Implementations: WebSerialTransport (real hardware over Web Serial) and
 * LoopbackTransport (the core's VirtualMachine, no hardware).
 */

/**
 * UART settings for a USB-RS485 adapter. These are a transport-side concern:
 * the machine profile only records that the link is RS-485 with an unknown
 * framing (MachineProfile.link), so the concrete UART parameters live here
 * and are adjustable in the connection bar for protocol discovery.
 */
export type SerialSettings = {
  baud: number;
  dataBits: 7 | 8;
  stopBits: 1 | 2;
  parity: "none" | "even" | "odd";
  flowControl: "none" | "hardware";
};

export const DEFAULT_SERIAL_SETTINGS: SerialSettings = {
  baud: 9600, // UNVERIFIED — the RS-485 wire framing is not yet characterised
  dataBits: 8,
  stopBits: 1,
  parity: "none",
  flowControl: "none",
};

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
