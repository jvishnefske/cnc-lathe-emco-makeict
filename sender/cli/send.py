#!/usr/bin/env python3
"""send.py — headless G-code sender for the EMCO PC Turn 50 toolchain.

Streams a G-code file to a serial port (normally the pty printed by
tools/virtual-emco.py) using the "PlainGcode" dialect from
sender/INTERFACE.md: one CR-terminated line at a time, waiting for the
machine's "ok" before sending the next, and honouring XON/XOFF (and,
on a real port, CTS) flow control.

NOTE: this dialect is a development and training fiction. The real
machine's DB9 is an RS-485 bus with an unknown, non-text protocol
(docs/30-serial-protocol.md) — do NOT point this tool at the real lathe
expecting it to work; today its targets are the virtual machine and any
future device that adopts the dialect.

    python3 sender/cli/send.py --port /dev/ttyUSB0 part.ngc
    python3 sender/cli/send.py --port /dev/pts/7 --single-block part.ngc
    python3 sender/cli/send.py --dry-run part.ngc

Ctrl-C at any point sends the best-effort stop sequence (feed hold '!'
then Ctrl-X abort) and closes the port.

  *** THAT STOP IS ADVISORY, NOT A SAFETY FUNCTION. ***
  The real emergency stop is the mains switch on the control box, with a
  person standing at the machine. Never run this unattended.

Transports: uses pyserial when available (required for real hardware —
it does CTS and baud properly). If pyserial is missing, a stdlib
termios fallback still works for local ptys, so the virtual-machine
workflow needs no third-party packages at all.
"""

import argparse
import os
import select
import sys
import time

XON = 0x11
XOFF = 0x13
STOP_SEQUENCE = b"!\x18"   # feed hold, then abort/reset

try:
    import serial            # pyserial
    HAVE_PYSERIAL = True
except ImportError:
    serial = None
    HAVE_PYSERIAL = False


# ------------------------------------------------------------- transports

class PyserialTransport:
    def __init__(self, args):
        self.ser = serial.Serial(
            port=args.port,
            baudrate=args.baud,
            bytesize={7: serial.SEVENBITS, 8: serial.EIGHTBITS}[args.databits],
            parity={"none": serial.PARITY_NONE, "even": serial.PARITY_EVEN,
                    "odd": serial.PARITY_ODD}[args.parity],
            stopbits={1: serial.STOPBITS_ONE, 2: serial.STOPBITS_TWO}[args.stopbits],
            rtscts=args.rtscts,
            xonxoff=False,     # we handle XON/XOFF ourselves, visibly
            timeout=0)
        self.name = args.port

    def read(self, timeout):
        r, _, _ = select.select([self.ser.fileno()], [], [], timeout)
        return self.ser.read(4096) if r else b""

    def write(self, data):
        self.ser.write(data)
        self.ser.flush()

    def close(self):
        self.ser.close()


class TermiosTransport:
    """Stdlib fallback: fine for ptys (baud is meaningless there). For real
    hardware install pyserial instead — this fallback sets raw mode and baud
    but does no hardware flow control."""

    def __init__(self, args):
        import termios
        self.fd = os.open(args.port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        attrs = termios.tcgetattr(self.fd)
        attrs[0] = attrs[1] = attrs[3] = 0          # raw
        speed = getattr(termios, "B%d" % args.baud, termios.B9600)
        attrs[4] = attrs[5] = speed
        attrs[6][termios.VMIN] = 0
        attrs[6][termios.VTIME] = 0
        termios.tcsetattr(self.fd, termios.TCSANOW, attrs)
        self.name = args.port

    def read(self, timeout):
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return b""
        try:
            return os.read(self.fd, 4096)
        except OSError:
            return b""

    def write(self, data):
        os.write(self.fd, data)

    def close(self):
        os.close(self.fd)


def open_transport(args):
    if HAVE_PYSERIAL:
        return PyserialTransport(args)
    print("note: pyserial not installed — using stdlib pty fallback.")
    print("      This is fine for tools/virtual-emco.py ptys. For a REAL")
    print("      serial port, run inside `nix develop` (from the repo root)")
    print("      or `pip install pyserial` first.")
    return TermiosTransport(args)


# ------------------------------------------------------------------ sender

class Transcript:
    def __init__(self, path):
        self.f = open(path, "a")
        self.f.write("# send.py transcript started %s\n"
                     % time.strftime("%Y-%m-%d %H:%M:%S"))

    def log(self, direction, data):
        stamp = "%.3f" % time.monotonic()
        if isinstance(data, bytes):
            hx = " ".join("%02x" % b for b in data)
            asc = "".join(chr(b) if 0x20 <= b <= 0x7E else "."
                          for b in data)
            self.f.write("%s %s %s |%s|\n" % (stamp, direction, hx, asc))
        else:
            self.f.write("%s %s %s\n" % (stamp, direction, data))
        self.f.flush()

    def close(self):
        self.f.close()


def load_program(path):
    """Return (line_number, text) pairs worth sending: blank and
    comment-only lines are skipped (they earn no ack from the machine
    model, and skipping keeps progress honest)."""
    lines = []
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for n, raw in enumerate(f, start=1):
            text = raw.rstrip("\r\n")
            stripped = text.strip()
            if not stripped:
                continue
            body = stripped.split(";", 1)[0].strip()
            if not body or (body.startswith("(") and body.endswith(")")):
                continue
            lines.append((n, text))
    return lines


class MachineTalk:
    """Reads the port, splits machine responses into lines, tracks flow
    control state."""

    def __init__(self, transport, transcript, verbose):
        self.t = transport
        self.transcript = transcript
        self.verbose = verbose
        self.buf = bytearray()
        self.xoff = False

    def poll(self, timeout):
        """Return a list of complete response lines received within timeout."""
        data = self.t.read(timeout)
        if not data:
            return []
        self.transcript.log("RX", data)
        out = []
        for b in data:
            if b == XOFF:
                self.xoff = True
            elif b == XON:
                self.xoff = False
            else:
                self.buf.append(b)
                if b in (0x0A, 0x0D):
                    line = self.buf.decode("ascii", "replace").strip()
                    self.buf = bytearray()
                    if line:
                        out.append(line)
        return out

    def wait_clear_to_send(self, timeout):
        """Honour XOFF: wait (up to timeout) until the machine says XON."""
        deadline = time.monotonic() + timeout
        while self.xoff and time.monotonic() < deadline:
            for line in self.poll(0.05):
                print("  machine: %s" % line)
        return not self.xoff


def send_stop(transport, transcript):
    print("\nsending stop sequence (feed hold + abort) ...")
    try:
        transport.write(STOP_SEQUENCE)
        transcript.log("TX", STOP_SEQUENCE)
    except OSError as e:
        print("  could not send stop: %s" % e)
    print("*** The stop sequence is ADVISORY ONLY. If the machine is still")
    print("*** moving, use the MAINS SWITCH on the control box. ***")


def run(args):
    lines = load_program(args.file)
    total = len(lines)
    if total == 0:
        print("nothing to send: %s has no sendable lines" % args.file)
        return 0

    if args.dry_run:
        print("dry run — nothing will be opened or sent. %d block(s):"
              % total)
        for n, text in lines:
            print("  would send [src line %4d]: %r + CR" % (n, text))
        return 0

    transcript = Transcript(args.transcript)
    print("transcript: %s" % args.transcript)
    try:
        transport = open_transport(args)
    except OSError as e:
        print("could not open %s: %s" % (args.port, e))
        return 1

    talk = MachineTalk(transport, transcript, args.verbose)
    started = time.monotonic()
    sent = 0
    status = 0
    try:
        # Drain anything the machine already said (banner, XON...).
        for line in talk.poll(0.3):
            print("  machine: %s" % line)

        for i, (n, text) in enumerate(lines, start=1):
            if args.single_block:
                try:
                    input("single block — Enter to send line %d (%r), "
                          "Ctrl-C to stop: " % (n, text))
                except EOFError:
                    print("stdin closed; stopping.")
                    break

            if not talk.wait_clear_to_send(args.timeout):
                print("machine held XOFF for %.0fs — stopping." % args.timeout)
                status = 1
                break

            payload = text.encode("ascii", "replace") + b"\r"
            transport.write(payload)
            transcript.log("TX", payload)
            pct = 100 * i // total
            print("[%4d/%d %3d%%] %s" % (i, total, pct, text), end="",
                  flush=True)

            # Wait for ok / error / ALARM
            t0 = time.monotonic()
            acked = False
            failed = None
            while time.monotonic() - t0 < args.timeout:
                for line in talk.poll(0.05):
                    low = line.lower()
                    if low == "ok":
                        acked = True
                    elif low.startswith("error") or low.startswith("alarm"):
                        failed = line
                    elif args.verbose or low.startswith("<"):
                        print("\n  machine: %s" % line, end="")
                if acked or failed:
                    break
            if failed:
                print("\nmachine said: %s" % failed)
                print("stopping (line %d of %s)." % (n, args.file))
                status = 1
                break
            if not acked:
                print("\nno ack within %.0fs — stopping. (Wrong port? Wrong "
                      "baud? Machine in alarm?)" % args.timeout)
                status = 1
                break
            sent += 1
            print("  ok (%.2fs)" % (time.monotonic() - t0))

            if args.settle:
                time.sleep(args.settle)

        else:
            elapsed = time.monotonic() - started
            print("done: %d/%d block(s) sent in %.1fs." % (sent, total,
                                                           elapsed))
    except KeyboardInterrupt:
        send_stop(transport, transcript)
        status = 130
    finally:
        transport.close()
        transcript.log("--", "closed (sent %d/%d)" % (sent, total))
        transcript.close()
    return status


def main():
    ap = argparse.ArgumentParser(
        description="Headless G-code sender for the EMCO PC Turn 50: streams "
                    "a file line-by-line with ack ('ok') pacing and XON/XOFF "
                    "flow control. Fallback for when a Web Serial browser is "
                    "not available; also the standard way to drive "
                    "tools/virtual-emco.py.",
        epilog="SAFETY: Ctrl-C sends a best-effort stop, but the only real "
               "stop is the mains switch on the control box. Stay at the "
               "machine for the entire run.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    ap.add_argument("file", help="G-code file to send")
    ap.add_argument("--port", "-p",
                    help="serial device or virtual-emco pty "
                         "(e.g. /dev/ttyUSB0, /dev/pts/7); required unless "
                         "--dry-run")
    ap.add_argument("--baud", "-b", type=int, default=9600,
                    help="baud rate")
    ap.add_argument("--databits", type=int, choices=(7, 8), default=8,
                    help="data bits")
    ap.add_argument("--parity", choices=("none", "even", "odd"),
                    default="none", help="parity")
    ap.add_argument("--stopbits", type=int, choices=(1, 2), default=1,
                    help="stop bits")
    ap.add_argument("--rtscts", action="store_true",
                    help="enable hardware RTS/CTS flow control (pyserial "
                         "only; part of the PlainGcode dev dialect — the "
                         "old 'the machine gates on CTS' community claim "
                         "is refuted, see docs/30-serial-protocol.md)")
    ap.add_argument("--timeout", type=float, default=120.0, metavar="SEC",
                    help="max seconds to wait for a block's 'ok' (long moves "
                         "take real time)")
    ap.add_argument("--settle", type=float, default=0.0, metavar="SEC",
                    help="extra pause after each acked block")
    ap.add_argument("--single-block", action="store_true",
                    help="require Enter before each line is sent")
    ap.add_argument("--dry-run", action="store_true",
                    help="print exactly what would be sent and send nothing "
                         "(no port needed)")
    ap.add_argument("--transcript", metavar="FILE",
                    default=time.strftime("send-%Y%m%d-%H%M%S.log"),
                    help="timestamped hex/ASCII log of both directions")
    ap.add_argument("--verbose", "-v", action="store_true",
                    help="print every machine response line")
    args = ap.parse_args()

    if not args.dry_run and not args.port:
        ap.error("--port is required unless --dry-run")
    if not os.path.exists(args.file):
        ap.error("no such file: %s" % args.file)

    sys.exit(run(args))


if __name__ == "__main__":
    main()
