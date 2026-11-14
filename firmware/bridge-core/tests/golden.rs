//! A corpus that both implementations of this format are held to.
//!
//! There are two decoders for the bridge's records: the Rust one in the
//! integration tests, and the Python one in `tools/emco-capture.py` that
//! actually reads a capture. Two implementations of a wire format drift, and
//! they drift silently, and the moment you notice is the moment you have a
//! transcript from the machine that one of them reads wrong.
//!
//! So the encoder writes a fixed scenario to `tests/golden.bin`, this test
//! asserts the bytes have not changed, and `tools/test_emco_bridge.py` asserts
//! the Python decoder reproduces the same records from the same file. Neither
//! side can move without the other going red.
//!
//! To regenerate after a DELIBERATE format change (and bump
//! `PROTOCOL_VERSION` when you do):
//!
//! ```sh
//! BRIDGE_WRITE_GOLDEN=1 cargo test --test golden
//! ```

use std::path::PathBuf;

use emco_bridge_core::record::flag;
use emco_bridge_core::{event, Bridge, Config, PROTOCOL_VERSION};

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden.bin")
}

/// The scenario. Chosen so that every field of every record kind is exercised
/// by a value that would be wrong if a decoder read it at the wrong offset, in
/// the wrong width, or in the wrong byte order.
fn corpus() -> Vec<u8> {
    // The bit rate the machine's own COM ports use (doc 30 §1.3), so the gap
    // threshold in the corpus is the one a first capture would actually run
    // with. The ring is deliberately too small for step 8 below, so the corpus
    // contains a real dropped-record gap rather than a simulated one; every
    // other step is drained as it goes, exactly as the firmware's USB loop
    // drains it.
    let mut bridge: Bridge<512> = Bridge::new(Config::at_baud(9600));
    let mut out = Vec::new();

    // 1. Boot, so a transcript begins by naming the format it is in.
    bridge.on_event(0, event::BOOT, PROTOCOL_VERSION);
    drain(&mut bridge, &mut out);

    // 2. A clean burst at 9600 baud: five bytes, one character time apart.
    for (i, byte) in b"EMCO?".iter().enumerate() {
        bridge.on_rx(1_000 + i as u32 * 1_042, *byte, 0);
    }
    bridge.on_idle(100_000);
    drain(&mut bridge, &mut out);

    // 3. Zero bytes, which are what COBS exists for, and a gap wide enough to
    //    need both bytes of the u16.
    bridge.on_rx(200_000, 0x00, 0);
    bridge.on_rx(240_000, 0x00, 0);
    bridge.on_rx(240_100, 0xFF, 0);
    bridge.on_idle(400_000);
    drain(&mut bridge, &mut out);

    // 4. A framing error, alone, with its own timestamp -- the record this
    //    whole device exists to be able to produce.
    bridge.on_rx(500_000, 0x7F, flag::FRAMING | flag::NOISE);
    bridge.on_idle(600_000);
    drain(&mut bridge, &mut out);

    // 5. Our own transmission, then a reply: the direction tag doing its job.
    bridge.on_tx(700_000, 0xAA);
    bridge.on_tx(700_800, 0x55);
    bridge.on_rx(710_000, 0x01, 0);
    bridge.on_idle(800_000);
    drain(&mut bridge, &mut out);

    // 6. A record that spans the 32-bit microsecond wrap.
    bridge.on_rx(u32::MAX - 500, b'w', 0);
    bridge.on_rx(u32::MAX, b'r', 0);
    bridge.on_rx(200, b'p', 0);
    bridge.on_idle(1_000_000);
    drain(&mut bridge, &mut out);

    // 7. A full 64-byte record, which is the buffer limit rather than a bus
    //    condition, and the longest frame the format can produce.
    for i in 0..70u32 {
        bridge.on_rx(2_000_000 + i * 10, (i % 251) as u8, 0);
    }
    bridge.on_idle(3_000_000);
    drain(&mut bridge, &mut out);

    // 8. Overflow. NOT drained while it runs: the ring fills, records are
    //    lost, and the first one that fits afterwards carries DROPPED. That
    //    flag is the difference between a transcript with a gap and a
    //    transcript that lies about a quiet bus.
    for i in 0..80u32 {
        bridge.on_rx(4_000_000 + i * 100_000, 0xDD, 0);
        bridge.on_idle(4_000_000 + i * 100_000 + 50_000);
    }
    assert!(
        bridge.counters().dropped_records > 0,
        "the ring never filled"
    );
    drain(&mut bridge, &mut out);
    bridge.on_rx(8_000_000, 0xEE, 0);
    bridge.on_idle(8_100_000);
    drain(&mut bridge, &mut out);

    // 9. The counters, and the build variant that produced all of the above.
    bridge.on_status(
        9_000_000,
        flag::LISTEN_ONLY,
        "emco-rs485-bridge 0.1.0 listen-only 9600 8N1",
    );
    drain(&mut bridge, &mut out);
    out
}

fn drain<const N: usize>(bridge: &mut Bridge<N>, out: &mut Vec<u8>) {
    let mut chunk = [0u8; 256];
    loop {
        let n = bridge.read(&mut chunk);
        if n == 0 {
            return;
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

#[test]
fn the_corpus_has_not_changed() {
    let produced = corpus();
    let path = golden_path();
    if std::env::var_os("BRIDGE_WRITE_GOLDEN").is_some() {
        std::fs::write(&path, &produced).expect("could not write the corpus");
        eprintln!("wrote {} bytes to {}", produced.len(), path.display());
        return;
    }
    let expected = std::fs::read(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing; regenerate it with BRIDGE_WRITE_GOLDEN=1 cargo test --test golden",
            path.display()
        )
    });
    assert_eq!(
        expected.len(),
        produced.len(),
        "the corpus changed length: the record format moved, and \
         tools/test_emco_bridge.py is now reading the old one"
    );
    assert_eq!(hex(&expected), hex(&produced));
}

#[test]
fn the_corpus_contains_every_record_kind_and_the_awkward_cases() {
    // A corpus that quietly stopped covering something is a corpus that
    // guarantees agreement about nothing.
    use emco_bridge_core::cobs;
    use emco_bridge_core::record::{kind, MAX_PAYLOAD};

    let stream = corpus();
    let mut kinds = std::collections::BTreeSet::new();
    let mut flags = 0u8;
    let mut longest = 0usize;
    for frame in stream.split(|&b| b == 0) {
        if frame.is_empty() {
            continue;
        }
        let mut payload = [0u8; MAX_PAYLOAD];
        let n = cobs::decode(frame, &mut payload).expect("the corpus is not COBS");
        kinds.insert(payload[0]);
        if payload[0] != kind::EVENT {
            flags |= payload[5];
        }
        longest = longest.max(n);
    }
    assert_eq!(
        vec![kind::RX, kind::TX, kind::EVENT, kind::STATUS]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        kinds
    );
    assert_ne!(0, flags & flag::FRAMING, "no framing error in the corpus");
    assert_ne!(0, flags & flag::NOISE, "no noise flag in the corpus");
    assert_ne!(
        0,
        flags & flag::DROPPED,
        "no dropped-record gap in the corpus"
    );
    assert_ne!(
        0,
        flags & flag::LISTEN_ONLY,
        "no build variant in the corpus"
    );
    assert_eq!(MAX_PAYLOAD, longest, "no full-length record in the corpus");
}

fn hex(bytes: &[u8]) -> String {
    // Compared as hex so a failure prints something a person can diff, rather
    // than a wall of `[70, 72, ...]`.
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
