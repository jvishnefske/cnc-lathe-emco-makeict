//! Consistent Overhead Byte Stuffing: zero never appears inside a frame, so a
//! zero always means "frame ends here".
//!
//! Chosen over inventing a framing because a tested reference implementation
//! already exists on both sides of this project's other wire: `model-based`
//! uses the same envelope in `verify.py` and in the generated Rust, and
//! `tools/emco-capture.py` can therefore be written against a format somebody
//! else has already got wrong once and fixed.
//!
//! Why it matters here specifically: a capture bridge is a device you connect
//! to something already running, and unplug and reconnect while it runs. A
//! host that starts reading mid-record has to be able to find the next record
//! boundary without guessing, and "the next zero" is the whole answer.
//!
//! The overhead is one byte per 254, plus one.

/// Encodes `payload` into `out`, returning the encoded length.
///
/// `out` must have room for `payload.len() + payload.len() / 254 + 1`.
/// Returns 0 if it does not, rather than panicking: this runs on a device
/// nobody can attach a debugger to.
pub fn encode(payload: &[u8], out: &mut [u8]) -> usize {
    let needed = payload.len() + payload.len() / 254 + 1;
    if out.len() < needed {
        return 0;
    }
    let mut code_at = 0usize;
    let mut at = 1usize;
    let mut code = 1u8;
    for &byte in payload {
        if byte == 0 {
            out[code_at] = code;
            code_at = at;
            at += 1;
            code = 1;
        } else {
            out[at] = byte;
            at += 1;
            code += 1;
            if code == 0xFF {
                out[code_at] = code;
                code_at = at;
                at += 1;
                code = 1;
            }
        }
    }
    out[code_at] = code;
    at
}

/// Decodes one frame's worth of `encoded` (no delimiter) into `out`.
///
/// Returns `None` for a frame that is not valid COBS — which is what a
/// truncated frame looks like, and is the case a host has to handle every time
/// somebody unplugs the bridge mid-record.
pub fn decode(encoded: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut at = 0usize;
    let mut written = 0usize;
    while at < encoded.len() {
        let code = encoded[at] as usize;
        if code == 0 {
            return None;                    // a zero inside a frame: not COBS
        }
        at += 1;
        let run = code - 1;
        if at + run > encoded.len() {
            return None;                    // the frame ended mid-run
        }
        if written + run > out.len() {
            return None;
        }
        out[written..written + run].copy_from_slice(&encoded[at..at + run]);
        written += run;
        at += run;
        if code != 0xFF && at < encoded.len() {
            if written >= out.len() {
                return None;
            }
            out[written] = 0;
            written += 1;
        }
    }
    Some(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(payload: &[u8]) {
        let mut encoded = [0u8; 1024];
        let n = encode(payload, &mut encoded);
        assert!(n > 0, "encode refused {} bytes", payload.len());
        assert!(!encoded[..n].contains(&0), "a zero survived encoding");
        let mut decoded = [0u8; 1024];
        let m = decode(&encoded[..n], &mut decoded).expect("decode refused a frame");
        assert_eq!(payload, &decoded[..m]);
    }

    #[test]
    fn it_survives_the_awkward_shapes() {
        roundtrip(&[]);
        roundtrip(&[0]);
        roundtrip(&[0, 0, 0]);
        roundtrip(&[1, 2, 3]);
        roundtrip(&[0, 1, 0, 2, 0]);
        roundtrip(&[0xFF; 300]);
        roundtrip(&[0x00; 300]);
    }

    #[test]
    fn a_run_of_exactly_254_is_the_boundary_case() {
        // The one length where the code byte saturates and a new block starts
        // with nothing in it. Encoders get this wrong; this is why the test is
        // named after it rather than hidden in a loop.
        for len in 250..260 {
            let payload: alloc::vec::Vec<u8> = (0..len).map(|i| (i % 255 + 1) as u8).collect();
            roundtrip(&payload);
        }
    }

    #[test]
    fn a_truncated_frame_is_refused_rather_than_guessed() {
        let mut encoded = [0u8; 64];
        let n = encode(&[1, 2, 3, 4, 5, 6, 7, 8], &mut encoded);
        let mut out = [0u8; 64];
        // Cutting the frame short leaves a run that claims more than is there.
        assert_eq!(None, decode(&encoded[..n - 3], &mut out));
    }

    #[test]
    fn a_zero_inside_a_frame_is_not_cobs() {
        let mut out = [0u8; 16];
        assert_eq!(None, decode(&[3, 1, 2, 0, 1], &mut out));
    }

    extern crate alloc;
}
