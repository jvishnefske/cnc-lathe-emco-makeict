import { describe, expect, it } from "vitest";
import { concatBytes, type SerialSettings, type Transport } from "../src/transport/transport";
import { DEFAULT_SERIAL_SETTINGS } from "../src/transport/transport";
import { LoopbackTransport } from "../src/transport/loopback";
import type { VirtualMachineApi } from "../src/core/api";
import type { Dro } from "../src/core/types";

const enc = new TextEncoder();
const dec = new TextDecoder();

/** A VirtualMachine test double that echoes bytes back upper-cased. */
class EchoVm implements VirtualMachineApi {
  received: Uint8Array[] = [];
  steps: number[] = [];
  step(nowMs: number, hostTx: Uint8Array): Uint8Array {
    this.steps.push(nowMs);
    if (hostTx.length > 0) this.received.push(hostTx);
    return enc.encode(dec.decode(hostTx).toUpperCase());
  }
  dro(): Dro {
    return {
      xRadiusMm: 0, xDiameterMm: 0, zMm: 0, xWorkMm: 0, zWorkMm: 0,
      xDtgMm: 0, zDtgMm: 0, homed: false,
    };
  }
  injectFault(): void {}
}

describe("concatBytes", () => {
  it("handles empty, single and multiple chunks", () => {
    expect(concatBytes([]).length).toBe(0);
    const one = new Uint8Array([1, 2]);
    expect(concatBytes([one])).toBe(one);
    expect(Array.from(concatBytes([new Uint8Array([1]), new Uint8Array([2, 3])]))).toEqual([
      1, 2, 3,
    ]);
  });
});

describe("LoopbackTransport", () => {
  it("passes queued bytes to the VM on drain and returns its reply", async () => {
    const vm = new EchoVm();
    const t = new LoopbackTransport(vm);
    await t.connect(DEFAULT_SERIAL_SETTINGS);
    expect(t.connected).toBe(true);
    t.send(enc.encode("g1 x5\r"));
    t.send(enc.encode("g0 z0\r"));
    const reply = t.drain(100);
    expect(dec.decode(reply)).toBe("G1 X5\rG0 Z0\r");
    expect(vm.steps).toEqual([100]);
  });

  it("still steps the VM when nothing was sent", async () => {
    const vm = new EchoVm();
    const t = new LoopbackTransport(vm);
    await t.connect(DEFAULT_SERIAL_SETTINGS);
    t.drain(1);
    t.drain(2);
    expect(vm.steps).toEqual([1, 2]);
  });

  it("drops bytes and stops stepping when disconnected", async () => {
    const vm = new EchoVm();
    const t = new LoopbackTransport(vm);
    t.send(enc.encode("early\r")); // before connect: dropped
    await t.connect(DEFAULT_SERIAL_SETTINGS);
    await t.disconnect();
    t.send(enc.encode("late\r"));
    expect(t.drain(5).length).toBe(0);
    expect(vm.received).toEqual([]);
    expect(vm.steps).toEqual([]);
  });
});

/**
 * A scripted fake Transport — proves the Transport interface is enough to
 * drive a Session without hardware, and doubles as the template for future
 * transports.
 */
export class FakeTransport implements Transport {
  readonly kind = "loopback" as const;
  connectedFlag = false;
  sent: Uint8Array[] = [];
  toDeliver: Uint8Array[] = [];
  onDisconnect?: () => void;

  get connected(): boolean {
    return this.connectedFlag;
  }
  async connect(_s: SerialSettings): Promise<void> {
    this.connectedFlag = true;
  }
  async disconnect(): Promise<void> {
    this.connectedFlag = false;
  }
  send(bytes: Uint8Array): void {
    if (this.connectedFlag && bytes.length > 0) this.sent.push(bytes);
  }
  drain(_nowMs: number): Uint8Array {
    return concatBytes(this.toDeliver.splice(0));
  }
  /** Test hook: script bytes "from the machine". */
  deliver(text: string): void {
    this.toDeliver.push(enc.encode(text));
  }
  sentText(): string {
    return dec.decode(concatBytes(this.sent));
  }
}

describe("FakeTransport", () => {
  it("implements the Transport contract", async () => {
    const t = new FakeTransport();
    await t.connect(DEFAULT_SERIAL_SETTINGS);
    t.send(enc.encode("hello\r"));
    t.deliver("ok\r\n");
    expect(dec.decode(t.drain(0))).toBe("ok\r\n");
    expect(t.sentText()).toBe("hello\r");
    await t.disconnect();
    expect(t.connected).toBe(false);
  });
});
