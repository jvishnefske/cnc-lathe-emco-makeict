#!/usr/bin/env python3
"""Unit tests for the RS-485 bridge's record decoder.

Runs with no hardware, no pty, no pyserial and no Rust toolchain:

    python3 tools/test_emco_bridge.py         # or: python3 -m unittest

Two kinds of case here, and the second is the one that matters.

The FIRST kind is ordinary decoder testing: timestamps, flags, direction,
resynchronisation after a truncated frame. Written against locally built
frames, so a failure points at one line.

The SECOND kind is the CROSS-LANGUAGE CORPUS. There are two implementations of
this record format — the Rust encoder the bridge runs, and the Python decoder
in `tools/emco-capture.py` that reads a capture. Two implementations of a wire
format drift, they drift silently, and the moment anybody notices is the moment
there is a transcript from the actual machine that one of them reads wrong.
So `firmware/bridge-core/tests/golden.rs` writes a fixed corpus to
`golden.bin`, a Rust test asserts those bytes have not changed, and the cases
below assert this decoder reproduces exactly the records that produced them.
Neither side can move without the other going red.

Regenerate the corpus after a deliberate format change (and bump
`PROTOCOL_VERSION` in both places when you do):

    cd firmware/bridge-core && BRIDGE_WRITE_GOLDEN=1 cargo test --test golden
"""

import importlib.util
import os
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_ROOT = os.path.dirname(_HERE)
_spec = importlib.util.spec_from_file_location(
    "emco_capture", os.path.join(_HERE, "emco-capture.py"))
capture = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(capture)

GOLDEN = os.path.join(_ROOT, "firmware", "bridge-core", "tests", "golden.bin")


def frame(payload):
    """One record's payload, framed the way the bridge frames it."""
    return capture.cobs_encode(payload) + b"\x00"


def rx(micros, data, gaps=(), flags=0, kind=capture.KIND_RX):
    """Builds an RX/TX payload by hand, from the spec rather than from code.

    Deliberately not using an encoder shared with the decoder: a test that
    round-trips one implementation against itself proves that it is
    self-consistent, which is not the property anybody needs.
    """
    payload = bytes([kind]) + micros.to_bytes(4, "little") + bytes([flags, len(data)])
    payload += bytes(data)
    for gap in gaps:
        payload += int(gap).to_bytes(2, "little")
    return payload


class TestTheDecoderReadsTheFormat(unittest.TestCase):

    def setUp(self):
        self.decoder = capture.Decoder()

    def test_a_burst_keeps_its_inter_byte_gaps(self):
        """The measurement the whole device exists to make.

        A host timestamps at `read()`, which is after USB's 1 ms frames,
        `cdc_acm` buffering and scheduler latency. At 9600 baud a character is
        1042 microseconds, so host timing destroys precisely this.
        """
        records = self.decoder.feed(frame(rx(1_000, b"EMCO", (1042, 1042, 1042))))
        self.assertEqual(1, len(records))
        record = records[0]
        self.assertEqual(b"EMCO", record.data)
        self.assertEqual([0, 1042, 1042, 1042], record.gaps)
        self.assertEqual(1_000, record.micros)
        self.assertEqual([1_000, 2_042, 3_084, 4_126], record.byte_times())

    def test_direction_is_carried_by_the_record_and_not_inferred(self):
        """On 3-wire half-duplex, an echo and a reply are the same bytes."""
        records = self.decoder.feed(
            frame(rx(1_000, b"\xaa", kind=capture.KIND_TX))
            + frame(rx(2_000, b"\x01", kind=capture.KIND_RX)))
        self.assertEqual(["TX", "RX"], [r.name for r in records])

    def test_a_framing_error_arrives_as_a_record_of_one_with_its_own_time(self):
        """Doc 30 §2.2 A1: the wire may not be async-UART framed at all.

        So an error at a known timestamp is a POSITIVE finding. A decoder that
        reported it as "somewhere in these six bytes" would throw the finding
        away.
        """
        records = self.decoder.feed(
            frame(rx(500_000, b"\x7f", flags=capture.FLAG_FRAMING | capture.FLAG_NOISE)))
        self.assertEqual(1, len(records))
        self.assertEqual(500_000, records[0].micros)
        self.assertEqual(["FRAMING", "NOISE"], capture.flag_names(records[0].flags))

    def test_a_dropped_record_gap_is_visible_rather_than_silent(self):
        """A transcript short by three records is worse than no transcript.

        Somebody will read the gap as a quiet bus, and "the bus was quiet" is a
        finding this project would then write down.
        """
        records = self.decoder.feed(frame(rx(1, b"\xee", flags=capture.FLAG_DROPPED)))
        self.assertIn("DROPPED", capture.flag_names(records[0].flags))

    def test_an_event_carries_a_code_and_a_value(self):
        payload = bytes([capture.KIND_EVENT]) + (7).to_bytes(4, "little")
        payload += bytes([0x01]) + (1).to_bytes(4, "little")
        records = self.decoder.feed(frame(payload))
        self.assertEqual("EVENT", records[0].name)
        self.assertEqual(0x01, records[0].code)
        self.assertEqual(capture.PROTOCOL_VERSION, records[0].argument)

    def test_a_status_names_the_build_that_produced_the_transcript(self):
        """The evidence half of listen-only.

        The other halves are a compile-time feature and a link on the adapter
        that is not fitted. This is the half that survives into the log.
        """
        payload = bytes([capture.KIND_STATUS]) + (9).to_bytes(4, "little")
        payload += bytes([capture.FLAG_LISTEN_ONLY])
        for value in (164, 0, 0, 1):
            payload += value.to_bytes(4, "little")
        payload += b"emco-rs485-bridge 0.1.0 listen-only 9600 8N1"
        record = self.decoder.feed(frame(payload))[0]
        self.assertEqual("STATUS", record.name)
        self.assertIn("LISTEN-ONLY", capture.flag_names(record.flags))
        self.assertNotIn("TX-ARMED", capture.flag_names(record.flags))
        self.assertEqual(164, record.counters["rx_bytes"])
        self.assertEqual(1, record.counters["error_bytes"])
        self.assertIn("listen-only", record.text)

    def test_records_arriving_in_pieces_are_assembled(self):
        """USB delivers whatever fits a packet, not whatever fits a record."""
        stream = frame(rx(1, b"ab", (100,))) + frame(rx(2, b"cd", (100,)))
        seen = []
        for i in range(len(stream)):
            seen += self.decoder.feed(stream[i:i + 1])
        self.assertEqual([b"ab", b"cd"], [r.data for r in seen])

    def test_joining_a_running_transcript_resynchronises_at_the_next_zero(self):
        """The everyday case: the bridge was already running.

        Losing the frame you cut into is unavoidable. Losing everything after
        it is the failure this is guarding against, and it is what a
        length-prefixed format would do.
        """
        stream = (frame(rx(1, b"aaaa", (10, 10, 10)))
                  + frame(rx(2, b"bbbb", (10, 10, 10)))
                  + frame(rx(3, b"cccc", (10, 10, 10))))
        records = self.decoder.feed(stream[5:])
        # The frame we cut into is lost, and which way it is lost depends on
        # where the cut fell: a partial frame is sometimes still valid COBS and
        # then fails as a RECORD instead. Both are counted, and neither is
        # allowed to be silent.
        self.assertEqual(1, self.decoder.resyncs + self.decoder.unknown)
        self.assertEqual([b"bbbb", b"cccc"], [r.data for r in records])

    def test_every_cut_point_loses_only_what_it_cut_away(self):
        """Swept rather than sampled, because the interesting cuts are rare.

        A cut landing on a COBS code byte behaves differently from one landing
        in a run, and picking one offset by hand tests whichever of those the
        author happened to pick. The rule being checked is exact: every frame
        that BEGINS at or after the cut must come back, whole and in order.
        Losing more than that is a decoder one byte out forever, which is what
        a length-prefixed format would give you.
        """
        frames = [frame(rx(i, bytes([0x61 + i]) * 4, (10, 10, 10)))
                  for i in range(6)]
        stream = b"".join(frames)
        starts, at = [], 0
        for one in frames:
            starts.append(at)
            at += len(one)

        for cut in range(1, len(stream)):
            survivors = [i for i, begins in enumerate(starts) if begins >= cut]
            expected = [bytes([0x61 + i]) * 4 for i in survivors]
            decoder = capture.Decoder()
            records = decoder.feed(stream[cut:])
            with self.subTest(cut=cut):
                self.assertEqual(expected, [r.data for r in records])

    def test_a_truncated_frame_is_counted_rather_than_guessed_at(self):
        good = frame(rx(1, b"ab", (10,)))
        truncated = good[:len(good) - 3] + b"\x00"
        records = self.decoder.feed(truncated + good)
        self.assertEqual(1, self.decoder.resyncs)
        self.assertEqual([b"ab"], [r.data for r in records])

    def test_a_record_kind_from_a_newer_firmware_is_reported_not_invented(self):
        payload = bytes([0x7E]) + (1).to_bytes(4, "little") + bytes([0, 0])
        records = self.decoder.feed(frame(payload) + frame(rx(2, b"z")))
        self.assertEqual(1, self.decoder.unknown)
        self.assertEqual([b"z"], [r.data for r in records])

    def test_a_record_that_claims_more_bytes_than_it_carries_is_refused(self):
        payload = bytes([capture.KIND_RX]) + (1).to_bytes(4, "little")
        payload += bytes([0, 40]) + b"only three"
        records = self.decoder.feed(frame(payload))
        self.assertEqual([], records)
        self.assertEqual(1, self.decoder.unknown)


class TestTheClockIsUnwrapped(unittest.TestCase):
    """The bridge's counter is 32 bits of microseconds: ~71.6 minutes.

    A protocol-discovery capture is routinely longer than that, so the decoder
    un-wraps. Getting this wrong produces a transcript with a 4295-second gap
    in the middle of it, which reads as a machine that stopped.
    """

    def test_it_carries_on_counting_past_the_wrap(self):
        decoder = capture.Decoder()
        records = decoder.feed(
            frame(rx(0xFFFFFF00, b"w"))
            + frame(rx(0xFFFFFFFF, b"r"))
            + frame(rx(200, b"p")))
        times = [r.micros_abs for r in records]
        self.assertEqual([0xFFFFFF00, 0xFFFFFFFF, 0xFFFFFFFF + 201], times)
        self.assertTrue(all(b > a for a, b in zip(times, times[1:])),
                        f"the unwrapped clock went backwards: {times}")

    def test_the_raw_counter_is_still_available(self):
        """So a transcript can be compared against the device's own numbers."""
        decoder = capture.Decoder()
        record = decoder.feed(frame(rx(0xFFFFFFFF, b"r")))[0]
        self.assertEqual(0xFFFFFFFF, record.micros)


@unittest.skipUnless(os.path.exists(GOLDEN),
                     f"{GOLDEN} is missing; build it with "
                     "`cd firmware/bridge-core && BRIDGE_WRITE_GOLDEN=1 "
                     "cargo test --test golden`")
class TestTheRustEncoderAndThisDecoderAgree(unittest.TestCase):
    """The corpus, decoded here and asserted record by record.

    The expectations below are written from the SCENARIO in `golden.rs`, not
    from what this decoder happens to produce. That is the point: if the two
    implementations drift, one of them fails against a description of what the
    bridge was supposed to have seen.
    """

    @classmethod
    def setUpClass(cls):
        cls.decoder = capture.Decoder()
        with open(GOLDEN, "rb") as handle:
            cls.records = cls.decoder.feed(handle.read())

    def test_nothing_needed_resynchronising_and_nothing_was_unknown(self):
        self.assertEqual(0, self.decoder.resyncs)
        self.assertEqual(0, self.decoder.unknown)
        self.assertEqual(b"", bytes(self.decoder._pending),
                         "the corpus ended mid-frame")

    def test_it_opens_by_naming_the_format_it_is_in(self):
        first = self.records[0]
        self.assertEqual("EVENT", first.name)
        self.assertEqual(0x01, first.code)                    # BOOT
        self.assertEqual(capture.PROTOCOL_VERSION, first.argument,
                         "the corpus was written by a firmware speaking a "
                         "different version of this format than this decoder")

    def test_the_clean_burst_is_one_record_at_a_9600_baud_character_time(self):
        record = self.records[1]
        self.assertEqual(b"EMCO?", record.data)
        self.assertEqual(1_000, record.micros)
        self.assertEqual([0, 1042, 1042, 1042, 1042], record.gaps)
        self.assertEqual(0, record.flags)

    def test_zero_bytes_survive_the_framing(self):
        """COBS exists for exactly this, and a capture is full of zeros."""
        zeros = [r for r in self.records if r.data.startswith(b"\x00")]
        self.assertTrue(zeros, "the corpus lost its zero bytes")
        self.assertEqual(b"\x00", zeros[0].data)
        self.assertEqual(b"\x00\xff", zeros[1].data)
        self.assertEqual([0, 100], zeros[1].gaps)

    def test_the_framing_error_stands_alone_with_its_own_timestamp(self):
        errors = [r for r in self.records
                  if r.flags & (capture.FLAG_FRAMING | capture.FLAG_PARITY)]
        self.assertEqual(1, len(errors))
        self.assertEqual(500_000, errors[0].micros)
        self.assertEqual(b"\x7f", errors[0].data)
        self.assertEqual(["FRAMING", "NOISE"], capture.flag_names(errors[0].flags))

    def test_our_own_transmission_is_tagged_and_the_reply_is_not(self):
        sent = [r for r in self.records if r.name == "TX"]
        self.assertEqual(1, len(sent))
        self.assertEqual(b"\xaa\x55", sent[0].data)
        self.assertEqual([0, 800], sent[0].gaps)
        after = self.records[self.records.index(sent[0]) + 1]
        self.assertEqual("RX", after.name)
        self.assertEqual(b"\x01", after.data)

    def test_the_record_spanning_the_counter_wrap_reads_as_three_close_bytes(self):
        wrapped = next(r for r in self.records if r.data == b"wrp")
        self.assertEqual([0, 500, 201], wrapped.gaps,
                         "the wrap turned a 500 us gap into a 71-minute one")

    def test_the_longest_record_is_the_buffer_limit_and_carries_no_flag(self):
        longest = max(self.records, key=lambda r: len(r.data))
        self.assertEqual(64, len(longest.data))
        self.assertEqual(0, longest.flags,
                         "a buffer limit was reported as a bus condition")

    def test_the_overflow_gap_is_flagged_on_the_record_after_it(self):
        dropped = [r for r in self.records if r.flags & capture.FLAG_DROPPED]
        self.assertEqual(1, len(dropped))
        self.assertEqual(b"\xee", dropped[0].data)

    def test_it_closes_with_counters_that_add_up(self):
        status = self.records[-1]
        self.assertEqual("STATUS", status.name)
        self.assertIn("LISTEN-ONLY", capture.flag_names(status.flags))
        received = sum(len(r.data) for r in self.records if r.name == "RX")
        sent = sum(len(r.data) for r in self.records if r.name == "TX")
        # The counters count everything the bridge SAW; the transcript holds
        # everything it managed to SEND. The difference is exactly the bytes in
        # the records it dropped, which is the accounting that makes a gap
        # auditable rather than merely visible.
        self.assertEqual(status.counters["rx_bytes"],
                         received + status.counters["dropped_bytes"])
        self.assertEqual(sent, status.counters["tx_bytes"])
        self.assertEqual(1, status.counters["error_bytes"])
        self.assertIn("listen-only", status.text)

    def test_the_summary_says_something_a_findings_log_can_use(self):
        text = capture.summarise(self.decoder, self.records)
        self.assertIn("UART error flag", text)
        self.assertIn("gap(s)", text)
        self.assertIn("median inter-byte gap", text)


class TestTheSimulatorProducesADecodableStream(unittest.TestCase):
    """`tools/virtual-bridge.py`, decoded by the tool that reads real captures.

    This is what makes the framing specification executable before a board
    exists: the host half of the toolchain is exercised end to end, on a fake
    clock, with nothing plugged in. It says nothing about the real bus — the
    traffic is invented and labelled as such — and everything about whether the
    two halves of the tooling fit together.
    """

    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location(
            "virtual_bridge", os.path.join(_HERE, "virtual-bridge.py"))
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    def produce(self, polls=30, listen_only=True):
        bridge = self.module.VirtualBridge(listen_only=listen_only)
        chunks = []
        self.module.run(bridge, 0, False, chunks.append, lambda data: None,
                        deadline_polls=polls)
        return bridge, b"".join(chunks)

    def test_everything_it_emits_decodes(self):
        _bridge, stream = self.produce()
        decoder = capture.Decoder()
        records = decoder.feed(stream)
        self.assertEqual(0, decoder.resyncs)
        self.assertEqual(0, decoder.unknown)
        self.assertEqual(b"", bytes(decoder._pending), "it ended mid-frame")
        self.assertGreater(len(records), 30)

    def test_it_opens_by_naming_the_format_and_the_build(self):
        _bridge, stream = self.produce()
        records = capture.Decoder().feed(stream)
        self.assertEqual("EVENT", records[0].name)
        self.assertEqual(capture.PROTOCOL_VERSION, records[0].argument)
        self.assertEqual("STATUS", records[1].name)
        self.assertIn("SIMULATED", records[1].text,
                      "a simulated transcript must say so in its own status")

    def test_it_exercises_every_record_kind_and_the_awkward_flags(self):
        """A simulator that quietly stopped covering a case is a simulator
        that guarantees the tooling handles nothing."""
        _bridge, stream = self.produce(polls=40, listen_only=False)
        records = capture.Decoder().feed(stream)
        kinds = {r.name for r in records}
        self.assertEqual({"RX", "TX", "EVENT", "STATUS"}, kinds)
        flags = 0
        for record in records:
            flags |= record.flags
        for bit, name in ((capture.FLAG_FRAMING, "FRAMING"),
                          (capture.FLAG_NOISE, "NOISE"),
                          (capture.FLAG_DROPPED, "DROPPED"),
                          (capture.FLAG_TX_ARMED, "TX-ARMED")):
            self.assertTrue(flags & bit, f"the simulator never produced {name}")

    def test_the_inter_byte_gaps_recover_the_bit_rate(self):
        """The one measurement a USB dongle plus pyserial cannot give.

        A host timestamps at `read()`, after USB's 1 ms frames; at 9600 baud a
        character is 1042 microseconds, so the number below would be noise.
        Here it comes back as the bit rate, which is what a first capture from
        the machine is supposed to be able to tell you.
        """
        _bridge, stream = self.produce()
        records = capture.Decoder().feed(stream)
        gaps = [g for r in records if r.name in ("RX", "TX") for g in r.gaps[1:]]
        self.assertTrue(gaps)
        gaps.sort()
        median = gaps[len(gaps) // 2]
        self.assertAlmostEqual(9600, 10_000_000 / median, delta=100)

    def test_its_counters_account_for_what_it_dropped(self):
        bridge, stream = self.produce()
        records = capture.Decoder().feed(stream)
        status = records[-1]
        received = sum(len(r.data) for r in records if r.name == "RX")
        self.assertEqual(status.counters["rx_bytes"],
                         received + status.counters["dropped_bytes"])
        self.assertGreater(status.counters["dropped_bytes"], 0,
                           "the simulator never exercised the dropped path")

    def test_the_raw_port_carries_the_same_bytes_with_none_of_the_evidence(self):
        """Which is the whole argument for the second port.

        `emco-listen.py` reads this one and is not touched; everything that
        makes a capture worth having — the timestamps, the direction, the
        flags — is on the other one.
        """
        bridge = self.module.VirtualBridge()
        bridge.exchange()
        raw = bridge.raw()
        self.assertIn(self.module.POLL, raw)
        self.assertNotIn(b"\x01\x00\x00\x00", raw[:8],
                         "the raw port grew a header it must not have")


if __name__ == "__main__":
    unittest.main()
