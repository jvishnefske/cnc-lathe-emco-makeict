#!/usr/bin/env python3
"""Tests for tools/winnc-485-log.py, the reader for WinNC's RS-485 recording.

    python3 tools/test_winnc_485_log.py        # or: python3 -m unittest

The excerpt below is the first messages of the public `485tr-1.txt` (a PC Mill
50, LinuxCNC forum, docs/31-rs485-message-layer.md), copied byte for byte
except that its non-ASCII placeholder characters are dots. Set WINNC_485_LOG to
a copy of the whole file to also check the invariants doc 31 states about it.
"""

import importlib.util
import os
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("winnc_485_log", os.path.join(_HERE, "winnc-485-log.py"))
log = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(log)

EXCERPT = """\
==============================================================================
===                  Aufzeichnung der RS485 Schnittstelle                  ===
==============================================================================

O ------ 485 Out       Nr.:     1   Zeit:    0.000 ------
   L= 58   Z=  0   Q=250   Code= 49=1
    47 E1 7A 14  47 E1 7A 14  47 E1 7A 14  00 00 00 00      G.z.G.z.G.z.....
    00 80 0C 00  00 80 0C 00  00 80 0C 00  00 00 00 00      ................
    0B B6 60 0B  7C 86 5D 01  FA 10 0A 00  C2 02 01 03      ..`.|.].........
    58 59 5A 49  88 13                                      XYZI..
O ------ 485 Out       Nr.:     2   Zeit:    0.000 ------
   L=  6   Z=  0   Q=250   Code= 50=2
    0D 61                                                   .a
I ------ 485 In        Nr.:     1   Zeit:    0.055 ------
   L=  7   Z=250   Q=  0   FCode= 527 .. AC-Eingaenge
    C8 00                                                   ..
I ------ 485 In        Nr.:     2   Zeit:    0.068 ------
   L=  6   Z=250   Q=  0   FCode= 503 .. AC-Reset-Quit
    52                                                      R
O ------ 485 Out       Nr.:    15   Zeit:    0.082 ------
   L= 10   Z= 16   Q=250   Code= 73=I
    41 42 43 44  45 46                                      ABCDEF
O ------ 485 Out       Nr.:    16   Zeit:    0.151 ------
   L=  4   Z=  0   Q=250   Code= 99=c
I ------ 485 In        Nr.:   400   Zeit:   85.343 ------
   L=  5   Z=250   Q= 16   FCode= 567 ..
"""


class Parse(unittest.TestCase):
    def setUp(self):
        self.msgs = log.parse(EXCERPT)

    def test_every_message_is_read(self):
        self.assertEqual(len(self.msgs), 7)
        self.assertEqual([m.direction for m in self.msgs], ["out", "out", "in", "in", "out", "out", "in"])

    def test_header_fields(self):
        m = self.msgs[2]
        self.assertEqual((m.nr, m.time, m.length, m.dest, m.src), (1, 0.055, 7, 250, 0))
        self.assertEqual((m.code, m.name, m.fcode), (527, "AC-Eingaenge", True))
        o = self.msgs[1]
        self.assertEqual((o.code, o.name, o.fcode), (50, "2", False))

    def test_multi_line_payload(self):
        p = self.msgs[0].payload
        self.assertEqual(len(p), 54)
        self.assertEqual(p[:4], bytes.fromhex("47E17A14"))
        self.assertEqual(p[-6:], bytes.fromhex("58595A498813"))

    def test_ascii_column_that_looks_like_hex_is_not_payload(self):
        # "ABCDEF" in the ASCII column is hex digits; only the hex column counts.
        self.assertEqual(self.msgs[4].payload, b"ABCDEF")

    def test_empty_payloads_and_empty_names(self):
        self.assertEqual(self.msgs[5].payload, b"")
        self.assertEqual((self.msgs[6].code, self.msgs[6].name, self.msgs[6].payload), (567, "", b""))

    def test_length_counts_four_bytes_out_and_five_in(self):
        for m in self.msgs:
            self.assertEqual(m.overhead, 4 if m.direction == "out" else 5, m)

    def test_addresses(self):
        self.assertEqual(log.addr(250), "PC")
        self.assertEqual(log.addr(0), "AC")
        self.assertEqual(log.addr(16), "SPS")
        self.assertEqual(log.addr(7), "7")


class Errors(unittest.TestCase):
    def test_garbage_in_a_payload_names_its_line(self):
        bad = EXCERPT.replace("    0D 61 ", "    0D zz ")
        with self.assertRaisesRegex(log.LogError, r"line 13: expected hex"):
            log.parse(bad)

    def test_header_without_fields(self):
        with self.assertRaisesRegex(log.LogError, "no L=/Z=/Q= line"):
            log.parse("O ------ 485 Out       Nr.:     1   Zeit:    0.000 ------\n")

    def test_text_before_first_header(self):
        with self.assertRaisesRegex(log.LogError, "before the first message header"):
            log.parse("hello\n")


class Table(unittest.TestCase):
    def test_rows_group_by_direction_and_code(self):
        rows = log.code_table(log.parse(EXCERPT + EXCERPT.split("\n", 4)[4]))
        keys = [(r["direction"], r["code"]) for r in rows]
        self.assertEqual(keys, [("out", 49), ("out", 50), ("in", 527), ("in", 503),
                                ("out", 73), ("out", 99), ("in", 567)])
        self.assertTrue(all(r["count"] == 2 for r in rows))

    def test_markdown_renders_routes_and_spans(self):
        md = log.render_table(log.code_table(log.parse(EXCERPT)))
        self.assertIn("| out | 73 `I` |  | 1 | PC→SPS | 6 | 4 |", md)
        self.assertIn("| in | 527 | AC-Eingaenge | 1 | AC→PC | 2 | 5 |", md)
        self.assertEqual(log._span({3, 4, 2}), "2–4")
        self.assertEqual(log._span({1, 6}), "1, 6")

    def test_json_round_trip(self):
        d = log.parse(EXCERPT)[1].to_json()
        self.assertEqual(d["payload"], "0d61")
        self.assertEqual(d["code"], 50)


@unittest.skipUnless(os.environ.get("WINNC_485_LOG"), "set WINNC_485_LOG to the full 485tr-1.txt")
class FullRecording(unittest.TestCase):
    """The numbers docs/31 quotes from the whole recording."""

    def test_doc31_invariants(self):
        with open(os.environ["WINNC_485_LOG"], encoding="latin-1") as f:
            msgs = log.parse(f.read())
        self.assertEqual(len(msgs), 778)
        self.assertEqual(sum(m.direction == "out" for m in msgs), 255)
        self.assertEqual({m.overhead for m in msgs if m.direction == "out"}, {4})
        self.assertEqual({m.overhead for m in msgs if m.direction == "in"}, {5})
        self.assertEqual({m.src for m in msgs if m.direction == "out"}, {250})
        self.assertEqual({m.dest for m in msgs if m.direction == "in"}, {250})
        self.assertEqual({m.dest for m in msgs if m.direction == "out"}, {0, 16})
        fcodes = {m.code for m in msgs if m.direction == "in"}
        self.assertEqual((min(fcodes), max(fcodes)), (500, 567))


if __name__ == "__main__":
    unittest.main()
