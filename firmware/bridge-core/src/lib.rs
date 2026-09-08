//! `emco-bridge-core` — the part of the USB↔RS-485 capture bridge that has no
//! hardware in it.
//!
//! # What this is for
//!
//! The EMCO PC Turn 50's control box talks to its long-gone DOS PC over a
//! 3-wire half-duplex **RS-485** multidrop bus (`docs/30-serial-protocol.md`).
//! The wire protocol is unknown, the original master was a memory-mapped ISA
//! card rather than a UART, and **the framing may not be async-UART at all**.
//! Discovery therefore needs a capture that records more than bytes.
//!
//! A USB-RS485 dongle plus `pyserial` cannot give it. Host timestamps are
//! taken at `read()`, which is after USB's 1 ms frames, after `cdc_acm`
//! buffering and after scheduler latency — and at 9600 baud a character is
//! 1.04 ms, so host timing destroys exactly the inter-byte and inter-frame
//! signature that identifies a polled multidrop protocol. Framing and parity
//! errors are worse: pyserial cannot see one at all, and per doc 30 §2.2 (A1)
//! **an error at a known timestamp is a positive finding**, because it is
//! evidence about whether the wire is async-UART in the first place.
//!
//! So the bridge timestamps each byte in hardware, tags its direction, keeps
//! the UART's error flags, and sends the result to the host as records. This
//! crate is that record format, the segmentation rule that decides where one
//! record ends, and the accounting that makes a lost byte visible instead of
//! silent. It is the half that can be tested on a laptop, and it is tested
//! there on every `cargo test`.
//!
//! # What this is NOT
//!
//! Not a substitute for a logic analyser. Doc 30 rung 2 says an analyser
//! across `DATA+`/`DATA−` is the better *first* instrument, precisely because
//! the framing may not be async, and that stays true. What the bridge adds is
//! being **safe, always attached, timestamped and direction-tagged** — a
//! capture you can leave running through a power cycle, not a better scope.
//!
//! Nothing here is a safety function. See `docs/30-serial-protocol.md` §5.
//!
//! # The shape of it
//!
//! [`Bridge`] is the whole thing: feed it bytes with the time they arrived,
//! drain framed bytes out of it whenever the USB endpoint has room.
//!
//! ```
//! use emco_bridge_core::{Bridge, Config, flag};
//!
//! let mut bridge: Bridge<1024> = Bridge::new(Config::at_baud(9600));
//! bridge.on_rx(1_000, 0x41, 0);
//! bridge.on_rx(2_042, 0x42, 0);
//! bridge.on_idle(1_000_000);            // the gap ends the record
//!
//! let mut out = [0u8; 256];
//! let n = bridge.read(&mut out);
//! assert!(n > 0);                        // a COBS frame, zero-terminated
//! assert_eq!(out[n - 1], 0);
//! # let _ = flag::FRAMING;
//! ```

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod cobs;
pub mod record;
pub mod ring;
pub mod segment;

pub use record::{flag, kind, Counters, Record, MAX_BYTES, MAX_FRAME, MAX_PAYLOAD};
pub use ring::Ring;
pub use segment::{Config, Segmenter};

/// A byte stream in each direction, segmented, framed, and queued for USB.
///
/// `N` is the size of the outbound ring in bytes. It is the only tuning knob
/// that matters: USB drains in ~1 ms bursts and a 115200-baud bus produces
/// ~11.5 kB/s, so a few kilobytes covers any stall a host is likely to impose.
/// When it is not enough, records are dropped and **counted**, and the next
/// record to leave carries [`flag::DROPPED`] — see [`Bridge::read`].
pub struct Bridge<const N: usize> {
    rx: Segmenter,
    tx: Segmenter,
    ring: Ring<N>,
    counters: Counters,
    /// Set when a record could not be queued. Cleared onto the next record
    /// that can be, so a gap in a transcript is never silent.
    pending_drop: bool,
}

impl<const N: usize> Bridge<N> {
    pub const fn new(config: Config) -> Self {
        Self {
            rx: Segmenter::new(kind::RX, config),
            tx: Segmenter::new(kind::TX, config),
            ring: Ring::new(),
            counters: Counters::new(),
            pending_drop: false,
        }
    }

    /// One byte received from the bus, with the time it arrived and whatever
    /// the UART said about it (see [`flag`]).
    pub fn on_rx(&mut self, micros: u32, byte: u8, flags: u8) {
        self.counters.rx_bytes = self.counters.rx_bytes.wrapping_add(1);
        if flags != 0 {
            self.counters.error_bytes = self.counters.error_bytes.wrapping_add(1);
        }
        // A byte in one direction ends any record open in the other: on a
        // half-duplex bus the interleaving IS the finding, and a record whose
        // timestamp precedes one already sent would have to be re-sorted by
        // the host to see it.
        if let Some(record) = self.tx.flush() {
            self.queue(&record);
        }
        if let Some(record) = self.rx.push(micros, byte, flags) {
            self.queue(&record);
        }
    }

    /// One byte this device transmitted. Present in every build; the code that
    /// CALLS it is compiled out of a listen-only build.
    ///
    /// It exists because of the one thing that makes a 3-wire half-duplex bus
    /// different from a pair of wires: once transmit has ever been armed, your
    /// own echo and a reply are the same bytes on the same conductors, and
    /// only the device that drove them knows which is which.
    pub fn on_tx(&mut self, micros: u32, byte: u8) {
        self.counters.tx_bytes = self.counters.tx_bytes.wrapping_add(1);
        if let Some(record) = self.rx.flush() {
            self.queue(&record);
        }
        if let Some(record) = self.tx.push(micros, byte, 0) {
            self.queue(&record);
        }
    }

    /// Time has passed. Closes a record whose idle gap has elapsed.
    ///
    /// Called from the main loop rather than from an interrupt, so a bus that
    /// went quiet mid-record still delivers it instead of holding the last few
    /// bytes until the next ones arrive — which, on a bus that may be silent
    /// for hours, means until never.
    pub fn on_idle(&mut self, micros: u32) {
        if let Some(record) = self.rx.idle(micros) {
            self.queue(&record);
        }
        if let Some(record) = self.tx.idle(micros) {
            self.queue(&record);
        }
    }

    /// A named thing that happened, with a value: a break, a DE assertion, a
    /// host command. Codes live in [`event`].
    pub fn on_event(&mut self, micros: u32, code: u8, argument: u32) {
        let record = Record::event(micros, code, argument);
        self.queue(&record);
    }

    /// The counters and the build variant, as a record.
    ///
    /// Emitted at least at connect and on request. `describe` names the
    /// firmware and the build variant, which is what makes a transcript
    /// self-evidencing: it proves which firmware produced it, and therefore
    /// whether the device that recorded it could physically have transmitted.
    pub fn on_status(&mut self, micros: u32, flags: u8, describe: &str) {
        // Anything still open goes first, so the counters describe a state the
        // host has already been told everything about.
        self.flush(micros);
        let record = Record::status(micros, flags, &self.counters, describe);
        self.queue(&record);
    }

    /// Closes every open record. Call before a status or a disconnect.
    pub fn flush(&mut self, _micros: u32) {
        if let Some(record) = self.rx.flush() {
            self.queue(&record);
        }
        if let Some(record) = self.tx.flush() {
            self.queue(&record);
        }
    }

    /// Moves framed bytes out to the host. Returns how many were written.
    ///
    /// Whole frames only is NOT promised: the caller may take any number of
    /// bytes, because a USB endpoint has its own idea of a packet and the
    /// COBS delimiter is what re-synchronises the far end anyway. That is the
    /// point of COBS here — a host that connects mid-transcript, or that
    /// misses a chunk, finds the next zero and carries on.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        self.ring.read(out)
    }

    /// Records dropped, bytes seen, errors seen. Read-only.
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// How many bytes are waiting for the host.
    pub fn queued(&self) -> usize {
        self.ring.len()
    }

    fn queue(&mut self, record: &Record) {
        let mut frame = [0u8; MAX_FRAME];
        let mut copy;
        // The DROPPED flag rides on the next record that fits, which is the
        // only place it can go: the record that would have carried it is the
        // one that did not get sent.
        let record = if self.pending_drop {
            copy = record.clone();
            copy.flags |= flag::DROPPED;
            &copy
        } else {
            record
        };
        let n = record.encode_frame(&mut frame);
        if self.ring.write(&frame[..n]) {
            self.pending_drop = false;
        } else {
            self.pending_drop = true;
            self.counters.dropped_records = self.counters.dropped_records.wrapping_add(1);
            self.counters.dropped_bytes =
                self.counters.dropped_bytes.wrapping_add(record.len as u32);
        }
    }
}

/// Event codes carried by an `EVENT` record.
pub mod event {
    /// The bridge came up. Argument: the protocol version.
    pub const BOOT: u8 = 0x01;
    /// A line break was detected on the bus. Argument: unused.
    pub const BREAK: u8 = 0x02;
    /// The driver-enable line changed. Argument: 1 asserted, 0 released.
    ///
    /// Never emitted by a listen-only build, because that build has no code
    /// that can assert it.
    pub const DRIVER_ENABLE: u8 = 0x03;
    /// The outbound ring overflowed and records were lost. Argument: the
    /// running total of dropped records.
    pub const OVERFLOW: u8 = 0x04;
    /// The host asked for something the firmware will not do. Argument: the
    /// byte it asked with.
    pub const REFUSED: u8 = 0x05;
}

/// The record format's version, carried in the `BOOT` event.
///
/// Bumped when the layout changes in a way `tools/emco-capture.py` would
/// misread. A decoder that sees a version it does not know should say so and
/// stop, rather than produce a transcript that looks plausible.
pub const PROTOCOL_VERSION: u32 = 1;
