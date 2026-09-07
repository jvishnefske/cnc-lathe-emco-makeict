//! Where one record ends and the next begins.
//!
//! Bytes on a UART arrive one at a time and a transcript of one-byte records
//! would be correct and useless: the interesting structure on a polled
//! multidrop bus is which bytes belong together, and the only evidence of that
//! available at the wire is the gap between them.
//!
//! The rules, in the order they are applied:
//!
//! 1. **A byte with any error flag stands alone.** It flushes whatever is
//!    open, is emitted as a record of one byte, and carries exactly its own
//!    flags. This is what makes a framing or parity error a datum with a
//!    timestamp rather than a smear across a record — and per `docs/30` §2.2
//!    (A1) that datum is a positive finding about whether the wire is
//!    async-UART framed at all.
//! 2. **An idle gap ends a record.** The gap is a multiple of the character
//!    time at the configured bit rate, so "the bus went quiet" means the same
//!    thing at 9600 and at 115200.
//! 3. **A full record ends.** [`MAX_BYTES`] is a buffer limit, not a semantic
//!    one, and a record ended by it says so with no flag — the next record's
//!    first gap is small, which is how a host tells the two apart.
//! 4. **A byte in the other direction ends a record**, handled in
//!    [`crate::Bridge`] rather than here, because only it sees both.
//!
//! The clock is a plain `u32` of microseconds, free-running and wrapping every
//! ~71.6 minutes. Wrapping arithmetic is used throughout, so a record that
//! spans the wrap is still correct; the HOST un-wraps the absolute timestamps,
//! because it is the one with room for a 64-bit counter and a session length.

use crate::record::{Record, MAX_BYTES};

/// How the segmenter decides a gap is a gap.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// A gap of at least this many microseconds ends a record.
    pub gap_micros: u32,
}

impl Config {
    /// The gap that means "quiet" at a given bit rate.
    ///
    /// Three and a half character times at 8N1 (10 bits per character), which
    /// is the conventional idle-line threshold and is close to what an STM32's
    /// own IDLE-line detection reports. Being derived from the bit rate rather
    /// than fixed is the point: 3 ms is a long silence at 115200 and less than
    /// three characters at 9600.
    pub const fn at_baud(baud: u32) -> Self {
        let bit_micros = 1_000_000 / baud;
        Self { gap_micros: bit_micros * 35 }
    }

    pub const fn with_gap(gap_micros: u32) -> Self {
        Self { gap_micros }
    }
}

/// One direction's worth of segmentation.
pub struct Segmenter {
    kind: u8,
    config: Config,
    open: Record,
    /// The arrival time of the most recent byte in `open`.
    last: u32,
    has_open: bool,
}

impl Segmenter {
    pub const fn new(kind: u8, config: Config) -> Self {
        Self {
            kind,
            config,
            open: Record::empty(kind),
            last: 0,
            has_open: false,
        }
    }

    /// One byte. Returns a record if this byte closed one.
    ///
    /// Note the order: the byte that CLOSES a record is not in it. A gap means
    /// the previous run ended, so the new byte starts the next record — which
    /// is why the returned record and the new open record are both correct
    /// after a single call.
    pub fn push(&mut self, micros: u32, byte: u8, flags: u8) -> Option<Record> {
        let gap = micros.wrapping_sub(self.last);
        let boundary = !self.has_open
            || flags != 0
            || self.open.flags != 0
            || gap >= self.config.gap_micros
            || self.open.len as usize >= MAX_BYTES;

        let completed = if boundary { self.take() } else { None };

        if !self.has_open {
            self.open = Record::empty(self.kind);
            self.open.micros = micros;
            self.has_open = true;
        }
        let at = self.open.len as usize;
        self.open.data[at] = byte;
        self.open.gaps[at] = if at == 0 {
            0
        } else {
            // Saturating: a gap this wide inside a record cannot happen while
            // rule 2 holds, and if it somehow does, a clamped number is a
            // better answer than a wrapped one.
            gap.min(u16::MAX as u32) as u16
        };
        self.open.len += 1;
        self.open.flags |= flags;
        self.last = micros;
        // A record holding an error byte is already a boundary on its other
        // side too (`self.open.flags != 0` above), so it closes on the next
        // byte, the next idle poll, or the next flush -- whichever comes
        // first. Closing it HERE would mean one call producing two records,
        // and the second would have to be queued somewhere; the delay is
        // bounded by the idle poll and buys a much simpler contract.
        completed
    }

    /// Time passed with no byte. Returns a record if the gap closed one.
    pub fn idle(&mut self, micros: u32) -> Option<Record> {
        if !self.has_open {
            return None;
        }
        if micros.wrapping_sub(self.last) >= self.config.gap_micros {
            return self.take();
        }
        None
    }

    /// Closes whatever is open, regardless of the clock.
    pub fn flush(&mut self) -> Option<Record> {
        self.take()
    }

    fn take(&mut self) -> Option<Record> {
        if !self.has_open || self.open.len == 0 {
            self.has_open = false;
            return None;
        }
        self.has_open = false;
        Some(core::mem::replace(&mut self.open, Record::empty(self.kind)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{flag, kind};

    fn segmenter() -> Segmenter {
        // 1 ms gap: round numbers make the intent of each test readable.
        Segmenter::new(kind::RX, Config::with_gap(1_000))
    }

    #[test]
    fn bytes_close_together_are_one_record() {
        let mut s = segmenter();
        assert!(s.push(1_000, b'A', 0).is_none());
        assert!(s.push(1_100, b'B', 0).is_none());
        assert!(s.push(1_200, b'C', 0).is_none());
        let record = s.flush().expect("nothing was open");
        assert_eq!(3, record.len);
        assert_eq!(b"ABC", &record.data[..3]);
        assert_eq!(1_000, record.micros);
        assert_eq!(&[0, 100, 100], &record.gaps[..3]);
    }

    #[test]
    fn a_gap_starts_a_new_record_and_the_new_byte_is_in_it() {
        let mut s = segmenter();
        s.push(1_000, b'A', 0);
        s.push(1_100, b'B', 0);
        let first = s.push(5_000, b'C', 0).expect("the gap closed nothing");
        assert_eq!(b"AB", &first.data[..2]);
        let second = s.flush().expect("the new byte went nowhere");
        assert_eq!(b"C", &second.data[..1]);
        assert_eq!(5_000, second.micros);
    }

    #[test]
    fn an_error_byte_stands_alone_with_exactly_its_own_flags() {
        // The whole reason for a bridge rather than a dongle: pyserial cannot
        // see a framing error at all, and doc 30 §2.2 (A1) says one at a known
        // timestamp is a POSITIVE finding about whether the wire is
        // async-UART framed. A finding smeared across a six-byte record, with
        // no way to say which byte carried it, is not that.
        let mut s = segmenter();
        s.push(1_000, b'A', 0);
        let clean = s.push(1_100, 0xFF, flag::FRAMING).expect("no clean record");
        assert_eq!(0, clean.flags, "an error smeared across a clean record");
        assert_eq!(b"A", &clean.data[..1]);

        // The next clean byte closes the error record rather than joining it.
        let bad = s.push(1_150, b'B', 0).expect("the error byte went nowhere");
        assert_eq!(flag::FRAMING, bad.flags);
        assert_eq!(1, bad.len);
        assert_eq!(1_100, bad.micros);
        assert_eq!(0xFF, bad.data[0]);

        let after = s.flush().unwrap();
        assert_eq!(0, after.flags, "the error leaked forward");
        assert_eq!(b"B", &after.data[..1]);
    }

    #[test]
    fn an_error_byte_is_delivered_by_the_idle_poll_when_nothing_follows() {
        // A bus that goes quiet straight after an error is the interesting
        // case, and the one where holding the record until the next byte would
        // mean holding it forever.
        let mut s = segmenter();
        s.push(1_000, 0x00, flag::PARITY | flag::NOISE);
        let bad = s.idle(9_000).expect("the idle poll never delivered it");
        assert_eq!(flag::PARITY | flag::NOISE, bad.flags);
        assert_eq!(1, bad.len);
    }

    #[test]
    fn two_errors_in_a_row_are_two_records() {
        let mut s = segmenter();
        s.push(1_000, 0x01, flag::FRAMING);
        let first = s.push(1_100, 0x02, flag::PARITY).expect("the first was swallowed");
        assert_eq!(flag::FRAMING, first.flags);
        let second = s.flush().expect("the second was swallowed");
        assert_eq!(flag::PARITY, second.flags);
    }

    #[test]
    fn a_full_record_closes_without_claiming_a_gap() {
        let mut s = segmenter();
        let mut closed = None;
        for i in 0..(MAX_BYTES + 1) {
            if let Some(record) = s.push(1_000 + i as u32, b'x', 0) {
                closed = Some(record);
            }
        }
        let record = closed.expect("a full record never closed");
        assert_eq!(MAX_BYTES as u8, record.len);
        assert_eq!(0, record.flags, "a buffer limit is not a bus condition");
        let rest = s.flush().unwrap();
        assert_eq!(1, rest.len);
    }

    #[test]
    fn idle_closes_a_record_that_the_next_byte_would_have() {
        // The reason `idle` exists: on a bus that may be silent for hours,
        // waiting for the next byte to flush the last one means waiting
        // forever, and a capture that ends mid-record ends with no record.
        let mut s = segmenter();
        s.push(1_000, b'A', 0);
        assert!(s.idle(1_500).is_none(), "closed inside the gap");
        let record = s.idle(3_000).expect("the gap never closed it");
        assert_eq!(b"A", &record.data[..1]);
        assert!(s.idle(9_000).is_none(), "closed an empty record");
    }

    #[test]
    fn a_record_spanning_the_clock_wrap_is_still_correct() {
        // The counter is 32 bits of microseconds and wraps every ~71.6
        // minutes. A capture left running through one must not produce a
        // 4295-second gap.
        let mut s = segmenter();
        s.push(u32::MAX - 100, b'A', 0);
        s.push(u32::MAX, b'B', 0);
        s.push(50, b'C', 0);            // wrapped
        let record = s.flush().unwrap();
        assert_eq!(3, record.len);
        assert_eq!(&[0, 100, 51], &record.gaps[..3]);
    }

    #[test]
    fn the_gap_threshold_follows_the_bit_rate() {
        // 3.5 character times, so "quiet" means the same thing at both ends of
        // the sweep in doc 30 §7.
        assert_eq!(3_640, Config::at_baud(9600).gap_micros);
        assert_eq!(280, Config::at_baud(115_200).gap_micros);
    }
}
