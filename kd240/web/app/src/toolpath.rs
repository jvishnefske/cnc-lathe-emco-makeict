//! The toolpath view: the backplot in the lathe's own convention.
//!
//! Z runs left to right with the chuck on the left (Z negative), X runs up
//! from the centreline, and the profile is mirrored below it, dimmed, so the
//! part reads as the turned shape it will be (as `kd240/ui/toolpath.js`
//! draws it). Segments are `sender/core` geometry in work coordinates, X
//! radius. Arcs are tessellated here, at a density from the current zoom.
//!
//! Drag pans, the wheel zooms about the pointer, a double click fits.

use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use emco_sender_core::geom::{Backplot, Bounds, SegKind, Segment};

/// The view's pan and zoom.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolpathView {
    /// World point (z, x) at the centre of the view, mm.
    pub center: (f64, f64),
    /// Pixels per mm.
    pub scale: f64,
    fit_pending: bool,
}

impl Default for ToolpathView {
    fn default() -> Self {
        ToolpathView {
            center: (-20.0, 0.0),
            scale: 4.0,
            fit_pending: true,
        }
    }
}

impl ToolpathView {
    pub fn request_fit(&mut self) {
        self.fit_pending = true;
    }

    /// World (z, x radius) to screen.
    pub fn to_screen(&self, rect: Rect, z: f64, x: f64) -> Pos2 {
        let c = rect.center();
        Pos2::new(
            c.x + ((z - self.center.0) * self.scale) as f32,
            c.y - ((x - self.center.1) * self.scale) as f32,
        )
    }

    /// Screen to world (z, x radius).
    pub fn to_world(&self, rect: Rect, p: Pos2) -> (f64, f64) {
        let c = rect.center();
        (
            self.center.0 + f64::from(p.x - c.x) / self.scale,
            self.center.1 - f64::from(p.y - c.y) / self.scale,
        )
    }

    /// Fits `bounds` (and its mirror below the centreline) into `rect`.
    pub fn fit(&mut self, rect: Rect, bounds: &Bounds) {
        let x_max = bounds.x_max.abs().max(bounds.x_min.abs()).max(1.0);
        let (z0, z1) = (bounds.z_min.min(0.0), bounds.z_max.max(0.0));
        let w = (z1 - z0).max(1.0) * 1.1;
        let h = 2.0 * x_max * 1.1;
        let sx = f64::from(rect.width()) / w;
        let sy = f64::from(rect.height()) / h;
        self.scale = sx.min(sy).clamp(0.05, 2000.0);
        self.center = ((z0 + z1) / 2.0, 0.0);
        self.fit_pending = false;
    }

    /// Zooms by `factor` keeping the world point under `pointer` still.
    pub fn zoom_about(&mut self, rect: Rect, pointer: Pos2, factor: f64) {
        let before = self.to_world(rect, pointer);
        self.scale = (self.scale * factor).clamp(0.05, 2000.0);
        let after = self.to_world(rect, pointer);
        self.center.0 += before.0 - after.0;
        self.center.1 += before.1 - after.1;
    }

    pub fn pan(&mut self, delta: Vec2) {
        self.center.0 -= f64::from(delta.x) / self.scale;
        self.center.1 += f64::from(delta.y) / self.scale;
    }
}

/// The points along a segment, in world (z, x), arcs tessellated so that a
/// chord is about `px_per_chord` pixels at `scale`.
pub fn segment_points(seg: &Segment, scale: f64, px_per_chord: f64) -> Vec<(f64, f64)> {
    let (Some(cx), Some(cz), Some(ccw)) = (seg.cx, seg.cz, seg.ccw) else {
        return vec![(seg.z0, seg.x0), (seg.z1, seg.x1)];
    };
    if seg.kind != SegKind::Arc {
        return vec![(seg.z0, seg.x0), (seg.z1, seg.x1)];
    }
    // The (z, x) plane as a maths plane: ccw (G03) is counterclockwise.
    let a0 = (seg.x0 - cx).atan2(seg.z0 - cz);
    let a1 = (seg.x1 - cx).atan2(seg.z1 - cz);
    let r = (seg.x0 - cx).hypot(seg.z0 - cz);
    let tau = std::f64::consts::TAU;
    let mut sweep = if ccw { a1 - a0 } else { a0 - a1 };
    sweep = sweep.rem_euclid(tau);
    if sweep < 1e-9 {
        sweep = tau; // start == end: a full circle
    }
    let sweep = if ccw { sweep } else { -sweep };
    let n = ((sweep.abs() * r * scale / px_per_chord).ceil() as usize).clamp(8, 512);
    (0..=n)
        .map(|i| {
            let a = a0 + sweep * i as f64 / n as f64;
            (cz + r * a.cos(), cx + r * a.sin())
        })
        .collect()
}

fn colour(kind: SegKind) -> Color32 {
    match kind {
        SegKind::Rapid => Color32::from_rgb(0xd0, 0x6a, 0x3a),
        SegKind::Feed => Color32::from_rgb(0x3f, 0x9f, 0xe0),
        SegKind::Arc => Color32::from_rgb(0x52, 0xc0, 0x8a),
        SegKind::Thread => Color32::from_rgb(0xc8, 0x6c, 0xd8),
    }
}

/// Draws the view. `tool` is the tool tip in work coordinates (z, x radius);
/// `current_line` is the 0-based line executing.
pub fn show(
    ui: &mut egui::Ui,
    view: &mut ToolpathView,
    backplot: Option<&Backplot>,
    tool: Option<(f64, f64)>,
    current_line: Option<usize>,
) {
    let size = ui.available_size().max(Vec2::new(200.0, 160.0));
    let (resp, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    let rect = resp.rect;
    let visuals = ui.visuals();
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);

    if let Some(bp) = backplot {
        if view.fit_pending && !bp.segments.is_empty() {
            view.fit(rect, &bp.bounds);
        }
    }
    if resp.dragged() {
        view.pan(resp.drag_delta());
    }
    if resp.double_clicked() {
        match backplot {
            Some(bp) if !bp.segments.is_empty() => view.fit(rect, &bp.bounds),
            _ => *view = ToolpathView::default(),
        }
    }
    if let Some(p) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            view.zoom_about(rect, p, f64::from((scroll / 200.0).exp()));
        }
    }

    let painter = painter.with_clip_rect(rect);
    let axis = Stroke::new(1.0, visuals.weak_text_color());
    // The centreline (X0) and the work face (Z0).
    let left = view.to_world(rect, rect.left_center()).0;
    let right = view.to_world(rect, rect.right_center()).0;
    let y0 = view.to_screen(rect, 0.0, 0.0);
    painter.add(Shape::dashed_line(
        &[Pos2::new(rect.left(), y0.y), Pos2::new(rect.right(), y0.y)],
        axis,
        8.0,
        4.0,
    ));
    if (left..=right).contains(&0.0) {
        painter.line_segment(
            [Pos2::new(y0.x, rect.top()), Pos2::new(y0.x, rect.bottom())],
            axis,
        );
    }

    if let Some(bp) = backplot {
        for mirrored in [true, false] {
            for seg in &bp.segments {
                let pts: Vec<Pos2> = segment_points(seg, view.scale, 4.0)
                    .into_iter()
                    .map(|(z, x)| view.to_screen(rect, z, if mirrored { -x } else { x }))
                    .collect();
                let hot = current_line == Some(seg.line as usize) && !mirrored;
                let mut c = colour(seg.kind);
                if mirrored {
                    c = c.gamma_multiply(0.3);
                }
                let width = if hot { 3.0 } else { 1.5 };
                let stroke = Stroke::new(width, if hot { Color32::YELLOW } else { c });
                if seg.kind == SegKind::Rapid {
                    painter.extend(Shape::dashed_line(&pts, stroke, 5.0, 4.0));
                } else {
                    painter.add(Shape::line(pts, stroke));
                }
            }
        }
    }

    if let Some((z, x)) = tool {
        let p = view.to_screen(rect, z, x);
        // A tool tip pointing at the work: the cutter comes from +X.
        let s = 9.0;
        painter.add(Shape::convex_polygon(
            vec![
                p,
                Pos2::new(p.x - s * 0.6, p.y - s * 1.6),
                Pos2::new(p.x + s * 0.6, p.y - s * 1.6),
            ],
            Color32::from_rgba_unmultiplied(0xff, 0xd0, 0x40, 0xd0),
            Stroke::new(1.0, Color32::BLACK),
        ));
    }

    let hint = "drag: pan · wheel: zoom · double-click: fit · chuck ←  Z  → tailstock";
    painter.text(
        rect.left_bottom() + Vec2::new(6.0, -6.0),
        egui::Align2::LEFT_BOTTOM,
        hint,
        egui::FontId::proportional(11.0),
        visuals.weak_text_color(),
    );
    if let Some(p) = resp.hover_pos() {
        let (z, x) = view.to_world(rect, p);
        painter.text(
            rect.right_top() + Vec2::new(-6.0, 6.0),
            egui::Align2::RIGHT_TOP,
            format!("Z {z:.3}  X Ø {:.3}", 2.0 * x),
            egui::FontId::monospace(11.0),
            visuals.text_color(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 200.0))
    }

    #[test]
    fn chuck_is_left_and_x_is_up() {
        let v = ToolpathView {
            center: (0.0, 0.0),
            scale: 2.0,
            fit_pending: false,
        };
        let o = v.to_screen(rect(), 0.0, 0.0);
        assert_eq!(o, Pos2::new(200.0, 100.0));
        assert!(
            v.to_screen(rect(), -10.0, 0.0).x < o.x,
            "Z- toward the chuck, on the left"
        );
        assert!(v.to_screen(rect(), 0.0, 10.0).y < o.y, "X+ up");
        let (z, x) = v.to_world(rect(), v.to_screen(rect(), -7.5, 3.25));
        assert!((z + 7.5).abs() < 1e-4 && (x - 3.25).abs() < 1e-4);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let mut v = ToolpathView::default();
        let p = Pos2::new(50.0, 40.0);
        let before = v.to_world(rect(), p);
        v.zoom_about(rect(), p, 3.0);
        let after = v.to_world(rect(), p);
        assert!((before.0 - after.0).abs() < 1e-6 && (before.1 - after.1).abs() < 1e-6);
    }

    #[test]
    fn fit_shows_the_part_and_its_mirror() {
        let mut v = ToolpathView::default();
        v.fit(
            rect(),
            &Bounds {
                x_min: 0.0,
                x_max: 10.0,
                z_min: -40.0,
                z_max: 2.0,
            },
        );
        for (z, x) in [(-40.0, 10.0), (2.0, -10.0), (0.0, 0.0)] {
            assert!(rect().contains(v.to_screen(rect(), z, x)), "({z}, {x})");
        }
    }

    #[test]
    fn arcs_tessellate_the_right_way_round() {
        // A G03 quarter from (z=0, x=5) to (z=-5, x=0) about (0, 0):
        // counterclockwise in the (z, x) plane goes through (-3.5, 3.5).
        let seg = Segment {
            kind: SegKind::Arc,
            x0: 5.0,
            z0: 0.0,
            x1: 0.0,
            z1: -5.0,
            cx: Some(0.0),
            cz: Some(0.0),
            ccw: Some(true),
            line: 0,
            feed_mm_min: None,
            tool: None,
        };
        let pts = segment_points(&seg, 10.0, 4.0);
        let mid = pts[pts.len() / 2];
        assert!(mid.0 < -3.0 && mid.1 > 3.0, "{mid:?}");
        let (z, x) = *pts.last().unwrap();
        assert!((z + 5.0).abs() < 1e-9 && x.abs() < 1e-9);
        // The same ends clockwise go the long way, through (5, 0)... (z, x) = (0, -5).
        let cw = Segment {
            ccw: Some(false),
            ..seg
        };
        let pts = segment_points(&cw, 10.0, 4.0);
        assert!(pts.iter().any(|(_, x)| *x < -4.9));
    }
}
