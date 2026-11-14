//! The whole core, driven the way the firmware drives it.
//!
//! The unit tests next to each module check one rule each. These check the
//! things that only appear when the parts are wired together: that a direction
//! change is visible in the transcript, that a full ring loses records and
//! SAYS SO, and that what comes out is decodable by the same rules
//! `tools/emco-capture.py` applies.

use emco_bridge_core::record::{flag, kind, MAX_FRAME, MAX_PAYLOAD};
use emco_bridge_core::{cobs, event, Bridge, Config, PROTOCOL_VERSION};

/// One decoded record, with the fields named.
///
/// A tuple would do and did, and the assertions below then read `records[1].flags`
/// -- which is the flags, and there is no way to know that without counting.
/// Clippy complains about the type; the type was the symptom.
#[derive(Debug)]
struct Decoded {
    kind: u8,
    micros: u32,
    flags: u8,
    data: Vec<u8>,
    gaps: Vec<u16>,
}

/// Everything the host has to do, in the order it does it: split on zeros,
/// COBS-decode, read the header. Deliberately written here rather than
/// imported, because it is the Python decoder's algorithm and a shared helper
/// would let both be wrong the same way.
fn decode_all(stream: &[u8]) -> Vec<Decoded> {
    let mut out = Vec::new();
    for frame in stream.split(|&b| b == 0) {
        if frame.is_empty() {
            continue;
        }
        let mut payload = [0u8; MAX_PAYLOAD];
        let n = cobs::decode(frame, &mut payload).expect("a frame that is not COBS");
        let payload = &payload[..n];
        let micros = u32::from_le_bytes(payload[1..5].try_into().unwrap());
        match payload[0] {
            kind::RX | kind::TX => {
                let len = payload[6] as usize;
                let data = payload[7..7 + len].to_vec();
                let gaps = (0..len)
                    .map(|i| {
                        if i == 0 {
                            0
                        } else {
                            let at = 7 + len + 2 * (i - 1);
                            u16::from_le_bytes(payload[at..at + 2].try_into().unwrap())
                        }
                    })
                    .collect();
                out.push(Decoded {
                    kind: payload[0],
                    micros,
                    flags: payload[5],
                    data,
                    gaps,
                });
            }
            _ => out.push(Decoded {
                kind: payload[0],
                micros,
                flags: payload[5],
                data: payload[6..].to_vec(),
                gaps: vec![],
            }),
        }
    }
    out
}

fn drain<const N: usize>(bridge: &mut Bridge<N>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let n = bridge.read(&mut chunk);
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

#[test]
fn a_quiet_bus_produces_one_record_per_burst() {
    let mut bridge: Bridge<4096> = Bridge::new(Config::at_baud(9600));
    for (i, byte) in b"HELLO".iter().enumerate() {
        bridge.on_rx(1_000 + i as u32 * 1_042, *byte, 0);
    }
    bridge.on_idle(1_000_000);
    for (i, byte) in b"WORLD".iter().enumerate() {
        bridge.on_rx(1_000_000 + i as u32 * 1_042, *byte, 0);
    }
    bridge.on_idle(2_000_000);

    let records = decode_all(&drain(&mut bridge));
    assert_eq!(2, records.len(), "{records:?}");
    assert_eq!(b"HELLO".to_vec(), records[0].data);
    assert_eq!(b"WORLD".to_vec(), records[1].data);
    // The inter-byte gap: one character time at 9600 baud, which is the
    // number a host cannot measure and this device exists to report.
    assert_eq!(vec![0, 1042, 1042, 1042, 1042], records[0].gaps);
}

#[test]
fn our_own_transmission_is_tagged_as_ours() {
    // On a 3-wire half-duplex bus, once transmit is ever armed, your own echo
    // and a reply are the same bytes on the same conductors. Only the bridge
    // knows which, and this is where it says so.
    let mut bridge: Bridge<4096> = Bridge::new(Config::at_baud(9600));
    bridge.on_tx(1_000, 0x05);
    bridge.on_tx(2_042, 0x06);
    bridge.on_rx(20_000, 0x81, 0);
    bridge.on_idle(1_000_000);

    let records = decode_all(&drain(&mut bridge));
    assert_eq!(2, records.len(), "{records:?}");
    assert_eq!(kind::TX, records[0].kind);
    assert_eq!(vec![0x05, 0x06], records[0].data);
    assert_eq!(kind::RX, records[1].kind);
    assert_eq!(vec![0x81], records[1].data);
}

#[test]
fn a_direction_change_closes_the_open_record_in_order() {
    // Without this the TX record's timestamp would follow an RX record it
    // preceded, and the host would have to re-sort a transcript to see the
    // interleaving -- which IS the finding on a polled bus.
    let mut bridge: Bridge<4096> = Bridge::new(Config::with_gap(100_000));
    bridge.on_rx(1_000, b'a', 0);
    bridge.on_tx(1_100, b'B');
    bridge.on_rx(1_200, b'c', 0);
    bridge.flush(1_300);

    let records = decode_all(&drain(&mut bridge));
    let shape: Vec<(u8, u32)> = records.iter().map(|r| (r.kind, r.micros)).collect();
    assert_eq!(
        vec![(kind::RX, 1_000), (kind::TX, 1_100), (kind::RX, 1_200)],
        shape
    );
}

#[test]
fn an_error_byte_arrives_with_its_own_timestamp_and_flags() {
    let mut bridge: Bridge<4096> = Bridge::new(Config::with_gap(100_000));
    bridge.on_rx(1_000, b'a', 0);
    bridge.on_rx(1_100, 0xF0, flag::FRAMING | flag::NOISE);
    bridge.on_rx(1_200, b'b', 0);
    bridge.flush(1_300);

    let records = decode_all(&drain(&mut bridge));
    assert_eq!(3, records.len(), "{records:?}");
    assert_eq!(0, records[0].flags);
    assert_eq!(flag::FRAMING | flag::NOISE, records[1].flags);
    assert_eq!(1_100, records[1].micros);
    assert_eq!(vec![0xF0], records[1].data);
    assert_eq!(0, records[2].flags, "the error leaked into the next record");
    assert_eq!(1, bridge.counters().error_bytes);
}

#[test]
fn a_full_ring_loses_records_and_the_next_one_says_so() {
    // The failure this is guarding against is a SILENT gap. A transcript that
    // is short by three records and does not know it is worse than no
    // transcript, because somebody will conclude the bus went quiet.
    let mut bridge: Bridge<64> = Bridge::new(Config::with_gap(10));
    for i in 0..40u32 {
        bridge.on_rx(i * 1_000, b'x', 0);
        bridge.on_idle(i * 1_000 + 500);
    }
    assert!(
        bridge.counters().dropped_records > 0,
        "the ring never filled"
    );

    // Drain, then let one more record through: it must carry the flag.
    let _ = drain(&mut bridge);
    bridge.on_rx(100_000, b'z', 0);
    bridge.on_idle(200_000);
    let records = decode_all(&drain(&mut bridge));
    assert_eq!(1, records.len());
    assert_eq!(
        flag::DROPPED,
        records[0].flags & flag::DROPPED,
        "a gap in the transcript went unreported"
    );

    // And it is not sticky: the record after it is clean again.
    bridge.on_rx(300_000, b'y', 0);
    bridge.on_idle(400_000);
    let records = decode_all(&drain(&mut bridge));
    assert_eq!(0, records[0].flags & flag::DROPPED);
}

#[test]
fn a_status_record_carries_the_counters_and_the_build_variant() {
    let mut bridge: Bridge<4096> = Bridge::new(Config::at_baud(9600));
    bridge.on_rx(1_000, b'a', 0);
    bridge.on_rx(1_100, 0x00, flag::PARITY);
    bridge.on_status(
        9_000,
        flag::LISTEN_ONLY,
        "emco-rs485-bridge 0.1.0 listen-only 9600 8N1",
    );

    let stream = drain(&mut bridge);
    let records = decode_all(&stream);
    let status = records.last().expect("no status record");
    assert_eq!(kind::STATUS, status.kind);
    assert_eq!(flag::LISTEN_ONLY, status.flags & flag::LISTEN_ONLY);
    assert_eq!(0, status.flags & flag::TX_ARMED);

    let body = &status.data;
    assert_eq!(2, u32::from_le_bytes(body[0..4].try_into().unwrap())); // rx_bytes
    assert_eq!(0, u32::from_le_bytes(body[4..8].try_into().unwrap())); // tx_bytes
    assert_eq!(1, u32::from_le_bytes(body[12..16].try_into().unwrap())); // error_bytes
    let text = std::str::from_utf8(&body[16..]).expect("the description is not UTF-8");
    assert!(text.contains("listen-only"), "{text}");
}

#[test]
fn a_status_description_is_truncated_on_a_character_boundary() {
    // A description cut mid-codepoint is not UTF-8, and a decoder that then
    // refuses to read it turns a cosmetic overflow into a lost transcript.
    let mut bridge: Bridge<4096> = Bridge::new(Config::at_baud(9600));
    let long = "µ".repeat(80); // 160 bytes, cut inside a character
    bridge.on_status(1, 0, &long);
    let records = decode_all(&drain(&mut bridge));
    let text = std::str::from_utf8(&records[0].data[16..]);
    assert!(text.is_ok(), "the truncation produced invalid UTF-8");
}

#[test]
fn an_event_carries_a_code_and_an_argument() {
    let mut bridge: Bridge<4096> = Bridge::new(Config::at_baud(9600));
    bridge.on_event(0, event::BOOT, PROTOCOL_VERSION);
    let records = decode_all(&drain(&mut bridge));
    assert_eq!(kind::EVENT, records[0].kind);
    assert_eq!(event::BOOT, records[0].flags);
    assert_eq!(
        PROTOCOL_VERSION,
        u32::from_le_bytes(records[0].data[..4].try_into().unwrap())
    );
}

#[test]
fn no_frame_can_exceed_the_buffer_the_firmware_reserves() {
    // MAX_FRAME is what the firmware's stack buffer is sized from. An encoder
    // that could exceed it would fail on a full record, which is the one this
    // never sees on a quiet bench and always sees on a busy bus.
    let mut bridge: Bridge<8192> = Bridge::new(Config::with_gap(1_000_000));
    for i in 0..64u32 {
        bridge.on_rx(i, 0x00, 0); // zeros: the worst case for COBS
    }
    bridge.flush(0);
    let stream = drain(&mut bridge);
    let longest = stream.split(|&b| b == 0).map(|f| f.len()).max().unwrap();
    // Strictly less than: the frame is the payload plus its delimiter, and
    // MAX_FRAME is what the firmware's stack buffer is sized from.
    assert!(longest < MAX_FRAME, "{longest} + 1 > {MAX_FRAME}");
}

#[test]
fn a_reader_that_joins_late_resynchronises_at_the_next_zero() {
    // The everyday case: you plug the bridge into a laptop that was not
    // listening. Half a record arrives first, and COBS is what makes that
    // recoverable rather than a decoder that is one byte out forever.
    let mut bridge: Bridge<4096> = Bridge::new(Config::with_gap(1_000));
    for burst in 0..3u32 {
        for i in 0..4u32 {
            bridge.on_rx(burst * 100_000 + i * 100, b'a' + burst as u8, 0);
        }
        bridge.on_idle(burst * 100_000 + 50_000);
    }
    let stream = drain(&mut bridge);
    let late = &stream[3..]; // joined mid-frame
    let recovered: Vec<_> = late
        .iter()
        .position(|&b| b == 0)
        .map(|at| decode_all(&late[at..]))
        .expect("no delimiter to resynchronise on");
    assert_eq!(2, recovered.len(), "lost more than the frame we cut into");
    assert_eq!(b"bbbb".to_vec(), recovered[0].data);
}
