//! The integer limits the physical payload enforces (no allocation), and
//! what it tells a process model about its machine ([`Limits::encode`],
//! published on `app/unit/<node>/info`).

/// Most belt positions a machine has.
pub const MAX_BELTS: usize = 4;

/// The info record's i32 words: travel (4), rapid, maximum feed, spindle,
/// threading, counts per mm, the reference point (2).
const WORDS: usize = 11;
const TURRET: usize = 1 + 4 * WORDS;
const BELTS: usize = TURRET + 2;

/// `PhysicalConfig` as the physical payload holds it: integers only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub x_min_um: i32,
    pub x_max_um: i32,
    pub z_min_um: i32,
    pub z_max_um: i32,
    pub rapid_um_s: u32,
    pub max_feed_um_s: u32,
    pub spindle_max_rpm: i32,
    pub turret_stations: u8,
    /// Machine coordinates at the reference switches.
    pub ref_x_um: i32,
    pub ref_z_um: i32,
    pub search_um_s: u32,
    pub ref_x_first: bool,
    pub has_limits: bool,
    /// Step counts per mm (1 / the resolution).
    pub counts_per_mm: u32,
    /// Threading is synchronised only up to this speed, rpm.
    pub threading_max_rpm: i32,
    /// The belt positions' `(min, max)` rpm, the first [`Limits::belts`].
    pub belt_rpm: [(i32, i32); MAX_BELTS],
    /// How many belt positions.
    pub belts: u8,
}

impl Limits {
    /// Encoded length of the info record.
    pub const INFO_LEN: usize = 1 + 4 * WORDS + 1 + 1 + MAX_BELTS * 8;
    /// Info record version.
    pub const INFO_VERSION: u8 = 1;

    /// What a process model needs to know about the machine: travel,
    /// speeds, spindle, belts, turret. Fixed length, little-endian.
    pub fn encode(&self) -> [u8; Self::INFO_LEN] {
        let mut b = [0u8; Self::INFO_LEN];
        b[0] = Self::INFO_VERSION;
        let words = [
            self.x_min_um,
            self.x_max_um,
            self.z_min_um,
            self.z_max_um,
            self.rapid_um_s as i32,
            self.max_feed_um_s as i32,
            self.spindle_max_rpm,
            self.threading_max_rpm,
            self.counts_per_mm as i32,
            self.ref_x_um,
            self.ref_z_um,
        ];
        for (i, w) in words.iter().enumerate() {
            b[1 + 4 * i..5 + 4 * i].copy_from_slice(&w.to_le_bytes());
        }
        b[TURRET] = self.turret_stations;
        b[TURRET + 1] = self.belts;
        for (i, (lo, hi)) in self.belt_rpm.iter().enumerate() {
            b[BELTS + 8 * i..BELTS + 4 + 8 * i].copy_from_slice(&lo.to_le_bytes());
            b[BELTS + 4 + 8 * i..BELTS + 8 + 8 * i].copy_from_slice(&hi.to_le_bytes());
        }
        b
    }

    /// [`Limits::encode`]'s inverse: the fields a process model uses (the
    /// reference search speed and order are the physical model's own and
    /// come back zero). `None` for another length or version.
    pub fn decode_info(b: &[u8]) -> Option<Limits> {
        let b: &[u8; Self::INFO_LEN] = b.try_into().ok()?;
        if b[0] != Self::INFO_VERSION || usize::from(b[TURRET + 1]) > MAX_BELTS {
            return None;
        }
        let w =
            |i: usize| i32::from_le_bytes([b[1 + 4 * i], b[2 + 4 * i], b[3 + 4 * i], b[4 + 4 * i]]);
        let at = |o: usize| i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let mut belt_rpm = [(0, 0); MAX_BELTS];
        for (i, slot) in belt_rpm.iter_mut().enumerate() {
            *slot = (at(BELTS + 8 * i), at(BELTS + 4 + 8 * i));
        }
        Some(Limits {
            x_min_um: w(0),
            x_max_um: w(1),
            z_min_um: w(2),
            z_max_um: w(3),
            rapid_um_s: w(4) as u32,
            max_feed_um_s: w(5) as u32,
            spindle_max_rpm: w(6),
            threading_max_rpm: w(7),
            counts_per_mm: w(8) as u32,
            turret_stations: b[TURRET],
            belts: b[TURRET + 1],
            belt_rpm,
            ref_x_um: w(9),
            ref_z_um: w(10),
            search_um_s: 0,
            ref_x_first: true,
            has_limits: false,
        })
    }

    /// Belt `n` (1-based): its `(min, max)` rpm.
    pub fn belt(&self, n: u8) -> Option<(i32, i32)> {
        (n >= 1 && n <= self.belts).then(|| self.belt_rpm[usize::from(n) - 1])
    }

    /// Whether `(x, z)` (machine µm) is inside the travel.
    pub const fn contains(&self, x_um: i32, z_um: i32) -> bool {
        self.x_min_um <= x_um
            && x_um <= self.x_max_um
            && self.z_min_um <= z_um
            && z_um <= self.z_max_um
    }
}
