# 60 — LinuxCNC retrofit design (future option, not current work)

**Status: engineering design for a contingency, revised 2026-09-05 against
[`docs/_machine-facts.md`](_machine-facts.md).** The project decision
([`docs/adr/0001-serial-vs-retrofit.md`](adr/0001-serial-vs-retrofit.md)) is to stay
non-invasive: the control box stays shut, the machine keeps its original control, and LinuxCNC
runs as a training simulator only. This document exists so that if the revisit conditions in the
ADR are met, the makerspace can execute a retrofit from a written plan instead of improvising
around a dead machine. **Nobody should buy parts from this document until then.**

Since the machine's own schematics were recovered, this design is no longer built on community
folklore: the board set, motors, connector `X104` and the sensor complement are read from EMCO's
own documentation. What remains **UNVERIFIED** is flagged inline — above all the pin-by-pin
assignment and polarity inside `X104` (§9, risk R1), which **must be metered before anything is
connected**.

Related: [`docs/20-control-architecture.md`](20-control-architecture.md) (what is in the box),
[`docs/30-serial-protocol.md`](30-serial-protocol.md) (why the non-invasive path is hard),
[`docs/70-simulator.md`](70-simulator.md) (the simulator, which doubles as training for this
config), and the skeleton configuration in
[`linuxcnc/retrofit/emco-pcturn50-7i96s/`](../linuxcnc/retrofit/emco-pcturn50-7i96s/).

---

## 1. When a retrofit becomes necessary

Two trigger conditions, either sufficient (they are the ADR's revisit clauses):

1. **Protocol discovery fails or its result is unusable.** The ladder in doc 30 §4 is
   exhausted — no original software or ISA card found, silent bus, probing pointless — or
   discovery reveals a real-time master-driven bus protocol a PC application cannot safely
   speak. Doc 30 §4.3 already assesses this path as possibly intractable.
2. **The original electronics die.** The CPU board (`Y4A012000`/`Y4A015000`), the SPS 2000 PLC
   or the DC/DC converter fails. EMCO does not support these boards and no aftermarket exists;
   30-year-old electrolytics will get here on their own eventually.

The retrofit is deliberately scoped so most of it can be prepared — config, breakout,
bench-tested Mesa card, differential driver board — before the machine is ever opened.

## 2. Recommended approach: replace the CPU board only

Replace the brain, keep the muscles. A Mesa **7i96S** Ethernet motion card takes over the
role of the axis-controller CPU board `Y4A012000`, driving the existing stepper power card
through the existing 16-pin ribbon interface `X104`:

```
                 BEFORE                                      AFTER

 DOS PC ──RS-485 (via memory-mapped          LinuxCNC PC ──Ethernet──► Mesa 7i96S
          ISA card Y4A013000, X300)──►                                   │ single-ended
                                                                         ▼ step/dir TTL
 CPU board Y4A012000 (+Y4A015000)                        differential line drivers
        │                                                (AM26C31 class) + IDC16 breakout
        │ 16-pin ribbon from X104                                        │
        ▼                                                                ▼ new ribbon,
 stepper power card Y4A018000                            stepper power card Y4A018000
        │ X301 (X axis), X501 (Z axis)                          (unchanged)
        ▼                                                                │
 Berger Lahr VRDM 564/50 LN                                              ▼
 5-phase steppers, 0.95 A, pentagon                      same motors, same wiring (unchanged)

 spindle: FRC105 inverter Y4A400000 ──► keep if it accepts external enable/dir/speed (§4.4),
                                        else VFD + the existing ELIN 3-phase motor (§4.4)
```

### The `X104` interface — and the differential-pair constraint

`X104` is the 16-pin ribbon header on the CPU board that carries step/direction for **both**
axes to the stepper power card (which receives it on its `X1`/`X201` "CONTROL INTERFACE"
connectors). From the schematics [SCHEM]:

| Pins | Signals |
| --- | --- |
| 1–5 | X axis: `SR`, `DIR+`, `DIR−`, `CK+`, `CK−` |
| 6–10 | Z axis: `SR`, `DIR+`, `DIR−`, `CK+`, `CK−` |
| 16 | `E-Achse` (enable) |

The retrofit unplugs the ribbon from the CPU board and drives the power-card end from the Mesa
card instead. Two hard constraints fall out of the pinout:

1. **`DIR` and `CK` are differential pairs.** The 7i96S step/dir outputs are single-ended
   5 V-tolerant TTL. Feeding one leg of a differential receiver and grounding or floating the
   other is not a design, it is a hope. The breakout therefore carries **differential line
   drivers** — one AM26C31/26LS31-class quad RS-422 driver covers all four pairs (X `DIR`/`CK`,
   Z `DIR`/`CK`) — converting each Mesa step/dir output into a true ± pair. This is a real BOM
   item (§7) and a real bring-up step (§8 step 2), not an afterthought.
2. **`SR` (per axis) and `E-Achse` (pin 16) have undetermined semantics.** `SR` is plausibly
   current reduction ("Stromabsenkung") or a ready/enable line **[UNVERIFIED]**; `E-Achse` is
   an enable. Both must be traced to the power-card circuitry and metered against the live CPU
   board (if it still runs) before the Mesa drives them. Until then they are wired to spare
   7i96S outputs through the breakout so their polarity is a config change, not a rewire.

The exact pin order within each 5-pin axis group, and the +/− polarity of each pair, are
**UNVERIFIED** — the schematic gives the signal set, not a numbered map. §9 R1 makes metering
this the mandatory first act of any retrofit.

### Why this beats a full gut

1. **The 5-phase problem.** The axis motors are confirmed **Berger Lahr `VRDM 564/50 LN`
   5-phase steppers, 0.95 A, pentagon winding, 1000/500 steps/rev** [SCHEM]. Modern 5-phase
   drives are rare and expensive; known substitutes (Berger Lahr D450, Vexta UDX5128NA) are
   discontinued, and the Vexta route means rewiring ten motor leads into a pentagon. The
   existing `Y4A018000` power card already drives these motors correctly — full/half step and
   current are set on its DIP switch `S301` — on connectors that already exist. Keeping it
   sidesteps the single hardest sourcing problem in the machine. This is the strongest argument
   for the CPU-board-only swap.
2. **Cost.** One Mesa card, line drivers, a breakout, a PSU and wiring: ≈€400–550 (§7), versus
   ≈€1200–2500+ for motors, drives and a VFD — before machining new motor mounts.
3. **Reversibility.** The change is one ribbon cable moved from the CPU board to a breakout. If
   a protocol breakthrough or a donor board ever materialises, moving the ribbon back restores
   the original machine. No metal is cut, no loom is rewired.
4. **Precedent.** Community LinuxCNC retrofits of the PC Turn 50/55 family have driven the
   original stepper card through this header via a cheap IDC16 breakout (one PC Turn 50 build
   used a 7i96S with ClassicLadder on the turret). **[UNVERIFIED** for our exact hardware
   revision — but it agrees with our schematics that the header carries logic-level
   step/direction.**]**

### Why the 7i96S specifically

- Ethernet (`hm2_eth` driver) — no parallel-port or PCI archaeology on a modern PC.
- 5 step/dir channels: X, Z, plus spares for a turret axis or future C.
- 11 isolated inputs / 6 isolated outputs: enough for §4's plan with two spares.
- One encoder input: the spindle encoder needed for threading (§4.5).
- Isolated 0–10 V analogue output for spindle speed.
- ≈€140, current production, first-class LinuxCNC support — upstream ships
  `configs/by_interface/mesa/hm2-stepper/7i96s.ini` as a maintained example, from which the
  skeleton config here is derived.

### What happens to the PLC

The SPS 2000 was a slave on the RS-485 bus; with the bus master replaced, it has no
commander. In this design the PLC is bypassed: its machine-logic duties (turret sequencing,
interlocks, the robotic interface) move into LinuxCNC HAL/ClassicLadder, and its 24 V loads are
driven by relays from the 7i96S (§4.6–§4.7). The PLC boards stay physically in the rack,
unpowered or idle, preserving reversibility.

## 3. Rejected alternative: full gut

Strip the cabinet: new 2-phase NEMA 23/24 steppers (or servos), modern drives, a VFD for the
spindle, and a breakout for I/O.

**Why rejected now:** it forfeits every advantage in §2 — cost roughly triples, reversibility
drops to zero, and it converts the 5-phase problem from "avoided" into mandatory mechanical
work (motor mounts, couplings, pulleys). It also multiplies the unknowns: instead of one
unverified pinout (`X104`), every subsystem gets re-engineered at once.

**When it would actually be the better call — honestly:**

- The `Y4A018000` power card or a Berger Lahr motor is dead. Once 5-phase drives must be
  sourced or motors rewired anyway, most of the CPU-only advantage evaporates; new 2-phase
  motors and drives are cheaper than hunting D450s.
- The `FRC105` spindle inverter is dead **and** refuses external control. The fallback is
  clean here: the spindle motor is a standard **ELIN 3-phase 0.37 kW induction motor** [SCHEM],
  so a commodity VFD drives it directly — no motor swap needed. That containment is why a dead
  spindle drive alone does *not* force a full gut.
- Metering shows `X104` is not driveable as expected (not logic-level, or entangled with other
  boards in a way that cannot be cleanly separated).
- The makerspace wants performance the original power stage cannot give (rigid tapping, higher
  rapids). For a training lathe this is unlikely, but it is the legitimate form of the argument.

The signal plan below degrades gracefully: in a full gut the same 7i96S pins drive modern
step/dir drives directly (differential drivers become optional), and only scales and timings
change.

## 4. Signal-by-signal plan

EMCO-side facts are from the schematics [SCHEM] unless tagged; concrete pin choices live once,
in the HAL signal map (§5), so a correction is a one-file edit. 7i96S terminal names follow
Mesa's manual; verify against the delivered board revision.

### 4.1 Axis motion — step and direction

- X: 7i96S stepgen 0 → differential driver → `X104` X group (`CK±` = step, `DIR±` =
  direction). Z: stepgen 1 → `X104` Z group. `SR` and `E-Achse` from spare outputs (§2).
- Step timing starts conservative — `STEPLEN`/`STEPSPACE` 5 µs/5 µs, `DIRSETUP`/`DIRHOLD`
  10 µs/10 µs **[UNVERIFIED** — tune down only if the power card proves happy; early-90s
  5-phase electronics win no prizes for edge rates**]**.
- Scale: the manual states 0.001 mm per step and the motors 1000/500 steps/rev (half/full).
  Working placeholder: **1000 steps/mm** (i.e. 0.001 mm/step) **[UNVERIFIED** — implies 1 mm
  effective travel per motor rev through whatever reduction sits on the screw; ballscrew pitch
  and any belt ratio are unknown**]**. Measured with a dial indicator in §8 step 7 regardless.
- Velocity/accel: rapid 750 mm/min [29.5 in/min] = 12.5 mm/s; acceleration 100 mm/s²
  **[UNVERIFIED** estimate, same as the simulator**]**.

### 4.2 Reference sensors, and adding real limits

The machine has **no end-of-travel limit switches** — travel ends at mechanical stops and the
stepper stalls, silently losing position [MANUAL]. It **does** have per-axis reference
hardware: a mechanical reference switch (Schmersal or Omron) plus a **Balluff
`BES 516-343-E0-X`** NPN inductive "Sync-Bero" [SCHEM]. Reference is lost at every power-off.

- Wire each axis's reference switch to its own 7i96S input as the home switch
  (`HOME_SEARCH_VEL` toward it, matching the simulator's convention: X homes to +48 mm radius,
  Z to 0, all Z travel negative from there).
- Wire each Sync-Bero to its own input too. On the original control it refines the coarse
  switch position (like an encoder index). First commissioning ignores it; a later refinement
  can feed it into `HOME_USE_INDEX`-style homing via HAL logic. NPN output: needs a pull-up to
  the 24 V field supply and an input configured for sinking sensors.
- **Add true end-of-travel limit switches — an explicit improvement, not like-for-like.** Two
  microswitches per axis (or one switch at each hard stop), each axis pair in series into one
  input. The original design's "stall at the stops" behaviour is the single worst habit this
  machine teaches; a retrofit should not preserve it. Budgeted in §7; two inputs reserved in §5.

### 4.3 E-stop chain

**E-stop is a hardware chain, not a HAL signal.** The physical E-stop, the chip-guard and
wheel-cover interlocks [MANUAL], and drive fault contacts wire in series into a safety relay
that drops motor power directly (the cabinet already has two ABB BC6-30-01 contactors in its
power section [SCHEM] — reuse or replace per inspection). A contact from that chain feeds a
7i96S input so LinuxCNC *knows*; LinuxCNC's enable output drives the chain's coil so it can
*request* a stop. Software is never the only thing between a person and a spinning chuck —
same rule as doc 30 §5.

### 4.4 Spindle: enable, direction, speed

- Preferred: keep the **`FRC105` inverter (`Y4A400000`)** and inject enable / direction / speed
  reference at its input connector. Whether it accepts an analogue reference or expects digital
  commands from the CPU board is **[UNVERIFIED]** — first thing to establish once the box is
  open (§8 step 1).
- 7i96S side: `spindle-enable` and `spindle-direction` on two isolated outputs; speed from the
  isolated 0–10 V analogue output scaled to 130–3000 rpm. The three belt steps
  (130–1300 / 200–2000 / 300–3000 rpm) are changed by hand; the operator must tell LinuxCNC
  which belt is fitted (a HAL pin / INI choice), exactly as they had to tell the original
  control.
- Fallback if the FRC105 won't cooperate: commodity VFD (≈€120) driving the existing ELIN
  3-phase motor; identical three signals go to VFD terminals. Keep wires, not Modbus, for the
  first pass.

### 4.5 Spindle encoder — threading

**A spindle encoder exists**: encoder board `Y4A020000` feeds CPU connector `X109` with +5 V,
GND, `STROBE`, `SYNC` [SCHEM]. The original control threads with it (G33/G78) but only at
**≤400 rpm** [MANUAL]. Resolution and channel count are **[UNVERIFIED]** — the naming suggests
a count channel plus an index, not quadrature.

- Plan A: tap `STROBE`/`SYNC` into the 7i96S encoder input in counter+index mode. LinuxCNC
  threads acceptably on a modest count rate plus one index.
- Plan B (if the signal proves unusable): bracket-mount a small incremental encoder
  (100–360 PPR + index) on the spindle. The one piece of mechanical fabrication this retrofit
  may need; budgeted in §7.
- Honour the spirit of the 400 rpm limit until measurement says otherwise: thread at low belt
  step, and treat encoder count integrity at speed as a commissioning check (§8 step 10).

### 4.6 Turret index and sensing

**6 stations — 3 external + 3 internal (ø10 mm boring bars). Not 8; correct any config that
assumes 8** [MANUAL]. Mechanism: a **Maxon 24 VDC gearmotor, 60:1**, driven forward from PLC
output `A0.2`, ratcheting the turret round; position is sensed via `Strobe`/`Sync` from the
turret encoder board `Y4A020000`; reverse locks against the pawl (reverse-lock wiring
**[UNVERIFIED]** — facts list item 5) [SCHEM].

- Outputs: turret motor forward (relay from a 7i96S output), turret motor reverse/lock
  (second relay) **[UNVERIFIED** whether reversal is motor-polarity or a separate line**]**.
- Inputs: turret `Strobe` and `Sync` — the pulse pattern per station must be scoped
  **[UNVERIFIED]**.
- Logic: ClassicLadder sequences "rotate until station N, reverse to lock, assert
  tool-changed" on the `iocontrol.0.tool-change`/`tool-changed` handshake; the skeleton config
  ships it as a loopback with a marked TODO. The machine is fully usable for training with
  manual tool changes until this lands. Note the turret is an option [MANUAL] — confirm this
  unit actually has one fitted (facts list item 4).

### 4.7 Robotic-interface I/O

The original control exposed a 20–24 V "robotic interface" (outputs 0.2 A) through the PLC
[MANUAL]:

| Original | Function | Retrofit disposition |
| --- | --- | --- |
| `A0.0` | program stop | LinuxCNC handles program flow itself; expose a HAL pin if an external cell ever needs it |
| `A0.1` / `A0.3` | chuck open / closed | machine has a manual chuck/collet — signals existed for a pneumatic option; **deferred** unless that hardware exists **[UNVERIFIED]** |
| `A0.4` / `A0.5` | door | deferred (manual door); interlock handled in the E-stop chain (§4.3) |
| `A0.6` | tailstock | deferred (manual tailstock — variant to confirm, doc 30 §9) |
| `A0.7` | alarm | one 7i96S output → alarm lamp; wired in the base config |
| `E3.0`–`E3.7` | inputs: door, tailstock, chuck, feed stop | not reproduced 1:1; safety-relevant ones live in the E-stop chain, the rest arrive if/when automation hardware exists |

If the makerspace ever automates loading, a Mesa **7i84** on the 7i96S's Smart-Serial port adds
32 in / 16 out without redesign. Nothing else in this plan depends on it.

## 5. HAL signal map

Single source of truth for wiring; correct it here first, then in the HAL file. Field-side
`X104` pin numbers are deliberately blank until metered (§9 R1).

| HAL signal | LinuxCNC side | 7i96S terminal | Field side | Status |
|---|---|---|---|---|
| `x-step`, `x-dir` | stepgen 00 | TB1 STEP0/DIR0 | diff. driver → `X104` X `CK±`/`DIR±` | pins/polarity **UNVERIFIED** |
| `z-step`, `z-dir` | stepgen 01 | TB1 STEP1/DIR1 | diff. driver → `X104` Z `CK±`/`DIR±` | pins/polarity **UNVERIFIED** |
| (spare) | stepgen 02 | TB1 STEP2/DIR2 | unused / future turret or C | — |
| `axis-enable` | joint amp-enable | TB2 OUTPUT 5 | `X104` pin 16 `E-Achse` (+ per-axis `SR` if enables) | semantics **UNVERIFIED** |
| `x-home-sw` | `joint.0.home-sw-in` | TB3 INPUT 0 | X reference switch (Schmersal/Omron) | switch exists [SCHEM]; wiring TBD |
| `x-sync-bero` | HAL (future index refine) | TB3 INPUT 1 | Balluff BES 516-343-E0-X, NPN | exists [SCHEM]; unused at first |
| `z-home-sw` | `joint.1.home-sw-in` | TB3 INPUT 2 | Z reference switch | as X |
| `z-sync-bero` | HAL (future) | TB3 INPUT 3 | Z Sync-Bero | as X |
| `estop-ext` | `estop-latch.0.fault-in` | TB3 INPUT 4 | aux contact of hardware E-stop chain | design fixed; wiring TBD |
| `x-limits` | `joint.0.*-lim-sw-in` | TB3 INPUT 5 | **new** X end-of-travel switches, series pair | improvement (§4.2), to fit |
| `z-limits` | `joint.1.*-lim-sw-in` | TB3 INPUT 6 | **new** Z end-of-travel switches, series pair | improvement, to fit |
| `turret-strobe` | ClassicLadder in | TB3 INPUT 7 | turret encoder board `Y4A020000` Strobe | pattern **UNVERIFIED** |
| `turret-sync` | ClassicLadder in | TB3 INPUT 8 | turret encoder board Sync | **UNVERIFIED** |
| `spindle-ready` | motion / fault chain | TB3 INPUT 9 | FRC105 ready/fault contact if present | **UNVERIFIED** |
| (spare) | — | TB3 INPUT 10 | — | — |
| `estop-out` | `iocontrol.0.user-enable-out` | TB2 OUTPUT 0 | E-stop chain / enable relay coil | design fixed |
| `spindle-enable` | `spindle.0.on` | TB2 OUTPUT 1 | FRC105 enable (or VFD) | drive input **UNVERIFIED** |
| `spindle-dir` | `spindle.0.reverse` | TB2 OUTPUT 2 | FRC105/VFD direction | **UNVERIFIED** |
| `turret-fwd` | ClassicLadder out | TB2 OUTPUT 3 | relay → Maxon 24 V gearmotor forward (old `A0.2`) | motor confirmed [SCHEM]; relay to fit |
| `turret-rev` | ClassicLadder out | TB2 OUTPUT 4 | relay → turret reverse/lock | **UNVERIFIED** (§4.6) |
| `alarm-lamp` | HAL estop/alarm logic | (shared/expansion) | lamp (old `A0.7` role) | optional; move to 7i84 if outputs run short |
| `spindle-vel-cmd` | `spindle.0.speed-out-abs` | isolated 0–10 V out | FRC105/VFD speed reference | acceptance **UNVERIFIED** |
| `spindle-pos-fb` | spindle encoder counts | ENCODER A(/idx Z) | `X109` STROBE/SYNC from `Y4A020000` | resolution **UNVERIFIED** |

Output budget note: 6 outputs hold the essentials (estop, spindle ×2, turret ×2, enable); the
alarm lamp shares or waits for a 7i84. Input budget: 11 used exactly; the Sync-Beros are the
first candidates to surrender their inputs if something unforeseen needs one.

## 6. Skeleton configuration

Lives at [`linuxcnc/retrofit/emco-pcturn50-7i96s/`](../linuxcnc/retrofit/emco-pcturn50-7i96s/):
`emco-pcturn50-7i96s.ini`, `emco-pcturn50-7i96s.hal`, `tool.tbl`, `README.md`. Derived from the
upstream example `configs/by_interface/mesa/hm2-stepper/7i96s.ini` + `hm2-stepper-eth.hal`
(read against a 2.10-dev tree — re-check pin names against the 2.9.x actually installed) and
from the **verified** simulator config
[`linuxcnc/sim/emco-pcturn50/emco-pcturn50.ini`](../linuxcnc/sim/emco-pcturn50/emco-pcturn50.ini),
whose conventions it matches exactly:

- `[KINS] KINEMATICS = trivkins coordinates=xz`, `JOINTS = 2`, `[TRAJ] COORDINATES = X Z`;
- `[DISPLAY] LATHE = 1`, metric throughout (`LINEAR_UNITS = mm`; inch is a display concern);
- X is **radius**, −0.5..48 mm; Z is 0..−228 mm; machine zero at the reference position
  (X homes to +48, Z to 0, `HOME_SEQUENCE` X first); `EMCIO = iov2` (the v1 io E-stop bug is
  documented in the sim INI);
- `[HOSTMOT2] DRIVER=hm2_eth BOARD=7i96s CONFIG="num_encoders=1 num_pwmgens=1
  num_stepgens=3"` (the pwmgen feeds the board's isolated 0–10 V spindle output);
- every number that must be measured on the machine is tagged `UNVERIFIED` in a comment on the
  same line, and all of them live in the INI (never buried in the HAL) — the HAL reads
  `[SECTION]NAME` references, as upstream does.

It will not run a real machine as-is — deliberately. When a real value is measured, update the
INI, this document's §5, and the simulator profile together.

## 7. Bill of materials (rough 2026 prices)

| Item | Qty | ~Price | Notes |
|---|---|---|---|
| Mesa 7i96S | 1 | €140 [$150] | store.mesanet.com; check stock/revision |
| **Differential line driver** (AM26C31/26LS31 quad RS-422, on a small board) | 1–2 | €10 | §2 constraint: `DIR`/`CK` are differential pairs; 1 quad IC covers 4 pairs, second as spare |
| IDC16 breakout board + ribbon | 1 | €10 | carries the driver board's outputs to `X104` |
| 24 V DIN PSU, 60 W | 1 | €25 | field I/O, relays, sensors (Beros are 24 V) |
| 5 V PSU or DC-DC | 1 | €10 | line drivers + encoder board supply |
| End-of-travel microswitches + brackets | 4 | €25 | §4.2 improvement |
| Relays / SSRs (turret ×2, enable, spare) | 4 | €30 | DIN socketed |
| E-stop chain parts (safety relay/contactor, aux blocks, button if needed) | 1 | €60 | do not economise here; existing ABB contactors may serve after inspection |
| Spindle encoder + bracket (only if `X109` signal unusable) | 1 | €50–80 | §4.5 plan B |
| VFD 0.37 kW (only if FRC105 refuses external control) | 1 | €120 | §4.4 fallback; existing ELIN motor stays |
| Wire, ferrules, DIN rail, glands, labels | — | €50 | |
| Old-but-solid PC with Ethernet, LinuxCNC 2.9+ | 1 | €0–100 | `latency-histogram` it first; dedicated NIC for the 7i96S |
| **Total, recommended approach (no fallbacks triggered)** | | **≈ €400–550** | |
| *(Contrast: full gut — 2 motors+drives ≈ €400–700, VFD ≈ €120–300, mounts/machining, plus everything above except the drivers: total ≈ €1200–2500)* | | | |

## 8. Commissioning sequence

In order; do not skip. Doc 30 §5 safety rules apply throughout.

1. **Paper first, box open, power off.** Meter `X104` pin-by-pin (§9 R1): identify each pair,
   its polarity, `SR`, `E-Achse`; if the original CPU board still runs, scope its live `X104`
   output during a jog before removing anything — that recording is the ground truth for
   levels, polarity and timing. Identify the FRC105's control inputs. Update §5 and the INI;
   resolve or consciously defer every `UNVERIFIED`.
2. **Bench test, no machine.** 7i96S + driver board on the bench: ping, `mesaflash --device
   7i96s --addr <ip> --readhmid`, config loads, step/dir pairs toggle correctly and
   differentially on a scope at the breakout's `X104` end.
3. **Continuity and power checks, machine cold.** Mains off, cabinet isolated and proven dead.
   PSU rails verified on the bench before they touch anything. **Ribbon to `X104`
   disconnected** — prove no Mesa output can reach a motor circuit yet.
4. **E-stop chain first.** Wire and test the hardware chain with motor power still
   disconnected: the button and guard switches kill the enable relay; LinuxCNC sees it and can
   command it.
5. **Inputs live.** Low-voltage side only. Toggle every switch and Bero by hand; watch pins in
   Halshow. Fix inversions in the INI, not in your head.
6. **One axis, motors connected, other axis's ribbon lines lifted.** Connect `X104`. Small jog
   at low velocity, hand over the E-stop. Moves at all? Correct direction (fix with `SCALE`
   sign — positive X away from centreline, positive Z away from the chuck, matching the
   simulator)? No stall or power-card complaint?
7. **Scale and direction verification.** Command 10.000 mm [0.394 in], measure with a dial
   indicator; correct `SCALE` until commanded = measured over ≥ 40 mm [1.6 in] (X's whole
   travel is only 48 mm). Repeat per axis.
8. **Velocity/acceleration tuning.** Raise until the motor stalls, back off 30 %. 5-phase
   steppers report nothing when they lose steps — conservative wins; a training machine needs
   reliability, not rapids.
9. **Soft limits before homing.** Set `MIN_LIMIT`/`MAX_LIMIT` from hand-cranked measurements
   minus margin; wire and test the new end-of-travel switches; then configure homing at low
   `HOME_SEARCH_VEL` against the reference switches.
10. **Spindle.** Enable/direction/speed with an empty spindle; commanded vs tachometer rpm per
    belt step; calibrate the 0–10 V scaling. Then encoder: `spindle.0.revs` counts true, index
    once per rev, counts stay sane up to the belt step's top speed.
11. **Turret.** ClassicLadder sequence unloaded; every station indexes and locks; then `T`
    commands from MDI.
12. **Acceptance tests.** *Repeatability:* ten approaches to one X/Z position from one
    direction, dial-indicator spread ≤ 0.02 mm [0.001 in] target (**UNVERIFIED** — set from
    first results). *Test cut:* turn a diameter on aluminium bar, measure, set tool offset,
    re-cut; second diameter within 0.02–0.05 mm [0.001–0.002 in]. *Threading:* G76 an M10×1.5
    external thread at low belt step; verify with nut and gauge.
13. **Documentation.** Update §5, the INI, doc 20, and the doc 30 findings log (final entry:
    "retrofit executed, serial path closed"); wiring photo set into `docs/img/`.

## 9. Risks

| # | Risk | Impact | Mitigation |
|---|---|---|---|
| R1 | **`X104` pin-by-pin assignment and pair polarity are UNVERIFIED.** The schematic names the signals, not the pin order. | Wiring it wrong could kill the irreplaceable power card. | **Meter before connecting — mandatory** (§8 step 1). Scope the original CPU's live output if it still runs. No connection until §5 carries real pin numbers for this unit. |
| R2 | `SR` / `E-Achse` semantics wrong (current-reduction vs enable vs ready). | Motors at full current when idle, or no motion. | Trace to power-card circuitry in step 1; drive from spare outputs so polarity/role is a config change. |
| R3 | Power card or a motor already dead when the retrofit starts (may be *why* it starts). | Approach shifts to partial/full gut (§3). | Decision point explicit in §3; budget delta known in advance. |
| R4 | FRC105 won't accept external enable/speed. | Spindle sub-project becomes VFD retrofit. | Clean fallback: standard ELIN 3-phase motor + €120 VFD (§4.4). |
| R5 | `X109` spindle-encoder signal unusable for threading. | No threading until fixed. | §4.5 plan B bolt-on encoder — a planned, small job. |
| R6 | Turret sensing/reverse-lock scheme not as assumed. | Tool changes stay manual. | Machine fully usable for training meanwhile; ClassicLadder added once scoped (§4.6). |
| R7 | Differential drive levels/termination mismatch at the power card's receivers. | Missed steps, no motion. | AM26C31-class drivers are the RS-422 norm; scope at the power-card end in §8 steps 2/6; termination added if ringing shows. |
| R8 | PC latency (servo-thread overruns over Ethernet). | Following errors, watchdog trips. | `latency-histogram` before committing to a PC; dedicated NIC; 1 ms servo thread only — no software stepping. |
| R9 | Scope creep: retrofit starts while the non-invasive decision stands. | Box opened for nothing; ADR violated. | §1's triggers are the only entry conditions; changes go through [`docs/adr/0001-serial-vs-retrofit.md`](adr/0001-serial-vs-retrofit.md). |
