//! The A/B boot state, shared by Linux (the agent) and U-Boot (`boot.scr`).
//!
//! Two copies, each one 512-byte sector in the unpartitioned gap before the
//! first partition ([`COPY_LBA`], 1 MiB apart so one torn write cannot hit
//! both). A copy is:
//!
//! ```text
//! bytes 0..4    CRC-32 (IEEE, as U-Boot's `crc32`), big-endian, of bytes 4..512
//! bytes 4..     text, as U-Boot's `env export -t` writes it, NUL-padded:
//!                 ab_active=a
//!                 ab_magic=emco-ab-1
//!                 ab_seq=1f
//!                 ab_tries=0
//!                 ab_try=-
//! ```
//!
//! Numbers are lowercase hex without `0x`, which is how U-Boot's `setexpr`
//! prints and `itest` reads them. The newest valid copy (highest `ab_seq`)
//! wins. A writer, U-Boot or Linux, always overwrites the *other* copy with
//! `seq + 1`, so a write cut short by a power loss leaves the previous
//! state readable.
//!
//! * `ab_active`: the committed slot, booted when there is nothing to try.
//! * `ab_try`, `ab_tries`: a slot to boot `tries` more times. U-Boot
//!   decrements before it boots it; Linux commits it (`active = try`) once
//!   the system is healthy. If it never gets there, the count runs out and
//!   U-Boot boots `active` again: the rollback.

use std::fmt;

pub const MAGIC: &str = "emco-ab-1";
pub const SECTOR: usize = 512;
/// The two copies' sectors: 2 MiB and 3 MiB into the card, in the gap
/// before partition 1 (which starts at 8 MiB).
pub const COPY_LBA: [u64; 2] = [4096, 6144];

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Slot::A => "a",
            Slot::B => "b",
        }
    }
    pub fn parse(s: &str) -> Option<Slot> {
        match s {
            "a" => Some(Slot::A),
            "b" => Some(Slot::B),
            _ => None,
        }
    }
    /// The slot this boot came from: `emco.slot=a|b` on the kernel command
    /// line, as boot.scr sets it.
    pub fn from_cmdline(cmdline: &str) -> Option<Slot> {
        cmdline
            .split_whitespace()
            .filter_map(|w| w.strip_prefix("emco.slot="))
            .next_back()
            .and_then(Slot::parse)
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootState {
    pub seq: u32,
    pub active: Slot,
    pub try_slot: Option<Slot>,
    pub tries: u32,
}

impl BootState {
    /// What a fresh card holds: slot A, nothing to try.
    pub fn initial() -> BootState {
        BootState {
            seq: 1,
            active: Slot::A,
            try_slot: None,
            tries: 0,
        }
    }

    /// Is there a trial boot still to come?
    pub fn pending_try(&self) -> Option<Slot> {
        self.try_slot.filter(|_| self.tries > 0)
    }
}

/// CRC-32/IEEE (zlib's, U-Boot's).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

pub fn encode(s: &BootState) -> [u8; SECTOR] {
    // Sorted by name, as `env export` writes them.
    let text = format!(
        "ab_active={}\nab_magic={MAGIC}\nab_seq={:x}\nab_tries={:x}\nab_try={}\n",
        s.active,
        s.seq,
        s.tries,
        s.try_slot.map(Slot::as_str).unwrap_or("-"),
    );
    let mut out = [0u8; SECTOR];
    out[4..4 + text.len()].copy_from_slice(text.as_bytes());
    let crc = crc32(&out[4..]);
    out[..4].copy_from_slice(&crc.to_be_bytes());
    out
}

fn hex_u32(v: &str) -> Option<u32> {
    let v = v.strip_prefix("0x").unwrap_or(v);
    if v.is_empty() || v.len() > 8 {
        return None;
    }
    u32::from_str_radix(v, 16).ok()
}

pub fn decode(sector: &[u8]) -> Result<BootState, String> {
    if sector.len() != SECTOR {
        return Err(format!("{} bytes, want {SECTOR}", sector.len()));
    }
    let want = u32::from_be_bytes(sector[..4].try_into().unwrap());
    if crc32(&sector[4..]) != want {
        return Err("bad CRC".into());
    }
    let body = &sector[4..];
    let end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
    if body[end..].iter().any(|&b| b != 0) {
        return Err("data after the terminating NUL".into());
    }
    let text = std::str::from_utf8(&body[..end]).map_err(|_| "not UTF-8")?;
    let (mut magic, mut seq, mut active, mut try_slot, mut tries) = (None, None, None, None, None);
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once('=').ok_or("a line without =")?;
        let slot = match k {
            "ab_magic" => &mut magic,
            "ab_seq" => &mut seq,
            "ab_active" => &mut active,
            "ab_try" => &mut try_slot,
            "ab_tries" => &mut tries,
            other => return Err(format!("unknown key {other:?}")),
        };
        if slot.replace(v).is_some() {
            return Err(format!("{k} twice"));
        }
    }
    if magic != Some(MAGIC) {
        return Err(format!("magic {magic:?}, want {MAGIC:?}"));
    }
    let seq = seq.and_then(hex_u32).ok_or("bad ab_seq")?;
    let active = active.and_then(Slot::parse).ok_or("bad ab_active")?;
    let try_slot = match try_slot.ok_or("no ab_try")? {
        "-" => None,
        s => Some(Slot::parse(s).ok_or("bad ab_try")?),
    };
    let tries = tries.and_then(hex_u32).ok_or("bad ab_tries")?;
    Ok(BootState {
        seq,
        active,
        try_slot,
        tries,
    })
}

/// The two copies as read: which one is newest, and its state. `None` when
/// neither is valid.
pub fn newest(copies: [&[u8]; 2]) -> Option<(usize, BootState)> {
    let a = decode(copies[0]).ok();
    let b = decode(copies[1]).ok();
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.seq > a.seq { (1, b) } else { (0, a) }),
        (Some(a), None) => Some((0, a)),
        (None, Some(b)) => Some((1, b)),
        (None, None) => None,
    }
}

/// Where and what to write to replace the current state with `next`
/// (whose `seq` is ignored): the copy that is not the newest, with the next
/// sequence number.
pub fn plan_write(copies: [&[u8]; 2], next: BootState) -> (usize, [u8; SECTOR]) {
    let (index, seq) = match newest(copies) {
        Some((i, s)) => (1 - i, s.seq.wrapping_add(1)),
        None => (0, 1),
    };
    let next = BootState { seq, ..next };
    (index, encode(&next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(seq: u32, active: Slot, try_slot: Option<Slot>, tries: u32) -> BootState {
        BootState {
            seq,
            active,
            try_slot,
            tries,
        }
    }

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn round_trip() {
        for s in [
            BootState::initial(),
            st(0x1f, Slot::B, Some(Slot::A), 1),
            st(u32::MAX, Slot::A, None, 0),
        ] {
            assert_eq!(decode(&encode(&s)), Ok(s));
        }
    }

    #[test]
    fn the_layout_u_boot_sees() {
        let e = encode(&st(0x1f, Slot::A, Some(Slot::B), 1));
        let text = std::str::from_utf8(&e[4..]).unwrap().trim_end_matches('\0');
        assert_eq!(
            text,
            "ab_active=a\nab_magic=emco-ab-1\nab_seq=1f\nab_tries=1\nab_try=b\n"
        );
        assert_eq!(&e[..4], &crc32(&e[4..]).to_be_bytes());
    }

    #[test]
    fn what_u_boot_writes_reads_back() {
        // `env export -t` output, in any order, with 0x numbers too.
        let mut s = [0u8; SECTOR];
        let text = b"ab_try=b\nab_tries=0\nab_seq=0x20\nab_magic=emco-ab-1\nab_active=a\n";
        s[4..4 + text.len()].copy_from_slice(text);
        let crc = crc32(&s[4..]);
        s[..4].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(decode(&s), Ok(st(0x20, Slot::A, Some(Slot::B), 0)));
    }

    #[test]
    fn corrupt_copies_are_refused() {
        let good = encode(&BootState::initial());
        let mut flipped = good;
        flipped[10] ^= 1;
        assert!(decode(&flipped).is_err());
        assert!(
            decode(&[0u8; SECTOR]).is_err(),
            "all zero: CRC of zeros is not 0"
        );
        assert!(decode(&[0xffu8; SECTOR]).is_err());
        assert!(decode(&good[..100]).is_err());

        let with = |text: &[u8]| {
            let mut s = [0u8; SECTOR];
            s[4..4 + text.len()].copy_from_slice(text);
            let crc = crc32(&s[4..]);
            s[..4].copy_from_slice(&crc.to_be_bytes());
            s
        };
        for bad in [
            &b"ab_active=a\nab_magic=other\nab_seq=1\nab_tries=0\nab_try=-\n"[..],
            b"ab_active=c\nab_magic=emco-ab-1\nab_seq=1\nab_tries=0\nab_try=-\n",
            b"ab_active=a\nab_magic=emco-ab-1\nab_seq=1\nab_tries=0\n",
            b"ab_active=a\nab_magic=emco-ab-1\nab_seq=1\nab_tries=0\nab_try=-\nbootcmd=reset\n",
            b"ab_active=a\nab_active=b\nab_magic=emco-ab-1\nab_seq=1\nab_tries=0\nab_try=-\n",
            b"ab_active=a\nab_magic=emco-ab-1\nab_seq=zz\nab_tries=0\nab_try=-\n",
            b"ab_active=a\nab_magic=emco-ab-1\nab_seq=1\nab_tries=0\nab_try=-\n\0junk",
        ] {
            assert!(
                decode(&with(bad)).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn newest_valid_copy_wins() {
        let old = encode(&st(5, Slot::A, None, 0));
        let new = encode(&st(6, Slot::A, Some(Slot::B), 1));
        assert_eq!(
            newest([&old, &new]),
            Some((1, st(6, Slot::A, Some(Slot::B), 1)))
        );
        assert_eq!(newest([&new, &old]).unwrap().0, 0);
        let torn = [0x55u8; SECTOR];
        assert_eq!(newest([&torn, &old]), Some((1, st(5, Slot::A, None, 0))));
        assert_eq!(newest([&old, &torn]).unwrap().0, 0);
        assert_eq!(newest([&torn, &torn]), None);
    }

    #[test]
    fn writes_go_to_the_other_copy() {
        let a = encode(&st(5, Slot::A, None, 0));
        let b = encode(&st(4, Slot::A, None, 0));
        let next = st(0, Slot::A, Some(Slot::B), 1);
        let (i, bytes) = plan_write([&a, &b], next);
        assert_eq!(i, 1);
        assert_eq!(decode(&bytes), Ok(st(6, Slot::A, Some(Slot::B), 1)));
        // With nothing valid, start at copy 0, seq 1.
        let z = [0u8; SECTOR];
        let (i, bytes) = plan_write([&z, &z], next);
        assert_eq!((i, decode(&bytes).unwrap().seq), (0, 1));
    }

    #[test]
    fn a_torn_write_keeps_the_previous_state() {
        // Simulate the write of state 6 into copy 1 cut off at every byte:
        // whatever lands, the reader sees either 5 (old) or 6 (new), never
        // a mixture and never nothing.
        let a = encode(&st(5, Slot::A, None, 0));
        let b = encode(&st(4, Slot::A, None, 0));
        let (i, new) = plan_write([&a, &b], st(0, Slot::A, Some(Slot::B), 1));
        assert_eq!(i, 1);
        for cut in 0..=SECTOR {
            let mut torn = b;
            torn[..cut].copy_from_slice(&new[..cut]);
            let (_, s) = newest([&a, &torn]).expect("always something valid");
            assert!(
                s == st(5, Slot::A, None, 0) || s == st(6, Slot::A, Some(Slot::B), 1),
                "cut {cut}: {s:?}"
            );
        }
    }

    #[test]
    fn slots_and_the_command_line() {
        assert_eq!(
            Slot::from_cmdline("init=/x emco.slot=b quiet"),
            Some(Slot::B)
        );
        assert_eq!(Slot::from_cmdline("emco.slot=a emco.slot=b"), Some(Slot::B));
        assert_eq!(Slot::from_cmdline("emco.slot=c"), None);
        assert_eq!(Slot::from_cmdline("root=/dev/sda2"), None);
        assert_eq!(Slot::A.other(), Slot::B);
    }
}
