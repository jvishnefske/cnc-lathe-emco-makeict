#!/usr/bin/env python3
"""virtual-bridge.py — the USB↔RS-485 capture bridge, with no bridge.

WHAT THIS IS. `firmware/rs485-bridge/` is an STM32F4 that enumerates as two
USB CDC-ACM ports: one raw passthrough (which `tools/emco-listen.py` uses
unchanged) and one carrying timestamped, direction-tagged, flagged records
(which `tools/emco-capture.py` reads). This puts both of those on ptys, driven
by a FAKE CLOCK, so the host half of the toolchain is exercisable and the
framing specification is executable before any board exists.

    python3 tools/virtual-bridge.py
    # virtual RS-485 capture bridge up
    #   raw     : /dev/pts/7     (point tools/emco-listen.py here)
    #   records : /dev/pts/9     (point tools/emco-capture.py here)

WHAT THE TRAFFIC IS — read this before quoting anything it produces. The real
wire protocol of the EMCO PC Turn 50's RS-485 bus is COMPLETELY UNKNOWN
(docs/30-serial-protocol.md §2.2, A1). The original master was a memory-mapped
ISA card whose hardware did the serialising, and the framing may not be
async-UART at all. **Nothing this simulator emits is a claim about the real
machine.** The traffic below is an invented polled-multidrop pattern, in the
same spirit as `tools/virtual-emco.py`'s `PlainGcode` dialect: a development
and training fiction, chosen to exercise every record kind and every flag the
format can carry.

What DOES transfer is the record format itself, which is real:
`firmware/bridge-core/src/record.rs` is the specification, the encoder here is
`tools/emco-capture.py`'s, and `firmware/bridge-core/tests/golden.bin` pins the
Rust encoder and the Python decoder to each other.

Stdlib only. No pyserial, no hardware, no Rust toolchain.

    python3 tools/virtual-bridge.py --stdout        # the stream, no pty
    python3 tools/virtual-bridge.py --seconds 5     # then stop
    python3 tools/virtual-bridge.py --realtime      # pace it like a real bus
"""

import argparse
import importlib.util
import os
import pty
import select
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "emco_capture", os.path.join(_HERE, "emco-capture.py"))
capture = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(capture)


#: The fiction. A "master" polls an address, a "slave" answers, and the whole
#: exchange repeats. Nothing about it is evidence.
POLL = bytes([0x02, 0x01, 0x10, 0x03])
REPLY = bytes([0x02, 0x81, 0x00, 0x00, 0x03])

#: The bit rate the machine's own COM ports use (doc 30 §1.3). The bus rate is
#: unknown; this is a plausible starting guess and nothing more.
BAUD = 9600

#: One character at 8N1: ten bits.
CHARACTER_US = 10 * 1_000_000 // BAUD

#: 3.5 character times, the same idle threshold `bridge-core` derives.
GAP_US = 35 * (1_000_000 // BAUD)


class VirtualBridge:
    """The bridge's own behaviour, on a clock the caller supplies.

    No I/O and no `time` calls, exactly like `VirtualEmco`: the tests drive it
    with numbers, and the pty front end below drives it with a clock. A
    simulator that reads the wall clock is a simulator whose transcripts are
    not reproducible, and the whole point of these records is the timing.
    """

    #: Every this many polls, inject a framing error. Roughly 1 in 7, so a
    #: short capture contains one and a long one contains several.
    ERROR_EVERY = 7
    #: Every this many polls, pretend the outbound queue filled. Real bridges
    #: do this under load and the host tooling has to show the gap.
    DROP_EVERY = 11

    def __init__(self, listen_only=True, micros=0):
        self.listen_only = listen_only
        self.micros = micros
        self.polls = 0
        self.counters = {"rx_bytes": 0, "tx_bytes": 0,
                         "dropped_bytes": 0, "error_bytes": 0}
        self.pending_drop = False

    # ------------------------------------------------------------ the wire

    def boot(self):
        """What the bridge says before anything happens on the bus."""
        out = capture.encode_event(self.micros, 0x01, capture.PROTOCOL_VERSION)
        return out + self.status()

    def status(self):
        """The counters and the build variant.

        The variant is not decoration: a transcript that names the build that
        produced it proves whether the device could physically have
        transmitted. That is the EVIDENCE third of listen-only; the other two
        are a compile-time feature and a link on the adapter that is not
        fitted.
        """
        flags = (capture.FLAG_LISTEN_ONLY if self.listen_only
                 else capture.FLAG_TX_ARMED)
        variant = "listen-only" if self.listen_only else "tx-armed"
        return capture.encode_status(
            self.micros, flags, self.counters,
            f"virtual-bridge 0.1.0 {variant} {BAUD} 8N1 (SIMULATED)")

    def exchange(self):
        """One poll and its answer. Returns the framed records."""
        self.polls += 1
        out = b""

        # The master's poll. In a listen-only build this is somebody else's
        # traffic and arrives as RX; in a transmitting build the bridge is the
        # master and it is TX, which is the distinction the direction tag
        # exists to record.
        kind = capture.KIND_RX if self.listen_only else capture.KIND_TX
        out += self._burst(kind, POLL)

        # The turnaround. On a real half-duplex bus this is the gap between
        # releasing the driver and the far end starting to answer, and it is
        # one of the numbers a capture is FOR.
        self._advance(3 * CHARACTER_US)

        if self.polls % self.ERROR_EVERY == 0:
            # A byte the UART could not frame, alone, with its own timestamp.
            # Doc 30 §2.2 (A1): the wire may not be async-UART at all, so this
            # is a POSITIVE finding rather than noise, and it is the record
            # pyserial cannot produce.
            out += self._burst(
                capture.KIND_RX, bytes([0x7F]),
                flags=capture.FLAG_FRAMING | capture.FLAG_NOISE)
            self.counters["error_bytes"] += 1
        else:
            out += self._burst(capture.KIND_RX, REPLY)

        self._advance(GAP_US * 6)
        return out

    def _burst(self, kind, data, flags=0):
        """One record, with realistic per-byte spacing."""
        start = self.micros
        gaps = [CHARACTER_US] * (len(data) - 1)
        self._advance(CHARACTER_US * len(data))
        if kind == capture.KIND_RX:
            self.counters["rx_bytes"] += len(data)
        else:
            self.counters["tx_bytes"] += len(data)

        if self.polls % self.DROP_EVERY == 0 and not self.pending_drop:
            # The record never leaves. The NEXT one to leave says so — a gap in
            # a transcript that nobody can see is worse than no transcript,
            # because it reads as a quiet bus and "the bus was quiet" is a
            # finding this project would write down.
            self.pending_drop = True
            self.counters["dropped_bytes"] += len(data)
            return b""
        if self.pending_drop:
            flags |= capture.FLAG_DROPPED
            self.pending_drop = False
        return capture.encode_bytes(kind, start, data, gaps, flags)

    def _advance(self, micros):
        self.micros = (self.micros + micros) & 0xFFFFFFFF

    # ---------------------------------------------------------- raw port 1

    def raw(self):
        """What the passthrough port would have shown for the last exchange.

        Every byte, nothing added — which is exactly what `emco-listen.py`
        expects, and exactly what makes it useless for timing. Kept so that
        tool can be exercised against this simulator unchanged.
        """
        if self.polls % self.ERROR_EVERY == 0:
            return POLL + bytes([0x7F])
        return POLL + REPLY


def run(bridge, seconds, realtime, records_out, raw_out, deadline_polls=None):
    """Drives the simulator and writes both streams. Returns the poll count."""
    write_records = records_out
    write_raw = raw_out
    write_records(bridge.boot())
    started = time.monotonic()
    polls = 0
    while True:
        if deadline_polls is not None and polls >= deadline_polls:
            break
        if seconds and time.monotonic() - started >= seconds:
            break
        before = bridge.micros
        write_records(bridge.exchange())
        write_raw(bridge.raw())
        polls += 1
        if polls % 20 == 0:
            write_records(bridge.status())
        if realtime:
            # Pace to the simulated clock, so a capture taken against this
            # looks like one taken against a bus rather than like a file.
            elapsed = ((bridge.micros - before) & 0xFFFFFFFF) / 1_000_000
            time.sleep(elapsed)
        elif seconds:
            time.sleep(0.01)
    write_records(bridge.status())
    return polls


def main():
    parser = argparse.ArgumentParser(
        description="A simulated RS-485 capture bridge on two ptys.",
        epilog="The bus traffic is INVENTED. The real protocol is unknown; see "
               "docs/30-serial-protocol.md §2.2.")
    parser.add_argument("--stdout", action="store_true",
                        help="write the record stream to stdout instead of a pty")
    parser.add_argument("--seconds", type=float, default=0.0,
                        help="stop after this long (default: until Ctrl-C)")
    parser.add_argument("--polls", type=int, default=0,
                        help="stop after this many exchanges")
    parser.add_argument("--realtime", action="store_true",
                        help="pace output to the simulated clock")
    parser.add_argument("--tx-armed", action="store_true",
                        help="pretend to be a build with the transmit path "
                             "compiled in, so the TX records and the TX-ARMED "
                             "status flag can be exercised")
    args = parser.parse_args()

    bridge = VirtualBridge(listen_only=not args.tx_armed)

    if args.stdout:
        out = sys.stdout.buffer
        run(bridge, args.seconds, args.realtime,
            lambda data: (out.write(data), out.flush()),
            lambda data: None,
            deadline_polls=args.polls or None)
        return 0

    raw_master, raw_slave = pty.openpty()
    rec_master, rec_slave = pty.openpty()
    print("virtual RS-485 capture bridge up")
    print(f"  raw     : {os.ttyname(raw_slave)}     "
          "(point tools/emco-listen.py here)")
    print(f"  records : {os.ttyname(rec_slave)}     "
          "(point tools/emco-capture.py here)")
    print("  the bus traffic is INVENTED; see this file's docstring")
    sys.stdout.flush()

    def write(fd):
        def send(data):
            if not data:
                return
            # A pty with nobody reading it fills and blocks. A simulator that
            # stops because its reader went away is a simulator that looks
            # broken, so drop instead — which is also what the real device does
            # when its queue fills, and it reports that in the counters.
            try:
                _, ready, _ = select.select([], [fd], [], 0.05)
                if ready:
                    os.write(fd, data)
            except OSError:
                pass
        return send

    try:
        run(bridge, args.seconds, args.realtime or not args.seconds,
            write(rec_master), write(raw_master),
            deadline_polls=args.polls or None)
    except KeyboardInterrupt:
        print()
    finally:
        for fd in (raw_master, raw_slave, rec_master, rec_slave):
            os.close(fd)
    return 0


if __name__ == "__main__":
    sys.exit(main())
