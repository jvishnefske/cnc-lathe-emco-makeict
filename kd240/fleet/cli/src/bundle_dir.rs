//! A built bundle on the build host: the directory `nix build
//! .#kd240-bundle` makes, `bundle.json` and the files it lists.

use std::io::Read;
use std::path::{Path, PathBuf};

use emco_fleet_proto::bundle::Bundle;
use emco_fleet_proto::keys::hex;
use sha2::{Digest, Sha256};

pub struct BundleDir {
    pub dir: PathBuf,
    pub bundle: Bundle,
}

impl BundleDir {
    /// Read `bundle.json`, check it is well formed, and check every file
    /// against it (size and SHA-256), as the board will.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let json = dir.join("bundle.json");
        let text =
            std::fs::read_to_string(&json).map_err(|e| format!("{}: {e}", json.display()))?;
        let bundle: Bundle =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", json.display()))?;
        bundle
            .check()
            .map_err(|e| format!("{}: {e}", json.display()))?;
        let me = BundleDir {
            dir: dir.to_path_buf(),
            bundle,
        };
        for f in &me.bundle.files {
            let p = me.path(&f.name);
            let (size, sha) = hash_file(&p)?;
            if size != f.size || sha != f.sha256 {
                return Err(format!(
                    "{}: {size} bytes, SHA-256 {sha}; bundle.json says {} bytes, {}",
                    p.display(),
                    f.size,
                    f.sha256
                ));
            }
        }
        Ok(me)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

pub fn hash_file(p: &Path) -> Result<(u64, String), String> {
    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        if n == 0 {
            break;
        }
        size += n as u64;
        h.update(&buf[..n]);
    }
    Ok((size, hex(&h.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use emco_fleet_proto::bundle::{BundleFile, FILES, FORMAT};

    fn write(dir: &Path) -> Bundle {
        let files = FILES
            .iter()
            .map(|n| {
                let data = format!("{n} contents");
                std::fs::write(dir.join(n), &data).unwrap();
                BundleFile {
                    name: n.to_string(),
                    size: data.len() as u64,
                    sha256: hex(&Sha256::digest(&data)),
                }
            })
            .collect();
        let b = Bundle {
            format: FORMAT.into(),
            class: "kd240".into(),
            version: "/nix/store/x-1".into(),
            created: 0,
            files,
        };
        std::fs::write(dir.join("bundle.json"), serde_json::to_string(&b).unwrap()).unwrap();
        b
    }

    #[test]
    fn a_good_directory_opens() {
        let d = tempfile::tempdir().unwrap();
        let b = write(d.path());
        assert_eq!(BundleDir::open(d.path()).unwrap().bundle, b);
    }

    #[test]
    fn a_changed_file_is_caught_here_first() {
        let d = tempfile::tempdir().unwrap();
        write(d.path());
        std::fs::write(d.path().join("dtb"), "dtb contentz").unwrap();
        let e = BundleDir::open(d.path()).err().unwrap();
        assert!(e.contains("dtb"), "{e}");
        std::fs::remove_file(d.path().join("Image")).unwrap();
        assert!(BundleDir::open(d.path()).is_err());
    }
}
