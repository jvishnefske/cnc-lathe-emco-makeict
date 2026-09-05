# 60 — LinuxCNC retrofit design (future option, not current work)

**Status: engineering design for a contingency.** The project decision
([`docs/adr/0001-serial-vs-retrofit.md`](adr/0001-serial-vs-retrofit.md)) is to keep the machine
non-invasive: the AC95 stays, the control cabinet stays shut, and the machine is driven over
RS-232 per [`docs/30-serial-protocol.md`](30-serial-protocol.md). This document exists so that if
that path fails, the makerspace can execute a retrofit from a written plan instead of improvising
around a dead machine. Nobody should buy parts from this document until the ADR's revisit
conditions have been met.

Related: [`docs/20-control-architecture.md`](20-control-architecture.md) (what is in the box),
[`docs/70-simulator.md`](70-simulator.md) (the LinuxCNC simulator, which doubles as training for
this config), and the skeleton configuration in
[`linuxcnc/retrofit/emco-pcturn50-7i96s/`](../linuxcnc/retrofit/emco-pcturn50-7i96s/).

Hardware facts below marked **UNVERIFIED** are community-sourced and not yet confirmed on this
unit (see §9 and doc 30 §1). Photo-derived confirmation from `docs/_photos-*.md` is pending.

---

## 1. When a retrofit becomes necessary

Two trigger conditions, either sufficient:

1. **Protocol discovery fails.** The ladder in doc 30 §3 is exhausted: no original software found,
   nothing intelligible on the wire, active probing gets nowhere — or discovery succeeds but
   reveals a real-time protocol a sender cannot practically drive (doc 30, Hypothesis B worst
   case).
2. **The AC95 dies.** The board (part `Y4B081000`, **UNVERIFIED**) is unobtainable: EMCO does not
   support it, and no aftermarket exists. Any failure of the brain board — power section, CPU,
   firmware ROM — is terminal for the original control scheme. A 30-year-old board with
   electrolytic capacitors will eventually get here on its own.

The retrofit is the escape hatch for both. It is deliberately scoped so that most of it can be
prepared (config, breakout, bench-tested Mesa card) before the machine is ever opened.

## 2. Recommended approach: replace the AC95 only

Replace the brain, keep the muscles. A Mesa **7i96S** Ethernet motion card takes the AC95's place
and drives the existing stepper power card through the existing ribbon header:

```
                    BEFORE                                 AFTER

  DOS PC ──RS232──► AC95 (Y4B081000)          LinuxCNC PC ──Ethernet──► Mesa 7i96S
                      │                                                    │
                      │ 16-pin IDC ribbon                                  │ IDC16 breakout,
                      ▼ at X104                                            ▼ new ribbon to X104
              stepper power card  (Y4A018000)              stepper power card  (unchanged)
                      │                                                    │
                      ▼                                                    ▼
          Berger Lahr 5-phase motors (X, Z)             same motors, same wiring (unchanged)

              spindle board (Y4A400000)          spindle: enable/dir/0–10 V from 7i96S,
                 on X107/X1070                   into existing drive if possible (§4.3)
```

All part numbers and connector names above are **UNVERIFIED** community record; §9 requires
metering `X104` before connecting anything.

### Why this beats a full gut

1. **The 5-phase problem.** The axis motors are Berger Lahr 5-phase steppers (VRDM series,
   **UNVERIFIED**). Modern 5-phase drives are rare and expensive; the known substitutes are the
   Berger Lahr D450 (obsolete, eBay-only) and the Vexta UDX5128NA (also discontinued), and the
   Vexta route requires rewiring the motor's 10 leads into a pentagon configuration. The existing
   `Y4A018000` power card already drives these motors correctly, with matched currents, on
   connectors that already exist. Keeping it sidesteps the single hardest sourcing problem in the
   whole machine. This is the strongest argument for the AC95-only swap.
2. **Cost.** One Mesa card, one breakout, a PSU and wiring: roughly €250–400 (see §7), versus
   €1200–2500+ for motors, drives and a VFD — before machining new motor mounts.
3. **Reversibility.** The change is one ribbon cable moved from the AC95 to a breakout board. If
   an AC95 (or a protocol breakthrough) ever materialises, moving the ribbon back restores the
   original machine. No metal is cut, no loom is rewired.
4. **Precedent.** A documented community LinuxCNC retrofit of a PC Turn 50 used a Mesa 7i96S with
   ClassicLadder handling the turret; a PC Turn 55 retrofit replaced its AC95 with a Mesa 7i76e
   via a cheap IDC16 breakout on `X104`. We are not the first crew down this path.
   (**UNVERIFIED** for our exact hardware revision, but strong evidence the interface is
   step/direction at logic level on that header.)

### Why the 7i96S specifically

- Ethernet (`hm2_eth` driver) — no parallel port or PCI archaeology on a modern PC.
- 5 step/dir channels: X, Z, spare for a turret or future C axis.
- 11 isolated inputs / 6 outputs: enough for limits, home, E-stop, turret index and station
  sensing, spindle control (§4).
- One encoder input: the spindle encoder needed for threading (§4.4).
- Isolated 0–10 V analogue output for spindle speed.
- Cheap (~€120–150) and current production, with first-class LinuxCNC support — the upstream
  tree ships `configs/by_interface/mesa/hm2-stepper/7i96s.ini` as a maintained example, which the
  skeleton config in this repo is derived from.

## 3. Rejected alternative: full gut

Strip the cabinet: new 2-phase NEMA 23/24 steppers (or servos), modern digital drives, a VFD and
a standard 3-phase motor for the spindle, 7i96S or even a parallel-port BOB for I/O.

**Why rejected now:** it forfeits every advantage in §2 — cost triples, reversibility drops to
zero, and it converts the 5-phase problem from "avoided" into "mandatory mechanical work"
(motor mounts, couplings, possibly new pulleys for the spindle motor). It also multiplies the
unknowns: instead of one unverified pinout (`X104`), every subsystem must be re-engineered.

**When it would actually be the better call — honestly:**

- The `Y4A018000` power card or a Berger Lahr motor is dead. Once you must source 5-phase drives
  or rewire motors anyway, most of the AC95-only advantage evaporates; new 2-phase motors and
  drives are cheaper than hunting D450s.
- The spindle drive (`Y4A400000`, proprietary, **UNVERIFIED**) is dead. A VFD + standard motor is
  then the sane fix, and once the cabinet is open that deep, gutting the rest is incremental.
- Metering shows `X104` is not what the community says (not logic-level step/dir, or entangled
  with the spindle board in a way we cannot cleanly drive).
- The makerspace wants performance the original power stage cannot give (rigid tapping, higher
  rapids). This is a training lathe; that is unlikely, but it is the legitimate version of the
  argument.

The signal plan and config below are written for the recommended approach but degrade gracefully:
in a full gut, the same 7i96S pins drive modern step/dir drives instead of `X104`, and only the
scales/timings change.

## 4. Signal-by-signal plan

All pin assignments on the EMCO side are **UNVERIFIED** until metered (§9). 7i96S terminal names
follow Mesa's manual; the concrete pin numbers chosen here are recorded once, in the HAL signal
map (§5), so a correction is a one-file edit.

### 4.1 Axis motion — step and direction

- X step/dir: 7i96S stepgen 0 → IDC16 breakout → `X104` X-axis step/dir pair.
- Z step/dir: 7i96S stepgen 1 → `X104` Z-axis step/dir pair.
- **Before connecting:** identify `X104`'s pinout with the board out and a meter (§9, risk R1).
  Expected (**UNVERIFIED**): step, direction, and possibly enable/current-reduction per axis at
  5 V logic, plus grounds. Determine active edge and voltage level; the 7i96S outputs are 5 V
  tolerant TTL — if the power card expects open-collector or a different level, the breakout gains
  a buffer/level-shifter.
- Step timing (`STEPLEN`/`STEPSPACE`/`DIRSETUP`/`DIRHOLD`) starts conservative (5 µs/5 µs,
  1 µs/1 µs — **UNVERIFIED**) and is tuned down only if the power card proves happy. 5-phase
  drives of this era are not speed demons; there is no prize for aggressive timing.
- Scale: Berger Lahr 5-phase motors are typically 0.72°/step = 500 steps/rev (**UNVERIFIED**),
  ballscrew pitch unknown (**UNVERIFIED**, likely 2.5 mm [0.098 in]). Measured during
  commissioning (§6, step 7); placeholder values in the INI are tagged.

### 4.2 Limits, home and E-stop

- One reference/limit switch per axis is expected (**UNVERIFIED** — count and type to be
  established; training lathes of this class often have a single reference switch per axis and
  rely on soft limits). Wire each to its own 7i96S isolated input; configure as combined
  home+limit in the INI if that is what exists.
- **E-stop is a hardware chain, not a HAL signal.** The physical E-stop button, spindle drive
  fault and (if fitted) axis fault contacts wire in series into a relay that kills motor power
  directly. A contact from that chain feeds a 7i96S input so LinuxCNC *knows* about the stop;
  LinuxCNC's software E-stop output drives the chain's coil so it can *request* one. Software is
  never the only thing between a person and a spinning chuck — same rule as doc 30 §4.

### 4.3 Spindle: enable, direction, speed

- Preferred: keep the existing spindle drive and inject enable / direction / speed reference at
  its input connector (`X107`/`X1070`, **UNVERIFIED**). Whether it accepts an analogue reference
  is unknown; the proprietary EMCO drive may expect digital commands from the AC95, in which case
  this is the first candidate for partial gutting (VFD + existing motor if the motor is a
  standard induction type — **UNVERIFIED**).
- 7i96S side: `spindle-enable` and `spindle-direction` on two isolated outputs, speed on the
  isolated 0–10 V analogue output scaled to `spindleMaxRpm`.
- If a VFD is fitted instead, the identical three signals go to the VFD terminals (or Modbus,
  which the 7i96S can also do — keep it simple and use wires first).

### 4.4 Spindle encoder — threading

Threading (G33/G76) requires spindle position feedback. Whether the machine has any spindle
encoder is **UNVERIFIED** (it supported threading under the original control, so *something*
exists — possibly only a 1-per-rev sensor, possibly a slotted-disc encoder feeding the AC95 or
the spindle board).

- If a usable quadrature (or even single-channel + index) signal exists, wire it to the 7i96S
  encoder input.
- If not: fit a small bracket-mounted incremental encoder (or a printed slotted disc + two
  optical sensors) on the spindle, 100–360 PPR plus index. LinuxCNC threads happily on modest
  resolution. This is the one piece of mechanical fabrication the recommended approach may need.

### 4.5 Turret index and position

The 8-station (**UNVERIFIED** — count the stations) turret on these machines is a motor-ratchet
mechanism: a DC motor rotates the turret forward, a switch or sensor reports position, and
reversing against a pawl locks it.

- Inputs: turret index/position sensor(s) — one pulse per station, or a coded set
  (**UNVERIFIED**, meter it).
- Outputs: turret motor on, and turret motor reverse (lock) if the mechanism needs it
  (**UNVERIFIED**).
- Logic: ClassicLadder, as in the documented PC Turn 50 community retrofit — a small ladder
  program sequences "rotate until station N seen, reverse to lock, assert tool-changed". The HAL
  hooks (`iocontrol.0.tool-change`/`tool-changed`) are stubbed as a loopback in the skeleton
  config with a marked TODO.

### 4.6 Miscellaneous

- Coolant pump, if fitted (**UNVERIFIED**): one output.
- Cabinet door / chuck guard interlock, if fitted (**UNVERIFIED**): wire into the E-stop chain
  or feed-hold input, not merely into software.

## 5. HAL signal map

Single source of truth for wiring; correct it here first, then in the HAL file. 7i96S TB numbers
are from the Mesa 7i96S manual; verify against the delivered board revision.

| HAL signal | LinuxCNC side | 7i96S terminal | Field side | Status |
|---|---|---|---|---|
| `x-step`, `x-dir` | stepgen 00 | TB1 STEP0/DIR0 | `X104` X pair via IDC16 breakout | **UNVERIFIED** pinout |
| `z-step`, `z-dir` | stepgen 01 | TB1 STEP1/DIR1 | `X104` Z pair via IDC16 breakout | **UNVERIFIED** pinout |
| (spare) | stepgen 02 | TB1 STEP2/DIR2 | unused / future turret or C | — |
| `x-home-sw` | `joint.0.home-sw-in` (+ limits) | TB3 INPUT 0 | X reference switch | **UNVERIFIED** switch exists |
| `z-home-sw` | `joint.1.home-sw-in` (+ limits) | TB3 INPUT 1 | Z reference switch | **UNVERIFIED** |
| `estop-ext` | `estop-latch.0.fault-in` | TB3 INPUT 2 | aux contact of hardware E-stop chain | design fixed, wiring TBD |
| `turret-index` | ClassicLadder in | TB3 INPUT 3 | turret position sensor | **UNVERIFIED** sensor type |
| `turret-pos-b` | ClassicLadder in | TB3 INPUT 4 | second position line if coded | **UNVERIFIED** |
| `spindle-at-speed` | motion | TB3 INPUT 5 | drive "ready/at-speed" if available | **UNVERIFIED** |
| `probe-in` (future) | `motion.probe-input` | TB3 INPUT 6 | spare | — |
| `estop-out` | `iocontrol.0.user-enable-out` | TB2 OUTPUT 0 | E-stop chain coil / enable relay | design fixed |
| `spindle-enable` | `spindle.0.on` | TB2 OUTPUT 1 | spindle drive enable | **UNVERIFIED** drive input |
| `spindle-dir` | `spindle.0.reverse` | TB2 OUTPUT 2 | spindle drive direction | **UNVERIFIED** |
| `turret-motor` | ClassicLadder out | TB2 OUTPUT 3 | turret motor relay (fwd) | **UNVERIFIED** |
| `turret-lock` | ClassicLadder out | TB2 OUTPUT 4 | turret motor relay (rev/lock) | **UNVERIFIED** |
| `coolant-flood` | `iocontrol.0.coolant-flood` | TB2 OUTPUT 5 | coolant pump relay | **UNVERIFIED** pump exists |
| `spindle-vel-cmd` | `spindle.0.speed-out-abs` | TB4 analogue 0–10 V | spindle drive speed reference | **UNVERIFIED** drive accepts it |
| `spindle-pos-fb` | `spindle.0.revs` etc. | TB4 ENCODER A/B/Z | spindle encoder (§4.4) | **UNVERIFIED** encoder exists |

## 6. Skeleton configuration

Lives at [`linuxcnc/retrofit/emco-pcturn50-7i96s/`](../linuxcnc/retrofit/emco-pcturn50-7i96s/):
`emco-pcturn50.ini`, `emco-pcturn50.hal`, `tool.tbl` and a README. It is derived from the
upstream examples `configs/by_interface/mesa/hm2-stepper/7i96s.ini` +
`hm2-stepper-eth.hal` (for the `hm2_eth`/7i96S mechanics) and `configs/sim/axis/lathe.ini` (for
the lathe/trivkins shape) in the LinuxCNC source tree.

Key choices:

- `[KINS] KINEMATICS = trivkins coordinates=xz`, `JOINTS = 2`, `[TRAJ] COORDINATES = X Z`.
- `[DISPLAY] LATHE = 1`; `LINEAR_UNITS = mm`, metric throughout (inch is a display concern).
- `[HOSTMOT2] DRIVER=hm2_eth BOARD=7i96s CONFIG="num_encoders=1 num_pwmgens=0 num_stepgens=3"`.
- Every number that must be measured on the machine is a placeholder tagged `UNVERIFIED` in a
  comment on the same line, and they are all gathered in the INI (never buried in the HAL), so
  correcting one is a one-line edit. The HAL file reads them via `[SECTION]NAME` references,
  exactly as the upstream example does.

It will not run a real machine as-is — that is deliberate. It *will* load under a sim harness for
config-structure testing, and it shares its machine numbers with the simulator profile in
[`docs/70-simulator.md`](70-simulator.md) and `MachineProfile` in
[`sender/INTERFACE.md`](../sender/INTERFACE.md): when a real value is measured, update all three.

## 7. Bill of materials (rough 2026 prices)

| Item | Qty | ~Price | Notes |
|---|---|---|---|
| Mesa 7i96S | 1 | €140 | store.mesanet.com; check stock/revision |
| IDC16 breakout board + ribbon | 1 | €10 | generic; the "cheap IDC16 breakout" of the PC Turn 55 precedent |
| 24 V DIN PSU, 60 W | 1 | €25 | field I/O power for inputs/relays |
| 5 V PSU or DC-DC | 1 | €10 | 7i96S logic + breakout, if not shared |
| Relays / SSRs for turret, coolant, enable | 4 | €30 | DIN socketed |
| E-stop chain parts (contactor or safety relay, aux blocks) | 1 | €60 | do not economise here |
| Spindle encoder + bracket (if none usable found) | 1 | €50–80 | §4.4 |
| Wire, ferrules, DIN rail, glands, labels | — | €50 | |
| Old-but-solid PC with Ethernet, running LinuxCNC | 1 | €0–100 | latency-test it first |
| **Total, recommended approach** | | **≈ €350–500** | |
| *(Contrast: full gut — 2 motors+drives ≈ €400–700, VFD+motor ≈ €400–800, mounts/machining, total ≈ €1200–2500)* | | | |

## 8. Commissioning sequence

In order; do not skip. The doc 30 §4 safety rules apply throughout.

1. **Paper first.** `X104` pinout metered and recorded (§9 R1); HAL signal map (§5) updated;
   every `UNVERIFIED` in the INI either resolved or consciously deferred.
2. **Bench test.** 7i96S on the bench: ping, `mesaflash --device 7i96s --addr <ip> --readhmid`,
   LinuxCNC loads the config, stepgen pins toggle on a scope. No machine involved.
3. **Continuity and power checks, machine cold.** Mains off, cabinet isolated and proven dead.
   Verify PSU rails on the bench before they touch anything. Confirm no 7i96S output can reach a
   motor circuit yet: **ribbon to `X104` disconnected**.
4. **E-stop chain first.** Wire and test the hardware E-stop chain with motor power still
   disconnected. Prove the button kills the enable relay and that LinuxCNC sees and commands it.
5. **Inputs live.** Power the low-voltage side only. Toggle every switch by hand; watch the HAL
   pins in Halshow. Fix inversions in the INI, not in your head.
6. **One axis, motor connected, other axis disconnected.** Connect the `X104` ribbon. Small jog
   at low velocity, hand hovering over E-stop. Check: moves at all, correct direction (fix with
   `SCALE` sign), no stalling or drive complaints.
7. **Scale and direction verification.** Command 10.000 mm [0.394 in], measure with a dial
   indicator. Correct `SCALE` until commanded = measured over ≥ 50 mm [2 in]. Repeat per axis.
   Positive X must move the tool *away* from centreline; positive Z *away* from the chuck.
8. **Velocity/acceleration tuning.** Raise until the motor stalls, back off 30 %. 5-phase
   steppers give no fault signal for lost steps — be conservative; a training machine needs
   reliability, not rapids.
9. **Soft limits before homing.** Set `MIN_LIMIT`/`MAX_LIMIT` from hand-cranked measurements of
   true travel, minus margin, before trusting any automatic homing. Then configure and test
   homing at low `HOME_SEARCH_VEL`.
10. **Spindle.** Enable/direction/speed with no belt or an empty spindle first; verify commanded
    vs tachometer RPM; calibrate the 0–10 V scaling. Then encoder: `spindle.0.revs` counts
    correctly, index seen once per rev.
11. **Turret.** ClassicLadder sequence with the turret unloaded; verify every station indexes and
    locks; then `T` commands from MDI.
12. **Acceptance tests.**
    - *Repeatability:* ten times approach the same X/Z position from the same direction; dial
      indicator spread ≤ 0.02 mm [0.001 in] target (**UNVERIFIED** — set from first results).
    - *Test cut:* turn a diameter on aluminium round bar, measure, apply tool offset, re-cut,
      confirm the second diameter lands within 0.02–0.05 mm [0.001–0.002 in].
    - *Threading check:* G76 an M10×1.5 external thread in aluminium; verify with a nut and
      thread gauge; confirms encoder, scale and axis sync together.
13. **Documentation.** Update §5, the INI, `docs/20-control-architecture.md` and the findings log
    in doc 30 (final entry: "retrofit executed, serial path closed"), then a wiring photo set to
    `docs/img/`.

## 9. Risks

| # | Risk | Impact | Mitigation |
|---|---|---|---|
| R1 | **`X104` pinout is unconfirmed.** The entire recommended approach assumes logic-level step/dir on that header. | Connecting the 7i96S wrong could kill the irreplaceable power card. | **Meter before connecting — mandatory.** Ribbon off, power card identified, pins traced to the driver ICs; scope the AC95's own output at power-up if the AC95 still runs. No connection until the map in §5 is confirmed on this unit. |
| R2 | Power card or motor already dead when the retrofit starts (may be *why* it starts). | Approach flips to partial/full gut (§3). | Decision point is explicit in §3; budget difference is known in advance. |
| R3 | Spindle drive won't accept external enable/speed. | Spindle sub-project becomes VFD + motor swap. | §4.3 fallback; motor type to be identified through vents/photos before opening. |
| R4 | No usable spindle encoder signal. | No threading until one is fitted. | §4.4 bolt-on encoder is a planned, small job. |
| R5 | Turret sensing scheme unknown. | Tool changes manual until solved. | Machine is usable for training without auto turret; ClassicLadder added later. |
| R6 | 5 V TTL vs power card input expectations mismatch. | Missed steps or no motion. | Buffer/driver on the breakout; scope the levels in step 2/6 of §8. |
| R7 | PC latency (Ethernet servo thread overruns). | Following errors, watchdog trips. | `latency-histogram` before committing to a PC; hm2_eth needs a dedicated NIC and ~1 ms servo period only — no software stepping. |
| R8 | Scope creep: the retrofit starts while the serial path is still viable. | Machine opened for nothing; ADR violated. | This document's §1 triggers are the only entry conditions; changes go through the ADR. |
