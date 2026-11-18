//! The lathe unit on the middleware: what the process model sends the
//! physical model besides S88 phase commands, and what the physical model
//! publishes.
//!
//! Every key is under `app/`, the one family a gateway carries between a
//! node and the cluster ([`PREFIX`]), and everything is a publication:
//! payloads cannot make requests, and a gateway does not carry requests to
//! a payload's served keys. So commands follow recipe-core's rule that "a
//! report naming a command's id acknowledges it": the process model
//! publishes a phase command, the physical model applies it and publishes
//! the phase's report.
//!
//! | key | from | body |
//! |-----|------|------|
//! | `app/unit/<node>/phase/<em>.<phase>` | process | a recipe-core command ([`PHASES`] lists the phases and their parameters) |
//! | `app/unit/<node>/phase/<em>.<phase>/state` | physical | a recipe-core report, on every change and as a heartbeat |
//! | `app/unit/<node>/motion/segments` | process | [`encode_segments`] |
//! | `app/unit/<node>/cm` | physical | `[cm index u8]` ([`CMS`]) then its [`CmState`], on every change |
//! | `app/unit/<node>/info` | physical | the machine: travel, speeds, spindle, belts, turret (`Limits::encode`), at start and with every heartbeat |
//!
//! `<node>` is the physical unit's node id as 16 lowercase hex digits, as
//! in recipe-core's keys. Segments are taken only while `motion.run` is
//! Running, a whole batch or none; the motion state (`cm` index 5) says
//! how many slots are free and the last segment done, and the process model
//! never sends more than that. A batch that does not fit, has a point
//! outside the machine's travel or a feed above its maximum is dropped
//! and counted, and `motion.run` aborts. Positions are **machine**
//! coordinates in µm, X as radius; work offsets are the process model's.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use middleware::wasm_abi::machine::{QueueState, Segment};

/// Body version of [`encode_segments`].
pub const SEGMENTS_VERSION: u8 = 1;
/// Most segments in one request.
pub const MAX_BATCH: usize = 16;

/// The prefix of every key: the family a gateway carries.
pub const PREFIX: &str = "app/";

/// The control modules that publish a state, by index.
pub const CMS: [&str; 6] = ["x", "z", "spindle", "turret", "safety", "motion"];

/// The physical unit's phases: `(em, phase, parameter ranges)` in the
/// order recipe-core takes Start's arguments. Ranges here are the widest a
/// machine could have; the payload checks its own [`crate::Limits`].
pub const PHASES: [(&str, &str, &[&str]); 6] = [
    ("motion", "reference", &[]),
    ("motion", "move", &["x_um", "z_um", "feed_um_s"]),
    ("motion", "run", &[]),
    ("spindle", "run", &["rpm"]),
    ("spindle", "stop", &[]),
    ("turret", "index", &["station"]),
];

/// The physical unit's class, as it announces itself (`s88/unit`).
pub const UNIT_CLASS: &str = "lathe";

/// A control module's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmState {
    /// An axis: referenced, and its machine position when it is.
    Axis { referenced: bool, position_um: i32 },
    /// The spindle: commanded and measured speed, rpm.
    Spindle { commanded: i32, actual: i32 },
    /// The turret: station, and whether it is locked there.
    Turret { station: u8, ready: bool },
    /// The e-stop chain, and whether motion is stopped (an e-stop or a
    /// limit, until acknowledged).
    Safety { chain_closed: bool, stopped: bool },
    /// The segment queue.
    Motion(QueueState),
}

impl CmState {
    /// Longest encoding.
    pub const MAX_LEN: usize = 9;

    /// Encodes into `out`; its length.
    pub fn encode(&self, out: &mut [u8; Self::MAX_LEN]) -> usize {
        match *self {
            CmState::Axis {
                referenced,
                position_um,
            } => {
                out[0] = 1;
                out[1] = referenced as u8;
                out[2..6].copy_from_slice(&position_um.to_le_bytes());
                6
            }
            CmState::Spindle { commanded, actual } => {
                out[0] = 2;
                out[1..5].copy_from_slice(&commanded.to_le_bytes());
                out[5..9].copy_from_slice(&actual.to_le_bytes());
                9
            }
            CmState::Turret { station, ready } => {
                out[0] = 3;
                out[1] = station;
                out[2] = ready as u8;
                3
            }
            CmState::Safety {
                chain_closed,
                stopped,
            } => {
                out[0] = 4;
                out[1] = chain_closed as u8;
                out[2] = stopped as u8;
                3
            }
            CmState::Motion(q) => {
                out[0] = 5;
                out[1..8].copy_from_slice(&q.encode());
                8
            }
        }
    }

    /// Decodes one; `None` for another length or tag.
    pub fn decode(b: &[u8]) -> Option<Self> {
        let flag = |v: u8| match v {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        let i32_at = |at: usize| i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        Some(match (b.first()?, b.len()) {
            (1, 6) => CmState::Axis {
                referenced: flag(b[1])?,
                position_um: i32_at(2),
            },
            (2, 9) => CmState::Spindle {
                commanded: i32_at(1),
                actual: i32_at(5),
            },
            (3, 3) => CmState::Turret {
                station: b[1],
                ready: flag(b[2])?,
            },
            (4, 3) => CmState::Safety {
                chain_closed: flag(b[1])?,
                stopped: flag(b[2])?,
            },
            (5, 8) => CmState::Motion(QueueState::decode(&b[1..])?),
            _ => return None,
        })
    }
}

/// `app/unit/<node>` + `rest`, as text in `out`.
fn node_key(node: [u8; 8], rest: &str, out: &mut [u8]) -> Option<usize> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut at = 0;
    let mut put = |b: &[u8]| -> Option<()> {
        out.get_mut(at..at + b.len())?.copy_from_slice(b);
        at += b.len();
        Some(())
    };
    put(PREFIX.as_bytes())?;
    put(b"unit/")?;
    for b in node {
        put(&[HEX[usize::from(b >> 4)], HEX[usize::from(b & 15)]])?;
    }
    put(rest.as_bytes())?;
    Some(at)
}

/// Longest key this module writes: a phase's state key.
pub const MAX_KEY: usize = PREFIX.len() + recipe_core::wire::MAX_KEY;

/// A key in its buffer.
#[derive(Clone, Copy)]
pub struct KeyText {
    buf: [u8; MAX_KEY],
    len: usize,
}

impl KeyText {
    /// The key.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    fn of(f: impl FnOnce(&mut [u8; MAX_KEY]) -> Option<usize>) -> Option<Self> {
        let mut buf = [0u8; MAX_KEY];
        let len = f(&mut buf)?;
        Some(KeyText { buf, len })
    }
}

impl core::fmt::Debug for KeyText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `app/unit/<node>/cm`: every control module's state.
pub fn cm_key(node: [u8; 8]) -> Option<KeyText> {
    KeyText::of(|b| node_key(node, "/cm", b))
}

/// `app/unit/<node>/info`: the machine (`Limits::encode`).
pub fn info_key(node: [u8; 8]) -> Option<KeyText> {
    KeyText::of(|b| node_key(node, "/info", b))
}

/// `app/unit/<node>/motion/segments`.
pub fn segments_key(node: [u8; 8]) -> Option<KeyText> {
    KeyText::of(|b| node_key(node, "/motion/segments", b))
}

/// `app/` + recipe-core's command key for `em.phase`.
pub fn phase_key(node: [u8; 8], em: &str, phase: &str) -> Option<KeyText> {
    KeyText::of(|b| {
        let (head, tail) = b.split_at_mut(PREFIX.len());
        head.copy_from_slice(PREFIX.as_bytes());
        recipe_core::wire::command_key(node, em, phase, tail)
            .ok()
            .map(|k| PREFIX.len() + k.len())
    })
}

/// `app/` + recipe-core's state key for `em.phase`.
pub fn phase_state_key(node: [u8; 8], em: &str, phase: &str) -> Option<KeyText> {
    KeyText::of(|b| {
        let (head, tail) = b.split_at_mut(PREFIX.len());
        head.copy_from_slice(PREFIX.as_bytes());
        recipe_core::wire::state_key(node, em, phase, tail)
            .ok()
            .map(|k| PREFIX.len() + k.len())
    })
}

/// A control module's state as published on [`cm_key`]: its index, then
/// the state. `None` for an index past [`CMS`].
pub fn encode_cm(
    index: usize,
    state: &CmState,
    out: &mut [u8; 1 + CmState::MAX_LEN],
) -> Option<usize> {
    if index >= CMS.len() {
        return None;
    }
    out[0] = index as u8;
    let mut b = [0u8; CmState::MAX_LEN];
    let n = state.encode(&mut b);
    out[1..1 + n].copy_from_slice(&b[..n]);
    Some(1 + n)
}

/// [`encode_cm`]'s inverse.
pub fn decode_cm(b: &[u8]) -> Option<(usize, CmState)> {
    let (&i, rest) = b.split_first()?;
    let i = usize::from(i);
    (i < CMS.len()).then_some(())?;
    Some((i, CmState::decode(rest)?))
}

/// Longest batch body.
pub const MAX_BATCH_BODY: usize = 3 + MAX_BATCH * Segment::LEN;

/// [`encode_segments`] into a buffer; its length.
pub fn encode_segments_into(
    segments: &[Segment],
    last: bool,
    out: &mut [u8; MAX_BATCH_BODY],
) -> Option<usize> {
    if segments.len() > MAX_BATCH {
        return None;
    }
    out[0] = SEGMENTS_VERSION;
    out[1] = segments.len() as u8;
    out[2] = last as u8;
    for (i, s) in segments.iter().enumerate() {
        out[3 + i * Segment::LEN..3 + (i + 1) * Segment::LEN].copy_from_slice(&s.encode());
    }
    Some(3 + segments.len() * Segment::LEN)
}

/// A batch of segments as a publication: `[version][n][last u8]` then
/// the segments; `last` says the program has no more.
#[cfg(feature = "alloc")]
pub fn encode_segments(segments: &[Segment], last: bool) -> Option<Vec<u8>> {
    if segments.len() > MAX_BATCH {
        return None;
    }
    let mut out = Vec::with_capacity(3 + segments.len() * Segment::LEN);
    out.push(SEGMENTS_VERSION);
    out.push(segments.len() as u8);
    out.push(last as u8);
    for s in segments {
        out.extend_from_slice(&s.encode());
    }
    Some(out)
}

/// A decoded batch.
pub struct Batch<'a> {
    /// The program has no more segments after these.
    pub last: bool,
    body: &'a [u8],
}

impl<'a> Batch<'a> {
    /// Its segments.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + 'a {
        self.body
            .chunks_exact(Segment::LEN)
            .filter_map(Segment::decode)
    }

    /// How many.
    pub fn len(&self) -> usize {
        self.body.len() / Segment::LEN
    }

    /// None at all.
    pub fn is_empty(&self) -> bool {
        self.body.is_empty()
    }
}

/// A batch, checked; `None` if malformed (a bad segment anywhere refuses
/// it all).
pub fn decode_segments(body: &[u8]) -> Option<Batch<'_>> {
    let (&[v, n, last], rest) = body.split_first_chunk::<3>()?;
    let n = usize::from(n);
    if v != SEGMENTS_VERSION || n > MAX_BATCH || rest.len() != n * Segment::LEN || last > 1 {
        return None;
    }
    if rest
        .chunks_exact(Segment::LEN)
        .any(|c| Segment::decode(c).is_none())
    {
        return None;
    }
    Some(Batch {
        last: last == 1,
        body: rest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cm_states_round_trip_and_never_alias() {
        let all = [
            CmState::Axis {
                referenced: true,
                position_um: -12_345,
            },
            CmState::Spindle {
                commanded: -800,
                actual: -790,
            },
            CmState::Turret {
                station: 4,
                ready: false,
            },
            CmState::Safety {
                chain_closed: true,
                stopped: false,
            },
            CmState::Motion(QueueState {
                free: 9,
                done: 77,
                flags: QueueState::IDLE,
            }),
        ];
        for s in all {
            let mut b = [0u8; CmState::MAX_LEN];
            let n = s.encode(&mut b);
            assert_eq!(CmState::decode(&b[..n]), Some(s));
            assert_eq!(CmState::decode(&b[..n - 1]), None);
        }
        assert_eq!(
            CmState::decode(&[1, 2, 0, 0, 0, 0]),
            None,
            "a flag is 0 or 1"
        );
        assert_eq!(CmState::decode(&[]), None);
    }

    #[test]
    fn keys() {
        let node = [0x9a, 0x51, 0x0c, 0x77, 0x02, 0xe4, 0x18, 0x3b];
        assert_eq!(
            cm_key(node).unwrap().as_str(),
            "app/unit/9a510c7702e4183b/cm"
        );
        assert_eq!(
            segments_key(node).unwrap().as_str(),
            "app/unit/9a510c7702e4183b/motion/segments"
        );
        assert_eq!(
            phase_key(node, "motion", "reference").unwrap().as_str(),
            "app/unit/9a510c7702e4183b/phase/motion.reference"
        );
        assert_eq!(
            phase_state_key(node, "spindle", "run").unwrap().as_str(),
            "app/unit/9a510c7702e4183b/phase/spindle.run/state"
        );
        assert!(phase_key(node, "mo tion", "x").is_none());
        // recipe-core reads the key once the prefix is off.
        let k = phase_key(node, "turret", "index").unwrap();
        let p = recipe_core::wire::parse_key(&k.as_str()[PREFIX.len()..]).unwrap();
        assert_eq!((p.node, p.em, p.phase), (node, "turret", "index"));
    }

    #[test]
    fn cm_publications() {
        let mut b = [0u8; 1 + CmState::MAX_LEN];
        let s = CmState::Axis {
            referenced: true,
            position_um: 5,
        };
        let n = encode_cm(1, &s, &mut b).unwrap();
        assert_eq!(decode_cm(&b[..n]), Some((1, s)));
        assert!(encode_cm(6, &s, &mut b).is_none());
        b[0] = 9;
        assert_eq!(decode_cm(&b[..n]), None);
    }

    #[test]
    fn batches_encode_into_a_buffer_alike() {
        let segs = [Segment {
            seq: 4,
            x_um: 1,
            z_um: 2,
            feed: 3,
            flags: 0,
        }; 2];
        let mut b = [0u8; MAX_BATCH_BODY];
        let n = encode_segments_into(&segs, false, &mut b).unwrap();
        assert_eq!(&b[..n], &encode_segments(&segs, false).unwrap()[..]);
    }

    #[test]
    fn segment_batches() {
        let segs: Vec<Segment> = (1..=3)
            .map(|i| Segment {
                seq: i,
                x_um: 0,
                z_um: -1000 * i as i32,
                feed: 500,
                flags: 0,
            })
            .collect();
        let body = encode_segments(&segs, true).unwrap();
        assert_eq!(body.len(), 3 + 3 * Segment::LEN);
        let b = decode_segments(&body).unwrap();
        assert!(b.last && b.len() == 3);
        assert!(b.segments().eq(segs.iter().copied()));
        assert!(encode_segments(&[segs[0]; MAX_BATCH + 1], false).is_none());
        assert!(decode_segments(&body[..body.len() - 1]).is_none());
        let mut bad = body.clone();
        bad[3 + 16] = 0x40;
        assert!(
            decode_segments(&bad).is_none(),
            "a bad flag anywhere refuses the batch"
        );
        assert!(decode_segments(&[2, 0, 0]).is_none());
        assert!(decode_segments(&[1, 0, 2]).is_none());
    }
}
