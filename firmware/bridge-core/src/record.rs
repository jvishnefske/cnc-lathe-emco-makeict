//! What one record says, and exactly how it is laid out on the wire.
//!
//! This module is the specification. `tools/emco-capture.py` decodes what it
//! encodes, `tools/virtual-bridge.py` produces the same bytes without a board,
//! and `tests/golden.rs` freezes a corpus that the Python decoder is asserted
//! against — so the two implementations cannot drift apart without a test
//! going red in one language or the other.
//!
//! # Layout
//!
//! Every record is a payload, COBS-encoded, followed by a zero byte. The zero
//! is the frame delimiter and never occurs inside an encoded payload, so a
//! host that connects mid-transcript resynchronises at the next zero.
//!
//! All multi-byte integers are little-endian.
//!
//! ## `RX` (0x01) and `TX` (0x02)
//!
//! ```text
//!   0        1   kind
//!   1..5     4   micros   — arrival of byte 0, free-running, wraps at 2^32
//!   5        1   flags    — see `flag`; the OR over this record
//!   6        1   n        — how many bytes, 1..=MAX_BYTES
//!   7..7+n   n   the bytes, in arrival order
//!   7+n..    2(n-1)  gaps — microseconds from the previous byte, u16 each,
//!                          saturating; there is no gap before byte 0
//! ```
//!
//! The gaps are the reason this device exists. Timestamping at the host's
//! `read()` puts USB's 1 ms frames, `cdc_acm` buffering and scheduler latency
//! between the wire and the clock; at 9600 baud a character is 1042 µs, so
//! that destroys precisely the inter-byte signature that would tell you
//! whether the framing is async UART and what its bit rate is.
//!
//! ## `EVENT` (0x03)
//!
//! ```text
//!   0        1   kind
//!   1..5     4   micros
//!   5        1   code     — see `crate::event`
//!   6..10    4   argument
//! ```
//!
//! ## `STATUS` (0x04)
//!
//! ```text
//!   0        1   kind
//!   1..5     4   micros
//!   5        1   flags    — LISTEN_ONLY and TX_ARMED are meaningful here
//!   6..10    4   rx_bytes
//!   10..14   4   tx_bytes
//!   14..18   4   dropped_bytes
//!   18..22   4   error_bytes
//!   22..     *   UTF-8, up to STATUS_TEXT bytes: firmware id, build variant,
//!                bit rate, framing
//! ```
//!
//! The text is not decoration. A transcript that names the build variant that
//! produced it proves whether the device could physically have transmitted —
//! which is the evidence half of "listen-only", the other halves being a
//! compile-time feature and a link on the adapter that is not fitted.

/// Record kinds.
pub mod kind {
    /// Bytes the bridge received from the bus.
    pub const RX: u8 = 0x01;
    /// Bytes the bridge itself transmitted. On a 3-wire half-duplex bus your
    /// own echo and a reply are the same bytes; only the bridge knows which.
    pub const TX: u8 = 0x02;
    /// Something happened that is not a byte.
    pub const EVENT: u8 = 0x03;
    /// Counters and the build variant.
    pub const STATUS: u8 = 0x04;
}

/// Per-record condition flags.
///
/// The first four come from the UART and are the reason a bridge beats a
/// dongle: `pyserial` cannot report any of them, and per `docs/30` §2.2 (A1)
/// an error at a known timestamp is a POSITIVE finding — evidence about
/// whether the wire is async-UART framed at all.
pub mod flag {
    /// No stop bit where one was expected.
    pub const FRAMING: u8 = 0x01;
    /// Parity did not check.
    pub const PARITY: u8 = 0x02;
    /// The receiver saw noise on the line during a bit.
    pub const NOISE: u8 = 0x04;
    /// The UART's own receive register was overwritten: bytes were lost in
    /// hardware, before this crate ever saw them.
    pub const OVERRUN: u8 = 0x08;
    /// Records were lost between the previous record and this one because the
    /// outbound ring was full. A gap in a transcript, made visible.
    pub const DROPPED: u8 = 0x10;
    /// A line break was in progress.
    pub const BREAK: u8 = 0x20;
    /// STATUS only: the transmit path is not in this build.
    pub const LISTEN_ONLY: u8 = 0x40;
    /// STATUS only: the transmit path IS in this build.
    pub const TX_ARMED: u8 = 0x80;
}

/// The most bytes one `RX`/`TX` record carries.
///
/// Chosen so a record is comfortably under a USB full-speed bulk packet once
/// framed, which keeps a record from being split across two USB transactions
/// more often than it has to be.
pub const MAX_BYTES: usize = 64;

/// The longest `STATUS` description.
pub const STATUS_TEXT: usize = 96;

/// The largest payload any record can have: an `RX` record at `MAX_BYTES`.
pub const MAX_PAYLOAD: usize = 7 + MAX_BYTES + 2 * (MAX_BYTES - 1);

/// The largest framed record: COBS adds one byte per 254, plus the delimiter.
pub const MAX_FRAME: usize = MAX_PAYLOAD + MAX_PAYLOAD / 254 + 2;

/// Running totals, reported in `STATUS`.
///
/// Wrapping rather than saturating: a counter that stops is a counter that
/// lies about a long capture, and the host can un-wrap a monotonic u32 the
/// same way it un-wraps the timestamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub rx_bytes: u32,
    pub tx_bytes: u32,
    /// Bytes inside records that never left, because the ring was full.
    pub dropped_bytes: u32,
    /// Records that never left.
    pub dropped_records: u32,
    /// Bytes that arrived with at least one UART error flag set.
    pub error_bytes: u32,
}

impl Counters {
    pub const fn new() -> Self {
        Self {
            rx_bytes: 0,
            tx_bytes: 0,
            dropped_bytes: 0,
            dropped_records: 0,
            error_bytes: 0,
        }
    }
}

/// One record, before framing.
///
/// Fixed-size and `Clone` rather than borrowed: this crate has no allocator
/// and the firmware has no threads, and 200 bytes of stack copied at ~12 kB/s
/// is not a cost worth a lifetime parameter.
#[derive(Clone)]
pub struct Record {
    pub kind: u8,
    pub micros: u32,
    pub flags: u8,
    pub len: u8,
    pub data: [u8; MAX_BYTES],
    /// `gaps[i]` is the microseconds between byte `i-1` and byte `i`.
    /// `gaps[0]` is always zero and is never transmitted.
    pub gaps: [u16; MAX_BYTES],
    /// `EVENT` only.
    pub code: u8,
    /// `EVENT` argument, or unused.
    pub argument: u32,
    /// `STATUS` only.
    pub counters: Counters,
    /// `STATUS` only: how much of `text` is meaningful.
    pub text_len: u8,
    pub text: [u8; STATUS_TEXT],
}

impl Record {
    pub const fn empty(kind: u8) -> Self {
        Self {
            kind,
            micros: 0,
            flags: 0,
            len: 0,
            data: [0; MAX_BYTES],
            gaps: [0; MAX_BYTES],
            code: 0,
            argument: 0,
            counters: Counters::new(),
            text_len: 0,
            text: [0; STATUS_TEXT],
        }
    }

    pub fn event(micros: u32, code: u8, argument: u32) -> Self {
        let mut record = Self::empty(kind::EVENT);
        record.micros = micros;
        record.code = code;
        record.argument = argument;
        record
    }

    pub fn status(micros: u32, flags: u8, counters: &Counters, describe: &str) -> Self {
        let mut record = Self::empty(kind::STATUS);
        record.micros = micros;
        record.flags = flags;
        record.counters = *counters;
        // Truncated on a CHARACTER boundary, not a byte one: a description cut
        // mid-codepoint is not UTF-8, and a decoder that then refuses to read
        // it turns a cosmetic overflow into a lost transcript.
        let mut end = describe.len().min(STATUS_TEXT);
        while end > 0 && !describe.is_char_boundary(end) {
            end -= 1;
        }
        record.text[..end].copy_from_slice(&describe.as_bytes()[..end]);
        record.text_len = end as u8;
        record
    }

    /// The payload, without framing. Returns how many bytes were written.
    ///
    /// `out` must be at least [`MAX_PAYLOAD`] long; anything shorter is a
    /// programming error and is reported as a zero-length record rather than
    /// as a panic, because this runs in an interrupt-adjacent path on a device
    /// nobody can attach a debugger to.
    pub fn encode(&self, out: &mut [u8]) -> usize {
        if out.len() < MAX_PAYLOAD {
            return 0;
        }
        out[0] = self.kind;
        out[1..5].copy_from_slice(&self.micros.to_le_bytes());
        match self.kind {
            kind::RX | kind::TX => {
                out[5] = self.flags;
                let n = self.len as usize;
                out[6] = self.len;
                out[7..7 + n].copy_from_slice(&self.data[..n]);
                let mut at = 7 + n;
                for gap in &self.gaps[1..n] {
                    out[at..at + 2].copy_from_slice(&gap.to_le_bytes());
                    at += 2;
                }
                at
            }
            kind::EVENT => {
                out[5] = self.code;
                out[6..10].copy_from_slice(&self.argument.to_le_bytes());
                10
            }
            kind::STATUS => {
                out[5] = self.flags;
                out[6..10].copy_from_slice(&self.counters.rx_bytes.to_le_bytes());
                out[10..14].copy_from_slice(&self.counters.tx_bytes.to_le_bytes());
                out[14..18].copy_from_slice(&self.counters.dropped_bytes.to_le_bytes());
                out[18..22].copy_from_slice(&self.counters.error_bytes.to_le_bytes());
                let n = self.text_len as usize;
                out[22..22 + n].copy_from_slice(&self.text[..n]);
                22 + n
            }
            _ => 0,
        }
    }

    /// The payload, COBS-encoded, with its delimiting zero.
    ///
    /// `out` must be at least [`MAX_FRAME`] long.
    pub fn encode_frame(&self, out: &mut [u8]) -> usize {
        let mut payload = [0u8; MAX_PAYLOAD];
        let n = self.encode(&mut payload);
        if n == 0 || out.len() < MAX_FRAME {
            return 0;
        }
        let encoded = crate::cobs::encode(&payload[..n], out);
        out[encoded] = 0;
        encoded + 1
    }
}
