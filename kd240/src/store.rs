//! The program store: one `<name>.nc` file per program in a directory
//! (`<data>/programs`).
//!
//! Names come from the web, so they are checked before they touch the
//! file system: 1 to 64 characters of `A-Z a-z 0-9 . _ -`, starting with
//! a letter or digit, so no path separator, no `..` and no hidden file.

use serde::Serialize;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Largest program accepted, bytes.
pub const MAX_PROGRAM: usize = 1 << 20;

/// A stored program, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProgramInfo {
    pub name: String,
    pub bytes: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
}

/// True when `name` is a valid program name.
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        && !name.contains("..")
}

/// Writes `bytes` to `path` through a temporary file in the same directory,
/// renamed over it, so a reader or a power cut sees the old or the new file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// The store.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
}

fn invalid(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid program name {name:?}"),
    )
}

impl Store {
    /// Opens (creating) the directory.
    pub fn open(dir: PathBuf) -> io::Result<Store> {
        std::fs::create_dir_all(&dir)?;
        Ok(Store { dir })
    }

    fn path(&self, name: &str) -> io::Result<PathBuf> {
        if !valid_name(name) {
            return Err(invalid(name));
        }
        Ok(self.dir.join(format!("{name}.nc")))
    }

    /// Programs by name.
    pub fn list(&self) -> io::Result<Vec<ProgramInfo>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let file = entry.file_name();
            let Some(name) = file.to_str().and_then(|f| f.strip_suffix(".nc")) else {
                continue;
            };
            if !valid_name(name) {
                continue;
            }
            let meta = entry.metadata()?;
            if !meta.is_file() {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            out.push(ProgramInfo {
                name: name.to_owned(),
                bytes: meta.len(),
                modified,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.path(name)?)
    }

    pub fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_PROGRAM {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "program too large",
            ));
        }
        write_atomic(&self.path(name)?, bytes)
    }

    pub fn delete(&self, name: &str) -> io::Result<()> {
        std::fs::remove_file(self.path(name)?)
    }

    /// A name for a captured program not used yet: `received-<secs>`,
    /// with `-2`, `-3`, … if needed.
    pub fn fresh_name(&self, unix_secs: u64) -> String {
        let base = format!("received-{unix_secs}");
        let mut name = base.clone();
        let mut n = 2;
        while self.path(&name).map(|p| p.exists()).unwrap_or(false) {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh directory under the target directory (no tempfile crate).
    pub fn tempdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kd240-lathe-test-{}-{tag}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn kd7_names_that_could_leave_the_directory_are_refused() {
        for ok in ["O0001", "face-turn_2.v3", "a", &"x".repeat(64)] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "../etc/passwd",
            "a/b",
            ".hidden",
            "a..b",
            "-x",
            "a b",
            "é",
            &"x".repeat(65),
            "a\\b",
            "a\0",
        ] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn kd7_write_list_read_delete() {
        let store = Store::open(tempdir("store")).unwrap();
        store.write("B", b"M30\n").unwrap();
        store.write("A", b"G0 X1\nM30\n").unwrap();
        std::fs::write(store.dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(store.dir.join(".x.nc"), b"x").unwrap();
        let names: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|p| (p.name, p.bytes))
            .collect();
        assert_eq!(names, [("A".to_owned(), 10), ("B".to_owned(), 4)]);
        assert_eq!(store.read("A").unwrap(), b"G0 X1\nM30\n");
        store.delete("A").unwrap();
        assert!(store.read("A").is_err());
        assert_eq!(
            store.read("../x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(store.write("big", &vec![b'x'; MAX_PROGRAM + 1]).is_err());
        assert_eq!(store.fresh_name(7), "received-7");
        store.write("received-7", b"%").unwrap();
        assert_eq!(store.fresh_name(7), "received-7-2");
    }
}
