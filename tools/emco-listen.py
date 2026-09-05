#!/usr/bin/env python3
"""emco-listen.py — passive RS-485 bus logger for the EMCO PC Turn 50.

THE LOWEST-RISK WIRE STEP. This tool never transmits a byte. It opens the
port, listens, and logs everything it hears — timestamped, in hex and
ASCII.

THE PHYSICAL LAYER IS RS-485, NOT RS-232. The control box's rear DB9 is
bulkhead connector X300, a 3-wire half-duplex differential multidrop bus
(schematic sheet "KOMMUNIKATION RS485"; see docs/_machine-facts.md):

    X300 pin 2   GND / signal ground
    X300 pin 3   DATA−
    X300 pin 8   DATA+
    (pin 9 carries +5 V for the bus bias network — leave it alone)

Connect with a USB-RS485 adapter ONLY: adapter A/D+ to pin 8, B/D− to
pin 3, ground to pin 2, transmit driver disabled or unconnected.

  *** DO NOT CONNECT AN RS-232 ADAPTER OR PC COM PORT. ***
  RS-232 is a different electrical standard: single-ended, up to ±12 V.
  Driving it into this low-voltage differential bus can damage the
  machine's irreplaceable transceivers. The old community claim that this
  DB9 is RS-232 is wrong — see docs/30-serial-protocol.md.

Two modes:

  Single listen (default):
    python3 tools/emco-listen.py --port /dev/ttyUSB0 --baud 9600

  Baud/framing sweep — try each (baud, framing) combination for a dwell
  period and score how printable the received bytes look. The bus bit
  rate is UNKNOWN (the original master was a memory-mapped ISA card whose
  hardware did the serialising), so sweeping is honest guesswork:
    python3 tools/emco-listen.py --port /dev/ttyUSB0 --sweep

Expectation management: the framing may not be async-UART at all, and the
bus slaves were probably silent without their PC master. A logic analyser
across DATA+/DATA− beats a UART sweep — capture edges first, decide what
the framing is afterwards. Silence is still a finding: log it
(docs/30-serial-protocol.md §8).

Notes on "read-only": no data byte is ever written, and RTS/DTR are forced
low before open (on most USB-RS485 dongles RTS is the transmit-enable, so
this also keeps the driver off the bus). The modem-line logging (CTS etc.)
is kept from the tool's RS-232-era ancestry; most RS-485 adapters expose
nothing meaningful there.

A transcript file is always written (see --transcript).

Requires pyserial. If it is missing, run `nix develop` from the repo root
or `pip install pyserial`.
"""

import argparse
import sys
import time

try:
    import serial
except ImportError:
    print("This tool needs pyserial for serial-port access.")
    print("Fix: run `nix develop` from the repo root (pyserial and socat are")
    print("in the dev shell), or `pip install pyserial`.")
    sys.exit(1)


SWEEP_BAUDS = [300, 600, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200]
SWEEP_FRAMINGS = ["8N1", "7E1", "7O1", "8E1", "8N2"]

PRINTABLE = set(range(0x20, 0x7F)) | {0x09, 0x0A, 0x0D}


def parse_framing(framing):
    if len(framing) != 3:
        raise ValueError("framing must look like 8N1, 7E1, ...")
    bits = {"7": serial.SEVENBITS, "8": serial.EIGHTBITS}[framing[0]]
    par = {"N": serial.PARITY_NONE, "E": serial.PARITY_EVEN,
           "O": serial.PARITY_ODD}[framing[1].upper()]
    stop = {"1": serial.STOPBITS_ONE, "2": serial.STOPBITS_TWO}[framing[2]]
    return bits, par, stop


def open_readonly(port, baud, framing):
    """Open without transmitting: RTS/DTR forced low before open, no flow
    control, and we simply never call write()."""
    bits, par, stop = parse_framing(framing)
    ser = serial.serial_for_url(port, do_not_open=True)
    ser.baudrate = baud
    ser.bytesize = bits
    ser.parity = par
    ser.stopbits = stop
    ser.rtscts = False
    ser.dsrdtr = False
    ser.xonxoff = False
    ser.timeout = 0.05
    ser.rts = False
    ser.dtr = False
    ser.open()
    return ser


def modem_lines(ser):
    try:
        return {"CTS": ser.cts, "DSR": ser.dsr, "DCD": ser.cd, "RI": ser.ri}
    except OSError:
        # Some adapters/ptys have no modem lines at all.
        return {}


def hexdump_lines(data, width=16):
    for off in range(0, len(data), width):
        chunk = data[off:off + width]
        hx = " ".join("%02x" % b for b in chunk)
        asc = "".join(chr(b) if 0x20 <= b <= 0x7E else "." for b in chunk)
        yield "%-*s |%s|" % (width * 3, hx, asc)


def printable_score(data):
    if not data:
        return 0.0
    return sum(1 for b in data if b in PRINTABLE) / len(data)


class Log:
    def __init__(self, path):
        self.f = open(path, "a")
        self.write("# emco-listen transcript, started %s"
                   % time.strftime("%Y-%m-%d %H:%M:%S"))

    def write(self, line, echo=True):
        stamp = time.strftime("%H:%M:%S") + ".%03d" % (time.time() % 1 * 1000)
        text = "%s %s" % (stamp, line)
        self.f.write(text + "\n")
        self.f.flush()
        if echo:
            print(text)

    def close(self):
        self.f.close()


def listen_once(ser, log, duration, modem_interval, echo=True):
    """Listen for `duration` seconds (None = forever). Returns (bytes
    received, printable score). Logs data and modem-line transitions."""
    total = bytearray()
    last_modem = None
    next_modem = 0.0
    t0 = time.monotonic()
    while duration is None or time.monotonic() - t0 < duration:
        data = ser.read(4096)
        now = time.monotonic()
        if data:
            total += data
            for line in hexdump_lines(data):
                log.write("RX %s" % line, echo)
        if now >= next_modem:
            next_modem = now + modem_interval
            lines = modem_lines(ser)
            if lines and lines != last_modem:
                desc = " ".join("%s=%d" % (k, int(v))
                                for k, v in lines.items())
                log.write("MODEM %s" % desc, echo)
                last_modem = lines
    return bytes(total), printable_score(bytes(total))


def cmd_single(args, log):
    ser = open_readonly(args.port, args.baud, args.framing)
    log.write("-- listening on %s @ %d %s (read-only; TX never driven). "
              "Ctrl-C to stop." % (args.port, args.baud, args.framing))
    try:
        data, score = listen_once(ser, log,
                                  args.duration, args.modem_interval)
    finally:
        ser.close()
    log.write("-- done: %d byte(s), printable ratio %.0f%%"
              % (len(data), score * 100))
    if not data:
        log.write("-- heard nothing. Try power-cycling the control box while "
                  "listening (boot banners are common), pressing panel keys, "
                  "or sweeping bauds with --sweep.")


def cmd_sweep(args, log):
    bauds = args.bauds or SWEEP_BAUDS
    framings = args.framings.split(",") if args.framings else SWEEP_FRAMINGS
    results = []
    log.write("-- sweep: %d baud(s) x %d framing(s), %.1fs each. "
              "Generate traffic if you can (power-cycle the box, press "
              "panel keys)." % (len(bauds), len(framings), args.dwell))
    try:
        for baud in bauds:
            for framing in framings:
                try:
                    ser = open_readonly(args.port, baud, framing)
                except (OSError, ValueError) as e:
                    log.write("-- %6d %s: cannot open (%s)"
                              % (baud, framing, e))
                    continue
                log.write("-- trying %6d %s ..." % (baud, framing))
                try:
                    data, score = listen_once(ser, log, args.dwell,
                                              args.modem_interval,
                                              echo=not args.quiet_sweep)
                finally:
                    ser.close()
                results.append((score, len(data), baud, framing,
                                bytes(data[:32])))
                log.write("-- %6d %s: %5d byte(s), printable %3.0f%%"
                          % (baud, framing, len(data), score * 100))
    except KeyboardInterrupt:
        log.write("-- sweep interrupted; partial results follow.")

    results.sort(key=lambda r: (r[0], r[1]), reverse=True)
    log.write("== sweep summary (best first) ==")
    for score, count, baud, framing, sample in results:
        if count == 0:
            continue
        asc = "".join(chr(b) if 0x20 <= b <= 0x7E else "."
                      for b in sample)
        log.write("  %6d %s  %5d bytes  %3.0f%% printable  %r"
                  % (baud, framing, count, score * 100, asc))
    if not any(r[1] for r in results):
        log.write("  (no data heard on any setting — expected if the bus "
                  "slaves only speak when the old PC master polls them, or "
                  "if the framing is not async UART. A silent bus is a real "
                  "finding: log it. Next: a logic analyser on DATA+/DATA-, "
                  "and the memory-window route in tools/dosbox-capture/)")


def main():
    ap = argparse.ArgumentParser(
        description="Passive, never-transmitting logger for the EMCO "
                    "PC Turn 50's RS-485 bus (control-box DB9 = bulkhead "
                    "X300: pin 2 GND, pin 3 DATA-, pin 8 DATA+; +5 V bias "
                    "on pin 9 — leave it alone). Use a USB-RS485 adapter, "
                    "half-duplex, receive-only: A/D+ to pin 8, B/D- to "
                    "pin 3, GND to pin 2. Logs RX data (hex+ASCII, "
                    "timestamped) and can sweep baud rates and framings, "
                    "scoring each for printable output — the real bit rate "
                    "is unknown and the framing may not be async UART at "
                    "all; a logic analyser on DATA+/DATA- is better when "
                    "available.",
        epilog="WARNING: an RS-232 adapter or PC COM port is the WRONG "
               "ELECTRICAL STANDARD for this connector (single-ended, up "
               "to +/-12 V into a 5 V differential bus) and risks damaging "
               "the machine's irreplaceable transceivers. RS-485 hardware "
               "only. Meter the DB9 first (docs/30-serial-protocol.md "
               "sec. 7). Use this before emco-probe.py — listening is "
               "free, transmitting is not.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    ap.add_argument("--port", "-p", required=True,
                    help="USB-RS485 adapter device, e.g. /dev/ttyUSB0 "
                         "(never an RS-232 adapter — see below)")
    ap.add_argument("--baud", "-b", type=int, default=9600,
                    help="baud rate (single-listen mode)")
    ap.add_argument("--framing", default="8N1",
                    help="data bits/parity/stop bits (single-listen mode)")
    ap.add_argument("--duration", type=float, default=None, metavar="SEC",
                    help="stop after SEC seconds (default: run until Ctrl-C)")
    ap.add_argument("--sweep", action="store_true",
                    help="cycle through candidate baud/framing combinations "
                         "and report a printable-byte score for each")
    ap.add_argument("--bauds", type=int, nargs="+", metavar="BAUD",
                    help="bauds to sweep (default: %s)"
                         % " ".join(map(str, SWEEP_BAUDS)))
    ap.add_argument("--framings", metavar="LIST",
                    help="comma-separated framings to sweep "
                         "(default: %s)" % ",".join(SWEEP_FRAMINGS))
    ap.add_argument("--dwell", type=float, default=5.0, metavar="SEC",
                    help="seconds to listen per sweep combination")
    ap.add_argument("--modem-interval", type=float, default=0.05,
                    metavar="SEC",
                    help="how often to sample CTS/DSR/DCD/RI")
    ap.add_argument("--transcript", metavar="FILE",
                    default=time.strftime("listen-%Y%m%d-%H%M%S.log"),
                    help="transcript file (always written)")
    ap.add_argument("--quiet-sweep", action="store_true",
                    help="during a sweep, log RX data to the transcript only")
    args = ap.parse_args()

    log = Log(args.transcript)
    print("transcript: %s" % args.transcript)
    try:
        if args.sweep:
            cmd_sweep(args, log)
        else:
            cmd_single(args, log)
    except KeyboardInterrupt:
        log.write("-- stopped by user.")
    except serial.SerialException as e:
        log.write("-- serial error: %s" % e)
        sys.exit(1)
    finally:
        log.close()


if __name__ == "__main__":
    main()
