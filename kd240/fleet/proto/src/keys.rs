//! Keys and signed envelopes: ECDSA P-256 (the owner's PKCS#11 smart card)
//! and ed25519 (software keys, kept for tests and tools).
//!
//! The key material and the signature check are middleware's
//! (`middleware::loader::trust::{PublicKey, KeyKind, Signature}` and
//! `verify_signature`), the same code that checks a signed WASM payload on
//! a node, so one hardware key and one verifier serve both. What stays here
//! is the fleet's own domain ([`SIGNING_CONTEXT`]), the [`Envelope`], and
//! the key text the board's configuration holds.
//!
//! Public key text, in the style of Nix's binary-cache keys:
//!
//! * ed25519: `<name>:<base64 of the 32-byte public key>`
//! * P-256:   `<name>:p256:<base64 of the DER SubjectPublicKeyInfo>`
//!
//! The SPKI's SHA-256 ([`PublicKey::fingerprint`]) is the key's id: the
//! same digest `openssl x509 -pubkey -noout | openssl pkey -pubin -outform
//! der | sha256sum` gives for the card's certificate, so a key on the board
//! can be checked against the card by hand.
//!
//! An ed25519 secret is `<name>:<base64 of the 32-byte seed>`, kept on the
//! build host only. The P-256 secret never leaves the card: the CLI signs
//! through PKCS#11, behind the [`Signer`] trait.
//!
//! An [`Envelope`] carries the signed JSON as a string, so the bytes that
//! were signed are the bytes that are verified: no canonical JSON is needed.
//! The signature covers [`SIGNING_CONTEXT`] followed by the payload, so a
//! signature made with this key for anything else cannot be passed off as
//! an order.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signer as _, SigningKey};
use middleware::loader::trust::{self, KeyKind};
use p256::pkcs8::{DecodePublicKey, EncodePublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// base64 of the signature: 64 bytes, ed25519, or P-256 `r ‖ s`.
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
        if !key.verify(&signing_input(&self.payload), &sig) {
            return Err(VerifyError::BadSignature);
        }
        serde_json::from_str(&self.payload).map_err(|e| VerifyError::BadPayload(e.to_string()))
    }
}

/// The bytes a signature covers: [`SIGNING_CONTEXT`] ‖ payload.
pub fn signing_input(payload: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIGNING_CONTEXT.len() + payload.len());
    m.extend_from_slice(SIGNING_CONTEXT);
    m.extend_from_slice(payload.as_bytes());
    m
}

/// Something that signs orders: a software key here, or the smart card in
/// the CLI.
pub trait Signer {
    /// The key name the board's trusted key has.
    fn key_name(&self) -> &str;
    /// Sign the message (the whole [`signing_input`], not a hash of it):
    /// 64 bytes, ed25519 or P-256 ECDSA-SHA256 `r ‖ s`.
    fn sign_message(&self, msg: &[u8]) -> Result<Vec<u8>, String>;
}

/// Sign an order with any [`Signer`].
pub fn sign_order(signer: &dyn Signer, order: &Order) -> Result<Envelope, String> {
    let payload = serde_json::to_string(order).map_err(|e| e.to_string())?;
    let sig = signer.sign_message(&signing_input(&payload))?;
    Ok(Envelope {
        key: signer.key_name().to_string(),
        payload,
        sig: B64.encode(sig),
    })
}

fn check_name(name: &str) -> Result<(), String> {
    if crate::valid_name(name) {
        Ok(())
    } else {
        Err(format!("bad key name {name:?}"))
    }
}

fn split_key(text: &str) -> Result<(String, [u8; 32]), String> {
    let text = text.trim();
    let (name, b64) = text.split_once(':').ok_or("a key is <name>:<base64>")?;
    check_name(name)?;
    let bytes = B64.decode(b64).map_err(|_| "key is not base64")?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| "key is not 32 bytes")?;
    Ok((name.to_string(), arr))
}

/// An ed25519 signing key in a file (tests, tools; the fleet's own key is
/// the card's P-256 key).
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
        check_name(name)?;
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
            key: trust::PublicKey::Ed25519(self.key.verifying_key().to_bytes()),
        }
    }

    /// Sign an order.
    pub fn sign(&self, order: &Order) -> Envelope {
        sign_order(self, order).expect("ed25519 signing cannot fail")
    }
}

impl Signer for SecretKey {
    fn key_name(&self) -> &str {
        &self.name
    }
    fn sign_message(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.key.sign(msg).to_bytes().to_vec())
    }
}

/// A software P-256 key, for tests: the same signatures the card makes,
/// without the card. The fleet's real P-256 key never leaves the card.
pub struct P256SecretKey {
    name: String,
    key: p256::ecdsa::SigningKey,
}

impl P256SecretKey {
    pub fn generate(name: &str) -> Result<Self, String> {
        check_name(name)?;
        let mut seed = [0u8; 32];
        loop {
            getrandom::fill(&mut seed).map_err(|e| format!("no randomness: {e}"))?;
            if let Ok(key) = p256::ecdsa::SigningKey::from_slice(&seed) {
                return Ok(P256SecretKey {
                    name: name.into(),
                    key,
                });
            }
        }
    }

    pub fn public(&self) -> PublicKey {
        PublicKey::from_p256(&self.name, self.key.verifying_key())
    }

    pub fn sign(&self, order: &Order) -> Envelope {
        sign_order(self, order).expect("P-256 signing cannot fail")
    }
}

impl Signer for P256SecretKey {
    fn key_name(&self) -> &str {
        &self.name
    }
    fn sign_message(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        use p256::ecdsa::signature::Signer as _;
        let sig: p256::ecdsa::Signature = self.key.sign(msg);
        Ok(sig.to_bytes().to_vec())
    }
}

/// A key the board (and optionally the server) trusts: a name, and
/// middleware's key, always a valid point of its curve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    name: String,
    key: trust::PublicKey,
}

/// The DER SPKI prefix of an ed25519 key (RFC 8410), before the 32 key bytes.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

impl PublicKey {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (name, rest) = text.split_once(':').ok_or("a key is <name>:<base64>")?;
        if let Some(b64) = rest.strip_prefix("p256:") {
            let der = B64.decode(b64).map_err(|_| "key is not base64")?;
            return Self::p256_from_spki(name, &der);
        }
        let (name, bytes) = split_key(text)?;
        Self::from_mw(&name, trust::PublicKey::Ed25519(bytes))
            .map_err(|_| "not an ed25519 public key".into())
    }

    /// A middleware key under a name, if it is a point of its curve.
    pub fn from_mw(name: &str, key: trust::PublicKey) -> Result<Self, String> {
        check_name(name)?;
        if !trust::is_valid_key(&key) {
            return Err(format!("not a valid {} public key", key.kind().name()));
        }
        Ok(PublicKey {
            name: name.into(),
            key,
        })
    }

    fn from_p256(name: &str, vk: &p256::ecdsa::VerifyingKey) -> Self {
        let point = vk.to_encoded_point(true);
        let bytes: [u8; trust::P256_KEY_LEN] = point
            .as_bytes()
            .try_into()
            .expect("a compressed P-256 point is 33 bytes");
        PublicKey {
            name: name.into(),
            key: trust::PublicKey::P256(bytes),
        }
    }

    /// A P-256 key from its DER SubjectPublicKeyInfo.
    pub fn p256_from_spki(name: &str, der: &[u8]) -> Result<Self, String> {
        check_name(name)?;
        let key = p256::ecdsa::VerifyingKey::from_public_key_der(der)
            .map_err(|e| format!("not a P-256 public key: {e}"))?;
        Ok(Self::from_p256(name, &key))
    }

    /// A P-256 key from its SEC1 point (PKCS#11 `CKA_EC_POINT`, unwrapped),
    /// compressed or not.
    pub fn p256_from_sec1(name: &str, point: &[u8]) -> Result<Self, String> {
        check_name(name)?;
        let key = p256::ecdsa::VerifyingKey::from_sec1_bytes(point)
            .map_err(|e| format!("not a P-256 point: {e}"))?;
        Ok(Self::from_p256(name, &key))
    }

    /// Middleware's key: what `mwctl` and a node's `sys/trust/claim` use.
    pub fn mw(&self) -> &trust::PublicKey {
        &self.key
    }

    pub fn to_text(&self) -> String {
        match &self.key {
            trust::PublicKey::Ed25519(k) => format!("{}:{}", self.name, B64.encode(k)),
            trust::PublicKey::P256(_) => {
                format!("{}:p256:{}", self.name, B64.encode(self.spki_der()))
            }
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> KeyKind {
        self.key.kind()
    }

    pub fn algorithm(&self) -> &'static str {
        match self.kind() {
            KeyKind::Ed25519 => "ed25519",
            KeyKind::P256 => "ecdsa-p256-sha256",
        }
    }

    /// The DER SubjectPublicKeyInfo (P-256 with the uncompressed point, as
    /// OpenSSL and the card's certificate write it).
    pub fn spki_der(&self) -> Vec<u8> {
        match &self.key {
            trust::PublicKey::Ed25519(k) => [&ED25519_SPKI_PREFIX[..], k].concat(),
            trust::PublicKey::P256(k) => p256::ecdsa::VerifyingKey::from_sec1_bytes(k)
                .expect("checked when made")
                .to_public_key_der()
                .expect("a P-256 key always encodes")
                .into_vec(),
        }
    }

    /// The key id: lowercase hex SHA-256 of the DER SPKI.
    pub fn fingerprint(&self) -> String {
        hex(&Sha256::digest(self.spki_der()))
    }

    /// Does `sig` sign `msg` (the whole signing input)? Middleware's
    /// `verify_signature`, with this key's algorithm.
    pub fn verify(&self, msg: &[u8], sig: &[u8]) -> bool {
        let Ok(sig) = <[u8; trust::SIGNATURE_LEN]>::try_from(sig) else {
            return false;
        };
        trust::verify_signature(&self.key, msg, &trust::Signature(sig))
    }
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
            action: Action::Reboot { bundle: None },
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
    fn p256_sign_and_verify() {
        let sk = P256SecretKey::generate("emco-fleet-sign").unwrap();
        let text = sk.public().to_text();
        assert!(text.starts_with("emco-fleet-sign:p256:MFkw"), "{text}");
        let pk = PublicKey::parse(&text).unwrap();
        assert_eq!(pk, sk.public());
        let env = sk.sign(&order());
        assert_eq!(env.verify(std::slice::from_ref(&pk)).unwrap(), order());

        let mut bad = env.clone();
        bad.payload = bad.payload.replace("\"seq\":7", "\"seq\":8");
        assert_eq!(
            bad.verify(std::slice::from_ref(&pk)),
            Err(VerifyError::BadSignature)
        );

        // Same name, other key.
        let mallory = P256SecretKey::generate("emco-fleet-sign").unwrap();
        assert_eq!(
            mallory.sign(&order()).verify(&[pk]),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn an_ed25519_signature_is_not_a_p256_one() {
        // Two keys with one name: the board's key decides the algorithm.
        let ed = SecretKey::generate("k").unwrap();
        let p = P256SecretKey::generate("k").unwrap();
        assert_eq!(
            ed.sign(&order()).verify(&[p.public()]),
            Err(VerifyError::BadSignature)
        );
        assert_eq!(
            p.sign(&order()).verify(&[ed.public()]),
            Err(VerifyError::BadSignature)
        );
    }

    /// Made with OpenSSL (`openssl genpkey -algorithm EC -pkeyopt
    /// ec_paramgen_curve:P-256`; `openssl dgst -sha256 -sign`, the DER
    /// signature converted to r ‖ s), so the verifier is checked against
    /// another implementation, not only against itself.
    #[test]
    fn p256_vector_from_openssl() {
        let pk = PublicKey::parse(P256_VECTOR_KEY).unwrap();
        let sig = B64.decode(P256_VECTOR_SIG).unwrap();
        assert!(pk.verify(VECTOR_MSG, &sig));
        assert!(!pk.verify(b"emco-fleet order v1\nhellp", &sig));
        assert_eq!(pk.fingerprint(), P256_VECTOR_FPR);
    }

    /// `openssl genpkey -algorithm ed25519`; `openssl pkeyutl -sign -rawin`.
    #[test]
    fn ed25519_vector_from_openssl() {
        let pk = PublicKey::parse(ED25519_VECTOR_KEY).unwrap();
        let sig = B64.decode(ED25519_VECTOR_SIG).unwrap();
        assert!(pk.verify(VECTOR_MSG, &sig));
        assert!(!pk.verify(b"emco-fleet order v1\nhellp", &sig));
        assert_eq!(pk.fingerprint(), ED25519_VECTOR_FPR);
    }

    /// The OpenSSL vectors, checked by middleware's verifier directly: the
    /// fleet's keys are middleware keys, nothing in between.
    #[test]
    fn vectors_verify_through_middleware() {
        for (key, sig, kind) in [
            (P256_VECTOR_KEY, P256_VECTOR_SIG, KeyKind::P256),
            (ED25519_VECTOR_KEY, ED25519_VECTOR_SIG, KeyKind::Ed25519),
        ] {
            let pk = PublicKey::parse(key).unwrap();
            assert_eq!(pk.kind(), kind);
            assert_eq!(pk.mw().as_bytes().len(), kind.key_len());
            let sig = trust::Signature(B64.decode(sig).unwrap().try_into().unwrap());
            assert!(trust::verify_signature(pk.mw(), VECTOR_MSG, &sig));
            assert!(!trust::verify_signature(pk.mw(), b"hellp", &sig));
            // The middleware key round-trips to the same text.
            let again = PublicKey::from_mw("vec", *pk.mw()).unwrap();
            assert_eq!(again.to_text(), key);
        }
    }

    #[test]
    fn invalid_points_are_refused() {
        assert!(PublicKey::from_mw("k", {
            let mut k = [0xff; 33];
            k[0] = 2; // x above the field prime: no such point
            trust::PublicKey::P256(k)
        })
        .is_err());
    }

    const VECTOR_MSG: &[u8] = b"emco-fleet order v1\nhello";
    include!("keys_vectors.rs");

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
        env.sig = B64.encode(sk.sign_message(env.payload.as_bytes()).unwrap());
        assert_eq!(env.verify(&[sk.public()]), Err(VerifyError::BadSignature));

        let pk = P256SecretKey::generate("fleet-1").unwrap();
        let mut env = pk.sign(&order());
        env.sig = B64.encode(pk.sign_message(env.payload.as_bytes()).unwrap());
        assert_eq!(env.verify(&[pk.public()]), Err(VerifyError::BadSignature));
    }

    #[test]
    fn garbage_signature_fails() {
        let sk = SecretKey::generate("fleet-1").unwrap();
        let p = P256SecretKey::generate("fleet-2").unwrap();
        let keys = [sk.public(), p.public()];
        for mut env in [sk.sign(&order()), p.sign(&order())] {
            env.sig = "not base64!".into();
            assert_eq!(env.verify(&keys), Err(VerifyError::BadSignature));
            env.sig = B64.encode([0u8; 10]);
            assert_eq!(env.verify(&keys), Err(VerifyError::BadSignature));
            env.sig = B64.encode([0u8; 64]);
            assert_eq!(env.verify(&keys), Err(VerifyError::BadSignature));
        }
    }

    #[test]
    fn bad_key_text() {
        assert!(PublicKey::parse("nocolon").is_err());
        assert!(PublicKey::parse("a/b:AAAA").is_err());
        assert!(PublicKey::parse("a:AAAA").is_err());
        assert!(PublicKey::parse("a:p256:AAAA").is_err());
        assert!(PublicKey::parse("a/b:p256:MFkw").is_err());
        // An ed25519 SPKI is not a P-256 one.
        let ed = SecretKey::generate("a").unwrap().public();
        assert!(PublicKey::p256_from_spki("a", &ed.spki_der()).is_err());
    }

    #[test]
    fn sec1_and_spki_agree() {
        let sk = P256SecretKey::generate("k").unwrap();
        let point = sk.key.verifying_key().to_encoded_point(false);
        let pk = PublicKey::p256_from_sec1("k", point.as_bytes()).unwrap();
        assert_eq!(pk, sk.public());
        assert_eq!(pk.algorithm(), "ecdsa-p256-sha256");
        assert_eq!(pk.fingerprint().len(), 64);
    }
}
