"""The decisions behind the emco_lathe screen, kept free of Qt and LinuxCNC.

Everything the screen *enforces* lives here, so it can be tested on any
computer with plain `python3 -m unittest` and no LinuxCNC installed:

    python3 linuxcnc/sim/emco-pcturn50/tests/test_emco_rules.py

The handler (emco_lathe_handler.py) only feeds these functions what LinuxCNC
reports and acts on what they return. Each rule cites the document that
asks for it, so a disagreement between screen and docs can be settled by
reading one or the other.
"""

import hashlib
import json
import os
import re
from dataclasses import dataclass

# -----------------------------------------------------------------------------
# Numbers the INI cannot express
# -----------------------------------------------------------------------------
# Everything else (travels, the reference position) is read from the INI by
# the handler and passed in as a Machine. These two are not LinuxCNC settings
# at all, so they live here, once.
#
# Belt ranges: three V-belt positions, moved by hand with the power off.
# docs/10-machine-reference.md §4, docs/40-operator-guide.md §6. [MANUAL]
BELT_RANGES = {
    1: (130, 1300),
    2: (200, 2000),
    3: (300, 3000),
}
# Position 2 "covers most work on this machine" (doc 40 §6).
DEFAULT_BELT = 2
# Threading must stay at or below 400 rpm whatever the belt (doc 40 §6). [MANUAL]
THREADING_MAX_RPM = 400


@dataclass(frozen=True)
class Machine:
    """What the rules need to know about the machine, read from the INI."""
    x_retract: float            # [JOINT_0]HOME, radius: the fully retracted X
    z_park: float               # [JOINT_1]HOME: where programs should finish
    tool_change_retracts: bool  # [EMCIO]TOOL_CHANGE_POSITION is set


# -----------------------------------------------------------------------------
# Reference gate — doc 40 §5
# -----------------------------------------------------------------------------
class ReferenceGate:
    """Has the machine been referenced since it last lost its position?

    The real lathe has no absolute position memory, so the docs require a
    re-reference after every power-on, every E-stop and every stall. LinuxCNC
    keeps its `homed` flags across an E-stop, so trusting them would teach
    exactly the habit the docs warn against. The gate therefore remembers
    E-stops itself: homed flags only count once they have gone from not-all-
    homed to all-homed *after* the most recent E-stop.
    """

    def __init__(self):
        self._estop_seen = True     # power-on counts as having lost position
        self._was_all_homed = False

    @property
    def position_lost(self):
        """True from an E-stop (or power-on) until the next reference run."""
        return self._estop_seen

    def update(self, estop, machine_on, homed):
        all_homed = bool(homed) and all(homed)
        if estop:
            self._estop_seen = True
        elif all_homed and not self._was_all_homed:
            # A fresh transition into all-homed: a real reference run.
            self._estop_seen = False
        self._was_all_homed = all_homed
        return self.reason(estop, machine_on, all_homed)

    def reason(self, estop, machine_on, all_homed):
        """None when satisfied, else what the operator must do next."""
        if estop:
            return "E-STOP — reset, then reference"
        if not machine_on:
            return "Machine off — switch on, then reference"
        if self._estop_seen and all_homed:
            return "Position lost at E-stop — Home All again"
        if not all_homed:
            return "Not referenced — Home All first"
        return None


# -----------------------------------------------------------------------------
# Belt and spindle — doc 40 §6
# -----------------------------------------------------------------------------
def belt_range(belt):
    return BELT_RANGES[belt]


def clamp_rpm(belt, rpm):
    lo, hi = BELT_RANGES[belt]
    return max(lo, min(hi, rpm))


def spindle_violation(belt, rpm, threading):
    """None if `rpm` is acceptable on `belt`, else a reason to stop.

    rpm == 0 is a stopped spindle and always acceptable.
    """
    rpm = abs(rpm)
    if rpm == 0:
        return None
    if threading and rpm > THREADING_MAX_RPM:
        return "Threading at {:.0f} rpm — the limit is {} rpm (doc 40 §6)".format(
            rpm, THREADING_MAX_RPM)
    lo, hi = BELT_RANGES[belt]
    if not lo <= rpm <= hi:
        return "{:.0f} rpm is outside belt {} ({}–{} rpm) — change the belt or the S word".format(
            rpm, belt, lo, hi)
    return None


# -----------------------------------------------------------------------------
# Preflight — the proving ladder, doc 40 §10 and house style, doc 50 §8
# -----------------------------------------------------------------------------
ERROR = "error"
WARNING = "warning"


@dataclass(frozen=True)
class Finding:
    line: int       # 1-based, as the editor shows it
    severity: str   # ERROR blocks cycle start; WARNING is shown
    message: str


_COMMENT = re.compile(r"\([^)]*\)")
# A word with a plain number. Words with [expressions] or #parameters are
# skipped: this is a line scanner, not an interpreter, and guessing at a
# subroutine argument would be worse than saying nothing.
_WORD = re.compile(r"([A-Z])\s*([-+]?(?:\d+\.?\d*|\.\d+))(?![\d.\[#])")
_OWORD = re.compile(r"^O\s*<[^>]*>|^O\d+", re.I)
_TOL = 0.001


def _code_part(raw):
    line = raw.split(";", 1)[0]
    return _COMMENT.sub(" ", line).upper().strip()


def _words(code):
    out = []
    for letter, value in _WORD.findall(code):
        out.append((letter, float(value)))
    return out


def preflight(text, belt, machine):
    """Scan a program for the mistakes the docs single out.

    Tracks just enough modal state to be right about the one trap that
    matters most: under G7 (diameter mode) the fully retracted position is
    X = 2 * x_retract, and `G53 G0 X48` only retracts half way
    (gcode/tutorial-part.ngc explains this at length).
    """
    findings = []
    lines = text.splitlines()

    # House style: a header that says what stock and tools the program wants.
    header = " ".join(l for l in lines[:40] if l.strip().startswith("(")).upper()
    for need in ("STOCK", "TOOL"):
        if need not in header:
            findings.append(Finding(1, WARNING,
                "No '{}' in the header comment — say what the program expects "
                "so the operator can check the setup (doc 40 §10.2)".format(need.title())))

    diameter = False
    metric_seen = False
    css = False
    spindle_on = False
    s_active = 0.0
    seen_motion = False
    retracted = False       # X is known to be fully retracted
    last_motion = None      # (line, is_g53, x, z) of the last motion line
    in_sub = False

    for n, raw in enumerate(lines, 1):
        code = _code_part(raw)
        if not code:
            continue
        if _OWORD.match(code):
            # Subroutine bodies run wherever they are called from; their
            # moves are not "the first move of the program".
            if "ENDSUB" in code:
                in_sub = False
            elif "SUB" in code.split():
                in_sub = True
            continue

        words = _words(code)
        gs = [v for l, v in words if l == "G"]
        ms = [v for l, v in words if l == "M"]
        vals = {l: v for l, v in words if l not in "GM"}

        if 21 in gs:
            metric_seen = True
        if 20 in gs:
            findings.append(Finding(n, WARNING, "G20 inches — house style is millimetres (G21)"))
        if 7 in gs:
            diameter = True
        if 8 in gs:
            diameter = False
        if 96 in gs:
            css = True
        if 97 in gs:
            css = False

        if "S" in vals:
            s_active = vals["S"]
            if not css and s_active > 0:
                lo, hi = BELT_RANGES[belt]
                if not lo <= s_active <= hi:
                    findings.append(Finding(n, ERROR,
                        "S{:g} is outside belt {} ({}–{} rpm)".format(s_active, belt, lo, hi)))

        if 3 in ms or 4 in ms:
            spindle_on = True
        if 5 in ms or 2 in ms or 30 in ms:
            spindle_on = False

        if 33 in gs or 33.1 in gs or 76 in gs:
            if s_active > THREADING_MAX_RPM:
                findings.append(Finding(n, ERROR,
                    "Threading with S{:g} — the limit is {} rpm".format(s_active, THREADING_MAX_RPM)))

        if 6 in ms:
            if spindle_on:
                findings.append(Finding(n, ERROR,
                    "Tool change with the spindle running — M5 first (doc 40 §8)"))
            if not retracted and not machine.tool_change_retracts:
                findings.append(Finding(n, ERROR,
                    "Tool change without a full X retract — the turret sweeps a wide circle (doc 40 §8)"))

        has_motion_axis = "X" in vals or "Z" in vals
        moves = has_motion_axis and not (set(gs) & {10, 28, 30, 92, 43, 43.1})
        if moves and not in_sub:
            g53 = 53 in gs
            x_retract_value = machine.x_retract * (2 if diameter else 1)
            if not seen_motion:
                seen_motion = True
                if not (g53 and "X" in vals and vals["X"] >= x_retract_value - _TOL):
                    hint = "G53 G0 X{:g}".format(x_retract_value)
                    if diameter and g53 and "X" in vals and abs(vals["X"] - machine.x_retract) < _TOL:
                        msg = ("First move retracts only half way: under G7 an X value is a "
                               "diameter, so the full retract is {}".format(hint))
                    else:
                        msg = "First move is not a full X retract — open with {} (doc 50 §8)".format(hint)
                    findings.append(Finding(n, ERROR, msg))
            if "X" in vals:
                retracted = g53 and vals["X"] >= x_retract_value - _TOL
            last_motion = (n, g53, vals.get("X"), vals.get("Z"), x_retract_value)

    if not metric_seen:
        findings.append(Finding(1, WARNING, "No G21 — state millimetres explicitly (doc 50 §8)"))

    if last_motion is not None:
        n, g53, x, z, x_full = last_motion
        parked = (g53 and x is not None and x >= x_full - _TOL
                  and z is not None and abs(z - machine.z_park) < _TOL)
        if not parked:
            findings.append(Finding(n, WARNING,
                "Program does not finish at G53 G0 X{:g} Z{:g} — leave the machine clear "
                "for the next person (doc 50 §8)".format(x_full, machine.z_park)))

    findings.sort(key=lambda f: (f.line, f.severity != ERROR))
    return findings


# -----------------------------------------------------------------------------
# Touch-off — doc 40 §9
# -----------------------------------------------------------------------------
def touchoff_work_z():
    """§9.1: with the tool tip on the fresh face, that face becomes G54 Z0."""
    return ["G10 L20 P1 Z0"]


def touchoff_tool_x(tool, measured_diameter, diameter_mode):
    """§9.2 step 2: the skim-cut diameter becomes the tool's X offset.

    Issued in radius mode with an explicit radius, rather than trusting how a
    given LinuxCNC version applies G7 to G10, and the operator's diameter mode
    is restored afterwards.
    """
    if measured_diameter <= 0:
        raise ValueError("measured diameter must be positive")
    cmds = ["G8", "G10 L10 P{} X{:.4f}".format(int(tool), measured_diameter / 2.0)]
    if diameter_mode:
        cmds.append("G7")
    return cmds


def touchoff_tool_z(tool):
    """§9.2 step 3: the tool tip touching the faced end is this tool's Z0."""
    return ["G10 L10 P{} Z0".format(int(tool))]


def verify_work_z():
    """§9.1 step 6: the tool should stop 5 mm clear of the fresh face."""
    return ["G0 Z5"]


def verify_tool(diameter_mode, bar_diameter=20.0):
    """§9.2 step 5: a known bar diameter, 5 mm off the face."""
    x = bar_diameter if diameter_mode else bar_diameter / 2.0
    return ["G0 X{:g} Z5".format(x)]


# -----------------------------------------------------------------------------
# Proven programs — doc 40 §10.1 "even for the second run after one small edit"
# -----------------------------------------------------------------------------
def program_hash(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


class ProvenStore:
    """Which program texts have completed a supervised first run.

    Keyed on the text's hash, so any edit at all makes a program unproven.
    Kept per user under ~/.config, never in the repository.
    """

    def __init__(self, path=None):
        self.path = path or os.path.join(
            os.path.expanduser("~"), ".config", "emco-lathe", "proven.json")
        try:
            with open(self.path) as f:
                self._proven = json.load(f)
        except (OSError, ValueError):
            self._proven = {}

    def is_proven(self, text):
        return program_hash(text) in self._proven

    def mark_proven(self, text, name):
        self._proven[program_hash(text)] = name
        os.makedirs(os.path.dirname(self.path), exist_ok=True)
        tmp = self.path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(self._proven, f, indent=1, sort_keys=True)
        os.replace(tmp, self.path)
