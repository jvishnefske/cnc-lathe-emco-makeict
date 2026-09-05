# 30 — The PC↔machine link: specification and working log

**Status: living document, substantially revised 2026-09-04.** The photo catalogue of the
machine's own manuals and electrical documentation — consolidated in
[`docs/_machine-facts.md`](_machine-facts.md), with the underlying page-by-page transcriptions
in [`docs/_photos-0614-0713.md`](_photos-0614-0713.md),
[`docs/_photos-0714-0813.md`](_photos-0714-0813.md) and
[`docs/_photos-0814-0914.md`](_photos-0814-0914.md) — has overturned the community-sourced
premise this project started from. **The link is RS-485, not RS-232, and it does not carry
G-code.** This page records what is now known, what remains assumed, and how discovery proceeds
from here. The risk of the non-invasive approach is concentrated in this document, and that risk
is now assessed as **high** — see the honest position statement in §4.3 and the ADR
([`docs/adr/0001-serial-vs-retrofit.md`](adr/0001-serial-vs-retrofit.md)).

Related documents:

- [`docs/20-control-architecture.md`](20-control-architecture.md) — what is inside the red box
- [`docs/60-retrofit-design.md`](60-retrofit-design.md) — the escape hatch this page may end up
  pointing at
- [`docs/70-simulator.md`](70-simulator.md) — the LinuxCNC training simulator
- [`sender/INTERFACE.md`](../sender/INTERFACE.md) — the sender's software contract (`Dialect`
  trait, `MachineProfile`); see §6 for what this evidence means for it

---

## 1. What the link actually is

From the machine's own documentation (machine-description manual, valid from 1.1.1993; software
description EN 0091 v2.0; electrical documentation "VS 4301, Edition 93-5" with full schematics —
all photographed with the machine). These are documentary facts, **from manual/schematic, not yet
re-measured on this unit**.

### 1.1 Topology

```
  DOS PC (386/486, MS-DOS)
  ┌─────────────────────────────┐
  │ FANOT / PCNC software        │  EMCO "Control System GE FANUC Series 0-TC" v2.0
  │   │  writes/reads            │  (the Fanuc-0T interpreter, planner, screens — ALL on the PC)
  │   ▼                          │
  │ ISA card Y4A013000           │  8-bit ISA, memory-mapped: 4 KB dual-port RAM window,
  │ "FC-Einschub RS485"          │  default CC000–CCFFF, jumpers ST1–ST3 (CC000–CFFFF).
  │  [terminator plug][cable]    │  NOT a UART. Card hardware serialises onto the wire.
  └───────┬─────────────────────┘
          │ K1000: 9-pole cable, 3-wire half-duplex RS-485
          ▼
  DB9 "X300" on the red control box   ◄── the connector we can reach
          │ K1010
          ▼
  Axiscontroller CPU board Y4A012000 (+Y4A015000)  — connector X2000
          │ K1020 (daisy chain)
          ▼
  SPS 2000 PLC board Y4A011000 — connector X201
  (bus terminated/biased at both ends: 330 Ω to +5 V, 120 Ω across, 330 Ω to GND)
```

Other devices could sit on the same bus: the optional EMCO control keyboard takes an RS-485
address 0–15 (`ET0:adr` install parameter), and alarm texts name bus devices "AC, SPS, MT, PLC,
control keyboard, axis controller". This is a **multidrop device bus**, with the PC card as
master. Without the card, the software raises alarm **"2520 ORDxx RS485 device absent"**.

### 1.2 The DB9 (X300) pinout — from schematic sheet "KOMMUNIKATION RS485"

| Pin | Function (schematic) | Note |
|-----|---------------------|------|
| 2 | GND / SIGN (signal ground) | |
| 3 | DATA− | RS-485 B/A naming varies; polarity per schematic |
| 8 | DATA+ | |
| 5 | GND | present at terminated ends (bias network return) |
| 9 | +5 V | present at terminated ends (bias network supply) |
| 1, 4, 6, 7 | not used | per schematic |

**Verify by metering before connecting anything** (§7). And a hard warning: **never plug an
RS-232 device into this connector.** RS-232 drives ±12 V; the RS-485 transceivers and the +5 V
on pin 9 are not built for it. The community claim of "RX/TX/CTS/GND" RS-232 on this DB9 is
**refuted** for this machine.

### 1.3 The machine's RS-232 that does exist — and what it is for

The manuals do describe RS-232: **COM1/COM2 on the DOS PC itself**, used only to transfer part
programs and tool-offset files between the PC and external devices (F9 = output, F10 = input;
device 1=COM1, 2=COM2, A/B/C = drives). Fixed settings: **9600 baud, ISO code, 7 data bits,
even parity** (stop bits: table row illegible in the photo, WinNC screens elsewhere show 1 —
re-check the physical page). This is a file-transfer port on a computer that no longer exists.
It never carried motion control and it is not the DB9 on the cabinet.

---

## 2. Known versus assumed

Be pedantic about the difference; a volunteer will stand next to a spinning chuck on the strength
of this section.

### 2.1 Known — verified on this machine or read from its own documentation

"Doc" = from the photographed manuals/schematics that shipped with this machine (strong, but not
re-measured on this unit). "Us" = physically verified by us.

| # | Fact | Source |
|---|------|--------|
| K1 | EMCO PC Turn 50; control cabinet with keyswitch, DB9 female, fused IEC inlet | Us (photos IMG_0614/0615) |
| K2 | The original DOS host PC and its ISA interface card are gone | Us |
| K3 | PC↔machine link is 3-wire half-duplex RS-485 multidrop; DB9 pinout per §1.2 | Doc (IMG_0740, IMG_0902) |
| K4 | The PC-side interface is a memory-mapped ISA card (Y4A013000), 4 KB dual-port window CC000–CFFFF; not a COM port | Doc (IMG_0791, IMG_0892) |
| K5 | The Fanuc-0T interpreter, planner and UI all ran on the PC (`FANOT`, `C:\PCNC`); the cabinet holds an axis-controller CPU (Y4A012000+Y4A015000), an SPS 2000 PLC (Y4A011000), a stepper power board (Y4A018000) and an FRC105 spindle inverter (Y4A400000) | Doc (IMG_0790, IMG_0897) |
| K6 | Axis motors: Berger Lahr VRDM 564/50LN 5-phase steppers, 0.95 A, 500/1000 steps/rev (full/half) | Doc (IMG_0894, IMG_0905) |
| K7 | X travel 48 mm [1.89 in] usable; Z 228 mm [8.97 in] usable (275 mm mechanical); feed 0–750 mm/min, max 1500 mm/min; spindle 130–3000 rpm in 3 belt ranges; threading ≤400 rpm | Doc (IMG_0735, IMG_0744, IMG_0750, IMG_0714) |
| K8 | No axis over-travel limit switches — travel ends at mechanical stops (stepper stalls, "tack-tack"); but each axis has a reference switch and an inductive sync sensor for homing, and a spindle encoder (STROBE + SYNC) exists on CPU input X109 | Doc (IMG_0744, IMG_0894, IMG_0904) |
| K9 | Serial settings 9600/7/E belong to the PC's own COM ports, for file transfer only (§1.3) | Doc (IMG_0772, IMG_0882) |
| K10 | Alarm list includes RS-485 comms alarms 2520–2528 and "983 RS485 ERROR - INITIALIZING PC BOARD" | Doc (IMG_0792/0793, IMG_0658) |

### 2.2 Assumed / unknown — the honest gap list

| # | Item | Status |
|---|------|--------|
| A1 | Wire protocol on the RS-485 bus: framing, bit rate, addressing, message set | **Completely unknown.** Serialisation is done by card hardware from dual-port RAM; it may not even be async-UART framed. No public documentation is known. |
| A2 | Whether any bus device transmits unsolicited (e.g. at power-up) | Unknown; PC card is presumed master, slaves presumed silent until polled |
| A3 | Community name "AC95", part `Y4B081000` for the brain board | **Not confirmed.** This unit's electrical doc names the CPU "Axiscontroller" Y4A012000 + Y4A015000. Treat "AC95/Y4B081000" as community hearsay about related EMCO models until a board photo of *this* cabinet says otherwise. |
| A4 | Command semantics: does the PC send blocks, or low-level motion (step streams, segment setpoints)? | Unknown. Given K5 (interpreter on the PC), expect **low-level**; alarm 315/325/335 texts ("step motor fell out of step") imply the CPU supervises motor rotation, so the CPU does at least local motion execution. |
| A5 | DB9 electrical state on this physical unit | Not yet metered — do §7 before any connection |
| A6 | Stop bits on the COM1/COM2 file-transfer table | Manual photo illegible (probably 1); irrelevant to the machine link |

---

## 3. The original hypotheses, and the verdict

The project began with two competing hypotheses about the wire. Both are recorded here with the
outcome, because the reasoning is instructive and because the community record that spawned
hypothesis A is still circulating.

### Hypothesis A — plain G-code text over RS-232 with CTS/XON gating: **REFUTED**

This was the community's best guess ("DB9 uses RX/TX/CTS/GND, machine raises CTS to ask for
data") and the basis of the sender's `PlainGcode` dialect. The machine's own manuals refute it
three ways: the interface is explicitly RS-485 ("the interface is a serial interface RS 485",
machine-description manual p12); the PC side is a memory-mapped proprietary card, not a UART; and
the only RS-232 in the system is a PC-side file-transfer port (§1.3), which is almost certainly
where the community's 9600/7/E folklore came from. **Nobody should wire an RS-232 probe to this
machine expecting G-code.**

### Hypothesis B — lower-level commands because the interpreter ran on the PC: **CONFIRMED in substance**

The Fanuc-0T emulation — interpreter, cycles, screens — demonstrably ran on the PC (K5). The
cabinet is an executor: an axis-controller CPU driving a stepper power stage, plus a PLC. The
wire carries a proprietary multidrop device protocol between them. The pessimistic version of
this hypothesis is the one that came true: there is **no reference implementation we possess, no
documentation of the framing, and the original transmitting hardware is unobtainium** (the ISA
card left with the dead PC).

### What this means

| | Before 2026-09-04 | Now |
|---|---|---|
| Transport | RS-232, Web Serial-friendly | RS-485 half-duplex; reachable electrically with a USB-RS485 adapter (the shop already has MAX485 modules — photo IMG_0616), but transport was never the hard part |
| Payload | plausibly G-code text | proprietary, unknown framing, possibly not UART-async at all |
| Reference implementation | "find the DOS floppies, run under DOSBox" | requires the DOS software **and** the proprietary ISA card **and** an ISA PC — or emulating the card's dual-port RAM, which still doesn't reveal the wire encoding the card hardware generates |
| Tractability | days-to-weeks | **may not be tractable at all**; see §4.3 |

This substantially strengthens the retrofit case. The project owner's decision to stay
non-invasive for now stands — the box stays shut, nothing is gutted — but the retrofit
([`docs/60-retrofit-design.md`](60-retrofit-design.md)) is now the more likely eventual outcome,
and the trigger conditions in §4.4 and the ADR are the tripwire.

---

## 4. Discovery procedure — the revised ladder

Safest and highest-value first. Every session gets a findings-log entry (§8), null results
included.

### Rung 1 — archaeology: software, card, and papers (zero risk, highest value)

The only known route to a reference implementation is the original equipment:

1. **Search everything that came with the machine** for floppies (`FANOT`, `PCNC`, "machine-data
   disk", WinNC, SPS/turret disk), the ISA card Y4A013000 (a full-length 8-bit card with two
   9-pin sockets and a terminator plug), manuals not yet photographed, and any old PC carcass.
2. Ask the EMCO user community (CNC-Ecke, PraktikerForum, CNCzone EMCO threads) for: the FANOT /
   WinNC software, a spare Y4A013000 card, or — best of all — any protocol notes. EMCO Austria
   has historically answered polite requests about obsolete trainers.
3. If card + software are obtained: build a period ISA PC (386/486 or an industrial ISA
   backplane), run the real stack against the real machine, and **passively tap the RS-485 bus**
   (USB-RS485 adapter in receive-only, or a logic analyser across DATA+/DATA−) while exercising
   every function. That capture makes everything else on this page easy.
4. Software alone (no card): run it under an emulator (86Box/QEMU — DOSBox-X cannot map a custom
   ISA memory window without patching) with a **custom device emulating the 4 KB dual-port RAM
   at CC000**. Logging FANOT's reads/writes reveals the *command semantics* — message layouts,
   polling structure, what a "go" looks like — even though the wire *encoding* stays hidden in
   the card hardware. That is still most of the reverse-engineering battle. The
   `tools/dosbox-capture/` harness documents this route step by step (86Box/PCem/QEMU device
   models, what to log, what to extract); a scripted responder in the spirit of
   `tools/virtual-emco.py` is the natural next step once first accesses are captured. Nobody
   has done this yet — it is the most promising avenue on this page.

### Rung 2 — passive listen on the bus (low risk)

Expectation management: the PC card was almost certainly bus master, so with no master present
the slaves are probably silent. But the CPU↔SPS daisy chain might carry traffic, and power-up
behaviour is unknown (A2). Cheap to check:

1. Meter the DB9 first (§7). Confirm pins 3/8 look like a biased differential pair
   (idle differential of roughly +0.2–1 V from the bias network) and that pin 9 carries +5 V.
2. Attach a USB-RS485 adapter (A/B to pins 8/3, ground to pin 2) **receive-only**: TX disabled
   or the adapter's driver-enable held off. `tools/emco-listen.py` sweeps candidate bit rates
   and framings across repeated power cycles and logs raw bytes and timing. A logic analyser on
   DATA± is better than a UART sweep, because the framing may not be async at all — capture
   edges, then decide what it is.
3. Deliverable: either "bus is silent without a master" (a real finding — log it) or captured
   frames, which would be gold.

### Rung 3 — guarded active probing (managed risk, low expected value)

Only worthwhile with material from rung 1 or 2 to imitate; blind probing of an unknown multidrop
protocol with unknown framing is close to pointless and is the riskiest activity on this page.
If attempted: `tools/emco-probe.py` sends single reviewed frames, full §5 safety rules, two
people, one change per session, everything logged. Replaying captured master frames from rung 1
is the only probing strategy with a realistic chance.

### Rung 4 — decide

If rung 1 produces neither software nor card within a reasonable search (suggested budget: one
season of forum/archive hunting alongside other work), and rung 2 finds a silent bus, the
non-invasive path has no remaining moves. Take the decision through the ADR's revisit clause and
execute [`docs/60-retrofit-design.md`](60-retrofit-design.md). The same applies immediately if
the cabinet electronics are found dead.

### 4.3 Honest position statement

With the ISA card gone, this machine's original control scheme cannot run, full stop — the
protocol question is now *reverse-engineer a proprietary bus with no transmitter to observe*,
which may be intractable. The realistic success path is narrow: find the software (semantics via
RAM-window emulation) **and** ideally the card (wire encoding via passive tap). The sender
project remains fully useful against the simulator
([`docs/70-simulator.md`](70-simulator.md)) and as the operator UI for whichever control path
wins, but nobody should promise the shop that the real lathe will move over this DB9 soon.

---

## 5. Safety rules for work near the machine

These apply to rungs 2 and 3 and to all later testing against the real machine. The lathe can
move a carriage and spin a chuck under program control; treat every probe as if it will.

1. **Two people.** One at the keyboard; one at the machine within reach of the stops, watching
   the chuck and carriage, doing nothing else.
2. **Know your stop, and it is not software.** The EMERGENCY-OFF and the keyswitch/mains on the
   cabinet are the stop (the E-stop chain drops the motor contactors — that is real). The
   sender's abort is advisory only; `sender/INTERFACE.md` says the same. Rehearse physically
   hitting the stop before the first active probe.
3. **Empty machine.** No workpiece. The chuck key never stays in the chuck. Turret clear of the
   chuck, tailstock retracted.
4. **Carriage mid-travel** before probing, so an unexpected move has room in both directions.
   (The manual says over-travel only stalls the stepper against mechanical stops — do not rely
   on that as a feature.)
5. **No loose clothing, no gloves near rotating parts, hair tied back, eye protection** whenever
   mains is on.
6. **Hands out of the work envelope** whenever the cabinet is powered — not just when motion is
   expected. The premise of probing is that we do not know what it will do.
7. **Electrical discipline at the DB9:** meter before connecting; RS-485 adapters only, never
   RS-232 line drivers; mind the +5 V on pin 9. Power down between cable reconfigurations.
8. **One change per probe**, predicted in writing first.
9. **No unattended operation, ever** — including "just a long capture while I get coffee" with
   mains on.
10. **Log everything** (§8). A null result someone can trust is a result.

---

## 6. Consequences for the sender (`sender/INTERFACE.md`)

- The `PlainGcode` dialect's premise (CR-terminated G-code, CTS gating) is now known to be wrong
  for the real machine. It remains the right language for the **simulator and training** use of
  the sender, which is unaffected.
- The `Dialect` trait is still exactly the right seam: a future `emco-485` dialect (framing from
  rung 1/2 findings) slots in without touching `Session`, the UI or the backplot. The
  `MachineProfile.dialect` value `"emco"` should be read as "the real-machine dialect,
  currently unimplementable"; `MachineProfile.link` records the transport truth (`kind:
  "rs485"`, bit rate unknown), and Web Serial would only ever see it through a USB-RS485
  adapter, *if* the framing turns out to be async-UART-compatible at all.
- Web Serial can talk to USB-RS485 CDC adapters, so the browser transport is not the blocker;
  the protocol is. `INTERFACE.md`'s note describing `PlainGcode` as "the community's best guess
  at the AC95" needs revision by its owners to cite this document's verdict.

---

## 7. DB9 (X300) metering form — fill in at the machine

Machine powered, nothing connected to the DB9. DMM referenced to pin 2 unless stated. Expected
values from the schematic (§1.2); the point of the exercise is to confirm this unit matches its
paperwork.

| Pin | Expected | Measured (V) | Continuity to | Matches? |
|-----|----------|--------------|---------------|----------|
| 1 | n/c | | | ☐ |
| 2 | GND/SIGN | 0 (reference) | cabinet PE? ☐ | ☐ |
| 3 | DATA− (biased low-ish) | | | ☐ |
| 4 | n/c | | | ☐ |
| 5 | GND | ~0 | pin 2 ☐ | ☐ |
| 6 | n/c | | | ☐ |
| 7 | n/c | | | ☐ |
| 8 | DATA+ (biased high-ish) | | | ☐ |
| 9 | +5 V | ~5 | | ☐ |

Also record: connector gender ☐ female (expected) ☐ male; differential voltage pin 8→3 at idle:
______ V (bus bias predicts a small positive value); any change when the keyswitch is cycled.

Cabling note: the old null-modem-vs-straight-through question is **obsolete** — this is not
RS-232. The correct connection is a purpose-made tap: adapter A/B (or D+/D−) to pins 8/3, signal
ground to pin 2, nothing on pin 9. Label the cable "RS-485 ONLY — NOT RS-232" and photograph it
for `docs/img/`.

Bit-rate sweep for rung 2, in case the framing is async UART (**assumption A1 — it may not
be**): 9600, 19200, 38400, 57600, 115200, 4800, 2400 at 8N1 and 7E1. A logic-analyser edge
capture beats the sweep and should be preferred when available.

---

## 8. Findings log

Append-only, newest first. Template:

> ### YYYY-MM-DD — short title
> - **Who:** names/initials
> - **Rung:** 1 / 2 / 3 (per §4)
> - **Setup:** hardware, tool + version/commit, machine power state
> - **Did:** what was actually done, exact frames if rung 3
> - **Observed:** raw facts only — capture-file paths (under `tools/captures/`), voltages,
>   analyser screenshots, machine behaviour
> - **Interpretation:** clearly separated from Observed; may be wrong
> - **Promotions:** any §2.2 item resolved, or §2.1 item contradicted
> - **Next:** the single most useful next step

### 2026-09-04 — Photo catalogue overturns the RS-232 premise

- **Who:** documentation team (photo catalogue agents + doc revision)
- **Rung:** 1 (archaeology — the machine's own binders)
- **Setup:** 301 reference photos of the manuals and electrical documentation shipped with the
  machine, catalogued in `docs/_photos-0614-0713.md`, `_photos-0714-0813.md`,
  `_photos-0814-0914.md`
- **Did:** read and transcribed the Start-Up Guide, the Software Description EN 0091 v2.0
  (1993), the machine-description manual, the Classroom Guide, and the complete electrical
  documentation VS 4301 Edition 93-5 including the "KOMMUNIKATION RS485" schematic
- **Observed:** PC↔machine link is 3-wire half-duplex RS-485 (DB9 pins 2/3/8 = GND/DATA−/DATA+,
  5 V on pin 9 at terminated ends); PC-side interface is a memory-mapped 8-bit ISA card
  Y4A013000 (dual-port RAM window CC000–CFFFF); software `FANOT` runs the whole Fanuc-0T
  emulation on the PC; alarm "2520 ORDxx RS485 device absent" without the card; COM1/COM2
  9600/7/E is a separate file-transfer function; cabinet boards identified (Y4A012000 CPU,
  Y4A011000 SPS, Y4A018000 stepper, Y4A400000 FRC105); X104 ribbon pinout documented; motors
  Berger Lahr VRDM 564/50LN
- **Interpretation:** Hypothesis A (G-code over RS-232) refuted; Hypothesis B confirmed in
  substance; protocol discovery now requires the original software and ideally the original ISA
  card; retrofit likelihood substantially increased
- **Promotions:** K3–K10 promoted into §2.1; former assumptions about RS-232 pinout and "AC95
  Y4B081000" demoted/corrected (see A3)
- **Next:** archaeology (rung 1): search the machine's accessories for floppies and the ISA
  card; meter the DB9 (§7) on a shop visit

---

## 9. Open questions — take this list to the shop

1. Are there floppies, a software box, or (jackpot) the old PC or its ISA card Y4A013000
   anywhere with the machine? Photograph every label. Look for `FANOT`/`PCNC` disks and the
   "machine-data disk" and SPS/turret disk the install chapter mentions.
2. Meter the DB9 per §7 and fill in the form. Does this unit match the VS 4301 schematic?
3. Does anything appear on the bus at power-up (rung 2)? Even "silent" is a finding.
4. Type plate close-up: serial number and year are illegible in IMG_0615/IMG_0914 — photograph
   them properly.
5. The COM1/COM2 stop-bits table row (Software Description, ch. A, "Parameter of serial
   interface") was illegible — re-photograph that page.
6. Does the turret index when the machine is powered standalone (keyswitch on, no PC)? Any signs
   of life — fans, LEDs visible through vents, relays clicking?
7. Which belt position is the spindle currently in (1/2/3)? (Needed later for any control.)
8. Confirm the chip-guard and wheel-cover safety switches function (E-stop chain, doc 60 §4.2).
9. Is the tailstock the manual or the electromechanical variant?
10. Any EMCO control keyboard (accessory 795 000) in the shop? It is an RS-485 bus device — a
    second protocol specimen and potentially a working bus master for study.
