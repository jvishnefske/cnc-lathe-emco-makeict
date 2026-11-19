//! The physical model on the ABI: channels, subscriptions and the outs of
//! [`crate::lathe::Lathe`] turned into publications.
//!
//! Inputs arrive on the machine channels (bound to the node's driver) and
//! on the unit's command keys (`app/unit/<node>/phase/<em>.<phase>` and
//! `.../motion/segments`, [`cnc_model::wire`]); reports go to the phases'
//! state keys and control-module states to `app/unit/<node>/cm`. A tick
//! every 100 ms sends a heartbeat of every report once a second.

use cnc_model::wire::{self, CmState, KeyText, PHASES};
use mw_payload::channel::machine::{Digital, Position, QueueState, Setpoint};
use mw_payload::channel::{Channel, Output};
use mw_payload::{Payload, Slot};

use crate::lathe::{Input, Lathe, Out, Outs};
use crate::machine::LIMITS;

mw_payload::channels! {
    x_pos: in position,
    z_pos: in position,
    x_home: in digital,
    z_home: in digital,
    spindle_rpm: out setpoint,
    spindle_actual: in setpoint,
    turret_station: out setpoint,
    turret_ready: in digital,
    estop: in digital,
    motion: out segments,
    queue: in segments,
}

/// The input channels, in [`Physical::decode`]'s order.
const INPUTS: [&str; 8] = [
    "x_pos",
    "z_pos",
    "x_home",
    "z_home",
    "spindle_actual",
    "turret_ready",
    "estop",
    "queue",
];

struct Physical {
    lathe: Lathe,
    inputs: [Option<Channel>; 8],
    motion: Option<Channel>,
    spindle: Option<Channel>,
    turret: Option<Channel>,
    commands: [Option<Slot>; 6],
    segments: Option<Slot>,
    state_keys: [Option<KeyText>; 6],
    cm_key: Option<KeyText>,
    info_key: Option<KeyText>,
    ticks: u32,
}

impl Physical {
    fn decode(i: usize, data: &[u8]) -> Option<Input> {
        let on = || Digital::decode(data).map(|d| d.on);
        Some(match i {
            0 => Input::XPos(Position::decode(data)?),
            1 => Input::ZPos(Position::decode(data)?),
            2 => Input::XHome(on()?),
            3 => Input::ZHome(on()?),
            4 => Input::SpindleActual(Setpoint::decode(data)?.value),
            5 => Input::TurretReady(on()?),
            6 => Input::EStop(on()?),
            _ => Input::Queue(QueueState::decode(data)?),
        })
    }

    fn apply(&mut self, outs: Outs) {
        for o in outs.iter() {
            let _ = match o {
                Out::Segment(s) => self.motion.as_mut().map(|c| c.publish(&s.encode())),
                Out::Spindle(rpm) => self
                    .spindle
                    .as_mut()
                    .map(|c| c.publish(&Setpoint { value: rpm }.encode())),
                Out::Turret(st) => self.turret.as_mut().map(|c| {
                    c.publish(
                        &Setpoint {
                            value: i32::from(st),
                        }
                        .encode(),
                    )
                }),
                Out::Report(i) => {
                    let (state, last, vals, n) = self.lathe.report(i);
                    let mut buf = [0u8; recipe_core::wire::MAX_BODY];
                    match (
                        self.state_keys[i],
                        recipe_core::wire::encode_report(state, last, &vals[..n], &mut buf),
                    ) {
                        (Some(k), Ok(body)) => Some(mw_payload::publish(k.as_str(), body)),
                        _ => None,
                    }
                }
                Out::Cm(i, s) => {
                    let mut buf = [0u8; 1 + CmState::MAX_LEN];
                    match (self.cm_key, wire::encode_cm(i, &s, &mut buf)) {
                        (Some(k), Some(n)) => Some(mw_payload::publish(k.as_str(), &buf[..n])),
                        _ => None,
                    }
                }
            };
        }
    }
}

impl Physical {
    /// The machine's info, then every report and state again.
    fn announce(&mut self) {
        if let Some(k) = self.info_key {
            let _ = mw_payload::publish(k.as_str(), &LIMITS.encode());
        }
        let beat = self.lathe.heartbeat();
        self.apply(beat);
    }
}

impl Payload for Physical {
    fn init() -> Self {
        let node = mw_payload::serve::node_id()
            .map(|n| n.0)
            .unwrap_or_default();
        let open = |name: &str| Channel::open(name).ok();
        let mut commands = [None; 6];
        let mut state_keys = [None; 6];
        for (i, (em, phase, _)) in PHASES.iter().enumerate() {
            commands[i] = wire::phase_key(node, em, phase)
                .and_then(|k| mw_payload::subscribe(k.as_str()).ok());
            state_keys[i] = wire::phase_state_key(node, em, phase);
        }
        let segments =
            wire::segments_key(node).and_then(|k| mw_payload::subscribe(k.as_str()).ok());
        let mut me = Physical {
            lathe: Lathe::new(LIMITS),
            inputs: INPUTS.map(open),
            motion: open("motion"),
            spindle: open("spindle_rpm"),
            turret: open("turret_station"),
            commands,
            segments,
            state_keys,
            cm_key: wire::cm_key(node),
            info_key: wire::info_key(node),
            ticks: 0,
        };
        let _ = mw_payload::set_tick(100);
        mw_payload::LogLine::new()
            .push_str("cnc-physical: ")
            .push_str(crate::machine::NAME)
            .log();
        me.announce();
        me
    }

    fn on_message(&mut self, slot: Slot, data: &[u8]) {
        if let Some(i) = self
            .inputs
            .iter()
            .position(|c| c.is_some_and(|c| c.is(slot)))
        {
            if let Some(input) = Self::decode(i, data) {
                let outs = self.lathe.input(input);
                self.apply(outs);
            }
        } else if let Some(i) = self.commands.iter().position(|s| *s == Some(slot)) {
            let outs = self.lathe.command(i, data);
            self.apply(outs);
        } else if self.segments == Some(slot) {
            let outs = self.lathe.segments(data);
            self.apply(outs);
        }
    }

    fn tick(&mut self, _now_ms: u32) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(10) {
            self.announce();
        }
    }
}

mw_payload::payload!(Physical);
