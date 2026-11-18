//! The KD240 fleet protocol, shared by the server, the board agent and the
//! build host's CLI.
//!
//! * [`keys`]: P-256 (the owner's smart card) and ed25519 keys, and the
//!   signed [`Envelope`](keys::Envelope). Every update manifest and every
//!   command is signed offline on the build host; the server only stores
//!   and relays envelopes.
//! * [`bundle`]: what one A/B slot holds (`bundle.json`).
//! * [`bootstate`]: the A/B boot state U-Boot and Linux share.
//! * [`msg`]: what is signed ([`Order`](msg::Order)) and what goes over the
//!   wire unsigned (polls, telemetry, results).
//! * [`policy`]: the board's decisions as pure functions: which orders it
//!   accepts, when it may switch or reboot (only with the lathe service
//!   idle or disconnected), which slot it may write, and how a trial boot
//!   is committed or rolled back.
//! * [`token`]: bearer tokens, compared in constant time.
#![forbid(unsafe_code)]

pub mod bootstate;
pub mod bundle;
pub mod keys;
pub mod msg;
pub mod policy;
pub mod token;

/// Unix seconds, from the system clock (0 if it is before 1970).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A device id or key name: `[A-Za-z0-9._-]`, 1 to 64 characters, not
/// starting with a dot. Used in URLs and file names, so kept narrow.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn names() {
        assert!(valid_name("kd240"));
        assert!(valid_name("kd240-1.shop_a"));
        assert!(!valid_name(""));
        assert!(!valid_name(".."));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a b"));
        assert!(!valid_name(&"x".repeat(65)));
    }
}
