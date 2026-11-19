#!/usr/bin/env python3
"""emco-capture.py — read the RS-485 capture bridge's framed record port.

WHAT THIS TALKS TO. The bridge (`firmware/rs485-bridge/`) is an STM32F4 with a
USB connector and an RS-485 transceiver. It enumerates as TWO USB CDC-ACM
serial ports:

    port 1   raw passthrough — every byte, nothing added.
             `tools/emco-listen.py` works against this UNCHANGED. That tool is
             the safety-reviewed one with the RS-232 warnings, and it is not
             touched.

    port 2   framed records — THIS tool. Each record carries a HARDWARE
             microsecond timestamp, a direction tag, and the UART's own
             framing/parity/noise/overrun flags.

WHY THE SECOND PORT EXISTS, which is the entire reason to build hardware
rather than buy a dongle:

  * Host timestamps are taken at `read()`, which is after USB's 1 ms frames,
    after `cdc_acm` buffering, and after scheduler latency. At 9600 baud a
    character is 1042 µs, so host timing destroys exactly the inter-byte and
    inter-frame signature that identifies a polled multidrop protocol — the
    thing we are trying to find out (docs/30-serial-protocol.md §2.2, A1).

  * On a 3-wire half-duplex bus, once transmit has ever been armed, your own
    echo and a reply are the same bytes on the same conductors. Only the
    bridge knows which.

  * pyserial cannot see a framing or parity error at all. Doc 30 records that
    the wire MAY NOT BE ASYNC-UART AT ALL, so an error at a known timestamp is
    a POSITIVE finding, not noise.

WHAT IT IS NOT. Doc 30 rung 2 is right that a logic analyser across DATA+/
DATA− is the better FIRST instrument, precisely because the framing may not be
async. This does not replace one. What it adds is a safe, always-attached,
timestamped, direction-tagged capture you can leave running through a power
cycle. Use both.

    python3 tools/emco-capture.py --port /dev/ttyACM1
    python3 tools/emco-capture.py --port /dev/ttyACM1 --raw capture.bin
    python3 tools/emco-capture.py --replay capture.bin      # no hardware

THE PHYSICAL LAYER IS RS-485, NOT RS-232. The control box's rear DB9 is
bulkhead connector X300 (pin 2 GND, pin 3 DATA−, pin 8 DATA+). Never connect
an RS-232 adapter or PC COM port to it. Leave pin 9 (+5 V) alone, and DO NOT
FIT A THIRD 120 Ω TERMINATOR: doc 30 §1.1 records the bus as already
terminated and biased at both ends.

Stdlib only for decoding and replay; pyserial is needed only to open a real
port. Log every session in doc 30 §8, null results included.
"""

import argparse
import sys
import time

# ---------------------------------------------------------------- the format
#
# One record is a payload, COBS-encoded, followed by a zero. The zero is the
# delimiter and never occurs inside an encoded payload, so a host that
# connects mid-transcript resynchronises at the next zero — which is the
# everyday case, because you plug this into a laptop that was not listening.
#
# The specification lives in `firmware/bridge-core/src/record.rs`, and
# `firmware/bridge-core/tests/golden.bin` is a corpus the Rust encoder writes
# and `tools/test_emco_bridge.py` asserts THIS decoder reproduces. Neither
# implementation can move without the other going red.

PROTOCOL_VERSION = 1

KIND_RX, KIND_TX, KIND_EVENT, KIND_STATUS = 0x01, 0x02, 0x03, 0x04

KIND_NAMES = {KIND_RX: "RX", KIND_TX: "TX", KIND_EVENT: "EVENT",
              KIND_STATUS: "STATUS"}

FLAG_FRAMING = 0x01
FLAG_PARITY = 0x02
FLAG_NOISE = 0x04
FLAG_OVERRUN = 0x08
FLAG_DROPPED = 0x10
FLAG_BREAK = 0x20
FLAG_LISTEN_ONLY = 0x40
FLAG_TX_ARMED = 0x80

FLAG_NAMES = [
    (FLAG_FRAMING, "FRAMING"), (FLAG_PARITY, "PARITY"), (FLAG_NOISE, "NOISE"),
    (FLAG_OVERRUN, "OVERRUN"), (FLAG_DROPPED, "DROPPED"), (FLAG_BREAK, "BREAK"),
    (FLAG_LISTEN_ONLY, "LISTEN-ONLY"), (FLAG_TX_ARMED, "TX-ARMED"),
]

EVENT_NAMES = {0x01: "BOOT", 0x02: "BREAK", 0x03: "DRIVER-ENABLE",
               0x04: "OVERFLOW", 0x05: "REFUSED"}


def flag_names(flags):
    return [name for bit, name in FLAG_NAMES if flags & bit]


def cobs_decode(frame):
    """One COBS frame, without its delimiter. None if it is not valid COBS.

    A truncated frame is exactly what an unplugged bridge produces, so this
    returns None rather than raising: the caller counts it and finds the next
    delimiter, which is what COBS is for.
    """
    out = bytearray()
    at = 0
    while at < len(frame):
        code = frame[at]
        if code == 0:
            return None
        at += 1
        run = code - 1
        if at + run > len(frame):
            return None
        out += frame[at:at + run]
        at += run
        if code != 0xFF and at < len(frame):
            out.append(0)
    return bytes(out)


def cobs_encode(payload):
    """The inverse, used by `tools/virtual-bridge.py` and by the tests."""
    out = bytearray([0])
    code_at, code = 0, 1
    for byte in payload:
        if byte == 0:
            out[code_at] = code
            code_at = len(out)
            out.append(0)
            code = 1
        else:
            out.append(byte)
            code += 1
            if code == 0xFF:
                out[code_at] = code
                code_at = len(out)
                out.append(0)
                code = 1
    out[code_at] = code
    return bytes(out)


# ---------------------------------------------------------------- encoding
#
# The bridge itself encodes in Rust (`firmware/bridge-core`). These exist for
# `tools/virtual-bridge.py`, which produces a record stream with no board, so
# that the host tooling can be exercised — and the framing spec read as
# executable code — before any hardware exists. One Python encoder, used by the
# simulator and by nothing else; `tools/test_emco_bridge.py` deliberately
# builds its payloads by hand instead, because a test that round-trips one
# implementation against itself proves only that it is self-consistent.


def encode_bytes(kind, micros, data, gaps=(), flags=0):
    """An RX or TX record. `gaps` holds len(data) - 1 inter-byte times."""
    if not 1 <= len(data) <= 64:
        raise ValueError("a record carries 1 to 64 bytes")
    if len(gaps) != len(data) - 1:
        raise ValueError("one gap between each pair of bytes, and no others")
    payload = bytes([kind]) + (micros & 0xFFFFFFFF).to_bytes(4, "little")
    payload += bytes([flags, len(data)]) + bytes(data)
    for gap in gaps:
        payload += min(int(gap), 0xFFFF).to_bytes(2, "little")
    return cobs_encode(payload) + b"\x00"


def encode_event(micros, code, argument=0):
    payload = bytes([KIND_EVENT]) + (micros & 0xFFFFFFFF).to_bytes(4, "little")
    payload += bytes([code]) + (argument & 0xFFFFFFFF).to_bytes(4, "little")
    return cobs_encode(payload) + b"\x00"


def encode_status(micros, flags, counters, text):
    payload = bytes([KIND_STATUS]) + (micros & 0xFFFFFFFF).to_bytes(4, "little")
    payload += bytes([flags])
    for name in ("rx_bytes", "tx_bytes", "dropped_bytes", "error_bytes"):
        payload += (counters.get(name, 0) & 0xFFFFFFFF).to_bytes(4, "little")
    encoded = text.encode("utf-8")[:96]
    while encoded and encoded[-1] & 0xC0 == 0x80:
        encoded = encoded[:-1]              # never cut a character in half
    return cobs_encode(payload + encoded) + b"\x00"


class Record:
    """One decoded record.

    `micros` is the bridge's own free-running counter, which is 32 bits and
    wraps every ~71.6 minutes. `micros_abs` is the un-wrapped version the
    decoder maintains, which is the one to use for anything longer than an
    hour — and a protocol-discovery capture is routinely longer than an hour.
    """

    __slots__ = ("kind", "micros", "micros_abs", "flags", "data", "gaps",
                 "code", "argument", "counters", "text")

    def __init__(self, kind, micros):
        self.kind = kind
        self.micros = micros
        self.micros_abs = micros
        self.flags = 0
        self.data = b""
        #: Microseconds from the previous byte. `gaps[0]` is always 0.
        self.gaps = []
        self.code = 0
        self.argument = 0
        self.counters = {}
        self.text = ""

    @property
    def name(self):
        return KIND_NAMES.get(self.kind, f"0x{self.kind:02x}")

    def byte_times(self):
        """Absolute microseconds for each byte in this record."""
        times, now = [], self.micros_abs
        for i, gap in enumerate(self.gaps):
            now = self.micros_abs if i == 0 else now + gap
            times.append(now)
        return times

    def __repr__(self):
        return (f"<{self.name} t={self.micros_abs} flags={flag_names(self.flags)} "
                f"data={self.data.hex()} gaps={self.gaps}>")


class DecodeError(Exception):
    """A frame that decoded but does not fit the format."""


class Decoder:
    """Bytes in, records out. Keeps no clock and opens nothing."""

    def __init__(self):
        self._pending = bytearray()
        self._last_raw = None
        self._abs = 0
        #: Frames that were not valid COBS. Nonzero after joining a running
        #: transcript, and after every unplug. Not an error — the count is
        #: reported so a person can tell "one at connect" from "one a second".
        self.resyncs = 0
        #: Frames that were valid COBS but not a record this version knows.
        self.unknown = 0

    def feed(self, chunk):
        """Returns whatever complete records `chunk` finished."""
        self._pending += chunk
        records = []
        while True:
            at = self._pending.find(0)
            if at < 0:
                return records
            frame = bytes(self._pending[:at])
            del self._pending[:at + 1]
            if not frame:
                continue                      # a run of delimiters is nothing
            payload = cobs_decode(frame)
            if payload is None:
                self.resyncs += 1
                continue
            try:
                record = self._parse(payload)
            except DecodeError:
                self.unknown += 1
                continue
            records.append(record)

    def _unwrap(self, micros):
        """The bridge's 32-bit counter, made monotonic.

        Correct as long as records arrive in order and no two are more than
        2^32 µs (71.6 minutes) apart. A bus that is silent for longer than that
        is a bus with nothing on it, and the bridge's own STATUS records — sent
        periodically — keep the gap far below it in practice.
        """
        if self._last_raw is None:
            self._last_raw = micros
            self._abs = micros
            return micros
        # Masked subtraction: the difference between two 32-bit counter values
        # is right whether or not the counter wrapped between them, which is
        # the whole trick and the reason the firmware need not carry 64 bits.
        self._abs += (micros - self._last_raw) & 0xFFFFFFFF
        self._last_raw = micros
        return self._abs

    def _parse(self, payload):
        if len(payload) < 6:
            raise DecodeError("a record shorter than its own header")
        kind = payload[0]
        micros = int.from_bytes(payload[1:5], "little")
        record = Record(kind, micros)
        record.micros_abs = self._unwrap(micros)

        if kind in (KIND_RX, KIND_TX):
            if len(payload) < 7:
                raise DecodeError("an RX/TX record with no count")
            count = payload[6]
            end = 7 + count
            need = end + 2 * max(0, count - 1)
            if count == 0 or len(payload) < need:
                raise DecodeError(f"an RX/TX record claiming {count} bytes")
            record.flags = payload[5]
            record.data = bytes(payload[7:end])
            record.gaps = [0] + [
                int.from_bytes(payload[end + 2 * i:end + 2 * i + 2], "little")
                for i in range(count - 1)
            ]
        elif kind == KIND_EVENT:
            if len(payload) < 10:
                raise DecodeError("an EVENT record with no argument")
            record.code = payload[5]
            record.argument = int.from_bytes(payload[6:10], "little")
        elif kind == KIND_STATUS:
            if len(payload) < 22:
                raise DecodeError("a STATUS record with no counters")
            record.flags = payload[5]
            record.counters = {
                "rx_bytes": int.from_bytes(payload[6:10], "little"),
                "tx_bytes": int.from_bytes(payload[10:14], "little"),
                "dropped_bytes": int.from_bytes(payload[14:18], "little"),
                "error_bytes": int.from_bytes(payload[18:22], "little"),
            }
            # `errors="replace"` rather than a raise: a description that
            # arrived slightly wrong is not a reason to lose a transcript.
            record.text = payload[22:].decode("utf-8", errors="replace")
        else:
            raise DecodeError(f"record kind 0x{kind:02x} is not in version "
                              f"{PROTOCOL_VERSION} of this format")
        return record


# ------------------------------------------------------------------ printing


def render(record, first_abs=None):
    """One line per record, in the shape `emco-listen.py` prints bytes."""
    when = record.micros_abs if first_abs is None else record.micros_abs - first_abs
    stamp = f"{when / 1_000_000:12.6f}"
    flags = ",".join(flag_names(record.flags))
    tag = f" [{flags}]" if flags else ""

    if record.kind in (KIND_RX, KIND_TX):
        hexed = " ".join(f"{b:02x}" for b in record.data)
        ascii_ = "".join(chr(b) if 0x20 <= b < 0x7F else "." for b in record.data)
        gaps = ",".join(str(g) for g in record.gaps[1:])
        return (f"{stamp}  {record.name:<6}{tag} {hexed:<48} |{ascii_}|"
                + (f"  gaps(us) {gaps}" if gaps else ""))
    if record.kind == KIND_EVENT:
        name = EVENT_NAMES.get(record.code, f"0x{record.code:02x}")
        return f"{stamp}  EVENT  {name} arg={record.argument}"
    counters = " ".join(f"{k}={v}" for k, v in record.counters.items())
    return f"{stamp}  STATUS{tag} {counters}  {record.text}"


def summarise(decoder, records):
    """The paragraph a findings-log entry wants (doc 30 §8)."""
    if not records:
        return ("Nothing was received. On this bus that is a REAL FINDING and "
                "belongs in the log: the PC card was the master, so the slaves "
                "are expected to be silent without it (doc 30 §4 rung 2).")
    rx = [r for r in records if r.kind == KIND_RX]
    errors = [r for r in records if r.flags & (FLAG_FRAMING | FLAG_PARITY
                                               | FLAG_NOISE | FLAG_OVERRUN)]
    dropped = [r for r in records if r.flags & FLAG_DROPPED]
    span = (records[-1].micros_abs - records[0].micros_abs) / 1_000_000
    lines = [
        f"{len(records)} records over {span:.3f} s",
        f"{sum(len(r.data) for r in rx)} bytes received in {len(rx)} bursts",
    ]
    if errors:
        lines.append(
            f"{len(errors)} record(s) with a UART error flag — these are "
            "POSITIVE evidence about the framing, not noise; quote the "
            "timestamps in the log")
    if dropped:
        lines.append(f"{len(dropped)} gap(s) where the bridge dropped records "
                     "(its outbound queue filled)")
    if decoder.resyncs:
        lines.append(f"{decoder.resyncs} frame(s) resynchronised — expect one "
                     "at connect; many means a link problem")
    if decoder.unknown:
        lines.append(f"{decoder.unknown} frame(s) this decoder does not "
                     "understand: the firmware may be newer than this tool")
    # The inter-byte gaps are the measurement worth having, so lead with them.
    gaps = [g for r in rx for g in r.gaps[1:]]
    if gaps:
        gaps.sort()
        middle = gaps[len(gaps) // 2]
        lines.append(
            f"median inter-byte gap {middle} µs — a character time; at 8N1 "
            f"that is about {round(10_000_000 / middle) if middle else 0} baud")
    return "\n".join("  " + line for line in lines)


# ---------------------------------------------------------------------- main


def open_port(port, baud):
    try:
        import serial
    except ImportError:
        print("Reading a real port needs pyserial.")
        print("Fix: run `nix develop` from the repo root, or `pip install pyserial`.")
        print("Or decode a saved capture with --replay, which needs nothing.")
        sys.exit(1)
    # A CDC-ACM endpoint's baud rate is decoration — the bit rate on the RS-485
    # side is set in the firmware, not here. Opened at a nominal rate anyway,
    # because some stacks object to none.
    handle = serial.serial_for_url(port, do_not_open=True)
    handle.baudrate = baud
    handle.timeout = 0.1
    # This port carries records, not bus traffic, so the bridge is what decides
    # whether anything is ever driven onto RS-485. Nothing here transmits.
    handle.rtscts = False
    handle.dsrdtr = False
    handle.open()
    return handle


def main():
    parser = argparse.ArgumentParser(
        description="Decode the RS-485 bridge's framed record port.",
        epilog="Log every session in docs/30-serial-protocol.md §8, including "
               "null results.")
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--port", help="the bridge's SECOND CDC port, e.g. /dev/ttyACM1")
    source.add_argument("--replay", help="decode a file written by --raw")
    parser.add_argument("--baud", type=int, default=115200,
                        help="nominal; a CDC endpoint ignores it (default 115200)")
    parser.add_argument("--raw", help="also write every byte received, verbatim")
    parser.add_argument("--seconds", type=float, default=0.0,
                        help="stop after this long (default: until Ctrl-C)")
    parser.add_argument("--relative", action="store_true",
                        help="print times from the first record rather than "
                             "from the bridge's power-up")
    args = parser.parse_args()

    decoder = Decoder()
    records = []
    first = None
    raw = open(args.raw, "wb") if args.raw else None

    def show(new):
        nonlocal first
        for record in new:
            if first is None:
                first = record.micros_abs
            records.append(record)
            print(render(record, first if args.relative else None), flush=True)

    try:
        if args.replay:
            with open(args.replay, "rb") as handle:
                show(decoder.feed(handle.read()))
        else:
            handle = open_port(args.port, args.baud)
            print(f"listening on {args.port}; Ctrl-C to stop", file=sys.stderr)
            deadline = time.monotonic() + args.seconds if args.seconds else None
            while deadline is None or time.monotonic() < deadline:
                chunk = handle.read(4096)
                if chunk:
                    if raw:
                        raw.write(chunk)
                        raw.flush()
                    show(decoder.feed(chunk))
    except KeyboardInterrupt:
        print(file=sys.stderr)
    finally:
        if raw:
            raw.close()

    print("\nSummary:", file=sys.stderr)
    print(summarise(decoder, records), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
