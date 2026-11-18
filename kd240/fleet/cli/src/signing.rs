//! Where the CLI's signatures come from: middleware's `mwctl` signers.
//!
//! * `--pkcs11 LABEL`: a key in a PKCS#11 token (the owner's smart card, a
//!   TPM, SoftHSM2 in tests), through `mwctl::pkcs11::Pkcs11Signer`. The
//!   same key signs payloads for middleware nodes (`mwctl --key
//!   pkcs11:LABEL`) and orders for the KD240s. Module, token and PIN come
//!   from `PKCS11_MODULE`, `PKCS11_TOKEN` and `PKCS11_PIN` (environment or
//!   `~/.env`), as for `mwctl`.
//! * `--key FILE`: a software ed25519 key from `emco-fleet keygen`, for
//!   tests and for a fleet without a card.
//!
//! Either way the board checks the signature with middleware's verifier
//! (`emco_fleet_proto::keys`), under the key's *name*: the name on the
//! board's trusted key must match `--key-name`.

use std::path::{Path, PathBuf};

use emco_fleet_proto::keys::{PublicKey, SecretKey, Signer};
use mwctl::sign::Signer as MwSigner;

/// An `mwctl` signer under a fleet key name.
pub struct Named {
    name: String,
    inner: Box<dyn MwSigner>,
}

impl Named {
    pub fn new(name: &str, inner: Box<dyn MwSigner>) -> Result<Self, String> {
        if !emco_fleet_proto::valid_name(name) {
            return Err(format!("bad key name {name:?}"));
        }
        Ok(Named {
            name: name.into(),
            inner,
        })
    }

    /// The text for the board's `trustedKeys`.
    pub fn public(&self) -> Result<PublicKey, String> {
        PublicKey::from_mw(&self.name, self.inner.public_key())
    }
}

impl Signer for Named {
    fn key_name(&self) -> &str {
        &self.name
    }
    fn sign_message(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        self.inner
            .sign(msg)
            .map(|s| s.0.to_vec())
            .map_err(|e| e.to_string())
    }
}

/// How the user named the key.
pub enum KeySource {
    File(PathBuf),
    Pkcs11 { label: String, name: String },
}

impl KeySource {
    pub fn from_args(
        key: Option<&Path>,
        pkcs11: Option<&str>,
        key_name: Option<&str>,
    ) -> Result<Self, String> {
        match (key, pkcs11) {
            (Some(_), Some(_)) => Err("give --key or --pkcs11, not both".into()),
            (None, None) => Err("a signing key is needed: --pkcs11 LABEL or --key FILE".into()),
            (Some(f), None) => Ok(KeySource::File(f.to_path_buf())),
            (None, Some(label)) => Ok(KeySource::Pkcs11 {
                label: label.into(),
                name: key_name.unwrap_or(label).into(),
            }),
        }
    }

    pub fn open(&self) -> Result<Box<dyn SignerWithPublic>, String> {
        match self {
            KeySource::File(p) => {
                let text =
                    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let k =
                    SecretKey::parse(text.trim()).map_err(|e| format!("{}: {e}", p.display()))?;
                Ok(Box::new(k))
            }
            KeySource::Pkcs11 { label, name } => {
                use mwctl::pkcs11::{pin, Location, Pkcs11Signer, ENV_PIN};
                let at = Location::from_settings();
                let pin = pin(ENV_PIN).map_err(|e| e.to_string())?;
                let s = Pkcs11Signer::open(&at, label, &pin).map_err(|e| e.to_string())?;
                eprintln!("emco-fleet: signing with {s:?} via {}", at.module.display());
                Ok(Box::new(Named::new(name, Box::new(s))?))
            }
        }
    }
}

/// A signer that can say its public key.
pub trait SignerWithPublic: Signer {
    fn public_key(&self) -> Result<PublicKey, String>;
}

impl SignerWithPublic for SecretKey {
    fn public_key(&self) -> Result<PublicKey, String> {
        Ok(self.public())
    }
}

impl SignerWithPublic for Named {
    fn public_key(&self) -> Result<PublicKey, String> {
        self.public()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emco_fleet_proto::keys::sign_order;
    use emco_fleet_proto::msg::{Action, Command, Order};

    fn order() -> Order {
        Order::Command(Command {
            device: "kd240".into(),
            seq: 1,
            not_after: 2,
            action: Action::Reboot { bundle: None },
        })
    }

    #[test]
    fn an_mwctl_seed_signs_orders_the_board_accepts() {
        let seed = mwctl::sign::Seed::new([7; 32]);
        let s = Named::new("fleet-sw", Box::new(seed)).unwrap();
        let pk = s.public().unwrap();
        assert_eq!(pk.algorithm(), "ed25519");
        let env = sign_order(&s, &order()).unwrap();
        assert_eq!(env.verify(&[pk]).unwrap(), order());
        assert!(Named::new("a/b", Box::new(mwctl::sign::Seed::new([1; 32]))).is_err());
    }

    #[test]
    fn key_sources() {
        assert!(KeySource::from_args(None, None, None).is_err());
        assert!(KeySource::from_args(Some(Path::new("k")), Some("l"), None).is_err());
        match KeySource::from_args(None, Some("mw-release"), None).unwrap() {
            KeySource::Pkcs11 { label, name } => assert_eq!((label, name).0, "mw-release"),
            _ => panic!(),
        }
        match KeySource::from_args(None, Some("mw-release"), Some("emco-card")).unwrap() {
            KeySource::Pkcs11 { name, .. } => assert_eq!(name, "emco-card"),
            _ => panic!(),
        }
    }

    fn softhsm_module() -> Option<PathBuf> {
        [
            "/usr/lib/softhsm/libsofthsm2.so",
            "/usr/lib/x86_64-linux-gnu/softhsm/libsofthsm2.so",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
    }

    /// Runs [`softhsm_card_signs_orders`] in a child test process with
    /// `SOFTHSM2_CONF` pointing at a fresh token directory (the crate
    /// forbids `unsafe`, so it cannot set the variable itself). Skipped,
    /// with a note, without SoftHSM2.
    #[test]
    fn softhsm_end_to_end() {
        let Some(_) = softhsm_module() else {
            eprintln!("SKIPPED: SoftHSM2 is not installed (apt install softhsm2)");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tokens")).unwrap();
        let conf = dir.path().join("softhsm2.conf");
        std::fs::write(
            &conf,
            format!(
                "directories.tokendir = {}\nobjectstore.backend = file\nlog.level = ERROR\n",
                dir.path().join("tokens").display()
            ),
        )
        .unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "signing::tests::softhsm_card_signs_orders",
                "--ignored",
                "--nocapture",
            ])
            .env("SOFTHSM2_CONF", &conf)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{text}");
        assert!(text.contains("1 passed"), "the child ran the test: {text}");
    }

    /// A P-256 key made in a token signs an order through mwctl, and the
    /// board's check (middleware's verifier) accepts it under its name and
    /// refuses a changed payload.
    #[test]
    #[ignore = "run by softhsm_end_to_end with SOFTHSM2_CONF set"]
    fn softhsm_card_signs_orders() {
        use middleware::loader::trust::KeyKind;
        use mwctl::pkcs11::{generate, init_token, Location, Pkcs11Signer};
        assert!(std::env::var_os("SOFTHSM2_CONF").is_some());
        let module = softhsm_module().unwrap();
        let so = cryptoki::types::AuthPin::new("so-test-0000".into());
        let user = cryptoki::types::AuthPin::new("user-test-1234".into());
        init_token(&module, "fleet-test", &so, &user).unwrap();
        let at = Location {
            module,
            token: Some("fleet-test".into()),
        };
        let mw_public = generate(&at, "emco-fleet", KeyKind::P256, &user).unwrap();
        let signer = Pkcs11Signer::open(&at, "emco-fleet", &user).unwrap();
        let s = Named::new("emco-fleet-card", Box::new(signer)).unwrap();
        let pk = s.public().unwrap();
        assert_eq!(pk.mw(), &mw_public);
        assert_eq!(pk.algorithm(), "ecdsa-p256-sha256");
        let text = pk.to_text();
        assert!(text.starts_with("emco-fleet-card:p256:MFkw"), "{text}");
        let board_key = PublicKey::parse(&text).unwrap();

        let env = sign_order(&s, &order()).unwrap();
        assert_eq!(env.verify(std::slice::from_ref(&board_key)).unwrap(), order());
        let mut bad = env.clone();
        bad.payload = bad.payload.replace("\"seq\":1", "\"seq\":9");
        assert!(bad.verify(&[board_key]).is_err());
    }
}
