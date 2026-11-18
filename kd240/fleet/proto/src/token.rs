//! Bearer tokens. The server holds each token as `sha256:<hex>` (or, for
//! convenience, the raw token, which it hashes on load), and compares the
//! SHA-256 of what a client presents against it with a constant-time
//! equality. Hashing first makes the comparison independent of the
//! presented token's length, too.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// A token as the server holds it: its SHA-256.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenHash([u8; 32]);

impl std::fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenHash(..)")
    }
}

impl TokenHash {
    /// The hash of a raw token.
    pub fn of(token: &str) -> Self {
        TokenHash(Sha256::digest(token.as_bytes()).into())
    }

    /// `sha256:<64 hex>` is a hash; anything else is a raw token. Leading and
    /// trailing white space is dropped. An empty token is refused.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty token".into());
        }
        match spec.strip_prefix("sha256:") {
            Some(hex) => {
                let bytes = decode_hex(hex).ok_or("sha256: wants 64 hex digits")?;
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| "sha256: wants 64 hex digits")?;
                Ok(TokenHash(arr))
            }
            None => Ok(TokenHash::of(spec)),
        }
    }

    /// Does `presented` hash to this? Constant time in the token's content.
    pub fn matches(&self, presented: &str) -> bool {
        let h: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        bool::from(self.0.ct_eq(&h))
    }

    /// `sha256:<hex>`, safe to keep in a config file the server reads.
    pub fn to_text(&self) -> String {
        let mut s = String::with_capacity(71);
        s.push_str("sha256:");
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

/// A new random token: 32 bytes, URL-safe base64 without padding.
pub fn generate() -> Result<String, String> {
    use base64::Engine;
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| format!("no randomness: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_and_hashed_forms_agree() {
        let raw = TokenHash::parse("  s3cret\n").unwrap();
        let hashed = TokenHash::parse(&TokenHash::of("s3cret").to_text()).unwrap();
        assert_eq!(raw, hashed);
        assert!(raw.matches("s3cret"));
        assert!(!raw.matches("s3cre"));
        assert!(!raw.matches("s3cret "));
        assert!(!raw.matches(""));
    }

    #[test]
    fn bad_specs() {
        assert!(TokenHash::parse("").is_err());
        assert!(TokenHash::parse("   ").is_err());
        assert!(TokenHash::parse("sha256:abc").is_err());
        assert!(TokenHash::parse(&format!("sha256:{}", "zz".repeat(32))).is_err());
    }

    #[test]
    fn generated_tokens_differ() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
    }

    #[test]
    fn debug_does_not_print_the_hash() {
        assert_eq!(format!("{:?}", TokenHash::of("x")), "TokenHash(..)");
    }
}
