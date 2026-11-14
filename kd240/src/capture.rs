//! Listen-only RS-485 capture, written in the capture bridge's record format.
//!
//! The KD240's RS-485 tty is opened **read-only** (`O_RDONLY | O_NOCTTY`):
//! this module never holds a handle it could write the bus with, and no
//! other module opens the tty. On Linux a UART drives its RS-485
//! transceiver's driver-enable only while it transmits, so a port nobody
//! writes keeps the KD240 off the bus. That is a software property; that the
//! KD240's transceiver DE really follows the UART must still be checked on
//! the board (see `kd240/README.md`).
//!
//! Each byte goes through [`crate::parmrk::Unmark`] (the kernel's in-band
//! framing/parity marks become the record flags) into an
//! [`emco_bridge_core::Bridge`], and the framed records are appended to a
//! file that `tools/emco-capture.py --replay` decodes unchanged.
//!
//! **Timestamps are host `read()` times, not wire times.** Bytes that arrive
//! in one `read()` share one timestamp. Doc 30 §4 rung 2 is explicit that
//! host timing destroys the inter-byte signature, so every `STATUS` record
//! says so in its text, and nothing timed from this capture should be
//! quoted in doc 30 §8. It is good for "is the bus silent?", byte values and
//! error positions; the bridge or a logic analyser is the timing instrument.

use crate::parmrk::Unmark;
use emco_bridge_core::{flag, Bridge, Config};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bytes of record ring between the decoder and the file.
const RING: usize = 16 * 1024;
/// A `STATUS` record this often.
const STATUS_EVERY: Duration = Duration::from_secs(10);

/// Parity setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parity {
    N,
    E,
    O,
}

/// Line framing, e.g. `8N1`, `7E1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Framing {
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
}

impl Framing {
    /// Parses `8N1`, `7E1`, `8O2`, … (data 5–8, parity N/E/O, stop 1–2).
    pub fn parse(s: &str) -> Option<Framing> {
        let b = s.as_bytes();
        if b.len() != 3 {
            return None;
        }
        let data_bits = b[0].checked_sub(b'0').filter(|d| (5..=8).contains(d))?;
        let parity = match b[1].to_ascii_uppercase() {
            b'N' => Parity::N,
            b'E' => Parity::E,
            b'O' => Parity::O,
            _ => return None,
        };
        let stop_bits = b[2].checked_sub(b'0').filter(|s| (1..=2).contains(s))?;
        Some(Framing {
            data_bits,
            parity,
            stop_bits,
        })
    }

    pub fn name(&self) -> String {
        let p = match self.parity {
            Parity::N => 'N',
            Parity::E => 'E',
            Parity::O => 'O',
        };
        format!("{}{}{}", self.data_bits, p, self.stop_bits)
    }
}

/// The bit rates doc 30 §7 sweeps, plus the slow ones.
pub const BAUD_RATES: [u32; 10] = [
    1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800,
];

/// What the web asks for. The port is not here: it comes from the command
/// line only, so the web cannot open an arbitrary device.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub baud: u32,
    pub framing: String,
}

/// The capture's state, as the API reports it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub running: bool,
    pub port: Option<String>,
    pub baud: u32,
    pub framing: String,
    pub file: Option<String>,
    pub rx_bytes: u32,
    pub error_bytes: u32,
    pub record_bytes: u64,
    pub started_unix: u64,
    pub error: Option<String>,
}

/// Bytes in, framed records out: the decoder, the bridge and the clock
/// origin, with no I/O (the thread below does the I/O).
pub struct Recorder {
    unmark: Unmark,
    bridge: Bridge<RING>,
    describe: String,
}

impl Recorder {
    pub fn new(baud: u32, framing: Framing) -> Recorder {
        Recorder {
            unmark: Unmark::new(framing.parity != Parity::N),
            bridge: Bridge::new(Config::at_baud(baud)),
            describe: format!(
                "kd240-linux listen-only {baud} {} HOST read() timestamps, not wire",
                framing.name()
            ),
        }
    }

    /// A `STATUS` record (listen-only, with the description).
    pub fn status(&mut self, micros: u32) {
        let describe = self.describe.clone();
        self.bridge.on_status(micros, flag::LISTEN_ONLY, &describe);
    }

    /// Bytes from one `read()`, all at `micros`.
    pub fn bytes(&mut self, micros: u32, tty: &[u8]) {
        for &b in tty {
            let bridge = &mut self.bridge;
            self.unmark
                .push(b, |byte, flags| bridge.on_rx(micros, byte, flags));
        }
    }

    /// Time passed with nothing read.
    pub fn idle(&mut self, micros: u32) {
        self.bridge.on_idle(micros);
    }

    /// Closes the open record.
    pub fn flush(&mut self, micros: u32) {
        self.bridge.flush(micros);
    }

    /// Appends every framed byte ready to `out`.
    pub fn drain(&mut self, out: &mut Vec<u8>) {
        let mut buf = [0u8; 1024];
        loop {
            let n = self.bridge.read(&mut buf);
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    pub fn rx_bytes(&self) -> u32 {
        self.bridge.counters().rx_bytes
    }

    pub fn error_bytes(&self) -> u32 {
        self.bridge.counters().error_bytes
    }
}

/// Configures `tty` raw, read-only use: the bit rate and framing, `CREAD`
/// and `CLOCAL`, no flow control, error marks in-band, reads returning
/// after 100 ms with nothing.
fn configure(tty: &std::fs::File, baud: u32, framing: Framing) -> io::Result<()> {
    use rustix::termios::{
        tcgetattr, tcsetattr, ControlModes, InputModes, OptionalActions, SpecialCodeIndex,
    };
    let mut t = tcgetattr(tty)?;
    t.make_raw();
    t.input_modes.remove(
        InputModes::IGNPAR
            | InputModes::IGNBRK
            | InputModes::BRKINT
            | InputModes::ISTRIP
            | InputModes::IXON
            | InputModes::IXOFF,
    );
    t.input_modes.insert(InputModes::INPCK | InputModes::PARMRK);
    t.control_modes.remove(
        ControlModes::CSIZE
            | ControlModes::PARENB
            | ControlModes::PARODD
            | ControlModes::CSTOPB
            | ControlModes::CRTSCTS,
    );
    t.control_modes
        .insert(ControlModes::CREAD | ControlModes::CLOCAL);
    t.control_modes.insert(match framing.data_bits {
        5 => ControlModes::CS5,
        6 => ControlModes::CS6,
        7 => ControlModes::CS7,
        _ => ControlModes::CS8,
    });
    match framing.parity {
        Parity::N => {}
        Parity::E => t.control_modes.insert(ControlModes::PARENB),
        Parity::O => t
            .control_modes
            .insert(ControlModes::PARENB | ControlModes::PARODD),
    }
    if framing.stop_bits == 2 {
        t.control_modes.insert(ControlModes::CSTOPB);
    }
    t.set_speed(baud)?;
    t.special_codes[SpecialCodeIndex::VMIN] = 0;
    t.special_codes[SpecialCodeIndex::VTIME] = 1;
    tcsetattr(tty, OptionalActions::Now, &t)?;
    Ok(())
}

/// Opens `port` for reading only.
fn open_read_only(port: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32)
        .open(port)
}

/// The capture controller: at most one capture at a time.
pub struct Capture {
    port: Option<PathBuf>,
    dir: PathBuf,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Capture {
    /// `port` is the RS-485 tty from the command line (`None`: capture
    /// unavailable); captures are written under `dir`.
    pub fn new(port: Option<PathBuf>, dir: PathBuf) -> io::Result<Capture> {
        std::fs::create_dir_all(&dir)?;
        let status = Status {
            port: port.as_ref().map(|p| p.display().to_string()),
            ..Status::default()
        };
        Ok(Capture {
            port,
            dir,
            status: Arc::new(Mutex::new(status)),
            stop: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
        })
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Starts a capture. Refused with no port, while one runs, or for a
    /// bit rate or framing outside the lists.
    pub fn start(&self, req: Request) -> Result<Status, String> {
        let port = self
            .port
            .clone()
            .ok_or("no RS-485 port configured (--rs485)")?;
        if !BAUD_RATES.contains(&req.baud) {
            return Err(format!("baud must be one of {BAUD_RATES:?}"));
        }
        let framing = Framing::parse(&req.framing).ok_or("framing must look like 8N1 or 7E1")?;
        let mut thread = self.thread.lock().map_err(|_| "capture lock poisoned")?;
        if self.status().running {
            return Err("a capture is already running".into());
        }
        if let Some(t) = thread.take() {
            let _ = t.join();
        }
        let tty = open_read_only(&port).map_err(|e| format!("open {}: {e}", port.display()))?;
        configure(&tty, req.baud, framing)
            .map_err(|e| format!("configure {}: {e}", port.display()))?;
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let name = format!("capture-{started}-{}-{}.bin", req.baud, framing.name());
        let file = std::fs::File::create(self.dir.join(&name))
            .map_err(|e| format!("create {name}: {e}"))?;
        {
            let mut s = self.status.lock().map_err(|_| "status lock poisoned")?;
            *s = Status {
                running: true,
                port: Some(port.display().to_string()),
                baud: req.baud,
                framing: framing.name(),
                file: Some(name),
                started_unix: started,
                ..Status::default()
            };
        }
        self.stop.store(false, Ordering::SeqCst);
        let (status, stop) = (self.status.clone(), self.stop.clone());
        let recorder = Recorder::new(req.baud, framing);
        *thread = Some(std::thread::spawn(move || {
            run(tty, file, recorder, status, stop)
        }));
        Ok(self.status())
    }

    /// Stops the running capture and waits for its file to be closed.
    pub fn stop(&self) -> Status {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(mut t) = self.thread.lock() {
            if let Some(t) = t.take() {
                let _ = t.join();
            }
        }
        self.status()
    }

    /// Captures on disk, newest first.
    pub fn list(&self) -> io::Result<Vec<(String, u64)>> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(&self.dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if valid_capture_name(&name) {
                v.push((name, e.metadata()?.len()));
            }
        }
        v.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(v)
    }
}

/// `capture-<digits>-<digits>-<framing>.bin`, nothing else.
pub fn valid_capture_name(name: &str) -> bool {
    let Some(stem) = name
        .strip_prefix("capture-")
        .and_then(|n| n.strip_suffix(".bin"))
    else {
        return false;
    };
    let parts: Vec<&str> = stem.split('-').collect();
    parts.len() == 3
        && parts[0].bytes().all(|b| b.is_ascii_digit())
        && !parts[0].is_empty()
        && parts[1].bytes().all(|b| b.is_ascii_digit())
        && !parts[1].is_empty()
        && Framing::parse(parts[2]).is_some()
}

fn run(
    mut tty: std::fs::File,
    mut file: std::fs::File,
    mut rec: Recorder,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
) {
    let origin = Instant::now();
    let micros = || origin.elapsed().as_micros() as u32; // wraps, like the bridge's
    let mut out = Vec::with_capacity(4096);
    let mut buf = [0u8; 512];
    let mut written: u64 = 0;
    let mut last_status = Instant::now();
    rec.status(micros());
    let mut error = None;
    while !stop.load(Ordering::SeqCst) {
        match tty.read(&mut buf) {
            Ok(0) => rec.idle(micros()),
            Ok(n) => rec.bytes(micros(), &buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                error = Some(format!("read: {e}"));
                break;
            }
        }
        if last_status.elapsed() >= STATUS_EVERY {
            rec.status(micros());
            last_status = Instant::now();
        }
        rec.drain(&mut out);
        if !out.is_empty() {
            if let Err(e) = file.write_all(&out) {
                error = Some(format!("write capture file: {e}"));
                break;
            }
            written += out.len() as u64;
            out.clear();
        }
        if let Ok(mut s) = status.lock() {
            s.rx_bytes = rec.rx_bytes();
            s.error_bytes = rec.error_bytes();
            s.record_bytes = written;
        }
    }
    let now = micros();
    rec.flush(now);
    rec.status(now);
    rec.drain(&mut out);
    let tail = file.write_all(&out).and_then(|()| file.sync_all());
    if let Ok(mut s) = status.lock() {
        s.running = false;
        s.rx_bytes = rec.rx_bytes();
        s.error_bytes = rec.error_bytes();
        s.record_bytes = written + out.len() as u64;
        s.error = error.or_else(|| tail.err().map(|e| format!("close capture file: {e}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits a record stream at zero bytes and COBS-decodes each frame.
    fn frames(stream: &[u8]) -> Vec<Vec<u8>> {
        stream
            .split(|b| *b == 0)
            .filter(|f| !f.is_empty())
            .map(|f| {
                let mut out = Vec::new();
                let mut i = 0;
                while i < f.len() {
                    let code = f[i] as usize;
                    out.extend_from_slice(&f[i + 1..i + code]);
                    i += code;
                    if code < 0xFF && i < f.len() {
                        out.push(0);
                    }
                }
                out
            })
            .collect()
    }

    #[test]
    fn framing_names_parse_and_print() {
        for s in ["8N1", "7E1", "8O2", "5N1"] {
            assert_eq!(Framing::parse(s).unwrap().name(), s);
        }
        assert_eq!(Framing::parse("7e1").unwrap().name(), "7E1");
        for bad in ["", "9N1", "8X1", "8N3", "8N", "8N11", "4N1"] {
            assert!(Framing::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn records_are_listen_only_status_then_rx_with_error_flags() {
        let mut rec = Recorder::new(9600, Framing::parse("7E1").unwrap());
        rec.status(0);
        rec.bytes(1_000, &[0x41, 0xFF, 0x00, 0x42, 0xFF, 0xFF]);
        rec.idle(1_000_000);
        let mut out = Vec::new();
        rec.drain(&mut out);
        let f = frames(&out);
        // The bridge's segmenter starts a new record when the flags change,
        // so the marked byte is a record of its own.
        assert_eq!(f.len(), 4, "{f:?}");
        let status = &f[0];
        assert_eq!(status[0], emco_bridge_core::record::kind::STATUS);
        assert_eq!(status[5] & flag::LISTEN_ONLY, flag::LISTEN_ONLY);
        assert_eq!(status[5] & flag::TX_ARMED, 0);
        let text = String::from_utf8_lossy(&status[22..]);
        assert!(
            text.contains("listen-only") && text.contains("HOST"),
            "{text}"
        );
        let rx = |i: usize| {
            (
                f[i][0],
                u32::from_le_bytes(f[i][1..5].try_into().unwrap()),
                f[i][5],
                f[i][7..].to_vec(),
            )
        };
        let kind = emco_bridge_core::record::kind::RX;
        assert_eq!(rx(1), (kind, 1_000, 0, vec![0x41]));
        assert_eq!(
            rx(2),
            (kind, 1_000, flag::FRAMING | flag::PARITY, vec![0x42])
        );
        assert_eq!(rx(3), (kind, 1_000, 0, vec![0xFF]));
        assert_eq!(rec.rx_bytes(), 3);
        assert_eq!(rec.error_bytes(), 1);
    }

    #[test]
    fn capture_names_are_the_only_files_served() {
        assert!(valid_capture_name("capture-1763000000-9600-8N1.bin"));
        for bad in [
            "capture-1-2-8N1.txt",
            "../capture-1-2-8N1.bin",
            "capture--2-8N1.bin",
            "capture-1-2-9Z1.bin",
            "x.bin",
        ] {
            assert!(!valid_capture_name(bad), "{bad}");
        }
    }

    #[test]
    fn start_is_refused_without_a_port_or_with_bad_settings() {
        let dir = crate::store::tests::tempdir("cap");
        let c = Capture::new(None, dir.clone()).unwrap();
        assert!(c
            .start(Request {
                baud: 9600,
                framing: "8N1".into()
            })
            .unwrap_err()
            .contains("--rs485"));
        let c = Capture::new(Some("/dev/null".into()), dir).unwrap();
        assert!(c
            .start(Request {
                baud: 9601,
                framing: "8N1".into()
            })
            .unwrap_err()
            .contains("baud"));
        assert!(c
            .start(Request {
                baud: 9600,
                framing: "8Q1".into()
            })
            .unwrap_err()
            .contains("framing"));
        // /dev/null is not a tty: configuring it fails and nothing runs.
        assert!(c
            .start(Request {
                baud: 9600,
                framing: "8N1".into()
            })
            .unwrap_err()
            .contains("configure"));
        assert!(!c.status().running);
    }
}
