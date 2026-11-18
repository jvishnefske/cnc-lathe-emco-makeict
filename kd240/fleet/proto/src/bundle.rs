//! A release bundle: what one A/B slot holds, as `bundle.json` describes it.
//!
//! `nix build .#kd240-bundle` writes the files and `bundle.json`; the CLI
//! puts the bundle into an order, signs the order with the card, uploads
//! the files to the server (stored by SHA-256) and publishes the order. The
//! board checks every file's size and SHA-256 as it writes the inactive
//! slot, and again by reading the slot back.
//!
//! | file | goes to | |
//! |---|---|---|
//! | `rootfs.squashfs` | the slot's raw partition (p2 / p3) | the NixOS closure, mounted at `/nix/store` |
//! | `Image` | `KD240BOOT:/<slot>/Image` | the kernel |
//! | `initrd` | `KD240BOOT:/<slot>/initrd` | |
//! | `dtb` | `KD240BOOT:/<slot>/dtb` | the K24 device tree |
//! | `cmdline` | `KD240BOOT:/<slot>/cmdline` | `bootargs=…`, imported by boot.scr |

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const FORMAT: &str = "emco-bundle-1";

/// The files of a bundle, in the order the board writes them: the big
/// root file system first, the boot files after it.
pub const FILES: [&str; 5] = ["rootfs.squashfs", "Image", "initrd", "dtb", "cmdline"];

/// The most the `cmdline` file may hold (U-Boot's bootargs, with room).
pub const MAX_CMDLINE: u64 = 4096;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    /// [`FORMAT`].
    pub format: String,
    /// The device class it is built for (`kd240`).
    pub class: String,
    /// The system it boots: its toplevel store path. The board reports the
    /// same string as its running system, so the two can be compared.
    pub version: String,
    /// Unix seconds when built (0 in a reproducible build).
    #[serde(default)]
    pub created: u64,
    pub files: Vec<BundleFile>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    pub name: String,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Bundle {
    /// Well formed: the right format, exactly the [`FILES`] (each once, in
    /// that order), sane sizes and hashes, and a printable version.
    pub fn check(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!("bundle format {:?}, want {FORMAT:?}", self.format));
        }
        if !crate::valid_name(&self.class) {
            return Err(format!("bad device class {:?}", self.class));
        }
        if self.version.is_empty()
            || self.version.len() > 256
            || !self.version.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err("bad bundle version".into());
        }
        let names: Vec<&str> = self.files.iter().map(|f| f.name.as_str()).collect();
        if names != FILES {
            return Err(format!("bundle files are {names:?}, want {FILES:?}"));
        }
        for f in &self.files {
            if !is_sha256_hex(&f.sha256) {
                return Err(format!("{}: bad sha256", f.name));
            }
            if f.size == 0 {
                return Err(format!("{}: empty", f.name));
            }
        }
        let cmdline = self.file("cmdline").expect("checked above");
        if cmdline.size > MAX_CMDLINE {
            return Err(format!(
                "cmdline is {} bytes, at most {MAX_CMDLINE}",
                cmdline.size
            ));
        }
        Ok(())
    }

    pub fn file(&self, name: &str) -> Option<&BundleFile> {
        self.files.iter().find(|f| f.name == name)
    }

    /// The bundle's identity: the SHA-256 of its JSON. Two bundles with the
    /// same files and version have the same digest.
    pub fn digest(&self) -> String {
        let j = serde_json::to_vec(self).expect("a Bundle always serializes");
        crate::keys::hex(&Sha256::digest(j))
    }

    /// Bytes the board writes for it.
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// The `cmdline` file's form: one `bootargs=…` line, for U-Boot's
/// `env import -t`. The arguments must not contain a newline or NUL.
pub fn cmdline_file(args: &str) -> Result<String, String> {
    if args.contains(['\n', '\0']) {
        return Err("kernel arguments contain a newline or NUL".into());
    }
    Ok(format!("bootargs={args}\n"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn bundle(version: &str) -> Bundle {
        Bundle {
            format: FORMAT.into(),
            class: "kd240".into(),
            version: version.into(),
            created: 1,
            files: FILES
                .iter()
                .map(|n| BundleFile {
                    name: n.to_string(),
                    size: 10,
                    sha256: "a".repeat(64),
                })
                .collect(),
        }
    }

    #[test]
    fn a_good_bundle() {
        assert_eq!(bundle("v1").check(), Ok(()));
        assert_eq!(bundle("v1").total_size(), 50);
        assert_eq!(bundle("v1").digest(), bundle("v1").digest());
        assert_ne!(bundle("v1").digest(), bundle("v2").digest());
    }

    #[test]
    fn bad_bundles() {
        let mut b = bundle("v1");
        b.format = "emco-bundle-2".into();
        assert!(b.check().is_err());

        let mut b = bundle("v1");
        b.files.swap(0, 1);
        assert!(b.check().is_err(), "order is fixed");

        let mut b = bundle("v1");
        b.files.pop();
        assert!(b.check().is_err(), "a file missing");

        let mut b = bundle("v1");
        b.files.push(b.files[0].clone());
        assert!(b.check().is_err(), "a file twice");

        let mut b = bundle("v1");
        b.files[4].name = "../../etc/shadow".into();
        assert!(b.check().is_err());

        let mut b = bundle("v1");
        b.files[1].sha256 = "A".repeat(64);
        assert!(b.check().is_err(), "uppercase hex");

        let mut b = bundle("v1");
        b.files[2].size = 0;
        assert!(b.check().is_err());

        let mut b = bundle("v1");
        b.files[4].size = MAX_CMDLINE + 1;
        assert!(b.check().is_err());

        assert!(bundle("has space").check().is_err());
        assert!(bundle("").check().is_err());
    }

    #[test]
    fn unknown_fields_are_refused() {
        let mut j = serde_json::to_value(bundle("v1")).unwrap();
        j["skip_verify"] = true.into();
        assert!(serde_json::from_value::<Bundle>(j).is_err());
    }

    #[test]
    fn cmdline_form() {
        assert_eq!(
            cmdline_file("init=/nix/store/x/init console=ttyPS1,115200").unwrap(),
            "bootargs=init=/nix/store/x/init console=ttyPS1,115200\n"
        );
        assert!(cmdline_file("a\nbootcmd=reset").is_err());
    }
}
