//! Everything the agent does to the system goes through [`System`], so the
//! tests run against a fake and never touch a disk or reboot anything.
//!
//! [`RealSystem`] writes exactly these, and the agent's tests check this
//! file for anything else (`lib.rs`, `executor_touches_only_the_slots`):
//!
//! * the inactive slot's raw partition, `/dev/disk/by-partuuid/<id>-02`
//!   (slot a) or `-03` (slot b), from its first byte;
//! * the inactive slot's directory on the FAT boot partition,
//!   `<boot>/a/` or `<boot>/b/` (`Image`, `initrd`, `dtb`, `cmdline`);
//! * the two boot-state sectors in the gap before partition 1
//!   (`emco_fleet_proto::bootstate::COPY_LBA`), on the disk those
//!   partitions belong to;
//!
//! and it runs one program, `systemctl` (`reboot`, and `list-units` for
//! telemetry). It never touches the QSPI boot firmware or its saved
//! environment, the partition table, the boot partition's `boot.scr`, or
//! the persistent data partition. Which slot it writes is the agent's
//! decision, made by `emco_fleet_proto::policy::stage_target`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use emco_fleet_proto::bootstate::{Slot, COPY_LBA, SECTOR};
use emco_fleet_proto::bundle::FILES;
use emco_fleet_proto::msg::{Telemetry, Temp};

use crate::telemetry::*;

/// Where one bundle file goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dest {
    pub slot: Slot,
    /// One of `emco_fleet_proto::bundle::FILES`.
    pub file: &'static str,
}

impl Dest {
    pub fn new(slot: Slot, name: &str) -> Option<Dest> {
        FILES
            .iter()
            .find(|f| **f == name)
            .map(|file| Dest { slot, file })
    }

    pub fn is_rootfs(&self) -> bool {
        self.file == FILES[0]
    }
}

/// A slot file being written. `finish` makes it durable (and, for a boot
/// file, gives it its name). Dropping it unfinished leaves junk in the
/// inactive slot, which is never booted unless marked.
pub trait SlotWriter: Write {
    fn finish(self: Box<Self>) -> Result<(), String>;
}

pub trait System {
    /// Unix seconds.
    fn now(&self) -> u64;
    /// Host facts for telemetry. The agent adds its own fields.
    fn host_telemetry(&self) -> Telemetry;
    /// `/run/current-system`'s store path: the running release's version.
    fn current_system(&self) -> Option<String>;
    /// The slot this boot came from (`emco.slot=` on the kernel command line).
    fn booted_slot(&self) -> Option<Slot>;
    /// The two boot-state sectors, as they are on the card.
    fn read_bootstate(&self) -> Result<[Vec<u8>; 2], String>;
    /// Write one boot-state copy and flush it to the card.
    fn write_bootstate(&self, copy: usize, sector: &[u8; SECTOR]) -> Result<(), String>;
    /// How many bytes `dest` can hold.
    fn capacity(&self, dest: &Dest) -> Result<u64, String>;
    fn open_write(&self, dest: &Dest) -> Result<Box<dyn SlotWriter>, String>;
    /// Read back the first `len` bytes of `dest` from the card itself (not
    /// from the page cache).
    fn open_read(&self, dest: &Dest, len: u64) -> Result<Box<dyn Read>, String>;
    /// Make a slot unbootable: wipe the root file system's superblock and
    /// remove its boot files.
    fn invalidate(&self, slot: Slot) -> Result<(), String>;
    fn reboot(&self) -> Result<(), String>;
}

/// The board.
pub struct RealSystem {
    /// The card's MBR disk id, lowercase hex (`/dev/disk/by-partuuid/<id>-0N`).
    pub disk_id: String,
    /// Where the FAT boot partition (KD240BOOT) is mounted.
    pub boot_dir: PathBuf,
}

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn store_target(link: &str) -> Option<String> {
    std::fs::canonicalize(link)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let shown = format!("{cmd:?}");
    let out = cmd.output().map_err(|e| format!("{shown}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail = err.len().saturating_sub(400);
        let tail = (tail..err.len())
            .find(|&i| err.is_char_boundary(i))
            .unwrap_or(err.len());
        Err(format!("{shown}: {}: {}", out.status, err[tail..].trim()))
    }
}

fn e(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |err| format!("{}: {err}", path.display())
}

/// Drop `path`'s cached pages, so the next read comes from the card.
fn drop_cache(f: &File, path: &Path) -> Result<(), String> {
    rustix::fs::fadvise(f, 0, None, rustix::fs::Advice::DontNeed)
        .map_err(|err| format!("{}: fadvise: {err}", path.display()))
}

impl RealSystem {
    fn partition(&self, n: u8) -> PathBuf {
        PathBuf::from(format!("/dev/disk/by-partuuid/{}-{n:02}", self.disk_id))
    }

    fn rootfs(&self, slot: Slot) -> PathBuf {
        self.partition(match slot {
            Slot::A => 2,
            Slot::B => 3,
        })
    }

    fn boot_file(&self, dest: &Dest) -> PathBuf {
        self.boot_dir.join(dest.slot.as_str()).join(dest.file)
    }

    fn path(&self, dest: &Dest) -> PathBuf {
        if dest.is_rootfs() {
            self.rootfs(dest.slot)
        } else {
            self.boot_file(dest)
        }
    }

    /// The whole disk partition 1 is on: `/dev/sda1` → `/dev/sda`, through
    /// sysfs (`/sys/class/block/sda1/..`).
    fn disk(&self) -> Result<PathBuf, String> {
        let p1 = self.partition(1);
        let dev = std::fs::canonicalize(&p1).map_err(e(&p1))?;
        let name = dev
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{}: no device name", dev.display()))?;
        let sys = PathBuf::from(format!("/sys/class/block/{name}"));
        let real = std::fs::canonicalize(&sys).map_err(e(&sys))?;
        if !real.join("partition").exists() {
            return Err(format!("{} is not a partition", dev.display()));
        }
        let parent = real
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{}: no parent disk", real.display()))?;
        Ok(PathBuf::from(format!("/dev/{parent}")))
    }
}

struct RawWriter {
    file: File,
    path: PathBuf,
}

impl Write for RawWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl SlotWriter for RawWriter {
    fn finish(self: Box<Self>) -> Result<(), String> {
        self.file.sync_all().map_err(e(&self.path))?;
        drop_cache(&self.file, &self.path)
    }
}

/// A boot file, written as `<name>.part` and renamed once synced.
struct FileWriter {
    file: File,
    tmp: PathBuf,
    path: PathBuf,
}

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl SlotWriter for FileWriter {
    fn finish(self: Box<Self>) -> Result<(), String> {
        self.file.sync_all().map_err(e(&self.tmp))?;
        std::fs::rename(&self.tmp, &self.path).map_err(e(&self.path))?;
        if let Some(dir) = self.path.parent() {
            File::open(dir).and_then(|d| d.sync_all()).map_err(e(dir))?;
        }
        drop_cache(&self.file, &self.path)
    }
}

fn temps() -> Vec<Temp> {
    let mut out = Vec::new();
    // hwmon: temp*_input in milli-degrees.
    if let Ok(dirs) = std::fs::read_dir("/sys/class/hwmon") {
        for d in dirs.flatten() {
            let dir = d.path();
            let name = read(dir.join("name"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten() {
                let fname = f.file_name().to_string_lossy().into_owned();
                let Some(chan) = fname
                    .strip_suffix("_input")
                    .filter(|c| c.starts_with("temp"))
                else {
                    continue;
                };
                if let Some(v) = read(f.path()).as_deref().and_then(hwmon_milli_c) {
                    let label = read(dir.join(format!("{chan}_label")))
                        .map(|l| l.trim().to_string())
                        .unwrap_or_else(|| chan.to_string());
                    out.push(Temp {
                        name: clip(&format!("{name}/{label}"), 40),
                        milli_c: v,
                    });
                }
            }
        }
    }
    // IIO (the Zynq UltraScale+ AMS): (raw + offset) * scale.
    if let Ok(dirs) = std::fs::read_dir("/sys/bus/iio/devices") {
        for d in dirs.flatten() {
            let dir = d.path();
            let name = read(dir.join("name"))
                .unwrap_or_default()
                .trim()
                .to_string();
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten() {
                let fname = f.file_name().to_string_lossy().into_owned();
                let Some(chan) = fname
                    .strip_suffix("_raw")
                    .filter(|c| c.starts_with("in_temp"))
                else {
                    continue;
                };
                let offset = read(dir.join(format!("{chan}_offset")));
                let scale = read(dir.join(format!("{chan}_scale")));
                if let Some(v) = read(f.path())
                    .and_then(|r| iio_milli_c(&r, offset.as_deref(), scale.as_deref()))
                {
                    let label = read(dir.join(format!("{chan}_label")))
                        .map(|l| l.trim().to_string())
                        .unwrap_or_else(|| chan.to_string());
                    out.push(Temp {
                        name: clip(&format!("{name}/{label}"), 40),
                        milli_c: v,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.truncate(8);
    out
}

impl System for RealSystem {
    fn now(&self) -> u64 {
        emco_fleet_proto::unix_now()
    }

    fn host_telemetry(&self) -> Telemetry {
        let (mem_total_kib, mem_available_kib) = read("/proc/meminfo")
            .map(|m| parse_meminfo(&m))
            .unwrap_or((0, 0));
        let failed = run(Command::new("systemctl").args([
            "list-units",
            "--state=failed",
            "--no-legend",
            "--plain",
        ]))
        .ok()
        .map(|s| count_lines(&s));
        Telemetry {
            hostname: clip(
                read("/proc/sys/kernel/hostname").unwrap_or_default().trim(),
                64,
            ),
            current_system: self.current_system(),
            kernel_release: clip(
                read("/proc/sys/kernel/osrelease")
                    .unwrap_or_default()
                    .trim(),
                64,
            ),
            kernel_version: clip(
                read("/proc/sys/kernel/version").unwrap_or_default().trim(),
                96,
            ),
            uptime_s: read("/proc/uptime")
                .and_then(|s| parse_uptime(&s))
                .unwrap_or(0),
            load: read("/proc/loadavg")
                .and_then(|s| parse_loadavg(&s))
                .unwrap_or([0.0; 3]),
            mem_total_kib,
            mem_available_kib,
            temps: temps(),
            failed_units: failed,
            ..Default::default()
        }
    }

    fn current_system(&self) -> Option<String> {
        store_target("/run/current-system")
    }

    fn booted_slot(&self) -> Option<Slot> {
        read("/proc/cmdline").and_then(|c| Slot::from_cmdline(&c))
    }

    fn read_bootstate(&self) -> Result<[Vec<u8>; 2], String> {
        let disk = self.disk()?;
        let mut f = File::open(&disk).map_err(e(&disk))?;
        drop_cache(&f, &disk)?;
        let mut out = [vec![0u8; SECTOR], vec![0u8; SECTOR]];
        for (lba, buf) in COPY_LBA.iter().zip(out.iter_mut()) {
            f.seek(SeekFrom::Start(lba * SECTOR as u64))
                .and_then(|_| f.read_exact(buf))
                .map_err(e(&disk))?;
        }
        Ok(out)
    }

    fn write_bootstate(&self, copy: usize, sector: &[u8; SECTOR]) -> Result<(), String> {
        let disk = self.disk()?;
        let lba = *COPY_LBA.get(copy).ok_or("no such boot-state copy")?;
        let mut f = OpenOptions::new()
            .write(true)
            .open(&disk)
            .map_err(e(&disk))?;
        f.seek(SeekFrom::Start(lba * SECTOR as u64))
            .and_then(|_| f.write_all(sector))
            .and_then(|_| f.sync_all())
            .map_err(e(&disk))
    }

    fn capacity(&self, dest: &Dest) -> Result<u64, String> {
        if !dest.is_rootfs() {
            // FAT space shows itself as a failed write.
            return Ok(u64::MAX);
        }
        let p = self.rootfs(dest.slot);
        File::open(&p)
            .and_then(|mut f| f.seek(SeekFrom::End(0)))
            .map_err(e(&p))
    }

    fn open_write(&self, dest: &Dest) -> Result<Box<dyn SlotWriter>, String> {
        if dest.is_rootfs() {
            let path = self.rootfs(dest.slot);
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(e(&path))?;
            return Ok(Box::new(RawWriter { file, path }));
        }
        let path = self.boot_file(dest);
        let dir = path.parent().expect("a boot file has a directory");
        std::fs::create_dir_all(dir).map_err(e(dir))?;
        let tmp = path.with_extension("part");
        let file = File::create(&tmp).map_err(e(&tmp))?;
        Ok(Box::new(FileWriter { file, tmp, path }))
    }

    fn open_read(&self, dest: &Dest, len: u64) -> Result<Box<dyn Read>, String> {
        let path = self.path(dest);
        let f = File::open(&path).map_err(e(&path))?;
        drop_cache(&f, &path)?;
        Ok(Box::new(f.take(len)))
    }

    fn invalidate(&self, slot: Slot) -> Result<(), String> {
        let path = self.rootfs(slot);
        let mut f = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(e(&path))?;
        f.write_all(&vec![0u8; 1 << 20])
            .and_then(|_| f.sync_all())
            .map_err(e(&path))?;
        for name in &FILES[1..] {
            let p = self.boot_file(&Dest { slot, file: name });
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(format!("{}: {err}", p.display())),
            }
        }
        Ok(())
    }

    fn reboot(&self) -> Result<(), String> {
        run(Command::new("systemctl").arg("reboot")).map(drop)
    }
}
