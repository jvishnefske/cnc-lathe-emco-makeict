#!/usr/bin/env python3
"""virtual-emco.py — a simulated EMCO PC Turn 50 control box on a pty.

WHAT THIS IS (and is not): this simulator speaks the project's "PlainGcode"
dialect from sender/INTERFACE.md — line-oriented ASCII G-code with XON/XOFF
pacing. That dialect is a DEVELOPMENT AND TRAINING FICTION for the sender
toolchain, NOT the real machine protocol. Per docs/_machine-facts.md, the
real DB9 (bulkhead X300) is an RS-485 bus driven by a memory-mapped ISA
card (Y4A013000 "PCCOM"), the wire protocol is unknown, and it is not
line-oriented text. Nothing learned from this simulator transfers to the
real wire; what DOES transfer is the machine model below (travels, turret,
stalls, referencing), which teaches the real lathe's habits.
(Historical note: this file was born "virtual-ac95.py" after the
community's name for the brain board; the actual CPU board is Y4A012000
and no "AC95" exists in this machine.)

Dialect summary:
  * CR (or LF) terminated ASCII G-code lines
  * XON/XOFF flow control (XOFF = busy, XON = ready)
  * "ok" after each block completes, "error: ..." for rejected blocks,
    "ALARM:n ..." when a soft limit trips
  * G-codes: G0/G1 moves, G4 dwell, G7/G8 diameter/radius mode (boots in
    RADIUS; internal X is always radius), G18/G40 accepted as no-ops,
    G20/G21 units, G28 reference, G53 machine-coordinate move, G54 work
    offset (preset X0 Z-50: stock face 50 mm from the Z reference — the
    same training convention as the Rust core and the LinuxCNC sim),
    G90/G91, G94/G95 feed per minute / per revolution
  * M2/M30 end, M3/M4/M5 spindle (S word required first), M6+T toolchange

Realtime bytes handled even mid-line:
  ?     (0x3F)  status:  <State|X:..|Z:..|S:..|T:n|Q:n|Ref:0/1>
  !     (0x21)  feed hold (pause current motion)
  ~     (0x7E)  resume from feed hold
  Ctrl-X (0x18) abort/reset: stop motion, spindle off, clear alarms/queue

Machine model — numbers from the EMCO manual via docs/_machine-facts.md
(EMCO's own documentation, not re-measured at the machine):
  * X travel 48 mm usable (X is RADIUS, mm); Z travel 228 mm usable
    (275 mm mechanical); resolution 0.001 mm
  * feed 0-750 mm/min (max 1500); rapid 750 mm/min
  * spindle 130-3000 rpm (three belt steps: 130-1300 / 200-2000 /
    300-3000; the sim uses the full range)
  * 6-station turret (3 external + 3 internal), an OPTION on this model —
    index time is a guess, UNVERIFIED

Honest limit/homing model, matching the real machine:
  * NO end-of-travel limit switches. Travel ends at mechanical stops and
    the stepper STALLS (the manual's audible "tack-tack"): the control
    keeps counting steps it believes were made, the carriage stops, and
    POSITION IS SILENTLY LOST. This simulator does exactly that — no
    clean error on the wire, just a diverged position. Stalls are noted
    on the simulator's own console, standing in for the audible cue.
  * There IS a per-axis reference system (mechanical switch + Balluff
    inductive "Sync-Bero"), and NO absolute position memory: the
    reference must be re-acquired after EVERY power-off. Here: the
    machine starts UNREFERENCED, `G28` runs the reference sequence, and
    a simulated power cycle (SIGUSR1 to this process, or
    VirtualEmco.power_cycle()) invalidates the reference again.
  * Soft limits are a SIMULATION SAFETY NET doing a job the real
    hardware does not do. They are active only while referenced (an
    unreferenced control cannot know where the stops are — that is the
    lesson) and can be disabled with --no-soft-limits to practise what a
    stall does to your coordinates.

Stdlib only — no pyserial needed. Point the sender (or picocom) at the
pty path printed on startup:

    python3 tools/virtual-emco.py
    # ... pty: /dev/pts/7
    python3 sender/cli/send.py --port /dev/pts/7 gcode/part.ngc

The simulation core (class VirtualEmco) is I/O-free and clock-injected,
so tools/test_virtual_emco.py exercises it with no hardware and no pty.
"""

import argparse
import math
import os
import re
import select
import signal
import sys
import time

XON = b"\x11"
XOFF = b"\x13"
CTRL_X = 0x18

DEFAULT_PROFILE = {
    "name": "EMCO PC Turn 50 (virtual)",
    # --- from the EMCO manual (docs/_machine-facts.md), not re-measured ---
    # X is radius, matching the repo-wide convention (INTERFACE.md).
    # Usable travel = the soft-limit window below.
    # Limit window matches the verified LinuxCNC simulator
    # (linuxcnc/sim/emco-pcturn50/emco-pcturn50.ini): X -0.5..48 radius
    # (the -0.5 lets a facing cut pass centreline), Z -228..+0.5.
    "soft_x_min_mm": -0.5,
    "soft_x_max_mm": 48.0,      # X travel 48 mm usable
    "soft_z_min_mm": -228.0,    # Z travel 228 mm usable
    "soft_z_max_mm": 0.5,
    "rapid_mm_min": 750.0,      # rapid traverse 750 mm/min
    "max_feed_mm_min": 1500.0,  # feed 0-750 mm/min, max 1500
    "resolution_mm": 0.001,
    "spindle_min_rpm": 130,     # 130-3000 rpm in three belt steps
    "spindle_max_rpm": 3000,
    "turret_stations": 6,       # 3 external + 3 internal; optional
    # --- simulation-only values (UNVERIFIED / arbitrary) -----------------
    "turret_index_seconds": 1.5,   # guess; measure at the machine
    # Mechanical hard stops sit a little beyond usable travel (Z has
    # 275 mm mechanical for 228 usable; the margin split is arbitrary).
    "hard_x_min_mm": -1.0,
    "hard_x_max_mm": 48.5,
    "hard_z_min_mm": -230.0,
    "hard_z_max_mm": 2.0,
    # G54 preset: stock face touched off 50 mm in from the Z reference,
    # X0 on the centreline. Matches the Rust core (sender/INTERFACE.md)
    # and the LinuxCNC training simulator convention.
    "g54_x_mm": 0.0,
    "g54_z_mm": -50.0,
    "reference_x_mm": 48.0,     # where the reference sensors sit (sim)
    "reference_z_mm": 0.0,
    "reference_sync_seconds": 1.0,  # switch + Sync-Bero settle (sim)
    "soft_limits": True,
}

_WORD_RE = re.compile(r"([A-Za-z])\s*([+-]?(?:\d+\.?\d*|\.\d+))")
_COMMENT_PAREN_RE = re.compile(r"\([^)]*\)")

# Machine states
IDLE, RUN, DWELL, TOOLCHANGE, HOMING, HOLD, ALARM = (
    "Idle", "Run", "Dwell", "Toolchange", "Homing", "Hold", "Alarm")


class VirtualEmco:
    """Pure simulation core. Feed it host bytes and a clock; it returns the
    bytes the machine would emit. No I/O, no real time.

    Two position pairs are tracked:
      x, z           what the CONTROL believes (step counting)
      true_x, true_z what the CARRIAGE actually did (the sim's truth)
    They diverge when a stepper stalls at a mechanical stop, exactly like
    the real machine. `notes` collects console-side events (stalls, power
    cycles) that make sound in the real world but no bytes on the wire.
    """

    def __init__(self, profile=None, now_ms=0.0,
                 true_start=(40.0, -20.0)):
        self.profile = dict(DEFAULT_PROFILE)
        if profile:
            self.profile.update(profile)
        self.now_ms = now_ms
        # where the carriage was physically left at "power-on"
        self.true_x, self.true_z = true_start
        self.notes = []           # console-side events, not wire bytes
        self.stall_count = 0
        self._out = bytearray()
        self._flow = None         # last flow byte sent
        self._cold_boot()
        self._emit(b"\r\n%s ready - NOT REFERENCED, send G28\r\n"
                   % self.profile["name"].encode("ascii", "replace"))
        self._send_flow(XON)

    def _cold_boot(self):
        # Control-side state that a power cycle resets. The carriage's
        # true position is physical and survives.
        self.x = 0.0              # believed position: meaningless until G28
        self.z = 0.0
        self.referenced = False
        self.tool = 1
        self.spindle_rpm = 0
        self.spindle_on = False
        self.spindle_dir = 0      # 0 off, +1 M3, -1 M4
        self.absolute = True      # G90
        self.metric = True        # G21
        self.diameter_mode = False  # G7 diameter / G8 radius (boot: radius)
        self.feed_per_rev = False   # G95 mm/rev vs G94 mm/min (boot: G94)
        self.offset_x = 0.0       # active work offset (machine frame);
        self.offset_z = 0.0       # zero until G54 is selected
        self.feed_mm_min = None   # F word; None until set (mm/rev if G95)
        self.state = IDLE
        self.queue = []
        self._linebuf = bytearray()
        self._motion = None       # see _start_motion / _start_homing
        self._busy_until = None   # dwell / toolchange
        self._hold_frac = None

    # ------------------------------------------------------------------ I/O

    def feed(self, data, now_ms):
        """Consume bytes from the host at time now_ms; return machine output."""
        self.step(now_ms)
        for b in data:
            if b == CTRL_X:
                self._abort()
            elif b == 0x3F:  # '?'
                self._status_report()
            elif b == 0x21:  # '!'
                self._feed_hold()
            elif b == 0x7E:  # '~'
                self._resume()
            elif b in (0x11, 0x13):
                pass  # host-side XON/XOFF: ignored, we only pace the host
            elif b in (0x0D, 0x0A):
                if self._linebuf:
                    self.queue.append(self._linebuf.decode("ascii", "replace"))
                    self._linebuf = bytearray()
                    if self.state not in (IDLE,):
                        self._send_flow(XOFF)
            else:
                self._linebuf.append(b)
        self._pump()
        return self._take_output()

    def step(self, now_ms):
        """Advance the simulation clock; return any output produced."""
        self.now_ms = max(self.now_ms, now_ms)
        self._pump()
        return self._take_output()

    def power_cycle(self, now_ms=None):
        """Simulate mains off/on: the control forgets everything, including
        the reference. The carriage stays physically where it was."""
        if now_ms is not None:
            self.now_ms = max(self.now_ms, now_ms)
        if self._motion:  # power died mid-move: carriage freezes mid-path
            _, _, tx, tz = self._interp_pos()
            self.true_x, self.true_z = tx, tz
        self._cold_boot()
        self.notes.append("power cycle: reference lost, believed position "
                          "reset to X0 Z0 (carriage really at X%.3f Z%.3f)"
                          % (self.true_x, self.true_z))
        self._flow = None
        self._emit(b"\r\n%s ready - NOT REFERENCED, send G28\r\n"
                   % self.profile["name"].encode("ascii", "replace"))
        self._send_flow(XON)
        return self._take_output()

    # ------------------------------------------------------------ execution

    def _pump(self):
        progressed = True
        while progressed:
            progressed = False
            if self.state in (RUN, HOMING) and self._motion:
                m = self._motion
                if self.now_ms >= m["t0"] + m["dur_ms"]:
                    self.x, self.z = m["x1"], m["z1"]
                    self.true_x, self.true_z = m["tx1"], m["tz1"]
                    if m.get("stalled"):
                        self.stall_count += 1
                        self.notes.append(
                            "STALL (tack-tack): carriage hit a mechanical "
                            "stop at X%.3f Z%.3f but the control believes "
                            "X%.3f Z%.3f — position lost, re-reference!"
                            % (self.true_x, self.true_z, self.x, self.z))
                    if m["kind"] == "home":
                        self.referenced = True
                    self._motion = None
                    self.state = IDLE
                    self._emit(b"ok\r\n")
                    progressed = True
            elif self.state in (DWELL, TOOLCHANGE):
                if self.now_ms >= self._busy_until:
                    self._busy_until = None
                    self.state = IDLE
                    self._emit(b"ok\r\n")
                    progressed = True
            if self.state == ALARM and self.queue:
                self.queue.pop(0)
                self._emit(b"error: alarm lock (send Ctrl-X to reset)\r\n")
                progressed = True
            elif self.state == IDLE and self.queue:
                line = self.queue.pop(0)
                self._execute(line)
                progressed = True
        if self.state == IDLE and not self.queue:
            self._send_flow(XON)

    def _execute(self, line):
        text = _COMMENT_PAREN_RE.sub(" ", line).split(";", 1)[0].strip()
        if not text:
            self._emit(b"ok\r\n")
            return
        words = []
        consumed = _WORD_RE.sub(" ", text)
        if consumed.strip():
            self._emit(("error: cannot parse %r\r\n"
                        % consumed.strip().split()[0]).encode())
            return
        for letter, value in _WORD_RE.findall(text):
            words.append((letter.upper(), float(value)))

        gcodes = [v for (l, v) in words if l == "G"]
        mcodes = [v for (l, v) in words if l == "M"]
        args = {l: v for (l, v) in words if l not in ("G", "M", "N")}

        for g in gcodes:
            if g == 20:
                self.metric = False
            elif g == 21:
                self.metric = True
            elif g == 90:
                self.absolute = True
            elif g == 91:
                self.absolute = False
            elif g == 7:              # diameter mode (the EMCO default way
                self.diameter_mode = True   # of programming X)
            elif g == 8:              # radius mode
                self.diameter_mode = False
            elif g == 94:             # feed per minute
                self.feed_per_rev = False
            elif g == 95:             # feed per revolution
                self.feed_per_rev = True
            elif g == 54:             # select the (preset) work offset
                self.offset_x = self.profile["g54_x_mm"]
                self.offset_z = self.profile["g54_z_mm"]
            elif g in (18, 40):       # XZ plane / cutter comp off: accepted
                pass                  # no-ops (the sim has no comp, and XZ
                                      # is the only plane it knows)
            elif g in (0, 1, 4, 28, 53):
                pass
            else:
                self._emit(("error: unsupported G%g\r\n" % g).encode())
                return
        for m in mcodes:
            if m not in (2, 3, 4, 5, 6, 30):
                self._emit(("error: unsupported M%g\r\n" % m).encode())
                return
        for letter in args:
            if letter not in ("X", "Z", "F", "S", "T", "P"):
                self._emit(("error: unsupported word %s%g\r\n"
                            % (letter, args[letter])).encode())
                return

        scale = 1.0 if self.metric else 25.4
        if "F" in args:
            # mm/min under G94, mm/rev under G95; clamped at motion time.
            self.feed_mm_min = args["F"] * scale
        if "S" in args:
            s = max(0, min(args["S"], self.profile["spindle_max_rpm"]))
            self.spindle_rpm = int(s)
            if self.spindle_on and self.spindle_rpm == 0:
                self.spindle_on = False
        for m in mcodes:
            if m in (3, 4):
                if self.spindle_rpm <= 0:
                    self._emit(b"error: spindle speed not set (S word)\r\n")
                    return
                self.spindle_on = True
                self.spindle_dir = 1 if m == 3 else -1
            elif m == 5:
                self.spindle_on = False
                self.spindle_dir = 0
            elif m in (2, 30):
                self.spindle_on = False
                self.spindle_dir = 0
                self.absolute = True
                self.metric = True

        if 28 in gcodes:
            self._start_homing()
            return

        if "T" in args:
            self._start_toolchange(int(args["T"]))
            return

        if 4 in gcodes:
            seconds = args.get("P", 0.0)
            if seconds < 0:
                self._emit(b"error: negative dwell\r\n")
                return
            self.state = DWELL
            self._busy_until = self.now_ms + seconds * 1000.0
            return

        motion_g = 0 if 0 in gcodes else (1 if 1 in gcodes else None)
        if motion_g is not None and ("X" in args or "Z" in args):
            self._start_motion(motion_g, args, scale,
                               machine_coords=53 in gcodes)
            return
        if motion_g == 1 and "X" not in args and "Z" not in args:
            self._emit(b"error: G1 without axis word\r\n")
            return

        self._emit(b"ok\r\n")

    def _start_motion(self, g, args, scale, machine_coords=False):
        p = self.profile
        x1, z1 = self.x, self.z
        # Internal x is always RADIUS in machine coordinates; incoming X
        # words are halved in G7 diameter mode, and shifted by the active
        # work offset unless G53 forced machine coordinates for this line.
        ox = 0.0 if machine_coords else self.offset_x
        oz = 0.0 if machine_coords else self.offset_z
        if "X" in args:
            v = args["X"] * scale
            if self.diameter_mode:
                v /= 2.0
            x1 = v + ox if self.absolute else self.x + v
        if "Z" in args:
            v = args["Z"] * scale
            z1 = v + oz if self.absolute else self.z + v

        # Soft limits: a simulation safety net, and only meaningful once
        # referenced — an unreferenced control has no idea where the
        # stops are, exactly like the real machine.
        if p["soft_limits"] and self.referenced:
            if not (p["soft_x_min_mm"] <= x1 <= p["soft_x_max_mm"]):
                self._trip_alarm(1, "soft limit X (target %.3f mm radius)"
                                 % x1)
                return
            if not (p["soft_z_min_mm"] <= z1 <= p["soft_z_max_mm"]):
                self._trip_alarm(2, "soft limit Z (target %.3f mm)" % z1)
                return

        if g == 1:
            if self.feed_mm_min is None or self.feed_mm_min <= 0:
                self._emit(b"error: no feed rate (F word) for G1\r\n")
                return
            if self.feed_per_rev:
                if not self.spindle_on or self.spindle_rpm <= 0:
                    self._emit(b"error: G95 feed needs the spindle "
                               b"running (M3/M4 with S)\r\n")
                    return
                rate = self.feed_mm_min * self.spindle_rpm
            else:
                rate = self.feed_mm_min
            rate = min(rate, p["max_feed_mm_min"])
        else:
            rate = p["rapid_mm_min"]
        dist = math.hypot(x1 - self.x, z1 - self.z)
        if dist <= 1e-9:
            self._emit(b"ok\r\n")
            return

        # True carriage path: same commanded delta, but each axis stalls
        # silently at its mechanical stop. No wire-side error — the real
        # machine has no switch to report it.
        tx1 = self.true_x + (x1 - self.x)
        tz1 = self.true_z + (z1 - self.z)
        ctx1 = min(max(tx1, p["hard_x_min_mm"]), p["hard_x_max_mm"])
        ctz1 = min(max(tz1, p["hard_z_min_mm"]), p["hard_z_max_mm"])
        stalled = (abs(ctx1 - tx1) > 1e-9) or (abs(ctz1 - tz1) > 1e-9)

        self._motion = {"kind": "move",
                        "x0": self.x, "z0": self.z, "x1": x1, "z1": z1,
                        "tx0": self.true_x, "tz0": self.true_z,
                        "tx1": ctx1, "tz1": ctz1,
                        "t0": self.now_ms,
                        "dur_ms": dist / rate * 60000.0,
                        "stalled": stalled}
        self.state = RUN
        self._send_flow(XOFF)

    def _start_homing(self):
        """Reference sequence: drive to the per-axis reference sensors
        (switch + Sync-Bero), then adopt machine coordinates. Works off the
        carriage's TRUE position — the sensors are physical."""
        p = self.profile
        rx, rz = p["reference_x_mm"], p["reference_z_mm"]
        dist = math.hypot(rx - self.true_x, rz - self.true_z)
        dur = (dist / p["rapid_mm_min"] * 60000.0
               + p["reference_sync_seconds"] * 1000.0)
        self._motion = {"kind": "home",
                        "x0": self.x, "z0": self.z, "x1": rx, "z1": rz,
                        "tx0": self.true_x, "tz0": self.true_z,
                        "tx1": rx, "tz1": rz,
                        "t0": self.now_ms, "dur_ms": dur,
                        "stalled": False}
        self.state = HOMING
        self._send_flow(XOFF)

    def _start_toolchange(self, t):
        stations = self.profile["turret_stations"]
        station = t if t <= stations else t // 100
        if not (1 <= station <= stations):
            self._emit(("error: no turret station %d (turret has %d: "
                        "3 external + 3 internal)\r\n"
                        % (station, stations)).encode())
            return
        if station == self.tool:
            self._emit(b"ok\r\n")
            return
        self.tool = station
        self.state = TOOLCHANGE
        self._busy_until = (self.now_ms
                            + self.profile["turret_index_seconds"] * 1000.0)
        self._send_flow(XOFF)

    # ------------------------------------------------------------ realtime

    def _interp_pos(self):
        """Returns (believed_x, believed_z, true_x, true_z)."""
        if self._motion:
            m = self._motion
            if self.state == HOLD and self._hold_frac is not None:
                frac = self._hold_frac
            else:
                frac = min(1.0, (self.now_ms - m["t0"]) / m["dur_ms"]) \
                    if m["dur_ms"] > 0 else 1.0
            return (m["x0"] + frac * (m["x1"] - m["x0"]),
                    m["z0"] + frac * (m["z1"] - m["z0"]),
                    m["tx0"] + frac * (m["tx1"] - m["tx0"]),
                    m["tz0"] + frac * (m["tz1"] - m["tz0"]))
        return self.x, self.z, self.true_x, self.true_z

    def _status_report(self):
        x, z, _, _ = self._interp_pos()   # the control reports its BELIEF
        sp = "%d%s" % (self.spindle_rpm if self.spindle_on else 0,
                       {1: " CW", -1: " CCW", 0: ""}[self.spindle_dir
                                                     if self.spindle_on else 0])
        self._emit(("<%s|X:%.3f|Z:%.3f|S:%s|T:%d|Q:%d|Ref:%d>\r\n"
                    % (self.state, x, z, sp, self.tool,
                       len(self.queue), int(self.referenced))).encode())

    def _feed_hold(self):
        if self.state in (RUN, HOMING) and self._motion:
            m = self._motion
            self._hold_frac = min(1.0, (self.now_ms - m["t0"]) / m["dur_ms"]) \
                if m["dur_ms"] > 0 else 1.0
            self._held_state = self.state
            self.state = HOLD
            self._emit(b"[hold]\r\n")

    def _resume(self):
        if self.state == HOLD and self._motion:
            m = self._motion
            remaining = m["dur_ms"] * (1.0 - self._hold_frac)
            m["t0"] = self.now_ms - m["dur_ms"] + remaining
            self._hold_frac = None
            self.state = getattr(self, "_held_state", RUN)
            self._emit(b"[resume]\r\n")

    def _abort(self):
        homing_aborted = self._motion and self._motion["kind"] == "home"
        self.x, self.z, self.true_x, self.true_z = self._interp_pos()
        if homing_aborted:
            self.referenced = False
        self._motion = None
        self._busy_until = None
        self._hold_frac = None
        self.queue = []
        self._linebuf = bytearray()
        self.spindle_on = False
        self.spindle_dir = 0
        self.state = IDLE
        self._emit(b"\r\nRESET\r\n")
        self._send_flow(XON)

    def _trip_alarm(self, code, message):
        self.state = ALARM
        self.queue = []
        self._motion = None
        self._busy_until = None
        self.spindle_on = False
        self.spindle_dir = 0
        self._emit(("ALARM:%d %s\r\n" % (code, message)).encode())
        self._send_flow(XOFF)

    # -------------------------------------------------------------- output

    def _emit(self, data):
        self._out += data

    def _send_flow(self, byte):
        if self._flow != byte:
            self._flow = byte
            self._out += byte

    def _take_output(self):
        out = bytes(self._out)
        self._out = bytearray()
        return out

    def take_notes(self):
        notes, self.notes = self.notes, []
        return notes


# ---------------------------------------------------------------- pty host

def hexdump(data):
    hx = " ".join("%02x" % b for b in data)
    asc = "".join(chr(b) if 0x20 <= b <= 0x7E else "." for b in data)
    return "%-48s |%s|" % (hx, asc)


def run_pty(args):
    master, slave = os.openpty()
    import termios
    attrs = termios.tcgetattr(slave)
    attrs[0] = attrs[1] = attrs[3] = 0          # iflag, oflag, lflag: raw
    attrs[6][termios.VMIN] = 1
    attrs[6][termios.VTIME] = 0
    termios.tcsetattr(slave, termios.TCSANOW, attrs)
    path = os.ttyname(slave)

    print("virtual EMCO PC Turn 50 control box up")
    print("  pty: %s" % path)
    print("  point the sender at it, e.g.:")
    print("    python3 sender/cli/send.py --port %s <file.ngc>" % path)
    print("  machine starts NOT REFERENCED — programs should begin with G28")
    print("  kill -USR1 %d   simulates a power cycle (reference lost)"
          % os.getpid())
    print("  Ctrl-C to shut down.\n")
    sys.stdout.flush()

    transcript = open(args.transcript, "a") if args.transcript else None

    def log(direction, data):
        stamp = time.strftime("%H:%M:%S") + ".%03d" % (time.time() % 1 * 1000)
        line = "%s %s %s" % (stamp, direction, hexdump(data))
        if not args.quiet:
            print(line)
        if transcript:
            transcript.write(line + "\n")
            transcript.flush()

    def log_note(text):
        line = "%s -- %s" % (time.strftime("%H:%M:%S"), text)
        print(line)                      # notes always print: they are
        if transcript:                   # the stand-in for audible cues
            transcript.write(line + "\n")
            transcript.flush()

    vm = VirtualEmco(now_ms=time.monotonic() * 1000.0)
    if args.start_referenced:
        vm.referenced = True
        vm.x, vm.z = vm.true_x, vm.true_z
        log_note("--start-referenced: skipping the G28 ritual (dev "
                 "shortcut; the real machine never grants this)")

    pending_power_cycle = []
    signal.signal(signal.SIGUSR1,
                  lambda *_: pending_power_cycle.append(True))

    out = vm.step(time.monotonic() * 1000.0)
    if out:
        os.write(master, out)
        log("TX", out)
    try:
        while True:
            try:
                ready, _, _ = select.select([master], [], [], 0.02)
            except InterruptedError:
                ready = []
            now = time.monotonic() * 1000.0
            if pending_power_cycle:
                del pending_power_cycle[:]
                out = vm.power_cycle(now)
                if out:
                    os.write(master, out)
                    log("TX", out)
            if ready:
                try:
                    data = os.read(master, 4096)
                except OSError:       # peer closed; pty may raise EIO
                    data = b""
                if data:
                    log("RX", data)
                    out = vm.feed(data, now)
                else:
                    out = vm.step(now)
            else:
                out = vm.step(now)
            if out:
                os.write(master, out)
                log("TX", out)
            for note in vm.take_notes():
                log_note(note)
    except KeyboardInterrupt:
        print("\nshutting down.")
    finally:
        if transcript:
            transcript.close()
        os.close(master)
        os.close(slave)


def main():
    ap = argparse.ArgumentParser(
        description="Simulated EMCO PC Turn 50 control box on a pty, for "
                    "developing and testing the sender with no hardware. "
                    "Speaks the project's PlainGcode development dialect "
                    "(sender/INTERFACE.md) — NOT the real machine protocol, "
                    "which is an unknown RS-485 format (see "
                    "docs/_machine-facts.md). Models real-machine habits: "
                    "no limit switches (stalls lose position silently), and "
                    "referencing (G28) required after every power-on.",
        epilog="Simulate a power cycle with SIGUSR1 (the pid is printed at "
               "startup); the reference is lost and G28 is needed again.\n"
               "Alternative pty plumbing via socat:\n"
               "  socat -d -d PTY,link=/tmp/emco-a,raw,echo=0 "
               "PTY,link=/tmp/emco-b,raw,echo=0\n"
               "Stdlib only — no pyserial needed.",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--transcript", metavar="FILE",
                    help="append a timestamped hex/ASCII log of both "
                         "directions to FILE")
    ap.add_argument("--quiet", action="store_true",
                    help="do not print traffic to stdout (stall and "
                         "power-cycle notes still print)")
    ap.add_argument("--start-referenced", action="store_true",
                    help="dev shortcut: skip the G28 requirement (the real "
                         "machine never allows this)")
    ap.add_argument("--no-soft-limits", action="store_true",
                    help="disable the simulated soft-limit safety net, to "
                         "practise what a stall does to coordinates")
    ap.add_argument("--rapid", type=float, metavar="MM_MIN",
                    help="override rapid rate (default %g mm/min, from the "
                         "EMCO manual)" % DEFAULT_PROFILE["rapid_mm_min"])
    ap.add_argument("--turret-seconds", type=float, metavar="SEC",
                    help="override turret index time (default %g s, "
                         "UNVERIFIED guess)"
                         % DEFAULT_PROFILE["turret_index_seconds"])
    args = ap.parse_args()

    if args.rapid:
        DEFAULT_PROFILE["rapid_mm_min"] = args.rapid
    if args.turret_seconds:
        DEFAULT_PROFILE["turret_index_seconds"] = args.turret_seconds
    if args.no_soft_limits:
        DEFAULT_PROFILE["soft_limits"] = False
    run_pty(args)


if __name__ == "__main__":
    main()
