#!/usr/bin/env python3
"""Unit tests for the emco_lathe screen's rules.

No LinuxCNC, no Qt, no display:

    python3 linuxcnc/sim/emco-pcturn50/tests/test_emco_rules.py   # or: python3 -m unittest
"""

import os
import sys
import tempfile
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_REPO = os.path.abspath(os.path.join(_HERE, "..", "..", "..", ".."))
sys.path.insert(0, os.path.join(_HERE, "..", "qtvcp", "screens", "emco_lathe"))

import emco_rules as R  # noqa: E402

# Matches emco-pcturn50.ini: [JOINT_0]HOME 48, [JOINT_1]HOME 0, TOOL_CHANGE_POSITION set.
SIM = R.Machine(x_retract=48.0, z_park=0.0, tool_change_retracts=True)
BARE = R.Machine(x_retract=48.0, z_park=0.0, tool_change_retracts=False)

HEADER = "( Stock : 12 mm alu )\n( Tools : T1 turning )\n"
GOOD_TAIL = "M5\nG53 G0 X96 Z0\nM30\n"


def errors(findings):
    return [f for f in findings if f.severity == R.ERROR]


class ReferenceGateTest(unittest.TestCase):
    def test_power_on_is_gated(self):
        g = R.ReferenceGate()
        self.assertIsNotNone(g.update(estop=True, machine_on=False, homed=[False, False]))
        self.assertIsNotNone(g.update(estop=False, machine_on=True, homed=[False, False]))

    def test_homing_satisfies(self):
        g = R.ReferenceGate()
        g.update(False, True, [False, False])
        g.update(False, True, [True, False])
        self.assertIsNone(g.update(False, True, [True, True]))
        self.assertFalse(g.position_lost)

    def test_estop_regates_even_though_linuxcnc_keeps_homed(self):
        g = R.ReferenceGate()
        g.update(False, True, [False, False])
        g.update(False, True, [True, True])
        g.update(True, False, [True, True])          # E-stop; homed flags survive
        reason = g.update(False, True, [True, True])  # reset + on
        self.assertIn("Position lost", reason)
        self.assertTrue(g.position_lost)

    def test_rehome_after_estop_satisfies(self):
        g = R.ReferenceGate()
        g.update(False, True, [False, False])
        g.update(False, True, [True, True])
        g.update(True, False, [True, True])
        g.update(False, True, [True, True])
        g.update(False, True, [False, False])  # unhomed / homing in progress
        self.assertIsNone(g.update(False, True, [True, True]))

    def test_homing_while_in_estop_does_not_count(self):
        g = R.ReferenceGate()
        g.update(True, False, [False, False])
        g.update(True, False, [True, True])
        self.assertIsNotNone(g.update(False, True, [True, True]))

    def test_machine_off_is_gated(self):
        g = R.ReferenceGate()
        g.update(False, True, [False, False])
        g.update(False, True, [True, True])
        self.assertIn("off", g.update(False, False, [True, True]))


class BeltTest(unittest.TestCase):
    def test_ranges_match_doc(self):
        self.assertEqual(R.belt_range(1), (130, 1300))
        self.assertEqual(R.belt_range(2), (200, 2000))
        self.assertEqual(R.belt_range(3), (300, 3000))

    def test_clamp(self):
        self.assertEqual(R.clamp_rpm(1, 3000), 1300)
        self.assertEqual(R.clamp_rpm(3, 100), 300)
        self.assertEqual(R.clamp_rpm(2, 800), 800)

    def test_violations(self):
        self.assertIsNone(R.spindle_violation(2, 0, threading=False))
        self.assertIsNone(R.spindle_violation(2, 1200, threading=False))
        self.assertIsNone(R.spindle_violation(2, -1200, threading=False))  # M4
        self.assertIn("belt 1", R.spindle_violation(1, 1400, threading=False))
        self.assertIn("400", R.spindle_violation(2, 500, threading=True))
        self.assertIsNone(R.spindle_violation(2, 350, threading=True))


class PreflightTest(unittest.TestCase):
    def test_example_programs_are_clean(self):
        for name in ("tutorial-part.ngc", "stepped-shaft.ngc"):
            with open(os.path.join(_REPO, "gcode", name)) as f:
                text = f.read()
            with self.subTest(program=name):
                self.assertEqual(R.preflight(text, belt=2, machine=SIM), [])

    def test_stepped_shaft_on_belt_1_flags_speed(self):
        with open(os.path.join(_REPO, "gcode", "stepped-shaft.ngc")) as f:
            text = f.read()
        errs = errors(R.preflight(text, belt=1, machine=SIM))
        self.assertTrue(any("S1400" in e.message for e in errs))

    def test_half_retract_under_g7(self):
        prog = HEADER + "G21 G7\nG53 G0 X48\nT1 M6\n" + GOOD_TAIL
        errs = errors(R.preflight(prog, 2, SIM))
        self.assertEqual(len(errs), 1)
        self.assertIn("half way", errs[0].message)
        self.assertEqual(errs[0].line, 4)

    def test_x48_is_full_retract_in_radius_mode(self):
        prog = HEADER + "G21 G8\nG53 G0 X48\nT1 M6\nM5\nG53 G0 X48 Z0\nM30\n"
        self.assertEqual(R.preflight(prog, 2, SIM), [])

    def test_first_move_not_a_retract(self):
        prog = HEADER + "G21 G7\nG0 X14 Z1\n" + GOOD_TAIL
        errs = errors(R.preflight(prog, 2, SIM))
        self.assertIn("G53 G0 X96", errs[0].message)

    def test_tool_change_with_spindle_on(self):
        prog = HEADER + "G21 G7\nG53 G0 X96\nS800 M3\nT3 M6\n" + GOOD_TAIL
        errs = errors(R.preflight(prog, 2, SIM))
        self.assertTrue(any("spindle running" in e.message for e in errs))

    def test_tool_change_without_retract_when_ini_does_not_retract(self):
        prog = HEADER + "G21 G7\nG53 G0 X96\nG0 X14 Z1\nT3 M6\n" + GOOD_TAIL
        self.assertEqual(errors(R.preflight(prog, 2, SIM)), [])
        errs = errors(R.preflight(prog, 2, BARE))
        self.assertTrue(any("retract" in e.message for e in errs))

    def test_threading_speed(self):
        prog = HEADER + "G21 G7\nG53 G0 X96\nS500 M3\nG76 P1 Z-10 I-1 J0.1 K0.6\n" + GOOD_TAIL
        errs = errors(R.preflight(prog, 2, SIM))
        self.assertTrue(any("Threading" in e.message for e in errs))

    def test_css_s_is_not_rpm(self):
        prog = HEADER + "G21 G7\nG53 G0 X96\nG96 S5000 D2000 M3\nG97\n" + GOOD_TAIL
        self.assertEqual(errors(R.preflight(prog, 2, SIM)), [])

    def test_house_style_warnings(self):
        prog = "G53 G0 X48\nG0 X10 Z1\nM30\n"
        f = R.preflight(prog, 2, SIM)
        self.assertEqual(errors(f), [])
        msgs = " ".join(x.message for x in f)
        self.assertIn("Stock", msgs)
        self.assertIn("G21", msgs)
        self.assertIn("does not finish", msgs)

    def test_subroutine_bodies_are_not_first_move(self):
        prog = (HEADER + "o<r> sub\n G0 X[#1] Z0.5\no<r> endsub\n"
                "G21 G7\nG53 G0 X96\no<r> call [10]\n" + GOOD_TAIL)
        self.assertEqual(R.preflight(prog, 2, SIM), [])


class TouchoffTest(unittest.TestCase):
    def test_work_z(self):
        self.assertEqual(R.touchoff_work_z(), ["G10 L20 P1 Z0"])

    def test_tool_x_is_radius_and_restores_g7(self):
        self.assertEqual(R.touchoff_tool_x(1, 11.98, diameter_mode=True),
                         ["G8", "G10 L10 P1 X5.9900", "G7"])
        self.assertEqual(R.touchoff_tool_x(3, 12, diameter_mode=False),
                         ["G8", "G10 L10 P3 X6.0000"])
        with self.assertRaises(ValueError):
            R.touchoff_tool_x(1, 0, True)

    def test_tool_z_and_verify(self):
        self.assertEqual(R.touchoff_tool_z(2), ["G10 L10 P2 Z0"])
        self.assertEqual(R.verify_work_z(), ["G0 Z5"])
        self.assertEqual(R.verify_tool(True), ["G0 X20 Z5"])
        self.assertEqual(R.verify_tool(False), ["G0 X10 Z5"])


class ProvenStoreTest(unittest.TestCase):
    def test_any_edit_unproves(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "sub", "proven.json")
            s = R.ProvenStore(path)
            self.assertFalse(s.is_proven("G0 X1\n"))
            s.mark_proven("G0 X1\n", "a.ngc")
            self.assertTrue(R.ProvenStore(path).is_proven("G0 X1\n"))
            self.assertFalse(R.ProvenStore(path).is_proven("G0 X1 \n"))

    def test_corrupt_file_is_empty(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "proven.json")
            with open(path, "w") as f:
                f.write("{not json")
            self.assertFalse(R.ProvenStore(path).is_proven("x"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
