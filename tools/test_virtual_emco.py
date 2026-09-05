#!/usr/bin/env python3
"""Unit tests for the VirtualEmco simulation core.

Run with no hardware, no pty and no pyserial:

    python3 tools/test_virtual_emco.py         # or: python3 -m unittest
"""

import importlib.util
import os
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "virtual_emco", os.path.join(_HERE, "virtual-emco.py"))
virtual_emco = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(virtual_emco)

VirtualEmco = virtual_emco.VirtualEmco
XON = virtual_emco.XON
XOFF = virtual_emco.XOFF


class Harness(unittest.TestCase):
    """Drives the core with a fake clock; collects all output."""

    def setUp(self):
        self.vm = VirtualEmco(now_ms=0.0)
        self.out = self.vm.step(0.0)  # banner + initial XON

    def send(self, text, at_ms):
        got = self.vm.feed(text.encode(), at_ms)
        self.out += got
        return got

    def tick(self, at_ms):
        got = self.vm.step(at_ms)
        self.out += got
        return got

    def reference(self, at_ms=0.0, settle_ms=30000.0):
        """Run the reference sequence and return the time it completed.

        The machine boots UNREFERENCED and soft limits are inactive until
        homed, exactly as on the real lathe — so any test that expects a
        soft-limit alarm has to reference first.
        """
        self.send("G28\r", at_ms)
        self.tick(settle_ms)
        self.assertTrue(self.vm.referenced, "reference sequence did not finish")
        return settle_ms


class TestParsing(Harness):

    def test_banner_and_initial_xon(self):
        self.assertIn(b"virtual", self.out.lower())
        self.assertIn(XON, self.out)

    def test_modal_line_acks_immediately(self):
        got = self.send("G21 G90\r", 10)
        self.assertIn(b"ok", got)

    def test_comments_and_blank_lines_ack(self):
        got = self.send("( face the part )\r", 10)
        self.assertIn(b"ok", got)
        got = self.send("N10 G90 ; absolute\r", 20)
        self.assertIn(b"ok", got)

    def test_unsupported_gcode_rejected(self):
        got = self.send("G33 Z-10 K1.5\r", 10)
        self.assertIn(b"error", got)
        self.assertIn(b"G33", got)

    def test_unsupported_word_rejected(self):
        got = self.send("G1 Q5 F100\r", 10)
        self.assertIn(b"error", got)

    def test_garbage_rejected(self):
        got = self.send("hello world\r", 10)
        self.assertIn(b"error", got)

    def test_g1_without_feed_rejected(self):
        got = self.send("G1 X5\r", 10)
        self.assertIn(b"error", got)
        self.assertIn(b"feed", got.lower())

    def test_lf_terminator_also_accepted(self):
        got = self.send("G90\n", 10)
        self.assertIn(b"ok", got)

    def test_spindle_needs_speed(self):
        got = self.send("M3\r", 10)
        self.assertIn(b"error", got)
        got = self.send("S1200 M3\r", 20)
        self.assertIn(b"ok", got)
        self.assertTrue(self.vm.spindle_on)
        self.assertEqual(self.vm.spindle_rpm, 1200)
        got = self.send("M5\r", 30)
        self.assertIn(b"ok", got)
        self.assertFalse(self.vm.spindle_on)


class TestMotionTiming(Harness):

    def test_rapid_takes_distance_over_rapid_rate(self):
        # 10 mm at the real rapid rate of 750 mm/min = 800 ms
        got = self.send("G0 X10\r", 0)
        self.assertNotIn(b"ok", got, "ok must not arrive before motion ends")
        got = self.tick(795)
        self.assertNotIn(b"ok", got, "ok arrived too early")
        got = self.tick(805)
        self.assertIn(b"ok", got, "ok missing after motion should be done")
        self.assertAlmostEqual(self.vm.x, 10.0)

    def test_feed_move_uses_f_word(self):
        # 6 mm at F600 = 600 ms
        self.send("G1 Z-6 F600\r", 0)
        self.assertNotIn(b"ok", self.tick(590))
        self.assertIn(b"ok", self.tick(610))
        self.assertAlmostEqual(self.vm.z, -6.0)

    def test_incremental_mode(self):
        self.send("G0 X5\r", 0)
        self.tick(1000)
        self.send("G91\r", 1000)
        self.send("G0 X2\r", 1000)
        self.tick(2000)
        self.assertAlmostEqual(self.vm.x, 7.0)

    def test_zero_length_move_acks_instantly(self):
        got = self.send("G0 X0 Z0\r", 0)
        self.assertIn(b"ok", got)

    def test_dwell(self):
        self.send("G4 P0.5\r", 0)
        self.assertNotIn(b"ok", self.tick(490))
        self.assertIn(b"ok", self.tick(510))

    def test_turret_index_delay(self):
        # station 1 -> 2, default 1.5 s
        got = self.send("T0202\r", 0)
        self.assertNotIn(b"ok", got)
        self.assertNotIn(b"ok", self.tick(1490))
        self.assertIn(b"ok", self.tick(1510))
        self.assertEqual(self.vm.tool, 2)

    def test_same_station_is_instant(self):
        got = self.send("T0101\r", 0)
        self.assertIn(b"ok", got)

    def test_status_reports_interpolated_position(self):
        self.send("G0 X10\r", 0)          # 800 ms total
        got = self.send("?", 400)          # halfway
        self.assertIn(b"<Run|", got)
        m = [p for p in got.split(b"|") if p.startswith(b"X:")]
        x = float(m[0][2:])
        self.assertAlmostEqual(x, 5.0, delta=0.1)

    def test_feed_hold_and_resume_stretch_the_move(self):
        self.send("G0 X10\r", 0)           # 800 ms of motion
        self.send("!", 100)                # hold after 100 ms
        got = self.tick(1000)              # long after it would have finished
        self.assertNotIn(b"ok", got, "motion completed during feed hold")
        self.send("~", 1000)               # resume; 700 ms remain
        self.assertNotIn(b"ok", self.tick(1690))
        self.assertIn(b"ok", self.tick(1710))


class TestFlowControlAndAlarms(Harness):

    def test_xoff_while_busy_then_xon_when_idle(self):
        got = self.send("G0 X10\r", 0)
        self.assertIn(XOFF, got, "no XOFF when motion started")
        got = self.tick(850)
        self.assertIn(b"ok", got)
        self.assertIn(XON, got, "no XON once idle again")

    def test_queued_lines_execute_in_order(self):
        self.send("G0 X5\r", 0)            # 400 ms
        self.send("G0 X10\r", 10)          # queued: +400 ms
        out = self.tick(390)
        self.assertEqual(out.count(b"ok"), 0)
        out = self.tick(410)
        self.assertEqual(out.count(b"ok"), 1)
        out = self.tick(820)
        self.assertEqual(out.count(b"ok"), 1)
        self.assertAlmostEqual(self.vm.x, 10.0)

    def test_soft_limit_trips_alarm_and_latches(self):
        t = self.reference()
        got = self.send("G0 X100\r", t)
        self.assertIn(b"ALARM:1", got)
        self.assertIn(XOFF, got)
        got = self.send("G0 X1\r", t + 10)
        self.assertIn(b"alarm lock", got)
        # Ctrl-X clears it
        got = self.send("\x18", t + 20)
        self.assertIn(b"RESET", got)
        self.assertIn(XON, got)
        self.send("G0 X1\r", t + 30)
        self.assertIn(b"ok", self.tick(t + 10000))

    def test_z_soft_limit(self):
        t = self.reference()
        got = self.send("G0 Z-500\r", t)
        self.assertIn(b"ALARM:2", got)

    def test_soft_limits_inactive_until_referenced(self):
        """Un-referenced, the control cannot know where the stops are, so an
        over-travel command is accepted and the carriage stalls silently."""
        got = self.send("G0 X100\r", 0)
        self.assertNotIn(b"ALARM", got)

    def test_abort_stops_motion_at_interpolated_position(self):
        self.send("G0 X10\r", 0)           # 800 ms
        self.send("\x18", 400)             # abort halfway
        self.assertEqual(self.vm.state, virtual_emco.IDLE)
        self.assertAlmostEqual(self.vm.x, 5.0, delta=0.1)
        got = self.tick(1000)
        self.assertNotIn(b"ok", got, "aborted move must not ack")

    def test_abort_kills_spindle_and_queue(self):
        self.send("S1000 M3\r", 0)
        self.send("G0 X10\r", 10)
        self.send("G0 X20\r", 20)
        self.send("\x18", 30)
        self.assertFalse(self.vm.spindle_on)
        self.assertEqual(self.vm.queue, [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
