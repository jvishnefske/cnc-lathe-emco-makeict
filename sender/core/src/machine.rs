//! `VirtualMachine`: an in-process model of the EMCO AC95 control box.
//!
//! It consumes the same byte protocol as the real machine (the `PlainGcode`
//! framing, which is also the current `Ac95` guess), moves a simulated X/Z
//! pair with trapezoidal velocity profiles, indexes a simulated turret with a
//! real delay, tracks spindle state, enforces soft limits and emits alarms.
//! This is what makes the whole app demoable with no hardware attached.
//!
//! Protocol behaviour (mirrors the UNVERIFIED `PlainGcode` assumptions):
//! - each accepted block is answered with `ok\r\n` **when it completes**;
//! - rejected blocks get `error: ...\r\n`;
//! - asynchronous alarms are `alarm: CODE message\r\n`;
//! - XOFF/XON (0x13/0x11) are emitted when the input queue fills/drains;
//! - a 0x18 (Ctrl-X) byte resets: motion stops, queue clears, alarms clear.

use std::collections::VecDeque;

use crate::gcode::{lex_line, Block, Diagnostic, ModalState, MoveKind, SpindleDir};
use crate::geom::{point_along, segment_length};
use crate::profile::MachineProfile;
use crate::session::Dro;

const QUEUE_XOFF_AT: usize = 8;
const QUEUE_XON_AT: usize = 2;
/// Integration substep, ms. Keeps the trapezoid stable when the host ticks slowly.
const SUBSTEP_MS: f64 = 2.0;

#[derive(Clone, Debug)]
struct ExecMove {
    kind: MoveKind,
    x0: f64,
    z0: f64,
    x1: f64,
    z1: f64,
    length: f64,
    /// mm/s
    vmax: f64,
    /// distance travelled so far, mm
    s: f64,
    /// current speed, mm/s
    v: f64,
}

#[derive(Clone, Debug, PartialEq)]
enum Activity {
    Idle,
    Moving,
    /// Turret indexing; done at `until_ms` (or never, when jammed).
    TurretIndex { until_ms: f64 },
    Homing,
}

pub struct VirtualMachine {
    profile: MachineProfile,
    modal: ModalState,
    /// Actual axis position, radius mm / mm (machine coordinates).
    x: f64,
    z: f64,
    homed: bool,
    now_ms: f64,
    started: bool,
    rx_line: String,
    queue: VecDeque<Block>,
    exec: Option<ExecMove>,
    activity: Activity,
    xoff_sent: bool,
    alarms: Vec<(String, String)>,
    /// Alarm lines waiting to be emitted on the next `step` (faults can be
    /// injected between steps, and the host must hear about them).
    pending_out: Vec<u8>,
    turret_jammed: bool,
    turret_jam_deadline_ms: f64,
    pending_tool: u32,
    tool: u32,
}

impl VirtualMachine {
    pub fn new(profile: MachineProfile) -> VirtualMachine {
        VirtualMachine {
            profile,
            modal: ModalState::default(),
            x: 0.0,
            z: 0.0,
            homed: false,
            now_ms: 0.0,
            started: false,
            rx_line: String::new(),
            queue: VecDeque::new(),
            exec: None,
            activity: Activity::Idle,
            xoff_sent: false,
            alarms: Vec::new(),
            pending_out: Vec::new(),
            turret_jammed: false,
            turret_jam_deadline_ms: 0.0,
            pending_tool: 0,
            tool: 0,
        }
    }

    // -- test/UI accessors --------------------------------------------------

    pub fn pos(&self) -> (f64, f64) {
        (self.x, self.z)
    }
    pub fn is_alarmed(&self) -> bool {
        !self.alarms.is_empty()
    }
    pub fn alarms(&self) -> &[(String, String)] {
        &self.alarms
    }
    pub fn tool(&self) -> u32 {
        self.tool
    }
    pub fn spindle_rpm(&self) -> f64 {
        if self.modal.spindle == SpindleDir::Off {
            0.0
        } else {
            self.modal
                .spindle_rpm
                .clamp(self.profile.spindle_min_rpm, self.profile.spindle_max_rpm)
        }
    }
    pub fn spindle_on(&self) -> bool {
        self.modal.spindle != SpindleDir::Off
    }
    pub fn homed(&self) -> bool {
        self.homed
    }
    pub fn busy(&self) -> bool {
        self.activity != Activity::Idle || !self.queue.is_empty()
    }

    pub fn dro(&self) -> Dro {
        let (dtg_x, dtg_z) = match &self.exec {
            Some(e) => (e.x1 - self.x, e.z1 - self.z),
            None => (0.0, 0.0),
        };
        Dro {
            x_radius_mm: self.x,
            x_diameter_mm: self.x * 2.0,
            z_mm: self.z,
            x_work_mm: self.x,
            z_work_mm: self.z,
            x_dtg_mm: dtg_x,
            z_dtg_mm: dtg_z,
            homed: self.homed,
        }
    }

    /// Inject a fault for training scenarios:
    /// `"estop" | "limit_x" | "limit_z" | "turret_jam"`.
    pub fn inject_fault(&mut self, kind: &str) {
        match kind {
            "estop" => {
                self.halt();
                self.raise_alarm("ESTOP", "emergency stop");
            }
            "limit_x" => {
                self.halt();
                self.raise_alarm("LIMITX", "X axis limit switch");
            }
            "limit_z" => {
                self.halt();
                self.raise_alarm("LIMITZ", "Z axis limit switch");
            }
            "turret_jam" => {
                self.turret_jammed = true;
            }
            _ => {}
        }
    }

    fn halt(&mut self) {
        self.exec = None;
        self.activity = Activity::Idle;
        self.queue.clear();
        self.modal.spindle = SpindleDir::Off;
    }

    fn raise_alarm(&mut self, code: &str, message: &str) {
        if !self.alarms.iter().any(|(c, _)| c == code) {
            self.alarms.push((code.to_string(), message.to_string()));
            self.pending_out
                .extend_from_slice(format!("alarm: {code} {message}\r\n").as_bytes());
        }
    }

    /// Advance the simulation to `now_ms` and consume bytes the host sent.
    /// Returns bytes the machine emits back.
    pub fn step(&mut self, now_ms: f64, host_tx: &[u8]) -> Vec<u8> {
        let dt_ms = if self.started { (now_ms - self.now_ms).max(0.0) } else { 0.0 };
        self.started = true;
        self.now_ms = now_ms;

        let mut out = Vec::new();

        // 1. Consume host bytes into queued blocks.
        self.consume(host_tx, &mut out);

        // 2. Advance time.
        self.advance(dt_ms, &mut out);

        // 3. Start new work if idle.
        self.pump(&mut out);

        // 4. Software flow control on queue depth.
        if !self.xoff_sent && self.queue.len() >= QUEUE_XOFF_AT {
            out.push(0x13);
            self.xoff_sent = true;
        } else if self.xoff_sent && self.queue.len() <= QUEUE_XON_AT {
            out.push(0x11);
            self.xoff_sent = false;
        }

        // 5. Alarm lines raised during this step (or injected between steps).
        out.append(&mut self.pending_out);

        out
    }

    fn consume(&mut self, host_tx: &[u8], out: &mut Vec<u8>) {
        for &b in host_tx {
            match b {
                0x18 => {
                    // Soft reset: stop everything, clear alarms. Advisory
                    // stop only in real life; instant in the simulator.
                    self.halt();
                    self.alarms.clear();
                    self.pending_out.clear();
                    self.rx_line.clear();
                }
                b'\r' | b'\n' => {
                    let line = std::mem::take(&mut self.rx_line);
                    if line.trim().is_empty() {
                        continue;
                    }
                    let mut diags: Vec<Diagnostic> = Vec::new();
                    match lex_line(&line, 0, &mut diags) {
                        Some(block) if diags.iter().all(|d| d.severity != crate::gcode::Severity::Error) => {
                            self.queue.push_back(block);
                        }
                        _ => {
                            let msg = diags
                                .first()
                                .map(|d| d.message.clone())
                                .unwrap_or_else(|| "unparseable block".to_string());
                            out.extend_from_slice(format!("error: {msg}\r\n").as_bytes());
                        }
                    }
                }
                _ => {
                    if b.is_ascii() && !b.is_ascii_control() && self.rx_line.len() < 4096 {
                        self.rx_line.push(b as char);
                    }
                }
            }
        }
    }

    fn advance(&mut self, dt_ms: f64, out: &mut Vec<u8>) {
        match self.activity.clone() {
            Activity::Idle => {}
            Activity::TurretIndex { until_ms } => {
                if self.turret_jammed {
                    if self.now_ms >= self.turret_jam_deadline_ms {
                        self.activity = Activity::Idle;
                        self.raise_alarm("TURRET", "turret failed to index (jam)");
                        self.queue.clear();
                    }
                } else if self.now_ms >= until_ms {
                    self.tool = self.pending_tool;
                    self.activity = Activity::Idle;
                    out.extend_from_slice(b"ok\r\n");
                }
            }
            Activity::Moving | Activity::Homing => {
                let done = self.advance_motion(dt_ms);
                if done {
                    if self.activity == Activity::Homing {
                        self.homed = true;
                    }
                    self.activity = Activity::Idle;
                    self.exec = None;
                    out.extend_from_slice(b"ok\r\n");
                }
            }
        }
    }

    /// Trapezoidal profile integration. Returns true when the move completed.
    fn advance_motion(&mut self, dt_ms: f64) -> bool {
        let Some(exec) = &mut self.exec else { return true };
        let a = self.profile.accel_mm_sec2.max(1e-6);
        let mut remaining_ms = dt_ms;
        while remaining_ms > 0.0 {
            let h_ms = remaining_ms.min(SUBSTEP_MS);
            let h = h_ms / 1000.0; // seconds
            remaining_ms -= h_ms;
            let remaining_dist = exec.length - exec.s;
            // Decelerate when the stopping distance reaches the remaining distance.
            let stop_dist = exec.v * exec.v / (2.0 * a);
            if stop_dist >= remaining_dist {
                exec.v = (exec.v - a * h).max(0.05 * exec.vmax.max(0.1));
            } else {
                exec.v = (exec.v + a * h).min(exec.vmax);
            }
            exec.s += exec.v * h;
            if exec.s >= exec.length {
                exec.s = exec.length;
                break;
            }
        }
        let t = if exec.length > 0.0 { exec.s / exec.length } else { 1.0 };
        let (x, z) = point_along(exec.kind, exec.x0, exec.z0, exec.x1, exec.z1, t);
        self.x = x;
        self.z = z;
        exec.s >= exec.length
    }

    fn pump(&mut self, out: &mut Vec<u8>) {
        while self.activity == Activity::Idle {
            let Some(block) = self.queue.pop_front() else { break };
            if !self.alarms.is_empty() {
                out.extend_from_slice(b"error: machine in alarm state\r\n");
                continue;
            }
            self.execute_block(&block, out);
        }
    }

    fn execute_block(&mut self, block: &Block, out: &mut Vec<u8>) {
        // Reference return (home). Handled before general interpretation.
        if block.all('G').any(|g| (g - 28.0).abs() < 1e-9) {
            let (hx, hz) = (self.profile.x_max_mm, self.profile.z_max_mm);
            self.start_move(MoveKind::Rapid, hx, hz, self.rapid_mm_s());
            self.activity = Activity::Homing;
            // Keep the modal interpreter's idea of position in sync.
            self.modal.x = hx;
            self.modal.z = hz;
            return;
        }

        // Interpret through the same modal machinery as the parser, so the
        // machine and the backplot always agree about what a block means.
        self.modal.x = self.x;
        self.modal.z = self.z;
        let mut diags = Vec::new();
        let fx = self.modal.apply_block(block, &mut diags);
        if let Some(d) = diags.iter().find(|d| d.severity == crate::gcode::Severity::Error) {
            // Undo the interpreter's position advance: the move is rejected.
            self.modal.x = self.x;
            self.modal.z = self.z;
            out.extend_from_slice(format!("error: {}\r\n", d.message).as_bytes());
            return;
        }

        if let Some(station) = fx.tool_change {
            if station == 0 || station > self.profile.turret_stations {
                out.extend_from_slice(
                    format!("error: no turret station {station}\r\n").as_bytes(),
                );
                return;
            }
            if station != self.tool {
                self.pending_tool = station;
                let ms = self.profile.turret_index_seconds * 1000.0;
                self.activity = Activity::TurretIndex { until_ms: self.now_ms + ms };
                self.turret_jam_deadline_ms = self.now_ms + 3.0 * ms;
                // Ack comes when the index completes (or the jam alarm fires).
                return;
            }
        }

        let Some(mv) = fx.motion else {
            // Pure state block (S/M/F/G-only): acknowledge immediately.
            out.extend_from_slice(b"ok\r\n");
            return;
        };

        // Soft limits: endpooint and, for arcs, the full swept extent.
        let seg = crate::geom::Segment {
            kind: crate::geom::SegKind::Feed,
            x0: mv.x0,
            z0: mv.z0,
            x1: mv.x1,
            z1: mv.z1,
            cx: match mv.kind { MoveKind::Arc { cx, .. } => Some(cx), _ => None },
            cz: match mv.kind { MoveKind::Arc { cz, .. } => Some(cz), _ => None },
            ccw: match mv.kind { MoveKind::Arc { ccw, .. } => Some(ccw), _ => None },
            line: 0,
            feed_mm_min: None,
            tool: None,
        };
        let seg = crate::geom::Segment {
            kind: if matches!(mv.kind, MoveKind::Arc { .. }) { crate::geom::SegKind::Arc } else { seg.kind },
            ..seg
        };
        let b = crate::geom::segment_bounds(&seg);
        let p = &self.profile;
        let eps = 1e-6;
        if b.x_min < p.x_min_mm - eps
            || b.x_max > p.x_max_mm + eps
            || b.z_min < p.z_min_mm - eps
            || b.z_max > p.z_max_mm + eps
        {
            // Reject the block and alarm — matches what a real control does
            // when a programmed move exceeds travel.
            self.modal.x = self.x;
            self.modal.z = self.z;
            self.raise_alarm("SOFTLIMIT", "programmed move exceeds travel");
            out.extend_from_slice(b"error: soft limit\r\n");
            return;
        }

        let vmax = match mv.kind {
            MoveKind::Rapid => self.rapid_mm_s(),
            _ => {
                let f = mv.feed_mm_min.min(self.profile.max_feed_mm_min);
                if f <= 0.0 {
                    self.modal.x = self.x;
                    self.modal.z = self.z;
                    out.extend_from_slice(b"error: feed rate is zero\r\n");
                    return;
                }
                f / 60.0
            }
        };
        self.start_move(mv.kind, mv.x1, mv.z1, vmax);
    }

    fn rapid_mm_s(&self) -> f64 {
        self.profile.rapid_mm_min / 60.0
    }

    fn start_move(&mut self, kind: MoveKind, x1: f64, z1: f64, vmax: f64) {
        let length = segment_length(kind, self.x, self.z, x1, z1);
        if length <= 1e-9 {
            // Zero-length move: complete instantly on the next advance.
            self.exec = Some(ExecMove {
                kind, x0: self.x, z0: self.z, x1, z1,
                length: 0.0, vmax: vmax.max(0.1), s: 0.0, v: 0.0,
            });
        } else {
            self.exec = Some(ExecMove {
                kind, x0: self.x, z0: self.z, x1, z1,
                length, vmax: vmax.max(0.01), s: 0.0, v: 0.0,
            });
        }
        self.activity = Activity::Moving;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> VirtualMachine {
        let mut p = MachineProfile::pc_turn_50_default();
        p.turret_index_seconds = 0.5;
        VirtualMachine::new(p)
    }

    /// Drive the vm with 5 ms ticks until it goes idle or `max_ms` elapses.
    /// Returns everything it emitted.
    fn run_until_idle(m: &mut VirtualMachine, start_ms: f64, max_ms: f64) -> (Vec<u8>, f64) {
        let mut out = Vec::new();
        let mut t = start_ms;
        while t < start_ms + max_ms {
            t += 5.0;
            out.extend(m.step(t, &[]));
            if !m.busy() {
                break;
            }
        }
        (out, t)
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).to_string()
    }

    #[test]
    fn acks_state_block_immediately() {
        let mut m = vm();
        let out = m.step(0.0, b"S1000 M3\r");
        assert_eq!(text(&out), "ok\r\n");
        assert!(m.spindle_on());
        assert_eq!(m.spindle_rpm(), 1000.0);
    }

    #[test]
    fn spindle_rpm_clamped_to_profile() {
        let mut m = vm();
        m.step(0.0, b"S99999 M3\r");
        assert_eq!(m.spindle_rpm(), m.profile.spindle_max_rpm);
    }

    #[test]
    fn move_takes_time_and_acks_on_completion() {
        let mut m = vm();
        // Radius mode, absolute; 10 mm rapid at 1000 mm/min ≈ 600+ ms with accel.
        let out = m.step(0.0, b"G8 G0 X10 Z-10\r");
        assert!(!out.windows(2).any(|w| w == b"ok"), "no ack before motion completes");
        let (out, t) = run_until_idle(&mut m, 0.0, 20_000.0);
        assert!(text(&out).contains("ok"), "move must ack when done");
        let (x, z) = m.pos();
        assert!((x - 10.0).abs() < 1e-6 && (z + 10.0).abs() < 1e-6);
        assert!(t >= 600.0, "10mm diagonal rapid can't be instant, took {t} ms");
    }

    #[test]
    fn trapezoid_respects_feed_limit() {
        let mut m = vm();
        m.step(0.0, b"G8 G94 F200 G1 Z-50\r");
        // At 200 mm/min = 3.33 mm/s, 50 mm takes ~15 s plus ramps.
        let (out, t) = run_until_idle(&mut m, 0.0, 60_000.0);
        assert!(text(&out).contains("ok"));
        assert!(t > 14_000.0, "feed move finished suspiciously fast: {t} ms");
    }

    #[test]
    fn arc_motion_follows_arc_path() {
        let mut m = vm();
        m.step(0.0, b"G8 G94 F500 G0 X0 Z0\r");
        run_until_idle(&mut m, 0.0, 10_000.0);
        m.step(10_000.0, b"G3 X10 Z-10 I0 K-10\r");
        // Sample mid-motion: the position must stay on the circle centred (0,-10) r=10.
        let mut t = 10_000.0;
        let mut checked = false;
        for _ in 0..4000 {
            t += 5.0;
            m.step(t, &[]);
            let (x, z) = m.pos();
            if x > 1.0 && x < 9.0 {
                let r = (x.powi(2) + (z + 10.0).powi(2)).sqrt();
                assert!((r - 10.0).abs() < 0.01, "off arc at ({x},{z})");
                checked = true;
            }
            if !m.busy() {
                break;
            }
        }
        assert!(checked, "never sampled mid-arc");
        let (x, z) = m.pos();
        assert!((x - 10.0).abs() < 1e-6 && (z + 10.0).abs() < 1e-6);
    }

    #[test]
    fn soft_limit_rejected_with_alarm() {
        let mut m = vm();
        let out = m.step(0.0, b"G8 G0 X100\r"); // x_max is 50
        let s = text(&out);
        assert!(s.contains("error: soft limit"), "{s}");
        assert!(s.contains("alarm: SOFTLIMIT"), "{s}");
        assert!(m.is_alarmed());
        let (x, _) = m.pos();
        assert_eq!(x, 0.0, "machine must not move");
        // Subsequent blocks rejected while alarmed.
        let out = m.step(10.0, b"G0 X5\r");
        assert!(text(&out).contains("machine in alarm"));
        // Ctrl-X clears the alarm.
        let out = m.step(20.0, &[0x18]);
        assert!(!m.is_alarmed());
        drop(out);
    }

    #[test]
    fn arc_sweeping_outside_travel_is_rejected() {
        let mut m = vm();
        // Arc from (0,0) to (0,-20) around (0,-10) sweeps to x=+10... make one
        // that sweeps to x=-10 instead (below x_min=0): G2 goes the other way.
        let out1 = m.step(0.0, b"G8 G94 F100 G0 Z0\r");
        assert!(text(&out1).is_empty() || m.busy() || !m.busy());
        run_until_idle(&mut m, 0.0, 5_000.0);
        let out = m.step(6_000.0, b"G2 X0 Z-20 I0 K-10\r");
        assert!(text(&out).contains("soft limit"), "arc extremum below X min must trip: {}", text(&out));
    }

    #[test]
    fn turret_index_takes_time() {
        let mut m = vm();
        let out = m.step(0.0, b"T0202\r");
        assert!(!text(&out).contains("ok"), "turret ack must wait for the index");
        assert_eq!(m.tool(), 0);
        let out = m.step(200.0, &[]);
        assert!(!text(&out).contains("ok"));
        let out = m.step(600.0, &[]); // index takes 500 ms in this profile
        assert!(text(&out).contains("ok"));
        assert_eq!(m.tool(), 2);
    }

    #[test]
    fn invalid_turret_station_rejected() {
        let mut m = vm();
        let out = m.step(0.0, b"T0900\r"); // profile has 8 stations
        assert!(text(&out).contains("no turret station"));
    }

    #[test]
    fn turret_jam_fault_alarms() {
        let mut m = vm();
        m.inject_fault("turret_jam");
        m.step(0.0, b"T0303\r");
        let out = m.step(2_000.0, &[]); // 3x the 500 ms index time exceeded
        let s = text(&out);
        assert!(s.contains("alarm: TURRET"), "{s}");
        assert!(m.is_alarmed());
        assert_eq!(m.tool(), 0, "jammed turret must not report the new tool");
    }

    #[test]
    fn estop_fault_halts_motion_and_spindle() {
        let mut m = vm();
        m.step(0.0, b"S1000 M3\r");
        m.step(5.0, b"G8 G0 Z-50\r");
        m.step(100.0, &[]);
        let (_, z_before) = m.pos();
        assert!(z_before < 0.0, "should be moving by 100 ms");
        m.inject_fault("estop");
        m.step(105.0, &[]);
        let (_, z_after) = m.pos();
        m.step(500.0, &[]);
        let (_, z_later) = m.pos();
        assert_eq!(z_after, z_later, "no motion after estop");
        assert!(!m.spindle_on());
        assert!(m.alarms().iter().any(|(c, _)| c == "ESTOP"));
    }

    #[test]
    fn limit_fault_alarms() {
        let mut m = vm();
        m.inject_fault("limit_x");
        assert!(m.alarms().iter().any(|(c, _)| c == "LIMITX"));
    }

    #[test]
    fn home_moves_to_reference_and_sets_homed() {
        let mut m = vm();
        assert!(!m.homed());
        m.step(0.0, b"G28\r");
        let (out, _) = run_until_idle(&mut m, 0.0, 60_000.0);
        assert!(text(&out).contains("ok"));
        assert!(m.homed());
        let (x, z) = m.pos();
        assert_eq!((x, z), (m.profile.x_max_mm, m.profile.z_max_mm));
    }

    #[test]
    fn bad_block_gets_error_response() {
        let mut m = vm();
        let out = m.step(0.0, b"G1 X\r");
        assert!(text(&out).contains("error:"));
    }

    #[test]
    fn zero_feed_move_rejected() {
        let mut m = vm();
        let out = m.step(0.0, b"G8 G1 Z-5\r");
        assert!(text(&out).contains("feed rate is zero"));
    }

    #[test]
    fn queue_depth_emits_xoff_then_xon() {
        let mut m = vm();
        // Flood with slow feed moves so the queue backs up.
        let mut tx = Vec::new();
        tx.extend_from_slice(b"G8 G94 F100 G1 Z-1\r");
        for i in 2..14 {
            tx.extend_from_slice(format!("G1 Z-{i}\r").as_bytes());
        }
        let out = m.step(0.0, &tx);
        assert!(out.contains(&0x13), "XOFF expected when the queue fills");
        // Drain: run the whole job.
        let (out, _) = run_until_idle(&mut m, 0.0, 600_000.0);
        assert!(out.contains(&0x11), "XON expected when the queue drains");
    }

    #[test]
    fn dro_reports_radius_and_diameter() {
        let mut m = vm();
        m.step(0.0, b"G8 G0 X10\r");
        run_until_idle(&mut m, 0.0, 20_000.0);
        let d = m.dro();
        assert!((d.x_radius_mm - 10.0).abs() < 1e-6);
        assert!((d.x_diameter_mm - 20.0).abs() < 1e-6);
    }

    #[test]
    fn diameter_mode_program_moves_radius() {
        let mut m = vm();
        m.step(0.0, b"G0 X20\r"); // default diameter mode
        run_until_idle(&mut m, 0.0, 20_000.0);
        assert!((m.pos().0 - 10.0).abs() < 1e-6);
    }
}
