//! ed25519 keys and signed envelopes.
//!
//! Key text, in the style of Nix's binary-cache keys:
//!
//! * secret: `<name>:<base64 of the 32-byte seed>`, kept on the build host
//!   only (never on the server, never in the repository or the Nix store)
//! * public: `<name>:<base64 of the 32-byte public key>`, configured on the
//!   board (and optionally on the server, which then refuses junk early)
//!
//! An [`Envelope`] carries the signed JSON as a string, so the bytes that
//! were signed are the bytes that are verified: no canonical JSON is needed.
//! The signature covers [`SIGNING_CONTEXT`] followed by the payload, so a
//! signature made with this key for anything else cannot be passed off as
//! an order.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::msg::Order;

/// Prefixed to every signed payload (domain separation).
pub const SIGNING_CONTEXT: &[u8] = b"emco-fleet order v1\n";

/// A signed order as stored by the server and sent to the board.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    /// The name of the key that signed it.
    pub key: String,
    /// The order, as JSON text: exactly the signed bytes.
    pub payload: String,
    /// base64 of the 64-byte ed25519 signature.
    pub sig: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// No trusted key has this name.
    UnknownKey(String),
    /// The signature does not decode, or does not verify.
    BadSignature,
    /// It verified, but the payload is not an [`Order`] this version knows.
    BadPayload(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::UnknownKey(k) => write!(f, "signed by an untrusted key {k:?}"),
            VerifyError::BadSignature => f.write_str("bad signature"),
            VerifyError::BadPayload(e) => write!(f, "signed payload is not a valid order: {e}"),
        }
    }
}

impl Envelope {
    /// The order inside, WITHOUT checking the signature. For the server's
    /// bookkeeping (which device, which sequence number) only. Nothing on
    /// the board may act on this.
    pub fn unverified_order(&self) -> Result<Order, String> {
        serde_json::from_str(&self.payload).map_err(|e| e.to_string())
    }

    /// Verify against the trusted keys, then parse.
    pub fn verify(&self, trusted: &[PublicKey]) -> Result<Order, VerifyError> {
        let key = trusted
            .iter()
            .find(|k| k.name == self.key)
            .ok_or_else(|| VerifyError::UnknownKey(self.key.clone()))?;
        let sig = B64
            .decode(self.sig.as_bytes())
            .map_err(|_| VerifyError::BadSignature)?;
        let sig = Signature::from_slice(&sig).map_err(|_| VerifyError::BadSignature)?;
        key.key
            .verify_strict(&signed_bytes(&self.payload), &sig)
            .map_err(|_| VerifyError::BadSignature)?;
        serde_json::from_str(&self.payload).map_err(|e| VerifyError::BadPayload(e.to_string()))
    }
}

fn signed_bytes(payload: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIGNING_CONTEXT.len() + payload.len());
    m.extend_from_slice(SIGNING_CONTEXT);
    m.extend_from_slice(payload.as_bytes());
    m
}

fn split_key(text: &str) -> Result<(String, [u8; 32]), String> {
    let text = text.trim();
    let (name, b64) = text.split_once(':').ok_or("a key is <name>:<base64>")?;
    if !crate::valid_name(name) {
        return Err(format!("bad key name {name:?}"));
    }
    let bytes = B64.decode(b64).map_err(|_| "key is not base64")?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "key is not 32 bytes")?;
    Ok((name.to_string(), arr))
}

/// The build host's signing key.
pub struct SecretKey {
    name: String,
    key: SigningKey,
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretKey({:?}, ..)", self.name)
    }
}

impl SecretKey {
    pub fn generate(name: &str) -> Result<Self, String> {
        if !crate::valid_name(name) {
            return Err(format!("bad key name {name:?}"));
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| format!("no randomness: {e}"))?;
        Ok(SecretKey {
            name: name.into(),
            key: SigningKey::from_bytes(&seed),
        })
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let (name, seed) = split_key(text)?;
        Ok(SecretKey {
            name,
            key: SigningKey::from_bytes(&seed),
        })
    }

    pub fn to_text(&self) -> String {
        format!("{}:{}", self.name, B64.encode(self.key.to_bytes()))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn public(&self) -> PublicKey {
        PublicKey {
            name: self.name.clone(),
            key: self.key.verifying_key(),
        }
    }

    /// Sign an order.
    pub fn sign(&self, order: &Order) -> Envelope {
        let payload = serde_json::to_string(order).expect("an Order always serializes");
        let sig = self.key.sign(&signed_bytes(&payload));
        Envelope {
            key: self.name.clone(),
            payload,
            sig: B64.encode(sig.to_bytes()),
        }
    }
}

/// A key the board (and optionally the server) trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    name: String,
    key: VerifyingKey,
}

impl PublicKey {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (name, bytes) = split_key(text)?;
        let key = VerifyingKey::from_bytes(&bytes).map_err(|_| "not an ed25519 public key")?;
        Ok(PublicKey { name, key })
    }

    pub fn to_text(&self) -> String {
        format!("{}:{}", self.name, B64.encode(self.key.to_bytes()))
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{Action, Command, Order};

    fn order() -> Order {
        Order::Command(Command {
            device: "kd240".into(),
            seq: 7,
            not_after: 2_000_000_000,
            action: Action::Reboot { toplevel: None },
        })
    }

    #[test]
    fn sign_and_verify() {
        let sk = SecretKey::generate("fleet-1").unwrap();
        let pk = PublicKey::parse(&sk.public().to_text()).unwrap();
        let env = sk.sign(&order());
        assert_eq!(env.verify(&[pk]).unwrap(), order());
    }

    #[test]
    fn secret_key_round_trips() {
        let sk = SecretKey::generate("fleet-1").unwrap();
        let again = SecretKey::parse(&sk.to_text()).unwrap();
        assert_eq!(sk.public(), again.public());
        assert!(!format!("{sk:?}").contains(&sk.to_text()[8..]));
    }

    #[test]
    fn tampered_payload_fails() {
        let sk = SecretKey::generate("fleet-1").unwrap();
        let mut env = sk.sign(&order());
        env.payload = env.payload.replace("\"seq\":7", "\"seq\":8");
        assert_eq!(env.verify(&[sk.public()]), Err(VerifyError::BadSignature));
    }

    #[test]
    fn other_key_fails() {
        let sk = SecretKey::generate("fleet-1").unwrap();
        let mallory = SecretKey::generate("fleet-1").unwrap();
        let env = mallory.sign(&order());
        assert_eq!(env.verify(&[sk.public()]), Err(VerifyError::BadSignature));
        let renamed = SecretKey::generate("other").unwrap().sign(&order());
        assert!(matches!(
            renamed.verify(&[sk.public()]),
            Err(VerifyError::UnknownKey(_))
        ));
    }

    #[test]
    fn signature_without_context_fails() {
        // A signature over the bare payload (no domain prefix) is not valid.
        let sk = SecretKey::generate("fleet-1").unwrap();
        let mut env = sk.sign(&order());
        let raw = SigningKey::from_bytes(
            &B64.decode(sk.to_text().split_once(':').unwrap().1)
                .unwrap()
                .try_into()
                .unwrap(),
        )
        .sign(env.payload.as_bytes());
        env.sig = B64.encode(raw.to_bytes());
        assert_eq!(env.verify(&[sk.public()]), Err(VerifyError::BadSignature));
    }

    #[test]
    fn garbage_signature_fails() {
        let sk = SecretKey::generate("fleet-1").unwrap();
        let mut env = sk.sign(&order());
        env.sig = "not base64!".into();
        assert_eq!(env.verify(&[sk.public()]), Err(VerifyError::BadSignature));
        env.sig = B64.encode([0u8; 10]);
        assert_eq!(env.verify(&[sk.public()]), Err(VerifyError::BadSignature));
    }

    #[test]
    fn bad_key_text() {
        assert!(PublicKey::parse("nocolon").is_err());
        assert!(PublicKey::parse("a/b:AAAA").is_err());
        assert!(PublicKey::parse("a:AAAA").is_err());
    }
}
