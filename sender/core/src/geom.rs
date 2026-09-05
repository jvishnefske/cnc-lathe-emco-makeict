//! Backplot geometry: turns parsed blocks into [`Segment`]s and computes exact
//! bounds, including arc extrema (not just endpoints).
//!
//! Coordinate frame: `x` is **radius** in mm, `z` is mm (negative toward the
//! chuck). Arcs live in the ZX plane; we treat `(z, x)` as a standard math
//! plane, so `ccw == true` (G03) means counterclockwise in that plane.

use serde::{Deserialize, Serialize};

use crate::gcode::{Block, Diagnostic, ModalState, MotionCmd, MoveKind};

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SegKind {
    Rapid,
    Feed,
    Arc,
    Thread,
}

/// Matches the `Segment` shape in `sender/INTERFACE.md`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub kind: SegKind,
    pub x0: f64,
    pub z0: f64,
    pub x1: f64,
    pub z1: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cx: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ccw: Option<bool>,
    /// 0-based source line that produced this move.
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feed_mm_min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<u32>,
}

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub x_min: f64,
    pub x_max: f64,
    pub z_min: f64,
    pub z_max: f64,
}

impl Bounds {
    pub fn empty() -> Bounds {
        Bounds { x_min: 0.0, x_max: 0.0, z_min: 0.0, z_max: 0.0 }
    }
    fn from_point(x: f64, z: f64) -> Bounds {
        Bounds { x_min: x, x_max: x, z_min: z, z_max: z }
    }
    fn include(&mut self, x: f64, z: f64) {
        self.x_min = self.x_min.min(x);
        self.x_max = self.x_max.max(x);
        self.z_min = self.z_min.min(z);
        self.z_max = self.z_max.max(z);
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StockSpec {
    pub diameter_mm: f64,
    pub length_mm: f64,
    /// z of the free end of the stock, usually 0.
    pub z_face_mm: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Backplot {
    pub segments: Vec<Segment>,
    pub bounds: Bounds,
}

/// Walk blocks with a fresh modal state, producing segments and exact bounds.
/// Semantic problems (bad arcs, missing feeds, …) are appended to `diags`.
///
/// Block-skip (`/`) lines are **included** in the backplot — the physical
/// block-delete switch defaults to off, so those blocks normally execute.
pub fn interpret(blocks: &[Block], diags: &mut Vec<Diagnostic>) -> (Vec<Segment>, Bounds) {
    let mut modal = ModalState::default();
    let mut segments = Vec::new();
    let mut bounds: Option<Bounds> = None;
    for b in blocks {
        let fx = modal.apply_block(b, diags);
        if let Some(mv) = fx.motion {
            let seg = segment_from_motion(&mv, b.line, modal.tool);
            let sb = segment_bounds(&seg);
            match &mut bounds {
                Some(bb) => {
                    bb.include(sb.x_min, sb.z_min);
                    bb.include(sb.x_max, sb.z_max);
                }
                None => bounds = Some(sb),
            }
            segments.push(seg);
        }
        if fx.end_of_program {
            break;
        }
    }
    (segments, bounds.unwrap_or_else(Bounds::empty))
}

fn segment_from_motion(mv: &MotionCmd, line: u32, tool: u32) -> Segment {
    let (kind, cx, cz, ccw, feed) = match mv.kind {
        MoveKind::Rapid => (SegKind::Rapid, None, None, None, None),
        MoveKind::Feed => (SegKind::Feed, None, None, None, Some(mv.feed_mm_min)),
        MoveKind::Thread => (SegKind::Thread, None, None, None, Some(mv.feed_mm_min)),
        MoveKind::Arc { cx, cz, ccw } => {
            (SegKind::Arc, Some(cx), Some(cz), Some(ccw), Some(mv.feed_mm_min))
        }
    };
    Segment {
        kind,
        x0: mv.x0,
        z0: mv.z0,
        x1: mv.x1,
        z1: mv.z1,
        cx,
        cz,
        ccw,
        line,
        feed_mm_min: feed,
        tool: if tool > 0 { Some(tool) } else { None },
    }
}

/// Exact bounds of one segment, including arc axis extrema.
pub fn segment_bounds(seg: &Segment) -> Bounds {
    let mut b = Bounds::from_point(seg.x0, seg.z0);
    b.include(seg.x1, seg.z1);
    if seg.kind != SegKind::Arc {
        return b;
    }
    let (cx, cz, ccw) = (seg.cx.unwrap(), seg.cz.unwrap(), seg.ccw.unwrap());
    let r = ((seg.x0 - cx).powi(2) + (seg.z0 - cz).powi(2)).sqrt();
    // Angles in the (z, x) plane.
    let a0 = (seg.x0 - cx).atan2(seg.z0 - cz);
    let a1 = (seg.x1 - cx).atan2(seg.z1 - cz);
    // Check each axis-aligned extreme angle (0, 90, 180, 270 deg) for
    // membership in the swept arc.
    use std::f64::consts::{FRAC_PI_2, PI};
    for (i, ang) in [0.0, FRAC_PI_2, PI, -FRAC_PI_2].iter().enumerate() {
        if angle_on_arc(a0, a1, *ang, ccw) {
            match i {
                0 => b.include(cx, cz + r),  // +z extreme
                1 => b.include(cx + r, cz),  // +x extreme
                2 => b.include(cx, cz - r),  // -z extreme
                _ => b.include(cx - r, cz),  // -x extreme
            }
        }
    }
    b
}

/// Is angle `a` on the arc from `a0` to `a1` swept in direction `ccw`?
fn angle_on_arc(a0: f64, a1: f64, a: f64, ccw: bool) -> bool {
    use std::f64::consts::TAU;
    let norm = |x: f64| ((x % TAU) + TAU) % TAU;
    let sweep = if ccw { norm(a1 - a0) } else { norm(a0 - a1) };
    let off = if ccw { norm(a - a0) } else { norm(a0 - a) };
    // A zero sweep from the modal machinery would be a full circle, which the
    // parser rejects; treat tiny sweeps as tiny, not full.
    off <= sweep + 1e-9
}

/// Arc length of a segment (line length for non-arcs). Used by the simulator.
pub fn segment_length(seg_kind: MoveKind, x0: f64, z0: f64, x1: f64, z1: f64) -> f64 {
    match seg_kind {
        MoveKind::Arc { cx, cz, ccw } => {
            use std::f64::consts::TAU;
            let r = ((x0 - cx).powi(2) + (z0 - cz).powi(2)).sqrt();
            let a0 = (x0 - cx).atan2(z0 - cz);
            let a1 = (x1 - cx).atan2(z1 - cz);
            let norm = |x: f64| ((x % TAU) + TAU) % TAU;
            let sweep = if ccw { norm(a1 - a0) } else { norm(a0 - a1) };
            r * sweep
        }
        _ => ((x1 - x0).powi(2) + (z1 - z0).powi(2)).sqrt(),
    }
}

/// Point at fraction `t` (0..=1 of path length) along a move. Used by the
/// simulator to interpolate arcs properly.
pub fn point_along(kind: MoveKind, x0: f64, z0: f64, x1: f64, z1: f64, t: f64) -> (f64, f64) {
    let t = t.clamp(0.0, 1.0);
    match kind {
        MoveKind::Arc { cx, cz, ccw } => {
            use std::f64::consts::TAU;
            let r = ((x0 - cx).powi(2) + (z0 - cz).powi(2)).sqrt();
            let a0 = (x0 - cx).atan2(z0 - cz);
            let a1 = (x1 - cx).atan2(z1 - cz);
            let norm = |x: f64| ((x % TAU) + TAU) % TAU;
            let sweep = if ccw { norm(a1 - a0) } else { norm(a0 - a1) };
            let a = if ccw { a0 + sweep * t } else { a0 - sweep * t };
            (cx + r * a.sin(), cz + r * a.cos())
        }
        _ => (x0 + (x1 - x0) * t, z0 + (z1 - z0) * t),
    }
}

/// Build the backplot for a parsed program. Bounds are unioned with the stock
/// outline so the UI viewport always contains the workpiece.
pub fn backplot(program: &crate::gcode::Program, stock: Option<StockSpec>) -> Backplot {
    let mut bounds = program.bounds;
    if program.segments.is_empty() {
        if let Some(s) = stock {
            bounds = Bounds {
                x_min: 0.0,
                x_max: s.diameter_mm / 2.0,
                z_min: s.z_face_mm - s.length_mm,
                z_max: s.z_face_mm,
            };
        }
    } else if let Some(s) = stock {
        bounds.include(0.0, s.z_face_mm - s.length_mm);
        bounds.include(s.diameter_mm / 2.0, s.z_face_mm);
    }
    Backplot { segments: program.segments.clone(), bounds }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode::Program;

    fn parse(src: &str) -> Program {
        Program::parse(src)
    }

    #[test]
    fn simple_program_segments() {
        // Radius mode, mm/min feed for easy numbers.
        let p = parse("G8 G94\nG0 X10 Z2\nG1 Z-20 F100\nG0 X15");
        assert_eq!(p.segments.len(), 3);
        assert_eq!(p.segments[0].kind, SegKind::Rapid);
        assert_eq!(p.segments[1].kind, SegKind::Feed);
        assert_eq!(p.segments[1].feed_mm_min, Some(100.0));
        assert_eq!(p.segments[1].line, 2);
        assert_eq!((p.segments[1].x1, p.segments[1].z1), (10.0, -20.0));
    }

    #[test]
    fn diameter_program_produces_radius_segments() {
        let p = parse("G0 X20 Z0"); // diameter mode default
        assert_eq!(p.segments[0].x1, 10.0);
    }

    #[test]
    fn arc_ik_form_geometry() {
        // Quarter arc: start (x=0,z=0), end (x=10,z=-10), centre at (x=0,z=-10).
        let p = parse("G8 G94 F100\nG0 X0 Z0\nG3 X10 Z-10 I0 K-10");
        let arcs: Vec<_> = p.segments.iter().filter(|s| s.kind == SegKind::Arc).collect();
        assert_eq!(arcs.len(), 1);
        let a = arcs[0];
        assert!((a.cx.unwrap() - 0.0).abs() < 1e-9);
        assert!((a.cz.unwrap() + 10.0).abs() < 1e-9);
        assert_eq!(a.ccw, Some(true));
    }

    #[test]
    fn arc_r_form_matches_ik_form() {
        let p_ik = parse("G8 G94 F100\nG0 X0 Z0\nG3 X10 Z-10 I0 K-10");
        let p_r = parse("G8 G94 F100\nG0 X0 Z0\nG3 X10 Z-10 R10");
        let a = &p_ik.segments[1];
        let b = &p_r.segments[1];
        assert!((a.cx.unwrap() - b.cx.unwrap()).abs() < 1e-6, "{:?} vs {:?}", a.cx, b.cx);
        assert!((a.cz.unwrap() - b.cz.unwrap()).abs() < 1e-6);
    }

    #[test]
    fn r_form_negative_r_takes_major_arc() {
        let minor = parse("G8 G94 F100\nG0 X0 Z0\nG3 X10 Z-10 R10");
        let major = parse("G8 G94 F100\nG0 X0 Z0\nG3 X10 Z-10 R-10");
        let cm = (minor.segments[1].cx.unwrap(), minor.segments[1].cz.unwrap());
        let cj = (major.segments[1].cx.unwrap(), major.segments[1].cz.unwrap());
        assert!(
            (cm.0 - cj.0).abs() > 1e-3 || (cm.1 - cj.1).abs() > 1e-3,
            "major and minor arcs must have different centres"
        );
        // Both radii are 10 from start point.
        for (cx, cz) in [cm, cj] {
            let r = (cx.powi(2) + cz.powi(2)).sqrt();
            assert!((r - 10.0).abs() < 1e-6);
        }
    }

    #[test]
    fn bounds_include_arc_extrema() {
        // Half arc from (x=0,z=0) to (x=0,z=-20), centre (0,-10), CCW.
        // The +x extreme of that arc is x=10 at z=-10 even though both
        // endpoints have x=0.
        let p = parse("G8 G94 F100\nG0 X0 Z0\nG3 X0 Z-20 I0 K-10");
        assert!((p.bounds.x_max - 10.0).abs() < 1e-9, "bounds {:?}", p.bounds);
        assert!((p.bounds.x_min - 0.0).abs() < 1e-9);
        assert!((p.bounds.z_min + 20.0).abs() < 1e-9);
    }

    #[test]
    fn bounds_do_not_include_unswept_extrema() {
        // Small 90-degree arc in one quadrant: no extreme beyond endpoints
        // except the swept one.
        let p = parse("G8 G94 F100\nG0 X0 Z0\nG3 X10 Z-10 I0 K-10");
        // Sweeps from +z extreme to +x extreme of circle centred (0,-10):
        // z never exceeds 0, x never exceeds 10.
        assert!(p.bounds.z_max <= 1e-9);
        assert!((p.bounds.x_max - 10.0).abs() < 1e-9);
    }

    #[test]
    fn thread_g33_emits_thread_segment() {
        let p = parse("G8\nG0 X5 Z2\nG33 Z-20 F1.5");
        let t: Vec<_> = p.segments.iter().filter(|s| s.kind == SegKind::Thread).collect();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].z1, -20.0);
    }

    #[test]
    fn g76_simplified_to_thread_with_warning() {
        let p = parse("G8\nG0 X8 Z2\nG76 X7.0 Z-15 F1.0");
        assert!(p.segments.iter().any(|s| s.kind == SegKind::Thread));
        assert!(p.diagnostics.iter().any(|d| d.message.contains("G76")));
    }

    #[test]
    fn segments_stop_after_m30() {
        let p = parse("G8 G94 F100\nG0 X5\nM30\nG0 X20");
        assert_eq!(p.segments.len(), 1);
    }

    #[test]
    fn tool_number_attached_to_segments() {
        let p = parse("T0101\nG8 G0 X5 Z1");
        assert_eq!(p.segments[0].tool, Some(1));
    }

    #[test]
    fn backplot_unions_stock_bounds() {
        let p = parse("G8 G0 X5 Z-10");
        let bp = backplot(
            &p,
            Some(StockSpec { diameter_mm: 40.0, length_mm: 60.0, z_face_mm: 0.0 }),
        );
        assert!((bp.bounds.x_max - 20.0).abs() < 1e-9);
        assert!((bp.bounds.z_min + 60.0).abs() < 1e-9);
    }

    #[test]
    fn segment_serializes_camel_case() {
        let p = parse("G8 G94 F100\nG0 X0 Z0\nG3 X10 Z-10 I0 K-10");
        let json = serde_json::to_string(&p.segments[1]).unwrap();
        assert!(json.contains("\"kind\":\"arc\""));
        assert!(json.contains("\"feedMmMin\""));
        assert!(json.contains("\"ccw\":true"));
        // Non-arc omits arc fields entirely.
        let json0 = serde_json::to_string(&p.segments[0]).unwrap();
        assert!(!json0.contains("ccw"));
    }

    #[test]
    fn point_along_arc_stays_on_radius() {
        let kind = MoveKind::Arc { cx: 0.0, cz: -10.0, ccw: true };
        for i in 0..=10 {
            let t = i as f64 / 10.0;
            let (x, z) = point_along(kind, 0.0, 0.0, 10.0, -10.0, t);
            let r = (x.powi(2) + (z + 10.0).powi(2)).sqrt();
            assert!((r - 10.0).abs() < 1e-9, "t={t} off circle: ({x},{z})");
        }
        // Endpoints exact.
        let (x, z) = point_along(kind, 0.0, 0.0, 10.0, -10.0, 1.0);
        assert!((x - 10.0).abs() < 1e-9 && (z + 10.0).abs() < 1e-9);
    }

    #[test]
    fn arc_length_quarter_circle() {
        let kind = MoveKind::Arc { cx: 0.0, cz: -10.0, ccw: true };
        let len = segment_length(kind, 0.0, 0.0, 10.0, -10.0);
        assert!((len - std::f64::consts::FRAC_PI_2 * 10.0).abs() < 1e-9);
    }
}
