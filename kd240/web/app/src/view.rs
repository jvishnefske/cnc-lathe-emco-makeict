//! Drawing the screen, every frame, from [`Screen`]: the layout of
//! `emco_lathe_handler.py`'s `_build`, in egui.
//!
//! Left: the toolpath over a tab set (Program, Preflight, MDI, Touch-off,
//! Programs). Right: everything an operator watches while the machine
//! moves (machine strip, DRO, belt and spindle, jog, program run box). The
//! log runs along the bottom.

use crate::link::*;
use crate::screen::{Prefs, Screen, Tab};
use crate::toolpath;
use egui::{Align, Color32, RichText, Ui};
use emco_rules::screen::{self as handler, Procedure, Wizard};
use emco_rules::{belt_range, Belt, Severity};

/// Choosing a local file to upload (a browser file picker on wasm32).
pub trait FilePicker {
    fn available(&self) -> bool;
    /// Opens the picker; the file arrives later through [`FilePicker::take`].
    fn open(&mut self);
    /// `(file name, text)` of a picked file, once.
    fn take(&mut self) -> Option<(String, String)>;
}

/// No file picker.
pub struct NoFiles;

impl FilePicker for NoFiles {
    fn available(&self) -> bool {
        false
    }
    fn open(&mut self) {}
    fn take(&mut self) -> Option<(String, String)> {
        None
    }
}

const RED: Color32 = Color32::from_rgb(0xef, 0x53, 0x50);
const AMBER: Color32 = Color32::from_rgb(0xff, 0xb3, 0x00);
const GREEN: Color32 = Color32::from_rgb(0x43, 0xa0, 0x47);

pub const SAFETY: &str = "Training simulator. Nothing here moves the real lathe: the machine is sender/core's \
virtual machine, and the lathe's RS-485 protocol is unknown. Abort is advisory, not a safety function: the stop \
is the cabinet's EMERGENCY-OFF and mains switch, with a person at the machine. Never run it unattended or remotely.";

pub const ABORT_NOTE: &str =
    "Advisory only, not a safety function. The stop is EMERGENCY-OFF and the mains \
switch at the machine.";

fn severity_colour(s: Severity) -> Color32 {
    match s {
        Severity::Error => RED,
        Severity::Warning => AMBER,
    }
}

/// A file name from the browser, made into a program name the service takes.
pub fn program_name_from_file(file: &str) -> String {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let mut s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("..") {
        s = s.replace("..", ".");
    }
    let s = s.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    s.chars().take(64).collect()
}

/// One frame.
pub fn show(
    ctx: &egui::Context,
    s: &mut Screen,
    link: &mut dyn MachineLink,
    prefs: &mut dyn Prefs,
    files: &mut dyn FilePicker,
) {
    if let Some((file, text)) = files.take() {
        s.editor_name = program_name_from_file(&file);
        s.editor_text = text;
        s.tab = Tab::Programs;
    }

    egui::TopBottomPanel::top("top").show(ctx, |ui| top_bar(ui, s, link, prefs));
    egui::TopBottomPanel::bottom("log")
        .resizable(true)
        .default_height(120.0)
        .show(ctx, |ui| log_pane(ui, s));
    egui::SidePanel::right("right")
        .resizable(true)
        .default_width(430.0)
        .min_width(360.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("right-scroll")
                .show(ui, |ui| {
                    machine_strip(ui, s, link);
                    ui.add_space(4.0);
                    dro_box(ui, s);
                    ui.add_space(4.0);
                    spindle_box(ui, s, link, prefs);
                    ui.add_space(4.0);
                    jog_box(ui, s, link);
                    ui.add_space(4.0);
                    run_box(ui, s, link);
                });
        });
    egui::CentralPanel::default().show(ctx, |ui| {
        let h = ui.available_height();
        let tool = s.status.as_ref().map(|st| (st.pos.z_work, st.pos.x_work));
        let line = s.status.as_ref().and_then(|st| st.current_line);
        ui.allocate_ui(
            egui::vec2(ui.available_width(), (h * 0.5).max(180.0)),
            |ui| {
                let bp = s.program.as_ref().and_then(|p| p.backplot.as_ref());
                toolpath::show(ui, &mut s.toolpath, bp, tool, line);
            },
        );
        ui.separator();
        tabs(ui, s, link, files);
    });
    belt_question(ctx, s, prefs);
}

fn top_bar(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink, prefs: &mut dyn Prefs) {
    ui.horizontal_wrapped(|ui| {
        ui.heading("EMCO PC Turn 50 — simulator");
        ui.separator();
        let (text, colour) = match &s.link_state {
            LinkState::Connected => ("link: connected".to_string(), GREEN),
            LinkState::Connecting => ("link: connecting…".to_string(), AMBER),
            LinkState::Lost(why) => (format!("link: {why}"), RED),
        };
        ui.label(RichText::new(text).color(colour));
        if let Some(st) = &s.status {
            ui.label(format!("transport: {}", st.transport));
        }
        ui.separator();
        ui.label("Token");
        let r = ui.add(
            egui::TextEdit::singleline(&mut s.token_input)
                .password(true)
                .desired_width(160.0)
                .hint_text("bearer token for changes"),
        );
        if ui
            .button(if s.token_input.trim().is_empty() {
                "Clear"
            } else {
                "Use"
            })
            .clicked()
            || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
        {
            s.apply_token(link, prefs);
        }
        ui.label(if s.token_set {
            "(set)"
        } else {
            "(none: reads only)"
        });
    });
    ui.label(RichText::new(SAFETY).small().color(AMBER));
}

fn machine_strip(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink) {
    ui.group(|ui| {
        let ok = s.referenced();
        let banner = handler::banner(s.gate_reason).to_string();
        let colour = if ok { GREEN } else { RED };
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(banner).strong().size(17.0).color(colour));
        });
        if s.status.as_ref().is_some_and(|st| st.estop) && s.caps.abort_resets {
            ui.label("On this machine link, Abort (advisory) is the reset.");
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(s.machine_on(), egui::Button::new("Home All"))
                .on_disabled_hover_text("The machine is off or in E-stop")
                .clicked()
            {
                s.home(link);
            }
            let homed = s.status.as_ref().is_some_and(|st| st.all_homed());
            ui.label(if homed {
                "machine flags: homed"
            } else {
                "machine flags: not homed"
            });
        });
        if s.caps.training_faults {
            ui.horizontal_wrapped(|ui| {
                ui.label("Training faults:");
                for (kind, label) in [
                    ("estop", "E-stop"),
                    ("limit_x", "Limit X"),
                    ("limit_z", "Limit Z"),
                    ("turret_jam", "Turret jam"),
                ] {
                    if ui
                        .add_enabled(
                            s.link_state == LinkState::Connected,
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        s.fault(kind, link);
                    }
                }
            });
        }
    });
}

fn dro_box(ui: &mut Ui, s: &Screen) {
    ui.group(|ui| {
        ui.label(RichText::new("Position (work / machine)").strong());
        let Some(st) = &s.status else {
            ui.label("No status yet.");
            return;
        };
        let dia = st.diameter_mode;
        let big = |v: f64| RichText::new(format!("{v:9.3}")).monospace().size(20.0);
        let small = |v: f64| RichText::new(format!("{v:9.3}")).monospace();
        egui::Grid::new("dro").num_columns(3).spacing([12.0, 2.0]).show(ui, |ui| {
            let k = if dia { 2.0 } else { 1.0 };
            ui.label(RichText::new(if dia { "X Ø" } else { "X r" }).size(18.0));
            ui.label(big(st.pos.x_work * k));
            ui.label(big(st.pos.x * k));
            ui.end_row();
            // Radius beneath the diameter: the machine moves in radius,
            // drawings are in diameter, and confusing the two is doc 50 §2's
            // whole point.
            ui.label("X radius");
            ui.label(small(st.pos.x_work));
            ui.label(small(st.pos.x));
            ui.end_row();
            ui.label(RichText::new("Z").size(18.0));
            ui.label(big(st.pos.z_work));
            ui.label(big(st.pos.z));
            ui.end_row();
            ui.label("to go X r / Z");
            ui.label(small(st.pos.x_dtg));
            ui.label(small(st.pos.z_dtg));
            ui.end_row();
            let (ox, oz) = st.pos.work_offset();
            ui.label("G54 offset X r / Z");
            ui.label(small(ox));
            ui.label(small(oz));
            ui.end_row();
            if let Some((sx, sz)) = st.sim {
                ui.label("simulator X r / Z").on_hover_text(
                    "Where the simulated carriage really is. It differs from the DRO after a stall.",
                );
                ui.label(small(sx));
                ui.label(small(sz));
                ui.end_row();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Tool T{}", st.tool));
            ui.separator();
            ui.label(if dia { "G7 diameter" } else { "G8 radius" });
            if !st.diameter_mode_known {
                ui.label(RichText::new("(assumed)").weak())
                    .on_hover_text("No block has said G7 or G8 since this screen connected; G7 is the default.");
            }
            ui.separator();
            ui.label(format!("F {:.1} mm/min", st.feed_mm_min));
        });
        ui.label(RichText::new(st.modal.join(" ")).monospace());
        if !st.alarms.is_empty() {
            for a in &st.alarms {
                ui.label(RichText::new(format!("ALARM {}: {}", a.code, a.message)).color(RED));
            }
        }
    });
}

fn spindle_box(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink, prefs: &mut dyn Prefs) {
    ui.group(|ui| {
        ui.label(RichText::new("Belt and spindle").strong());
        ui.horizontal(|ui| {
            for b in Belt::all() {
                let (lo, hi) = belt_range(b);
                if ui
                    .radio(s.belt == b, format!("Belt {}\n{lo}–{hi}", b.number()))
                    .clicked()
                    && s.belt != b
                {
                    s.set_belt(b, true, prefs);
                }
            }
        });
        let (lo, hi) = belt_range(s.belt);
        let ok = s.referenced();
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut s.rpm)
                    .range(f64::from(lo)..=f64::from(hi))
                    .speed(50.0)
                    .suffix(" rpm"),
            );
            if ui.add_enabled(ok, egui::Button::new("M3 FWD")).clicked() {
                s.spindle(1, link);
            }
            if ui.add_enabled(ok, egui::Button::new("M4 REV")).clicked() {
                s.spindle(-1, link);
            }
            if ui.button("M5 STOP").clicked() {
                s.spindle_stop(link);
            }
        });
        if let Some(st) = &s.status {
            let mut text = format!("S {:.0} rpm", st.spindle_rpm);
            if st.threading {
                text.push_str(&format!(
                    "  · threading (limit {} rpm)",
                    emco_rules::THREADING_MAX_RPM
                ));
            }
            ui.label(text);
        }
    });
}

fn jog_box(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink) {
    ui.group(|ui| {
        ui.label(RichText::new("Jog").strong());
        let ok = s.referenced() && s.machine_on();
        ui.horizontal(|ui| {
            // On a lathe X- moves toward the centreline, Z- toward the chuck.
            for (text, axis, dir) in [
                ("X−", Axis::X, -1.0),
                ("X+", Axis::X, 1.0),
                ("Z−", Axis::Z, -1.0),
                ("Z+", Axis::Z, 1.0),
            ] {
                if ui
                    .add_enabled(
                        ok,
                        egui::Button::new(RichText::new(text).size(16.0))
                            .min_size(egui::vec2(52.0, 30.0)),
                    )
                    .clicked()
                {
                    s.jog(axis, dir, link);
                }
            }
        });
        ui.horizontal(|ui| {
            let current = handler::JOG_INCREMENTS[s.jog_increment].0;
            egui::ComboBox::from_id_salt("jog-incr")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, (label, mm)) in handler::JOG_INCREMENTS.iter().enumerate() {
                        let enabled = *mm != 0.0 || s.caps.continuous_jog;
                        ui.add_enabled_ui(enabled, |ui| {
                            ui.selectable_value(&mut s.jog_increment, i, *label)
                                .on_disabled_hover_text(
                                    "This machine link jogs by increments only",
                                );
                        });
                    }
                });
            let max = (s.profile.max_feed_mm_min / 60.0).max(0.05);
            ui.add(
                egui::DragValue::new(&mut s.jog_rate_mm_s)
                    .range(0.05..=max)
                    .speed(0.05)
                    .fixed_decimals(2)
                    .suffix(" mm/s"),
            );
            ui.label("X in radius");
        });
    });
}

fn run_box(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink) {
    ui.group(|ui| {
        ui.label(RichText::new("Program").strong());
        if let Some(st) = &s.status {
            let line = st.current_line.map(|l| format!("line {}", l + 1)).unwrap_or_else(|| "—".into());
            ui.label(RichText::new(format!("{}   {line}", st.run.name().to_uppercase())).strong().size(15.0));
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(s.start_enabled(), egui::Button::new(RichText::new("Cycle start").strong()))
                .clicked()
            {
                s.cycle_start(link);
            }
            if ui.button("Pause / resume").clicked() {
                s.pause_resume(link);
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(RichText::new("Abort (advisory)").strong()).fill(Color32::from_rgb(0x8e, 0x24, 0x24)))
                .on_hover_text(ABORT_NOTE)
                .clicked()
            {
                s.abort(link);
            }
            ui.label(RichText::new(ABORT_NOTE).small());
        });
        ui.horizontal(|ui| {
            let forced = s.single_block_forced();
            let mut sb = s.single_block_effective();
            if ui
                .add_enabled(!forced, egui::Checkbox::new(&mut sb, "Single block"))
                .on_disabled_hover_text("An unproven program's first run is single block")
                .changed()
            {
                s.single_block = sb;
                link.send(Request::SingleBlock(sb));
            }
            let mut dry = s.dry_run;
            if ui.checkbox(&mut dry, "Dry run").changed() {
                s.set_dry_run(dry, link);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Feed override");
            if s.caps.feed_override {
                ui.label("100%");
            } else {
                ui.label(RichText::new("not available on this machine link").weak()).on_hover_text(
                    "The proving ladder's 25% first run cannot be applied: the first run is single block at the \
                     programmed feed.",
                );
            }
        });
        ui.label(s.proven_label());
    });
}

fn tabs(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink, files: &mut dyn FilePicker) {
    let findings = s.program.as_ref().map(|p| p.findings.len()).unwrap_or(0);
    ui.horizontal(|ui| {
        ui.selectable_value(&mut s.tab, Tab::Program, "Program");
        let pf = if findings > 0 {
            format!("Preflight ({findings})")
        } else {
            "Preflight".into()
        };
        ui.selectable_value(&mut s.tab, Tab::Preflight, pf);
        ui.selectable_value(&mut s.tab, Tab::Mdi, "MDI");
        ui.selectable_value(&mut s.tab, Tab::Touchoff, "Touch-off");
        ui.selectable_value(&mut s.tab, Tab::Programs, "Programs");
    });
    ui.separator();
    match s.tab {
        Tab::Program => program_pane(ui, s),
        Tab::Preflight => preflight_pane(ui, s),
        Tab::Mdi => mdi_pane(ui, s, link),
        Tab::Touchoff => touchoff_pane(ui, s, link),
        Tab::Programs => programs_pane(ui, s, link, files),
    }
}

fn program_pane(ui: &mut Ui, s: &mut Screen) {
    let Some(p) = &s.program else {
        ui.label("No program loaded. Load one from the Programs tab.");
        return;
    };
    let current = s.status.as_ref().and_then(|st| st.current_line);
    if current.is_some() && current != s.followed_line {
        s.followed_line = current;
        s.goto_line = current;
    }
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    let mut area = egui::ScrollArea::both()
        .id_salt("program")
        .auto_shrink([false, false]);
    if let Some(line) = s.goto_line.take() {
        let offset = (line as f32 * row_h - ui.available_height() / 2.0).max(0.0);
        area = area.vertical_scroll_offset(offset);
    }
    let error_lines: std::collections::BTreeMap<usize, Severity> = p
        .findings
        .iter()
        .map(|f| (f.line.saturating_sub(1), f.severity))
        .chain(p.machine_diagnostics.iter().map(|d| {
            (
                d.line,
                if d.error {
                    Severity::Error
                } else {
                    Severity::Warning
                },
            )
        }))
        .collect();
    area.show_rows(ui, row_h, p.lines.len(), |ui, range| {
        for i in range {
            let text = format!("{:5}  {}", i + 1, p.lines[i]);
            let mut rt = RichText::new(text).monospace();
            if let Some(sev) = error_lines.get(&i) {
                rt = rt.color(severity_colour(*sev));
            }
            if current == Some(i) {
                rt = rt
                    .background_color(Color32::from_rgb(0x55, 0x4a, 0x10))
                    .strong();
            }
            ui.label(rt);
        }
    });
}

fn preflight_pane(ui: &mut Ui, s: &mut Screen) {
    let Some(p) = &s.program else {
        ui.label("No program loaded.");
        return;
    };
    if p.text.is_none() {
        ui.label("Reading the program back from the machine…");
        return;
    }
    let mut goto = None;
    egui::ScrollArea::vertical()
        .id_salt("preflight")
        .max_height(ui.available_height() - 60.0)
        .show(ui, |ui| {
            if !p.loaded {
                ui.label(
                    RichText::new(
                        "The machine's own parser refused this program: it is not loaded.",
                    )
                    .color(RED),
                );
            }
            for d in &p.machine_diagnostics {
                let sev = if d.error {
                    Severity::Error
                } else {
                    Severity::Warning
                };
                let t = format!(
                    "MACHINE {} line {}: {}",
                    sev.as_str().to_uppercase(),
                    d.line + 1,
                    d.message
                );
                if ui
                    .link(RichText::new(t).color(severity_colour(sev)))
                    .clicked()
                {
                    goto = Some(d.line);
                }
            }
            if p.findings.is_empty() {
                ui.label(handler::PREFLIGHT_CLEAN);
            }
            for f in &p.findings {
                let t = format!(
                    "{} line {}: {}",
                    f.severity.as_str().to_uppercase(),
                    f.line,
                    f.message
                );
                if ui
                    .link(RichText::new(t).color(severity_colour(f.severity)))
                    .clicked()
                {
                    goto = Some(f.line.saturating_sub(1));
                }
            }
        });
    if let Some(line) = goto {
        s.goto_line = Some(line);
        s.tab = Tab::Program;
    }
    ui.separator();
    let mut ov = s.ladder.preflight_override();
    if ui.checkbox(&mut ov, handler::OVERRIDE_LABEL).changed() {
        s.set_override(ov);
    }
}

fn mdi_pane(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink) {
    let ok = s.referenced();
    let r = ui.add_enabled(
        ok,
        egui::TextEdit::singleline(&mut s.mdi)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .hint_text("e.g. G53 G0 X96"),
    );
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        s.mdi_submit(link);
        r.request_focus();
    }
    ui.label(handler::mdi_hint(&s.machine));
    if !ok {
        ui.label(RichText::new(handler::banner(s.gate_reason)).color(RED));
    }
    egui::ScrollArea::vertical()
        .id_salt("mdi-history")
        .show(ui, |ui| {
            for h in s.mdi_history.iter().rev().take(30) {
                ui.label(RichText::new(h).monospace().weak());
            }
        });
}

fn touchoff_pane(ui: &mut Ui, s: &mut Screen, link: &mut dyn MachineLink) {
    ui.horizontal(|ui| {
        if ui.button("Work Z0 (§9.1)").clicked() {
            s.wizard = Wizard::start(Procedure::WorkZ);
        }
        if ui.button("Tool offsets (§9.2)").clicked() {
            s.wizard = Wizard::start(Procedure::ToolOffsets);
        }
    });
    let step = s.wizard.current();
    ui.label(RichText::new(step.title).strong().size(16.0));
    ui.label(step.text);
    ui.add_space(6.0);
    if s.wizard.wants_diameter() {
        ui.horizontal(|ui| {
            ui.label("Measured Ø");
            ui.add(
                egui::DragValue::new(&mut s.measured_diameter)
                    .range(0.0..=2.0 * s.machine.x_retract)
                    .speed(0.01)
                    .fixed_decimals(3)
                    .suffix(" mm"),
            );
        });
    }
    if let Some((label, action)) = step.button {
        let tool_step = matches!(
            action,
            emco_rules::screen::WizardAction::ToolX | emco_rules::screen::WizardAction::ToolZ
        );
        let enabled = s.referenced() && (!tool_step || s.caps.tool_table);
        let r = ui.add_enabled(enabled, egui::Button::new(label));
        let r = if tool_step && !s.caps.tool_table {
            r.on_disabled_hover_text("This machine link has no tool table (G10 L10 / G43)")
        } else {
            r.on_disabled_hover_text(handler::banner(s.gate_reason).to_string())
        };
        if r.clicked() {
            s.wizard_do(link);
        }
        if tool_step && !s.caps.tool_table {
            ui.label(
                RichText::new(
                    "Not available here: sender/core's virtual machine has no tool table, so a G10 L10 would \
                     be ignored. Practise this step in the LinuxCNC simulator (emco_lathe).",
                )
                .color(AMBER),
            );
        }
    }
    ui.horizontal(|ui| {
        if ui
            .add_enabled(s.wizard.can_go_back(), egui::Button::new("◀ Back"))
            .clicked()
        {
            s.wizard.back();
        }
        if ui
            .add_enabled(s.wizard.can_go_next(), egui::Button::new("Next ▶"))
            .clicked()
        {
            s.wizard.next();
        }
    });
}

fn programs_pane(
    ui: &mut Ui,
    s: &mut Screen,
    link: &mut dyn MachineLink,
    files: &mut dyn FilePicker,
) {
    ui.columns(2, |cols| {
        let ui = &mut cols[0];
        ui.horizontal(|ui| {
            ui.label(RichText::new("On the machine").strong());
            if ui.button("Refresh").clicked() {
                link.send(Request::ListPrograms);
            }
        });
        let busy = s.status.as_ref().is_some_and(|st| st.run.busy());
        let mut action: Option<(u8, String)> = None;
        egui::ScrollArea::vertical()
            .id_salt("programs")
            .show(ui, |ui| {
                if s.programs.is_empty() {
                    ui.label("No programs stored.");
                }
                for p in &s.programs {
                    ui.horizontal(|ui| {
                        let loaded = s.program.as_ref().is_some_and(|l| l.name == p.name);
                        ui.label(RichText::new(&p.name).monospace().strong_if(loaded));
                        ui.label(RichText::new(format!("{} B", p.bytes)).weak());
                        if ui
                            .add_enabled(!busy, egui::Button::new("Load"))
                            .on_disabled_hover_text("Abort or finish first")
                            .clicked()
                        {
                            action = Some((0, p.name.clone()));
                        }
                        if ui.button("Edit").clicked() {
                            action = Some((1, p.name.clone()));
                        }
                        if ui.button("Delete").clicked() {
                            action = Some((2, p.name.clone()));
                        }
                    });
                }
            });
        match action {
            Some((0, n)) => s.load(&n, link),
            Some((1, n)) => s.edit(&n, link),
            Some((2, n)) => s.delete(&n, link),
            _ => {}
        }

        let ui = &mut cols[1];
        ui.label(RichText::new("Upload / edit").strong());
        ui.horizontal(|ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut s.editor_name)
                    .desired_width(160.0)
                    .hint_text("part.nc"),
            );
            if files.available() && ui.button("Open file…").clicked() {
                files.open();
            }
            if ui.button("Save to machine").clicked() {
                s.save_editor(link);
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("editor")
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut s.editor_text)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(14),
                );
            });
    });
}

trait StrongIf {
    fn strong_if(self, b: bool) -> Self;
}

impl StrongIf for RichText {
    fn strong_if(self, b: bool) -> Self {
        if b {
            self.strong().underline()
        } else {
            self
        }
    }
}

fn log_pane(ui: &mut Ui, s: &mut Screen) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Messages").strong());
        ui.checkbox(&mut s.show_sent, "show sent blocks");
        if ui.button("Clear").clicked() {
            s.log.clear();
        }
    });
    egui::ScrollArea::vertical()
        .id_salt("log")
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for l in &s.log {
                ui.label(RichText::new(l).monospace().size(12.0));
            }
            ui.scroll_to_cursor(Some(Align::BOTTOM));
        });
}

fn belt_question(ctx: &egui::Context, s: &mut Screen, prefs: &mut dyn Prefs) {
    if !s.ask_belt {
        return;
    }
    egui::Window::new("Which belt is fitted?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                "Tell the control which belt position is fitted (doc 40 §6).\n\
                 The real control uses it for speed monitoring and threading.",
            );
            for (belt, label) in handler::belt_choices() {
                let text = if belt == s.belt {
                    format!("{label}  (current)")
                } else {
                    label
                };
                if ui.button(text).clicked() {
                    s.set_belt(belt, true, prefs);
                    s.ask_belt = false;
                }
            }
            if ui.button("Cancel").clicked() {
                s.ask_belt = false;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kd240::CAPABILITIES;
    use crate::screen::tests::{status, MockLink};
    use crate::screen::MemoryPrefs;

    #[test]
    fn file_names_become_program_names() {
        assert_eq!(
            program_name_from_file("C:\\parts\\my part (2).ngc"),
            "my_part__2_.ngc"
        );
        assert_eq!(program_name_from_file("../..x.nc"), "x.nc");
        assert!(crate::kd240::valid_program_name(&program_name_from_file(
            "Ünïcode.nc"
        )));
    }

    /// The whole screen draws headless, in every tab, with and without
    /// status and a program, without panicking.
    #[test]
    fn every_tab_draws() {
        let ctx = egui::Context::default();
        let mut prefs = MemoryPrefs::default();
        let mut s = Screen::new(&prefs, CAPABILITIES);
        let mut link = MockLink::default();
        let frame = |s: &mut Screen, link: &mut MockLink, prefs: &mut MemoryPrefs| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| show(ctx, s, link, prefs, &mut NoFiles));
        };
        frame(&mut s, &mut link, &mut prefs);
        s.apply(Incoming::Link(LinkState::Connected), &mut link);
        s.apply(
            Incoming::Status(Box::new(status(RunState::Idle, false, true))),
            &mut link,
        );
        s.apply(
            Incoming::Reply(Reply::Loaded {
                name: "p.nc".into(),
                loaded: true,
                diagnostics: vec![],
            }),
            &mut link,
        );
        s.apply(
            Incoming::Reply(Reply::ProgramText {
                name: "p.nc".into(),
                text: "G21 G7\nG53 G0 X96\nG0 X20 Z1\nG1 Z-10 F100\nG2 X30 Z-15 R5\nG53 G0 X96 Z0\nM30\n".into(),
            }),
            &mut link,
        );
        assert!(s.ask_belt);
        for tab in [
            Tab::Program,
            Tab::Preflight,
            Tab::Mdi,
            Tab::Touchoff,
            Tab::Programs,
        ] {
            s.tab = tab;
            frame(&mut s, &mut link, &mut prefs);
        }
    }
}
