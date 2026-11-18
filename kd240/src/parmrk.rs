//! Undoing the kernel's in-band error marks (`termios` `PARMRK`).
//!
//! With `INPCK | PARMRK` set and `IGNPAR`, `IGNBRK`, `BRKINT` and `ISTRIP`
//! clear, the Linux tty layer reports a received byte with a framing or
//! parity error as `0xFF 0x00 <byte>`, a break as `0xFF 0x00 0x00`, and a
//! real `0xFF` as `0xFF 0xFF`. That is the only way a user-space program can
//! learn *where* in the stream the UART flagged an error, and per
//! `docs/30-serial-protocol.md` §2.2 (A1) an error at a known place is a
//! finding: evidence about whether the bus is async-UART framed at all.
//!
//! The kernel does not say which error it was. [`Unmark`] reports a marked
//! byte as [`flag::FRAMING`], plus [`flag::PARITY`] when parity checking is
//! on (either could have caused it), and a marked `0x00` as [`flag::BREAK`]
//! (a break and a framing error on a zero byte look the same).

use emco_bridge_core::flag;

/// The decoder. Feed it every byte read from the tty, in order.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unmark {
    state: State,
    parity: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum State {
    #[default]
    Plain,
    SawFf,
    SawFf00,
}

impl Unmark {
    /// A decoder; `parity` is whether the line checks parity.
    pub fn new(parity: bool) -> Self {
        Unmark {
            state: State::Plain,
            parity,
        }
    }

    fn error_flags(&self, byte: u8) -> u8 {
        if byte == 0 {
            flag::BREAK
        } else if self.parity {
            flag::FRAMING | flag::PARITY
        } else {
            flag::FRAMING
        }
    }

    /// Takes one tty byte; calls `out(byte, flags)` for each wire byte it
    /// completes (none, one, or two after a stray `0xFF`).
    pub fn push(&mut self, b: u8, mut out: impl FnMut(u8, u8)) {
        match self.state {
            State::Plain if b == 0xFF => self.state = State::SawFf,
            State::Plain => out(b, 0),
            State::SawFf => match b {
                0xFF => {
                    self.state = State::Plain;
                    out(0xFF, 0);
                }
                0x00 => self.state = State::SawFf00,
                _ => {
                    // Not a mark the kernel produces; keep both bytes.
                    self.state = State::Plain;
                    out(0xFF, 0);
                    out(b, 0);
                }
            },
            State::SawFf00 => {
                self.state = State::Plain;
                out(b, self.error_flags(b));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(parity: bool, input: &[u8]) -> Vec<(u8, u8)> {
        let mut u = Unmark::new(parity);
        let mut v = Vec::new();
        for &b in input {
            u.push(b, |b, f| v.push((b, f)));
        }
        v
    }

    #[test]
    fn plain_bytes_pass_through() {
        assert_eq!(run(false, b"AB\x00"), [(b'A', 0), (b'B', 0), (0, 0)]);
    }

    #[test]
    fn a_doubled_ff_is_one_ff() {
        assert_eq!(run(false, &[0xFF, 0xFF, 0x41]), [(0xFF, 0), (0x41, 0)]);
    }

    #[test]
    fn a_marked_byte_carries_the_error_flags() {
        assert_eq!(run(false, &[0xFF, 0x00, 0x41]), [(0x41, flag::FRAMING)]);
        assert_eq!(
            run(true, &[0xFF, 0x00, 0x41]),
            [(0x41, flag::FRAMING | flag::PARITY)]
        );
        assert_eq!(run(true, &[0xFF, 0x00, 0x00]), [(0x00, flag::BREAK)]);
        // A marked 0xFF.
        assert_eq!(
            run(false, &[0xFF, 0x00, 0xFF, 0x42]),
            [(0xFF, flag::FRAMING), (0x42, 0)]
        );
    }

    #[test]
    fn a_stray_ff_keeps_both_bytes() {
        assert_eq!(run(false, &[0xFF, 0x41]), [(0xFF, 0), (0x41, 0)]);
    }

    #[test]
    fn marks_split_across_reads_decode_the_same() {
        let mut u = Unmark::new(false);
        let mut v = Vec::new();
        for chunk in [&[0xFFu8][..], &[0x00], &[0x41, 0xFF], &[0xFF]] {
            for &b in chunk {
                u.push(b, |b, f| v.push((b, f)));
            }
        }
        assert_eq!(v, [(0x41, flag::FRAMING), (0xFF, 0)]);
    }
}
