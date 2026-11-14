//! The capture end to end over a real tty: a pseudo-terminal stands in for
//! the KD240's RS-485 port. Bytes written to the master side arrive at the
//! read-only slave the capture opened, are recorded in the bridge's format,
//! and `tools/emco-capture.py --replay` (when python3 is on the PATH)
//! decodes the file.

use emco_kd240::capture::{Capture, Request};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("emco-kd240-it-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_pty_capture_is_listen_only_and_decodes_with_the_bridge_tools() {
    use rustix::pty::{grantpt, openpt, ptsname, unlockpt, OpenptFlags};
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave = PathBuf::from(ptsname(&master, Vec::new()).unwrap().into_string().unwrap());
    let mut master = std::fs::File::from(master);

    let dir = tempdir("pty");
    let cap = Capture::new(Some(slave), dir.clone()).unwrap();
    let st = cap
        .start(Request {
            baud: 9600,
            framing: "8N1".into(),
        })
        .unwrap();
    assert!(st.running);
    assert!(cap
        .start(Request {
            baud: 9600,
            framing: "8N1".into()
        })
        .unwrap_err()
        .contains("already"));
    std::thread::sleep(Duration::from_millis(150));
    master.write_all(&[0x01, 0x02, b'A', b'B', 0xFF]).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let st = cap.stop();
    assert!(!st.running);
    assert_eq!(st.error, None);
    assert_eq!(st.rx_bytes, 5, "the tty layer's 0xFF doubling is undone");
    assert_eq!(st.error_bytes, 0);

    let list = cap.list().unwrap();
    assert_eq!(list.len(), 1);
    let file = dir.join(&list[0].0);
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(bytes.len() as u64, st.record_bytes);
    assert_eq!(
        *bytes.last().unwrap(),
        0,
        "records are zero-terminated COBS frames"
    );

    let tool = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tools/emco-capture.py");
    let Ok(out) = std::process::Command::new("python3")
        .arg("-I")
        .arg(&tool)
        .arg("--replay")
        .arg(&file)
        .output()
    else {
        eprintln!("python3 not found; skipped the decoder half");
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("01 02 41 42 ff"), "{stdout}");
    assert!(stdout.contains("STATUS [LISTEN-ONLY]"), "{stdout}");
    assert!(stdout.contains("HOST read() timestamps"), "{stdout}");
    assert!(
        !stdout.contains("TX "),
        "a listen-only capture has no TX record: {stdout}"
    );
    assert!(stderr.contains("5 bytes received"), "{stderr}");
    // Bytes from one read() share a timestamp: the gaps are 0, which is why
    // this capture is not a timing instrument (kd240/README.md).
    assert!(stdout.contains("gaps(us) 0,0,0,0"), "{stdout}");
}
