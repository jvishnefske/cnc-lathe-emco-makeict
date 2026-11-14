# 65 — Plan: the KD240 as the PC Turn 50's controller (CPU-board replacement)

**Status: plan, 2026-11-14. Nothing here is built, and nothing in the cabinet
is touched until step 0's decision is made.** This plan turns two documents
into one sequence of work:

- [`docs/60-retrofit-design.md`](60-retrofit-design.md): the repo's retrofit
  design. Replace only the CPU board `Y4A012000`, drive the existing stepper
  power card `Y4A018000` through ribbon `X104` with differential line
  drivers, and keep the motors, encoder, turret and E-stop chain. Its
  controller is a Mesa 7i96S and a LinuxCNC PC.
- `~/emco-pc-turn-50-control-system.md` (the owner's research notes, not in
  the repo yet): the same conclusion reached from the community record.
  Bypass the DB9, tap the internal Step/Dir ribbon behind AM26LS31-class
  drivers, read both spindle-encoder channels into hardware counters, and
  filter the turret's false first pulse. It surveys controller options
  (custom MCU, LinuxCNC + Mesa, MASSO/Acorn, Mach).

The new element is the controller itself. The **AMD Kria KD240 is already
running as this project's board** ([`kd240/README.md`](../kd240/README.md)):
NixOS on its SD card, updated over Ethernet, the web UI on port 8080, and a
listen-only RS-485 capture. Its Zynq UltraScale+ has the three tiers a
CPU-board replacement needs in one part: programmable logic for
deterministic step generation and encoder counting, two Cortex-R5F cores
for hard real-time loops, and the Linux A53s for the interpreter and UI.
It can replace both the 7i96S and the PC of doc 60.

---

## 0. Decisions and contradictions to settle first

**D1: the ADR.** [`docs/adr/0001-serial-vs-retrofit.md`](adr/0001-serial-vs-retrofit.md)
says to stay non-invasive. Doc 60 §1 opens the retrofit only on its
triggers: protocol discovery exhausted, or the electronics dead. The
owner's notes argue the DB9 route is a dead end, which is in substance
trigger 1. **Revisiting the ADR is the owner's decision**, and it should be
recorded there, not implied by this plan. Steps 1–3 below need no opened
cabinet and can go ahead either way. Step 4 onward can't.

**D2: where the two documents disagree.** The repo's facts carry source
tags from EMCO's own schematics and manuals ([SCHEM], [MANUAL]); the notes
are community-sourced, often about related machines. Until metered, the
repo's version stands, and each item goes in the step 1 measurement list:

| Topic | Repo (`_machine-facts.md`, doc 60) | Owner's notes | Plan uses |
|---|---|---|---|
| Turret stations | **6** (3 external + 3 internal) [MANUAL], "not 8" | 8-position | **6**, counted at the machine (step 1) |
| Turret drive | Maxon 24 V gearmotor 60:1 from PLC `A0.2`, reverse-lock wiring UNVERIFIED | reverse-pulse capacitor-discharge locking against a pawl | Scope it (step 1). The false-first-pulse logic applies either way |
| Spindle encoder | `Y4A020000` on `X109`: `STROBE`, `SYNC`, resolution UNVERIFIED | 1 PPR + 100 PPR channels | Count both channels on a scope at the machine before sizing counters |
| Spindle drive | `FRC105` inverter `Y4A400000`, ELIN 3-phase motor | Lenze VFD (8202-E is the PC Turn 55's) | FRC105; whether it takes an external reference is step 1's question |
| Brain board / host | CPU `Y4A012000`+`Y4A015000`; DOS PC with FANOT | AC88/AC95/ACC naming; Win95/98 + WinNC | Repo naming (doc 35: don't name code after AC95) |
| Threading limit | ≤400 rpm [MANUAL] | ~27 TPI on AC88 (user reports) | 400 rpm until the encoder is measured at speed |
| Step/Dir drivers | differential `CK±`/`DIR±` on `X104` pins 1–10, `SR`, `E-Achse` on 16 | AM26LS31CN at the ribbon; pin 9 measured as `CK Z+` | Agree. Pin order and polarity still to meter (doc 60 R1) |

**D3: the motion stack.** Two candidates, settled by the step 3 spike:

- **A. LinuxCNC on the A53, HostMot2 in the PL.** HostMot2 is Mesa's open
  FPGA firmware (stepgens, encoder counters, GPIO), the same logic as on
  the 7i96S. LinuxCNC drives it on SoC FPGAs through `hm2_soc_ol`. This
  keeps everything doc 60 and the simulator already have: the INI and HAL
  map, the `emco_lathe` screen and its rules, spindle-synchronised motion
  for G33/G76, homing, and the turret through ClassicLadder. It costs a
  PREEMPT_RT kernel (mainline since 6.12, so one more option in the kernel
  this board already cross-compiles) and a HostMot2 build for the K24's
  fabric, which nobody is known to have done for ZynqMP.
- **B. A custom stack.** Programmable-logic step generators and counters
  of our own; trajectory execution, electronic gearing for threading and
  the turret state machine on an R5F; the interpreter from `sender/core`
  on the A53. That's the "evaluate the R5F later" path. It owns
  everything and depends on no one, but it means writing the trajectory
  planner and spindle sync that LinuxCNC already has. The notes flag that
  as the large cost of the custom-MCU option.

**Recommendation: A, unless its spike fails.** The project already has a
LinuxCNC-shaped retrofit, simulator and operator training. B's real-time
work is the riskiest part of a lathe controller, and A gets it for free.
B stays the fallback, and the R5F remains useful under A for a
hardware-timestamped bus capture (`kd240/README.md`).

---

## 1. Measure (the cabinet stays shut, or is opened powered off only for inspection)

The doc 30 §5 safety rules apply, and two people are present for anything live.

1. Fill in doc 30 §7's DB9 form, and doc 60 §8 step 1's `X104` table:
   pin order, pair polarity, the `SR` and `E-Achse` roles, and the stepper
   card's `S301` DIP settings.
2. If the original CPU board still runs: scope `X104` while it jogs each
   axis (step pulse width, direction setup, rates at 750 mm/min). That
   sets the line-driver timing. **[needs the board alive]**
3. Count the spindle encoder's channels and pulses per revolution on a
   scope by turning the spindle by hand. That settles 1 + 100 PPR versus
   doc 60's "count + index".
4. Count turret stations. Scope `STROBE`/`SYNC` through an index and
   record the false first pulse (the notes give 0.56–0.68 s for the slip
   edge and ~3 s per station, on another unit). Find how the reverse lock
   is driven.
5. Identify the `FRC105` control inputs (enable, direction, speed
   reference).
6. Check electrolytics on the driver and power boards (both documents
   warn of this). Observe the inverter's discharge time before opening.

Deliverable: corrected `_machine-facts.md` / doc 60 §5 entries with real
pin numbers, plus a findings-log entry in doc 30 §8.

## 2. KD240 I/O budget (paper, no cabinet)

Map every signal in doc 60 §5 to a KD240 connector, from AMD's KD240 user
guide (UG1093) and the K24 SOM data sheet. **The KD240 connector details
below are from AMD's product material and are UNVERIFIED until read in
UG1093:**

| Need | Count | Candidate KD240 resource | Interface needed |
|---|---|---|---|
| X/Z step + dir → `X104` | 4 differential pairs | PMOD / PL GPIO (3.3 V) | AM26C31 quad RS-422 driver on a breakout board (both documents) |
| `SR` ×2, `E-Achse` | 3 outputs | PL GPIO | level shift to whatever step 1 finds |
| Spindle encoder (1 + 100 PPR or count + index) | 2 inputs | the carrier's encoder interface (ABZ, differential), if it accepts these | PL counter and timestamp capture |
| Turret `STROBE`, `SYNC` | 2 inputs | PL GPIO via opto-isolators | pull-ups per sensor type (notes: 4.7–10 kΩ for NPN), 5 V vs 24 V per step 1 |
| Reference switches + Sync-Beros | 4 inputs | PL GPIO via opto-isolators | Beros are NPN, 24 V field supply |
| New end-of-travel limits | 2 inputs | PL GPIO via opto-isolators | doc 60 §4.2 improvement |
| E-stop chain status, request | 1 in, 1 out | opto-isolated in, relay out | **the chain itself stays hardware** (doc 60 §4.3) |
| Spindle enable, direction | 2 outputs | relays | to `FRC105` or a VFD |
| Spindle speed | 1 analogue | the KD240 has no 0–10 V output: a DAC on the breakout, or PWM → isolated 0–10 V | to `FRC105` per step 1 |
| Turret forward, reverse | 2 outputs | relays | 24 V Maxon motor |
| Alarm lamp | 1 output | relay | doc 60 §4.7 `A0.7` |

Deliverable: a breakout-board schematic (AM26C31, opto inputs, relays,
DAC), reviewed against step 1's measurements before it's ordered.

## 3. Spikes on the bench (KD240 only; no lathe, no breakout)

Each spike is time-boxed, and its result goes in this document.

- **S1, real-time kernel.** Enable `PREEMPT_RT` in `kd240/nix/configuration.nix`
  (one more option in the cross-compiled kernel) and deploy it. Pass:
  `cyclictest` worst case under load, comparable to doc 60 R8's 1 ms
  servo-thread budget.
- **S2, programmable logic from NixOS.** Load a trivial bitstream (a PL GPIO
  blinker, or a counter on the encoder header) at runtime through the
  kernel's FPGA manager with a device-tree overlay, as a NixOS service, so
  PL designs ship through `nixos-rebuild` like everything else. Pass: a
  scope sees the PL pin toggle.
- **S3, HostMot2 on the K24 (decides D3).** Build HostMot2 for the K24's
  fabric with a pin file for the step 2 budget: 2–3 stepgens, one encoder,
  GPIO. Load it per S2, and run LinuxCNC with `hm2_soc_ol` against the
  existing retrofit INI (`linuxcnc/retrofit/emco-pcturn50-7i96s/`, adapted).
  Pass: LinuxCNC jogs X and Z, and a scope sees correct step/dir timing on
  the PL pins. If HostMot2 can't be built or run on ZynqMP in the time box,
  D3 falls to B.
- **S4, the operator UI.** Give `kd240`'s service a LinuxCNC transport
  next to the virtual machine (doc 35 §6 anticipated this: "Your frontend
  then talks to LinuxCNC"), so the existing web UI shows LinuxCNC's state
  and sends its commands. The simulator in `linuxcnc/sim/` is the test
  bench. The safety rule stands: no remote or unattended operation, and
  the UI's abort is advisory.
  *Prep, 2026-11-14:* the service's pump now runs any `Transport`
  (`kd240/src/machine.rs`); the virtual machine is one, and
  `kd240/src/linuxcnc.rs` is a LinuxCNC skeleton over a `Channels` trait
  shaped like LinuxCNC's Python `stat`/`command`/`error_channel`, tested
  against a fake. Its design note picks a Python sidecar on a Unix socket
  as the bridge (over `linuxcncrsh`, and over FFI to NML). Not wired into
  the service; no LinuxCNC on the board yet.

Under B, S3 becomes: PL stepgen and counter cores, R5F firmware (remoteproc
plus RPMsg from NixOS), and the R5F turret state machine, all tested
against `sender/core`'s `VirtualMachine`.

## 4. Bench bring-up with the real power card (requires D1)

Follow doc 60 §8's commissioning sequence with the KD240 and breakout in
place of the 7i96S. The cabinet's own E-stop chain is wired and tested
first. Then one axis at a time, at low step rates, motors off the load if
possible. The notes' axis-swap test (exchange the X and Z Step/Dir pairs)
isolates a bad driver channel from a bad motor.

## 5. The hard parts, each with its own acceptance test

- **Threading.** Spindle-synchronised Z feed from the encoder: both
  channels into PL counters, with index-latched position. Under A,
  LinuxCNC's spindle sync; under B, R5F electronic gearing. Accept:
  thread a test part at a low belt range; pitch checked with a gauge.
  Raise the rpm only after encoder count integrity is shown at speed
  (doc 60 §8 step 10).
- **Turret.** The state machine from the notes: sample `STROBE` before
  moving, require a full edge cycle to count a station (so the slip edge
  is never counted), stop on the target edge, reverse-lock, with a
  per-station watchdog (~3 s) and a ≥20 s homing timeout. Enforce EMCO's
  index duty limit (the notes cite EMCO alarm 6042, at most 14 swivels per interval; confirm in the alarm list, doc 10). It runs in
  the PL or on the R5F under B, or in ClassicLadder/HAL under A. Test it
  first against a recorded `STROBE`/`SYNC` trace from step 1, then on the
  machine.
- **Spindle.** `FRC105` driven from the breakout if step 1 shows it will
  take it, else the doc 60 §4.4 VFD fallback. RPM verified with an optical
  tachometer per belt range (the operator still tells the controller which
  belt is fitted).
- **X direction.** The front-mounted turret reverses ±X relative to
  rear-turret lathes (notes); set in the config and checked in the first
  jog.

## 6. What carries over, and what changes

- **Kept:** `kd240/` (NixOS image and deploys, the web UI, the capture
  instrument), doc 60's signal map and commissioning sequence, the
  simulator and `emco_lathe` screen, `sender/core` (G-code checks and
  backplot; the interpreter under B).
- **Changed:** doc 60's controller becomes "Mesa 7i96S **or** KD240 (doc
  65)". Doc 60 stays valid as the PC-and-Mesa fallback if the KD240 path
  stalls, since the breakout and measurements are shared.
- **Dropped:** nothing in the non-invasive path is removed. The capture
  tooling keeps working until D1 is decided.

## 7. Order of work

| Step | Needs | Output |
|---|---|---|
| 0 | the owner | D1 recorded in the ADR; D2 list accepted |
| 1 | shop visit, two people | measured `X104`, encoder, turret, FRC105, caps |
| 2 | step 1 | KD240 I/O map and breakout schematic |
| S1–S2 | KD240 on the bench | RT kernel numbers; PL loaded from NixOS |
| S3 | S2 | D3 decided (A or B) |
| S4 | S3 | the web UI on LinuxCNC (or on the B stack) |
| 4 | D1, steps 1–2, breakout built | axes move under the KD240 |
| 5 | step 4 | threading, turret, spindle accepted |

Steps 1, 2 and S1–S2 are independent and can run in parallel.
