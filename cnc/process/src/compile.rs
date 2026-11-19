//! A G-code program as the physical unit's operations.
//!
//! The sender's own interpreter (`emco_sender_core::gcode::ModalState`)
//! reads the program block by block, in **machine** coordinates (radius X,
//! the work offset applied: the site's default G54 unless the program sets
//! one). Each block can change the spindle, the tool and the position; this
//! turns them into the operations the physical model runs:
//!
//! * [`Op::Spindle`]: `spindle.run` with the signed speed, or
//!   `spindle.stop`;
//! * [`Op::Index`], `turret.index`;
//! * [`Op::Motion`], one `motion.run` with its segments: rapids, feeds
//!   (µm/s), threads (synchronised, µm per revolution), and arcs as chords
//!   within [`CHORD_TOLERANCE_UM`].
//!
//! Nothing here knows a machine beyond what its unit announced
//! ([`cnc_model::Limits`]): a segment outside the travel or a feed above
//! the maximum is an error before anything moves.

use cnc_model::{Limits, Segment};
use emco_sender_core::gcode::{lex_line, Diagnostic, ModalState, MoveKind, Severity, SpindleDir};
use emco_sender_core::geom::point_along;

/// Arcs are cut into chords no further than this from the true arc.
pub const CHORD_TOLERANCE_UM: f64 = 5.0;

/// One operation of the physical unit.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Run the spindle at this speed (negative: M4), or stop it (0).
    Spindle(i32),
    /// Index the turret.
    Index(u8),
    /// Move through these segments (machine µm), in one `motion.run`.
    Motion(Vec<Segment>),
}

/// A compiled program.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub ops: Vec<Op>,
    /// The source line of each operation (the first segment's for motion).
    pub lines: Vec<u32>,
    pub segments: usize,
}

/// Why a program cannot run.
#[derive(Debug, Clone, PartialEq)]
pub struct CompileError {
    pub line: u32,
    pub message: String,
}

fn err(line: u32, message: impl Into<String>) -> CompileError {
    CompileError {
        line,
        message: message.into(),
    }
}

fn um(mm: f64) -> i32 {
    (mm * 1000.0).round() as i32
}

/// Compiles `text` for a machine with `limits`, its work zero at
/// `(g54_x_mm, g54_z_mm)` (machine coordinates).
pub fn compile(
    text: &str,
    limits: &Limits,
    g54_x_mm: f64,
    g54_z_mm: f64,
) -> Result<Compiled, CompileError> {
    let mut diags: Vec<Diagnostic> = Vec::new();
    let mut modal = ModalState::with_work_offset(g54_x_mm, g54_z_mm);
    let mut ops: Vec<Op> = Vec::new();
    let mut lines = Vec::new();
    let mut motion: Vec<Segment> = Vec::new();
    let mut motion_line = 0;
    let mut seq = 0u32;
    let mut spindle = 0i32;
    let mut segments = 0;
    let flush_motion =
        |ops: &mut Vec<Op>, lines: &mut Vec<u32>, motion: &mut Vec<Segment>, line: u32| {
            if !motion.is_empty() {
                ops.push(Op::Motion(core::mem::take(motion)));
                lines.push(line);
            }
        };
    for (n, raw) in text.lines().enumerate() {
        let n = n as u32;
        let Some(block) = lex_line(raw, n, &mut diags) else {
            continue;
        };
        let fx = modal.apply_block(&block, &mut diags);
        if let Some(d) = diags.iter().find(|d| d.severity == Severity::Error) {
            return Err(err(d.line, d.message.clone()));
        }
        let rpm = match modal.spindle {
            SpindleDir::Cw => modal.spindle_rpm.round() as i32,
            SpindleDir::Ccw => -(modal.spindle_rpm.round() as i32),
            SpindleDir::Off => 0,
        };
        if rpm != spindle {
            if rpm.abs() > limits.spindle_max_rpm {
                return Err(err(
                    n,
                    format!(
                        "S{} is above the machine's {} rpm",
                        rpm.abs(),
                        limits.spindle_max_rpm
                    ),
                ));
            }
            flush_motion(&mut ops, &mut lines, &mut motion, motion_line);
            ops.push(Op::Spindle(rpm));
            lines.push(n);
            spindle = rpm;
        }
        if let Some(t) = fx.tool_change {
            if t == 0 || t > u32::from(limits.turret_stations) {
                return Err(err(
                    n,
                    format!(
                        "T{t}: the turret has stations 1 to {}",
                        limits.turret_stations
                    ),
                ));
            }
            flush_motion(&mut ops, &mut lines, &mut motion, motion_line);
            ops.push(Op::Index(t as u8));
            lines.push(n);
        }
        if let Some(mv) = fx.motion {
            if motion.is_empty() {
                motion_line = n;
            }
            let mut push =
                |x_mm: f64, z_mm: f64, feed: u32, flags: u8| -> Result<(), CompileError> {
                    let (x, z) = (um(x_mm), um(z_mm));
                    if !limits.contains(x, z) {
                        return Err(err(
                            n,
                            format!("X{:.3} Z{:.3} (machine) is outside the travel", x_mm, z_mm),
                        ));
                    }
                    seq += 1;
                    segments += 1;
                    motion.push(Segment {
                        seq,
                        x_um: x,
                        z_um: z,
                        feed,
                        flags,
                    });
                    Ok(())
                };
            let per_s = |mm_min: f64| ((mm_min * 1000.0 / 60.0).round() as u32).max(1);
            match mv.kind {
                MoveKind::Rapid => push(mv.x1, mv.z1, 0, Segment::RAPID)?,
                MoveKind::Feed => {
                    let f = per_s(mv.feed_mm_min);
                    if f > limits.max_feed_um_s {
                        return Err(err(
                            n,
                            format!(
                                "F{:.0} mm/min is above the machine's maximum",
                                mv.feed_mm_min
                            ),
                        ));
                    }
                    push(mv.x1, mv.z1, f, 0)?
                }
                MoveKind::Thread => {
                    if spindle == 0 {
                        return Err(err(n, "a thread with the spindle stopped"));
                    }
                    if spindle.abs() > limits.threading_max_rpm {
                        return Err(err(
                            n,
                            format!(
                                "threading at {} rpm: the limit is {}",
                                spindle.abs(),
                                limits.threading_max_rpm
                            ),
                        ));
                    }
                    let per_rev =
                        (mv.feed_mm_min / f64::from(spindle.abs()) * 1000.0).round() as u32;
                    push(mv.x1, mv.z1, per_rev, Segment::SYNC)?
                }
                MoveKind::Arc { .. } => {
                    let f = per_s(mv.feed_mm_min);
                    if f > limits.max_feed_um_s {
                        return Err(err(
                            n,
                            format!(
                                "F{:.0} mm/min is above the machine's maximum",
                                mv.feed_mm_min
                            ),
                        ));
                    }
                    let steps = chords(&mv);
                    for i in 1..=steps {
                        let (x, z) = point_along(
                            mv.kind,
                            mv.x0,
                            mv.z0,
                            mv.x1,
                            mv.z1,
                            f64::from(i) / f64::from(steps),
                        );
                        push(x, z, f, 0)?;
                    }
                }
            }
        }
        if fx.end_of_program {
            break;
        }
    }
    flush_motion(&mut ops, &mut lines, &mut motion, motion_line);
    if spindle != 0 {
        ops.push(Op::Spindle(0));
        lines.push(text.lines().count() as u32);
    }
    Ok(Compiled {
        ops,
        lines,
        segments,
    })
}

/// Chords for an arc so none strays more than [`CHORD_TOLERANCE_UM`].
fn chords(mv: &emco_sender_core::gcode::MotionCmd) -> u32 {
    let MoveKind::Arc { cx, cz, .. } = mv.kind else {
        return 1;
    };
    let r = ((mv.x0 - cx).powi(2) + (mv.z0 - cz).powi(2)).sqrt() * 1000.0;
    if r <= CHORD_TOLERANCE_UM {
        return 1;
    }
    // Sagitta s = r (1 - cos(θ/2)) ≤ tol  →  θ ≤ 2 acos(1 - tol / r).
    let max_angle = 2.0 * (1.0 - CHORD_TOLERANCE_UM / r).acos();
    let len = emco_sender_core::geom::segment_length(mv.kind, mv.x0, mv.z0, mv.x1, mv.z1) * 1000.0;
    let angle = len / r;
    ((angle / max_angle).ceil() as u32).clamp(1, 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pc_turn_50() -> Limits {
        cnc_model::PhysicalConfig::pc_turn_50().limits()
    }

    #[test]
    fn a_program_becomes_spindle_turret_and_motion() {
        let text =
            "G18 G21 G90 G95\nT1\nG97 S800 M3\nG0 X20 Z2\nG1 G94 Z-10 F120\nX24\nG0 Z2\nM5\nM30\n";
        let c = compile(text, &pc_turn_50(), 0.0, -50.0).unwrap();
        assert!(matches!(c.ops[0], Op::Index(1)), "{:?}", c.ops);
        assert_eq!(c.ops[1], Op::Spindle(800));
        let Op::Motion(m) = &c.ops[2] else {
            panic!("{:?}", c.ops)
        };
        // Diameter 20 → radius 10 mm; Z2 at G54 Z-50 → machine -48 mm.
        assert_eq!(
            (m[0].x_um, m[0].z_um, m[0].flags),
            (10_000, -48_000, Segment::RAPID)
        );
        assert_eq!(
            (m[1].z_um, m[1].feed),
            (-60_000, 2_000),
            "120 mm/min = 2 mm/s"
        );
        assert_eq!(m.iter().map(|s| s.seq).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(c.ops[3], Op::Spindle(0));
        assert_eq!(c.ops.len(), 4);
    }

    #[test]
    fn arcs_become_chords_within_tolerance() {
        let text = "G21 G90 G94 G18\nS500 M3\nG0 X0 Z0\nG3 X10 Z-5 R5 F60\nM30\n";
        let c = compile(text, &pc_turn_50(), 0.0, -50.0).unwrap();
        let Op::Motion(m) = &c.ops[1] else {
            panic!("{:?}", c.ops)
        };
        assert!(m.len() > 5, "{}", m.len());
        let last = m.last().unwrap();
        assert_eq!((last.x_um, last.z_um), (5_000, -55_000));
    }

    #[test]
    fn what_the_machine_cannot_do_is_refused_with_its_line() {
        let l = pc_turn_50();
        let e = compile("G0 X200 Z0\n", &l, 0.0, -50.0).unwrap_err();
        assert_eq!(e.line, 0);
        assert!(e.message.contains("outside the travel"), "{}", e.message);
        let e = compile("S5000 M3\n", &l, 0.0, -50.0).unwrap_err();
        assert!(e.message.contains("3000"), "{}", e.message);
        let e = compile("G0 X10 Z0\nT9\n", &l, 0.0, -50.0).unwrap_err();
        assert_eq!(e.line, 1);
        let e = compile("G94 G1 X10 Z-1 F5000\n", &l, 0.0, -50.0).unwrap_err();
        assert!(e.message.contains("maximum"), "{}", e.message);
    }
}
