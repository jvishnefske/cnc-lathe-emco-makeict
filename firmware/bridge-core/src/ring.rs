//! A byte ring between the interrupt that records and the endpoint that sends.
//!
//! Records are written whole or not at all. That is the important property and
//! it is worth the arithmetic: a half-written record on a full ring would be a
//! COBS frame with no delimiter, the host would resynchronise at the next zero
//! and silently lose the record after it as well, and the transcript would be
//! short by two records having reported neither. Refusing the write instead
//! costs one record and reports it — see `Bridge::queue` and `flag::DROPPED`.

/// A fixed-capacity byte ring. `N` bytes, of which `N - 1` are usable.
pub struct Ring<const N: usize> {
    buffer: [u8; N],
    head: usize,
    tail: usize,
}

impl<const N: usize> Ring<N> {
    pub const fn new() -> Self {
        Self {
            buffer: [0; N],
            head: 0,
            tail: 0,
        }
    }

    pub fn len(&self) -> usize {
        if self.head >= self.tail {
            self.head - self.tail
        } else {
            N - self.tail + self.head
        }
    }

    pub fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    /// One slot is always left free, so a full ring and an empty one are
    /// distinguishable without a third field.
    pub fn free(&self) -> usize {
        N - 1 - self.len()
    }

    /// All of `bytes`, or none of them. Returns whether it fitted.
    pub fn write(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > self.free() {
            return false;
        }
        for &byte in bytes {
            self.buffer[self.head] = byte;
            self.head = (self.head + 1) % N;
        }
        true
    }

    /// Moves up to `out.len()` bytes out. Returns how many.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let mut written = 0;
        while written < out.len() && !self.is_empty() {
            out[written] = self.buffer[self.tail];
            self.tail = (self.tail + 1) % N;
            written += 1;
        }
        written
    }
}

impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_gives_back_what_it_was_given() {
        let mut ring: Ring<16> = Ring::new();
        assert!(ring.write(&[1, 2, 3]));
        let mut out = [0u8; 8];
        assert_eq!(3, ring.read(&mut out));
        assert_eq!(&[1, 2, 3], &out[..3]);
        assert!(ring.is_empty());
    }

    #[test]
    fn a_write_that_does_not_fit_writes_nothing() {
        let mut ring: Ring<8> = Ring::new();
        assert!(ring.write(&[1, 2, 3, 4, 5, 6, 7])); // 7 of 7 usable
        assert!(!ring.write(&[9]));
        let mut out = [0u8; 16];
        assert_eq!(7, ring.read(&mut out));
        assert_eq!(&[1, 2, 3, 4, 5, 6, 7], &out[..7]);
    }

    #[test]
    fn it_wraps_without_reordering() {
        let mut ring: Ring<8> = Ring::new();
        let mut out = [0u8; 4];
        for round in 0..10u8 {
            assert!(ring.write(&[round, round + 1, round + 2]));
            assert_eq!(3, ring.read(&mut out));
            assert_eq!(&[round, round + 1, round + 2], &out[..3]);
        }
    }

    #[test]
    fn a_partial_read_leaves_the_rest_in_order() {
        let mut ring: Ring<32> = Ring::new();
        assert!(ring.write(&[1, 2, 3, 4, 5]));
        let mut out = [0u8; 2];
        assert_eq!(2, ring.read(&mut out));
        assert_eq!(&[1, 2], &out[..]);
        let mut rest = [0u8; 8];
        assert_eq!(3, ring.read(&mut rest));
        assert_eq!(&[3, 4, 5], &rest[..3]);
    }
}
