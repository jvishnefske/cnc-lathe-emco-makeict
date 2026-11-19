//! The process model's own wire: what an operator's tool (the KD240
//! service's `MeshTransport`, the fleet agent's interlock, the
//! host-in-the-loop tests) sends the process payload and reads back.
//!
//! | key | from | body |
//! |-----|------|------|
//! | `app/proc/<node>/cmd` | operator | a [`Cmd`] |
//! | `app/proc/<node>/program` | operator | [`program_chunks`] |
//! | `app/proc/<node>/state` | process | a [`ProcState`], on every change and every [`STATE_HEARTBEAT_MS`] |
//!
//! `<node>` is the process payload's node id as 16 lowercase hex digits.
//! The state is republished as a heartbeat so that a tool that subscribes
//! late (the fleet agent runs once a minute) learns it within a second,
//! and so that its silence means the process model is gone.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// How often the process model republishes its state unchanged, ms.
pub const STATE_HEARTBEAT_MS: u32 = 1000;

/// Most program text bytes in one [`program_chunks`] publication.
pub const PROGRAM_CHUNK: usize = 400;

/// Longest message a [`ProcState`] carries, bytes.
pub const MAX_MESSAGE: usize = 200;

/// An operator command (`app/proc/<node>/cmd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// Drive the physical unit with this node id: `[1][node 8]`.
    Assign([u8; 8]),
    /// Reference the machine: `[2]`.
    Reference,
    /// Run a stored program with the belt the operator says is fitted:
    /// `[3][program u32][belt u8]`.
    Run { program: u32, belt: u8 },
    /// Feed hold: `[4]`.
    Hold,
    /// Resume after a hold: `[5]`.
    Resume,
    /// Stop the job: `[6]`.
    Stop,
    /// Forget a finished or failed job: `[7]`.
    Clear,
}

impl Cmd {
    /// Decodes a command body.
    pub fn decode(b: &[u8]) -> Option<Cmd> {
        Some(match b {
            [1, node @ ..] if node.len() == 8 => Cmd::Assign(node.try_into().ok()?),
            [2] => Cmd::Reference,
            [3, p0, p1, p2, p3, belt] => Cmd::Run {
                program: u32::from_le_bytes([*p0, *p1, *p2, *p3]),
                belt: *belt,
            },
            [4] => Cmd::Hold,
            [5] => Cmd::Resume,
            [6] => Cmd::Stop,
            [7] => Cmd::Clear,
            _ => return None,
        })
    }

    /// Encodes it.
    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Cmd::Assign(n) => [&[1u8][..], &n].concat(),
            Cmd::Reference => vec![2],
            Cmd::Run { program, belt } => {
                let p = program.to_le_bytes();
                vec![3, p[0], p[1], p[2], p[3], belt]
            }
            Cmd::Hold => vec![4],
            Cmd::Resume => vec![5],
            Cmd::Stop => vec![6],
            Cmd::Clear => vec![7],
        }
    }
}

/// What the process model is doing, as it publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RunState {
    Idle = 0,
    Referencing = 1,
    Running = 2,
    Held = 3,
    Done = 4,
    Failed = 5,
}

impl RunState {
    /// From its byte; `None` for an unknown one.
    pub fn from_u8(b: u8) -> Option<RunState> {
        Some(match b {
            0 => RunState::Idle,
            1 => RunState::Referencing,
            2 => RunState::Running,
            3 => RunState::Held,
            4 => RunState::Done,
            5 => RunState::Failed,
            _ => return None,
        })
    }

    /// True while the process model may be moving the machine: a
    /// reference, a program, or a program held mid-way (its spindle may
    /// still turn).
    pub fn busy(self) -> bool {
        matches!(
            self,
            RunState::Referencing | RunState::Running | RunState::Held
        )
    }
}

/// A program the process model holds whole: its id, length and
/// [`program_hash`]. The state names the last one completed, so a tool
/// that uploaded a program knows the process model has exactly that text
/// before it asks to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredProgram {
    pub id: u32,
    pub len: u32,
    pub hash: u64,
}

impl StoredProgram {
    /// The record for `text` stored as `id`.
    pub fn of(id: u32, text: &[u8]) -> StoredProgram {
        StoredProgram {
            id,
            len: text.len() as u32,
            hash: program_hash(text),
        }
    }
}

/// FNV-1a, 64 bits, over a program's bytes: enough to tell one upload
/// from another, not a defence against a forger (who could publish any
/// state anyway; publications are not authenticated).
pub fn program_hash(text: &[u8]) -> u64 {
    text.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The process model's state (`app/proc/<node>/state`):
/// `[run u8][program u32][op u16][ops u16][n u8][message n][assigned u8]`,
/// then the unit's node id (8 bytes) when `assigned` is 1, then
/// `[stored u8]` and, when it is 1, the last program completed
/// (`[id u32][len u32][hash u64]`, [`StoredProgram`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcState {
    pub run: RunState,
    /// The program running, 0 when none.
    pub program: u32,
    /// The operation it is at, of `ops`.
    pub op: u16,
    pub ops: u16,
    /// Why it failed; empty otherwise. At most [`MAX_MESSAGE`] bytes.
    pub message: String,
    /// The physical unit it drives.
    pub unit: Option<[u8; 8]>,
    /// The last program it received whole.
    pub stored: Option<StoredProgram>,
}

impl ProcState {
    /// Encodes it (a longer message is cut at [`MAX_MESSAGE`] bytes, on a
    /// character boundary).
    pub fn encode(&self) -> Vec<u8> {
        let mut end = self.message.len().min(MAX_MESSAGE);
        while !self.message.is_char_boundary(end) {
            end -= 1;
        }
        let msg = &self.message.as_bytes()[..end];
        let mut b = vec![self.run as u8];
        b.extend_from_slice(&self.program.to_le_bytes());
        b.extend_from_slice(&self.op.to_le_bytes());
        b.extend_from_slice(&self.ops.to_le_bytes());
        b.push(msg.len() as u8);
        b.extend_from_slice(msg);
        match self.unit {
            Some(u) => {
                b.push(1);
                b.extend_from_slice(&u);
            }
            None => b.push(0),
        }
        match self.stored {
            Some(p) => {
                b.push(1);
                b.extend_from_slice(&p.id.to_le_bytes());
                b.extend_from_slice(&p.len.to_le_bytes());
                b.extend_from_slice(&p.hash.to_le_bytes());
            }
            None => b.push(0),
        }
        b
    }

    /// Decodes one; `None` if malformed.
    pub fn decode(b: &[u8]) -> Option<ProcState> {
        let (&[s, p0, p1, p2, p3, a0, a1, o0, o1, n], rest) = b.split_first_chunk::<10>()?;
        let n = usize::from(n);
        if rest.len() < n || n > MAX_MESSAGE {
            return None;
        }
        let (msg, tail) = rest.split_at(n);
        let (unit, tail) = match tail {
            [0, rest @ ..] => (None, rest),
            [1, rest @ ..] if rest.len() >= 8 => {
                let (u, rest) = rest.split_at(8);
                (Some(u.try_into().ok()?), rest)
            }
            _ => return None,
        };
        let stored = match tail {
            [0] => None,
            [1, r @ ..] if r.len() == 16 => Some(StoredProgram {
                id: u32::from_le_bytes(r[0..4].try_into().ok()?),
                len: u32::from_le_bytes(r[4..8].try_into().ok()?),
                hash: u64::from_le_bytes(r[8..16].try_into().ok()?),
            }),
            _ => return None,
        };
        Some(ProcState {
            run: RunState::from_u8(s)?,
            program: u32::from_le_bytes([p0, p1, p2, p3]),
            op: u16::from_le_bytes([a0, a1]),
            ops: u16::from_le_bytes([o0, o1]),
            message: String::from_utf8_lossy(msg).into_owned(),
            unit,
            stored,
        })
    }
}

/// A node id as 16 lowercase hex digits.
pub fn node_hex(node: [u8; 8]) -> String {
    node.iter().map(|b| format!("{b:02x}")).collect()
}

/// [`node_hex`]'s inverse: exactly 16 hex digits (either case).
pub fn parse_node(s: &str) -> Option<[u8; 8]> {
    let s = s.trim();
    if s.len() != 16 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// `app/proc/<node>/<what>`.
pub fn own_key(node: [u8; 8], what: &str) -> String {
    format!("app/proc/{}/{what}", node_hex(node))
}

/// A program as publications on `app/proc/<node>/program`: `[id u32][offset
/// u32][last u8]` then up to [`PROGRAM_CHUNK`] bytes each.
pub fn program_chunks(id: u32, text: &str) -> Vec<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let n = (bytes.len() - at).min(PROGRAM_CHUNK);
        let mut b = Vec::with_capacity(9 + n);
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&(at as u32).to_le_bytes());
        b.push(u8::from(at + n == bytes.len()));
        b.extend_from_slice(&bytes[at..at + n]);
        out.push(b);
        at += n;
        if at == bytes.len() {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip() {
        for c in [
            Cmd::Assign([1, 2, 3, 4, 5, 6, 7, 8]),
            Cmd::Reference,
            Cmd::Run {
                program: 7,
                belt: 2,
            },
            Cmd::Hold,
            Cmd::Resume,
            Cmd::Stop,
            Cmd::Clear,
        ] {
            assert_eq!(Cmd::decode(&c.encode()), Some(c));
        }
        assert_eq!(Cmd::decode(&[3, 0]), None);
    }

    #[test]
    fn states_round_trip() {
        for s in [
            ProcState {
                run: RunState::Idle,
                program: 0,
                op: 0,
                ops: 0,
                message: String::new(),
                unit: None,
                stored: None,
            },
            ProcState {
                run: RunState::Failed,
                program: 9,
                op: 3,
                ops: 12,
                message: "not referenced".into(),
                unit: Some([0x9a, 1, 2, 3, 4, 5, 6, 0xff]),
                stored: Some(StoredProgram::of(9, b"M30\n")),
            },
            ProcState {
                run: RunState::Done,
                program: 0,
                op: 0,
                ops: 0,
                message: String::new(),
                unit: None,
                stored: Some(StoredProgram::of(u32::MAX, b"")),
            },
        ] {
            assert_eq!(ProcState::decode(&s.encode()), Some(s));
        }
        let s = ProcState {
            run: RunState::Failed,
            program: 0,
            op: 0,
            ops: 0,
            message: "é".repeat(150),
            unit: None,
            stored: None,
        };
        let back = ProcState::decode(&s.encode()).unwrap();
        assert!(back.message.len() <= MAX_MESSAGE && back.message.ends_with('é'));
        let z = |tail: &[u8]| [&[0u8; 10][..], tail].concat();
        assert!(ProcState::decode(&z(&[0, 0])).is_some());
        assert_eq!(
            ProcState::decode(&[&[9u8][..], &z(&[0, 0])[1..]].concat()),
            None
        );
        assert_eq!(ProcState::decode(&z(&[])), None);
        assert_eq!(ProcState::decode(&z(&[0])), None, "no stored flag");
        assert_eq!(ProcState::decode(&z(&[1, 1])), None);
        assert_eq!(ProcState::decode(&z(&[0, 1, 0])), None, "a short record");
    }

    /// The fleet agent reads the run state as the state's first byte
    /// (kd240/fleet/proto/src/policy.rs, run_state_from_process), from a
    /// workspace that does not depend on this crate: these are frozen.
    #[test]
    fn run_state_bytes_are_frozen() {
        for (s, b) in [
            (RunState::Idle, 0),
            (RunState::Referencing, 1),
            (RunState::Running, 2),
            (RunState::Held, 3),
            (RunState::Done, 4),
            (RunState::Failed, 5),
        ] {
            assert_eq!(s as u8, b);
            assert_eq!(RunState::from_u8(b), Some(s));
        }
    }

    #[test]
    fn program_hashes_tell_texts_apart() {
        assert_eq!(program_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_ne!(program_hash(b"G0 X1\n"), program_hash(b"G0 X2\n"));
        assert_eq!(StoredProgram::of(3, b"M30\n").len, 4);
    }

    #[test]
    fn node_ids_as_text() {
        let n = [0x9a, 0x51, 0x0c, 0x77, 0x02, 0xe4, 0x18, 0x3b];
        assert_eq!(node_hex(n), "9a510c7702e4183b");
        assert_eq!(parse_node("9A510C7702E4183B"), Some(n));
        assert_eq!(parse_node("9a510c7702e4183"), None);
        assert_eq!(parse_node("9a510c7702e4183g"), None);
        assert_eq!(own_key(n, "cmd"), "app/proc/9a510c7702e4183b/cmd");
    }

    #[test]
    fn programs_chunk_and_the_last_says_so() {
        let text = "G0 X1\n".repeat(100);
        let c = program_chunks(3, &text);
        assert_eq!(c.len(), text.len().div_ceil(PROGRAM_CHUNK));
        assert!(c.iter().rev().skip(1).all(|b| b[8] == 0));
        assert_eq!(c.last().unwrap()[8], 1);
        assert_eq!(program_chunks(1, "").len(), 1);
    }
}
