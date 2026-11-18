//! The board's update policy, as pure functions with no I/O and no clock.
//!
//! The rules:
//!
//! 1. An order is accepted only if it verified against a trusted key (done
//!    before this module), names this device, has a sequence number above
//!    the last one accepted, has not expired, and carries a well-formed
//!    bundle for this device class ([`accept_order`]).
//! 2. Downloading and staging are always allowed: they write only the
//!    inactive slot, which nothing running uses. Rebooting is allowed only
//!    while the lathe service reports `run` = `idle` or `disconnected`;
//!    anything else, including "could not ask", defers it ([`interlock`]).
//!    The agent asks again immediately before it acts.
//! 3. The only slot ever written is the one this boot did not come from,
//!    and only while this boot is the committed one ([`stage_target`]): a
//!    trial boot never overwrites the slot it would fall back to.
//! 4. A trial boot commits itself once healthy ([`on_boot`]); one that
//!    never got there is reported as rolled back ([`trial_outcome`]).

use crate::bootstate::{BootState, Slot};
use crate::msg::Order;

// ------------------------------------------------------------ the interlock

/// The lathe service's `session.run` (`sender/core` `RunState`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunState {
    Disconnected,
    Idle,
    Running,
    Hold,
    Alarm,
    Homing,
    Toolchange,
    /// A value this agent does not know. Treated as busy.
    Other(String),
}

impl RunState {
    pub fn parse(s: &str) -> RunState {
        match s {
            "disconnected" => RunState::Disconnected,
            "idle" => RunState::Idle,
            "running" => RunState::Running,
            "hold" => RunState::Hold,
            "alarm" => RunState::Alarm,
            "homing" => RunState::Homing,
            "toolchange" => RunState::Toolchange,
            other => RunState::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            RunState::Disconnected => "disconnected",
            RunState::Idle => "idle",
            RunState::Running => "running",
            RunState::Hold => "hold",
            RunState::Alarm => "alarm",
            RunState::Homing => "homing",
            RunState::Toolchange => "toolchange",
            RunState::Other(s) => s,
        }
    }
}

/// `session.run` out of the service's `GET /api/state` body.
pub fn run_state_from_api(body: &serde_json::Value) -> Result<RunState, String> {
    body.get("session")
        .and_then(|s| s.get("run"))
        .and_then(|r| r.as_str())
        .map(RunState::parse)
        .ok_or_else(|| "no session.run in /api/state".to_string())
}

/// What the agent is about to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Download a bundle.
    Fetch,
    /// Write and check the inactive slot; nothing running uses it.
    Stage,
    /// Reboot now (into a new slot, or not).
    Reboot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    Proceed,
    /// Not now, and why. Never forced.
    Defer(String),
}

/// May `step` go ahead, given what the lathe service said (or why it could
/// not be asked)?
pub fn interlock(step: Step, local: &Result<RunState, String>) -> Gate {
    match step {
        Step::Fetch | Step::Stage => Gate::Proceed,
        Step::Reboot => match local {
            Ok(RunState::Idle | RunState::Disconnected) => Gate::Proceed,
            Ok(other) => Gate::Defer(format!("lathe service is {}", other.as_str())),
            Err(e) => Gate::Defer(format!("lathe service state unknown ({e})")),
        },
    }
}

// --------------------------------------------------------- order acceptance

/// Who the board is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub device: String,
    /// The device class bundles must be built for (`kd240`).
    pub class: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    /// Count its sequence number as used, so an honest server stops sending
    /// it. True only for orders that are validly signed, for this device
    /// and newer than the last, but unusable (expired, wrong class, …). A
    /// replay or another device's order leaves the counter alone.
    pub consume: bool,
}

fn refuse(reason: String, consume: bool) -> Refusal {
    Refusal { reason, consume }
}

/// The checks on a verified order before anything is done with it.
pub fn accept_order(order: &Order, me: &Identity, last_seq: u64, now: u64) -> Result<(), Refusal> {
    if order.device() != me.device {
        return Err(refuse(
            format!("for device {:?}, this is {:?}", order.device(), me.device),
            false,
        ));
    }
    if order.seq() <= last_seq {
        return Err(refuse(
            format!("replay: seq {} is not above {last_seq}", order.seq()),
            false,
        ));
    }
    let bundle = match order {
        Order::Manifest(m) => Some(&m.bundle),
        Order::Command(c) => {
            if now > c.not_after {
                return Err(refuse(
                    format!("expired at {} (now {now})", c.not_after),
                    true,
                ));
            }
            c.action.bundle()
        }
    };
    if let Some(b) = bundle {
        b.check().map_err(|e| refuse(e, true))?;
        if b.class != me.class {
            return Err(refuse(
                format!("bundle is for {:?}, this is {:?}", b.class, me.class),
                true,
            ));
        }
    }
    Ok(())
}

/// Whether a command needs the interlock (reboot) or not (stage).
pub fn needs_interlock(action: &crate::msg::Action) -> bool {
    matches!(action, crate::msg::Action::Reboot { .. })
}

// ------------------------------------------------------------- the slots

/// Where a bundle may be written, and what to write to the boot state
/// first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagePlan {
    pub slot: Slot,
    /// A boot state to write before touching the slot: the slot was marked
    /// to be tried, and must not be while it is half written.
    pub clear_try: Option<BootState>,
}

/// The slot a bundle may be written to: the other one, and only while this
/// boot is the committed one. During a trial boot (this slot not yet
/// committed) the other slot is the way back, so it is left alone.
pub fn stage_target(state: Option<&BootState>, booted: Option<Slot>) -> Result<StagePlan, String> {
    let booted =
        booted.ok_or("this boot's slot is unknown (no emco.slot= on the kernel command line)")?;
    let state = state.ok_or("the boot state is unreadable")?;
    if state.active != booted {
        return Err(format!(
            "this boot (slot {booted}) is not committed yet; slot {} is the way back",
            state.active
        ));
    }
    let slot = booted.other();
    let clear_try = (state.try_slot == Some(slot)).then_some(BootState {
        try_slot: None,
        tries: 0,
        ..*state
    });
    Ok(StagePlan { slot, clear_try })
}

/// The boot state that boots `slot` once at the next reboot.
pub fn trial(state: &BootState, slot: Slot) -> BootState {
    BootState {
        try_slot: Some(slot),
        tries: 1,
        ..*state
    }
}

/// What a healthy boot writes, if anything:
///
/// * a trial boot of `try` commits it (`active = booted`, nothing to try);
/// * a boot of the committed slot after a trial ran out clears the try;
/// * an unreadable state is rewritten with this slot committed;
/// * otherwise nothing (in particular a try still to come is kept).
pub fn on_boot(state: Option<&BootState>, booted: Slot) -> Option<BootState> {
    let Some(s) = state else {
        return Some(BootState {
            active: booted,
            ..BootState::initial()
        });
    };
    if s.try_slot == Some(booted) && s.active != booted {
        return Some(BootState {
            active: booted,
            try_slot: None,
            tries: 0,
            ..*s
        });
    }
    if s.active == booted && s.try_slot.is_some() && s.tries == 0 {
        return Some(BootState {
            try_slot: None,
            ..*s
        });
    }
    None
}

/// Is this boot a trial that still has to prove itself?
pub fn is_trial_boot(state: Option<&BootState>, booted: Slot) -> bool {
    state.is_some_and(|s| s.active != booted && s.try_slot == Some(booted))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrialOutcome {
    /// Booted and committed.
    Committed,
    /// It was tried and the board is back on the other slot.
    RolledBack,
    /// Not decided yet (the reboot is still to come, or the trial boot has
    /// not committed yet).
    Pending,
}

/// How the trial of `slot` went, seen from this boot.
pub fn trial_outcome(slot: Slot, booted: Option<Slot>, state: Option<&BootState>) -> TrialOutcome {
    let Some(booted) = booted else {
        return TrialOutcome::Pending;
    };
    let pending = state.and_then(BootState::pending_try);
    match (booted == slot, state.map(|s| s.active)) {
        (true, Some(active)) if active == slot => TrialOutcome::Committed,
        (true, _) => TrialOutcome::Pending,
        (false, _) if pending == Some(slot) => TrialOutcome::Pending,
        (false, _) => TrialOutcome::RolledBack,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::tests::bundle;
    use crate::msg::{Action, Command, Manifest};

    fn me() -> Identity {
        Identity {
            device: "kd240".into(),
            class: "kd240".into(),
        }
    }

    fn idle() -> Result<RunState, String> {
        Ok(RunState::Idle)
    }

    // ---- interlock

    #[test]
    fn stage_and_fetch_always_proceed() {
        for local in [
            idle(),
            Ok(RunState::Running),
            Ok(RunState::Alarm),
            Err("down".into()),
        ] {
            assert_eq!(interlock(Step::Stage, &local), Gate::Proceed);
            assert_eq!(interlock(Step::Fetch, &local), Gate::Proceed);
        }
    }

    #[test]
    fn reboot_only_when_idle_or_disconnected() {
        assert_eq!(interlock(Step::Reboot, &Ok(RunState::Idle)), Gate::Proceed);
        assert_eq!(
            interlock(Step::Reboot, &Ok(RunState::Disconnected)),
            Gate::Proceed
        );
        for busy in ["running", "hold", "alarm", "homing", "toolchange", "estop?"] {
            let g = interlock(Step::Reboot, &Ok(RunState::parse(busy)));
            assert!(
                matches!(&g, Gate::Defer(why) if why.contains(busy)),
                "{busy}: {g:?}"
            );
        }
    }

    #[test]
    fn unknown_state_defers() {
        let g = interlock(Step::Reboot, &Err("connection refused".into()));
        assert!(matches!(g, Gate::Defer(why) if why.contains("connection refused")));
    }

    #[test]
    fn run_state_from_the_service() {
        let body =
            serde_json::json!({"session": {"run": "running", "tool": 1}, "transport": "virtual"});
        assert_eq!(run_state_from_api(&body), Ok(RunState::Running));
        let body = serde_json::json!({"session": {"run": "idle"}});
        assert_eq!(run_state_from_api(&body), Ok(RunState::Idle));
        assert!(run_state_from_api(&serde_json::json!({"session": {}})).is_err());
        assert!(run_state_from_api(&serde_json::json!({"session": {"run": 3}})).is_err());
    }

    #[test]
    fn stage_needs_no_interlock() {
        assert!(!needs_interlock(&Action::Stage {
            bundle: bundle("a")
        }));
        assert!(needs_interlock(&Action::Reboot { bundle: None }));
        assert!(needs_interlock(&Action::Reboot {
            bundle: Some(bundle("a"))
        }));
    }

    // ---- acceptance

    fn manifest(seq: u64) -> Order {
        Order::Manifest(Manifest {
            device: "kd240".into(),
            seq,
            bundle: bundle("v1"),
            stage: false,
            created: 1,
            note: String::new(),
        })
    }

    fn command(seq: u64, not_after: u64, action: Action) -> Order {
        Order::Command(Command {
            device: "kd240".into(),
            seq,
            not_after,
            action,
        })
    }

    #[test]
    fn accepts_a_good_order() {
        assert_eq!(accept_order(&manifest(5), &me(), 4, 100), Ok(()));
        assert_eq!(
            accept_order(
                &command(5, 100, Action::Reboot { bundle: None }),
                &me(),
                4,
                100
            ),
            Ok(())
        );
    }

    #[test]
    fn replay_is_refused_without_consuming() {
        for seq in [3, 4] {
            let r = accept_order(&manifest(seq), &me(), 4, 0).unwrap_err();
            assert!(r.reason.contains("replay") && !r.consume);
        }
    }

    #[test]
    fn other_device_is_refused_without_consuming() {
        let mut o = manifest(9);
        if let Order::Manifest(m) = &mut o {
            m.device = "kd240-2".into();
        }
        let r = accept_order(&o, &me(), 0, 0).unwrap_err();
        assert!(!r.consume);
    }

    #[test]
    fn expired_wrong_class_and_bad_bundles_are_consumed() {
        let r = accept_order(
            &command(5, 99, Action::Reboot { bundle: None }),
            &me(),
            0,
            100,
        )
        .unwrap_err();
        assert!(r.reason.contains("expired") && r.consume);

        let mut o = manifest(5);
        if let Order::Manifest(m) = &mut o {
            m.bundle.class = "kr260".into();
        }
        assert!(accept_order(&o, &me(), 0, 0).unwrap_err().consume);

        let mut b = bundle("v1");
        b.files.pop();
        let o = command(5, 100, Action::Stage { bundle: b });
        assert!(accept_order(&o, &me(), 0, 0).unwrap_err().consume);
    }

    // ---- slots

    fn st(active: Slot, try_slot: Option<Slot>, tries: u32) -> BootState {
        BootState {
            seq: 9,
            active,
            try_slot,
            tries,
        }
    }

    #[test]
    fn stage_writes_only_the_other_slot_of_a_committed_boot() {
        let s = st(Slot::A, None, 0);
        assert_eq!(
            stage_target(Some(&s), Some(Slot::A)),
            Ok(StagePlan {
                slot: Slot::B,
                clear_try: None
            })
        );
        let s = st(Slot::B, None, 0);
        assert_eq!(stage_target(Some(&s), Some(Slot::B)).unwrap().slot, Slot::A);
    }

    #[test]
    fn a_trial_boot_never_overwrites_its_way_back() {
        // Booted B on trial; A is still active (committed).
        let s = st(Slot::A, Some(Slot::B), 0);
        let e = stage_target(Some(&s), Some(Slot::B)).unwrap_err();
        assert!(e.contains("not committed"), "{e}");
    }

    #[test]
    fn staging_over_a_marked_slot_clears_the_mark_first() {
        let s = st(Slot::A, Some(Slot::B), 1);
        let p = stage_target(Some(&s), Some(Slot::A)).unwrap();
        assert_eq!(p.slot, Slot::B);
        assert_eq!(p.clear_try, Some(st(Slot::A, None, 0)));
    }

    #[test]
    fn staging_needs_to_know_where_it_is() {
        assert!(stage_target(None, Some(Slot::A)).is_err());
        assert!(stage_target(Some(&st(Slot::A, None, 0)), None).is_err());
    }

    #[test]
    fn the_slot_state_machine() {
        // stage → try → (U-Boot decrements) → boot → commit.
        let s0 = st(Slot::A, None, 0);
        let s1 = trial(&s0, Slot::B);
        assert_eq!(s1, st(Slot::A, Some(Slot::B), 1));
        assert_eq!(
            trial_outcome(Slot::B, Some(Slot::A), Some(&s1)),
            TrialOutcome::Pending
        );
        let s2 = BootState { tries: 0, ..s1 }; // U-Boot, before booting B
        assert!(is_trial_boot(Some(&s2), Slot::B));
        assert_eq!(
            trial_outcome(Slot::B, Some(Slot::B), Some(&s2)),
            TrialOutcome::Pending
        );
        let s3 = on_boot(Some(&s2), Slot::B).unwrap();
        assert_eq!(s3, st(Slot::B, None, 0));
        assert_eq!(
            trial_outcome(Slot::B, Some(Slot::B), Some(&s3)),
            TrialOutcome::Committed
        );
        assert_eq!(on_boot(Some(&s3), Slot::B), None, "nothing more to write");
        assert!(!is_trial_boot(Some(&s3), Slot::B));
    }

    #[test]
    fn the_fallback() {
        // B never came up: U-Boot used the try, then booted A.
        let s2 = st(Slot::A, Some(Slot::B), 0);
        assert_eq!(
            trial_outcome(Slot::B, Some(Slot::A), Some(&s2)),
            TrialOutcome::RolledBack
        );
        assert!(!is_trial_boot(Some(&s2), Slot::A));
        // A, healthy, clears the spent try.
        assert_eq!(on_boot(Some(&s2), Slot::A), Some(st(Slot::A, None, 0)));
        let s3 = st(Slot::A, None, 0);
        assert_eq!(
            trial_outcome(Slot::B, Some(Slot::A), Some(&s3)),
            TrialOutcome::RolledBack
        );
    }

    #[test]
    fn a_try_still_to_come_is_kept() {
        let s = st(Slot::A, Some(Slot::B), 1);
        assert_eq!(on_boot(Some(&s), Slot::A), None);
    }

    #[test]
    fn an_unreadable_state_is_rewritten_for_this_slot() {
        assert_eq!(on_boot(None, Slot::B).unwrap().active, Slot::B);
    }

    #[test]
    fn a_boot_of_an_uncommitted_slot_that_was_not_tried_is_not_committed() {
        // Booted B by hand from the U-Boot prompt: not a trial, not committed.
        let s = st(Slot::A, None, 0);
        assert_eq!(on_boot(Some(&s), Slot::B), None);
        assert!(!is_trial_boot(Some(&s), Slot::B));
    }
}
