# EMCO PC Turn 50 — consolidated machine facts

Extracted from the 301 reference photographs of the machine and its binders
(`docs/_photos-0614-0713.md`, `docs/_photos-0714-0813.md`, `docs/_photos-0814-0914.md`).

**This file supersedes the community-sourced assumptions the project started from.** Several of
those turned out to be wrong for this machine — see "Corrections" at the end.

Confidence tags used below:
- **[PLATE]** read off the machine's own type plate or a board silkscreen
- **[SCHEM]** read off an EMCO schematic sheet in the binders
- **[MANUAL]** stated in EMCO printed documentation
- **[UNVERIFIED]** inferred, or not yet confirmed against the physical machine

---

## 1. Identity and power

| Item | Value | Source |
| --- | --- | --- |
| Model | EMCO PC TURN 50 | [PLATE] |
| Maker | EMCO Maier Ges.m.b.H., A-5400 Hallein, Austria | [PLATE] |
| Supply | 110 / 230 V, 50/60 Hz, 1/N/PE~ | [PLATE] |
| Current | 5.5 A @ 110 V / 2.6 A @ 230 V | [PLATE] |
| Rated power | 600 W, S1 100 % ED | [PLATE] |
| Control box label | `A6M 311 000 06G` "G.E-KASTEN" | [PLATE] |
| Mass | 85 kg | [MANUAL] |
| Serial number | illegible in photographs — **read it off the machine** | [UNVERIFIED] |

Internal power: 600 VA transformer (110–240 V primary taps, 18 V 7 A secondary) →
15000 µF / 40 V reservoir → 24 VDC rail. Fuses: 6.3 A T mains and 24 V, 2.5 A T boards. Two
ABB BC6-30-01 contactors in the power section. [SCHEM]

## 2. Travels, feeds, spindle

| Item | Metric | Imperial | Source |
| --- | --- | --- | --- |
| X travel (usable) | 48 mm | 1.88 in | [MANUAL] |
| Z travel (usable) | 228 mm (275 mm mechanical) | ~9.0 in | [MANUAL] |
| Step resolution | 0.001 mm | 0.0001 in | [MANUAL] |
| Feed range | 0–750 mm/min (max 1500) | 0–30 IPM | [MANUAL] |
| Rapid traverse | 750 mm/min | 29.5–30 IPM | [MANUAL] |
| Feed force | ~600 N | — | [MANUAL] |
| Spindle speed | 130–3000 rpm, three belt steps: 130–1300 / 200–2000 / 300–3000 | [MANUAL] |
| Spindle bore | ~16 mm | 0.625 in | [MANUAL] |
| Spindle taper | MT2 | | [MANUAL] |
| Tailstock | MT1, 35 mm quill travel | 1.375 in | [MANUAL] |
| Collets | ESX 25, 1.5–14 mm | | [MANUAL] |

X is a **radius** axis in the machine's own terms but the control's default programming mode is
**diameter** (G07/G08 switch this). [MANUAL]

## 3. Board set — corrected

The community record for related EMCO models names an "AC95 brain board, `Y4B081000`".
**No such board exists in this machine.** The actual card rack, from the schematics and from
photographs of the open control box:

| Board | Part | Role |
| --- | --- | --- |
| Axis controller / CPU | `Y4A012000` (+ `Y4A015000`) | the "brain" — motion and interpreter |
| Stepper power card | `Y4A018000` | 5-phase power stage, both axes |
| Spindle inverter | `FRC105` = `Y4A400000` | spindle drive |
| PLC ("SPS 2000") | `Y4A011000` / `Y4A016000` | machine logic, digital I/O |
| DC/DC converter | `Y4A014000` | board supplies |
| Turret board | `Y4A017000` | turret motor drive |
| Turret encoder board | `Y4A020000` | turret position |
| PC interface (ISA, "PCCOM") | `Y4A013000` | lives in the DOS PC, not the box |

Stepper board connectors, from its silkscreen: `X301` X-AXIS, `X501` Z-AXIS,
`X1`/`X201` CONTROL INTERFACE, `X4` E-OFF, plus per-axis ready LEDs. Full/half step and
motor current are set by DIP switch `S301`. [PLATE]

## 4. The PC link — RS-485, not RS-232

This is the single most important correction in this document.

- The link between the DOS PC and the machine is **RS-485**, driven by the proprietary EMCO
  **`Y4A013000` 8-bit ISA card** ("PC-Einschub RS 485" on the schematic; EMCO calls this
  generation's card the "RS485-Einschubkarte (AC88)", and "PCCOM" is the later **AC95** card,
  per EMCO's DNC-interface document; doc 31 §4). [SCHEM]
- The card is **memory-mapped** on the PC side: a **2 KB** dual-port RAM window, one of
  eight in CC000–CFFFF (default **CC000–CC7FF**), jumper-selectable via ST1–ST3, with a
  selectable IRQ (manual p.45, IMG_0791; an earlier "4 KB, default CC000–CCFFF" was a
  misreading). The PC software never sees a UART. **On the wire** the link is probably
  asynchronous UART framing at an unknown rate, perhaps 9-bit address-mark (doc 31 §5);
  unmeasured. [MANUAL]
- Schematic sheet "KOMMUNIKATION RS485": ISA card `Y4A013000` → cable `K1000` → control box
  bulkhead connector **`X300`, which is the rear DB9** → CPU board → SPS 2000. It is a **3-wire
  multidrop bus**. [SCHEM]

### DB9 (`X300`) pinout — RS-485

| Pin | Signal |
| --- | --- |
| 2 | GND / signal ground |
| 3 | DATA− |
| 8 | DATA+ |

Bus termination at both ends: 330 Ω from +5 V to DATA+, 120 Ω across DATA+/DATA−,
330 Ω from DATA− to GND. [SCHEM]

> **Do not connect an RS-232 adapter to this port.** RS-485 is differential at low voltage;
> RS-232 is single-ended at up to ±12 V. Use an RS-485 (A/B) interface only.

Without the ISA card the DOS software raises alarm **"2520 ORDxx RS485 device absent"**
(also 2523, 983). DOS software lives at `C:\PCNC\FANOT`; the machine must be powered on
before the PC boots. [MANUAL]

### The other serial port — file transfer only

The 9600/7E1 settings in the binders belong to the **DOS PC's own COM1/COM2**, used only for
moving program and tool-offset *files* to and from external devices. This is **not** the machine
link and must not be confused with it. [MANUAL]

Defaults: 9600 baud, even parity, 7 data bits, 1 stop bit (DNC: 9600 E 1 8).
Range: 110–9600 (19200 shown on the alarm page), parity E/O/N, 1–2 stop bits, 7–8 data bits.
ISO transfers require 7E. Control parameter `00000001` (bit 0: end transmission with ETX).
Program terminator `%` (0x25). No handshake setting is exposed. Device select: 1/2 = COM1/COM2,
a/b/c = drives, P = LPT1. F9 = output, F10 = input. [MANUAL]

## 5. Axis drives

- **Motors: Berger Lahr `VRDM 564/50 LN`, 5-phase, 0.95 A, pentagon winding, 1000/500 steps/rev.**
  Confirmed from the schematics — the community attribution was right. [SCHEM]
- 5-phase drives are rare and expensive. Replacements are Berger Lahr D450 or Vexta UDX5128NA
  (the latter needs the 10 motor leads rewired into a pentagon). **This is the strongest argument
  for keeping the original `Y4A018000` power stage in any retrofit.**

### `X104` — the retrofit tap point

A **16-pin ribbon header on the CPU board** carrying step/direction for **both** axes to the
stepper card: [SCHEM]

| Pins | Signals |
| --- | --- |
| 1–5 | X axis: `SR`, `DIR±`, `CK±` |
| 6–10 | Z axis: `SR`, `DIR±`, `CK±` |
| 16 | `E-Achse` (enable) |

`DIR` and `CK` are **differential pairs**. A retrofit feeding this header must drive them
differentially, not single-ended. **Meter this before connecting anything.** [UNVERIFIED —
pin-by-pin assignment within each group not yet confirmed]

## 6. Homing and limits

Reconciling two sources that at first look contradictory:

- There are **no end-of-travel limit switches**. Travel ends at mechanical stops and the stepper
  simply stalls — the manual describes the audible "tack-tack". [MANUAL]
- There **are** reference/homing sensors: per axis a mechanical reference switch (Schmersal or
  Omron) plus a **Balluff `BES 516-343-E0-X`** NPN inductive "Sync-Bero". [SCHEM]
- The reference point must be re-approached after **every** power-off — there is no absolute
  position memory. [MANUAL]
- The only safety interlock switches are on the **chip guard** and the **wheel cover**. [MANUAL]

So: homing works off reference sensors; over-travel protection is mechanical and relies on the
stepper losing steps. A retrofit should treat added end-of-travel limits as a real improvement,
not merely a like-for-like replacement.

## 7. Spindle

- Motor: **ELIN 3-phase, 0.37 kW, 1370 rpm, 220/380 V, IP54**, driven by the `FRC105`
  inverter from a 230 V bus. [SCHEM]
- Manual elsewhere quotes 370/550 W and ~7 Nm at the spindle. [MANUAL]
- **A spindle encoder exists.** Encoder board `Y4A020000` feeds CPU connector **`X109`** with
  `+5 V`, `GND`, `STROBE`, `SYNC`. [SCHEM]
- Threading is supported (G33, G78 two-block format) but limited to **≤400 rpm**; alarm 8010
  is "main drive synchronising mark not found", and the belt position must be entered for
  thread-cutting sync. Alarm 8020 fires at 80 % of max thread feed. [MANUAL]
- **Encoder resolution and channel count are not determinable from the photographs.** The
  `STROBE`/`SYNC` naming suggests one index pulse plus a count channel, but this needs
  confirming at the machine before any threading claim is made. [UNVERIFIED]

## 8. Turret

- **6 stations: 3 external + 3 internal (ø10 mm boring bars).** Not 8 — correct any config that
  assumes 8. [MANUAL]
- Max tool 12 mm (0.472 in), max stickout 12.7 mm (0.50 in). [MANUAL]
- Motor: **Maxon 24 VDC gearmotor, 60:1**, driven from PLC output `A0.2` "forward". [SCHEM]
- Position sensed via `Strobe`/`Sync` from the turret encoder board `Y4A020000`. [SCHEM]
- The turret is an **option** and requires the extra SPS/PLC card. Turret alarms 6040–6046. [MANUAL]
- Operator indexes manually with `CONTROL`+`1` in the DOS software. [MANUAL]

## 9. Digital I/O ("robotic interface")

20–24 V logic, outputs rated 0.2 A. [MANUAL]

| Output | Function | | Input | Function |
| --- | --- | --- | --- | --- |
| `A0.0` | program stop | | `E3.0`–`E3.7` | door, tailstock, chuck, feed stop |
| `A0.1` / `A0.3` | chuck open / closed | | | |
| `A0.4` / `A0.5` | door | | | |
| `A0.6` | tailstock | | | |
| `A0.7` | alarm | | | |

## 10. Corrections to the assumptions this project began with

| Assumption at project start | Reality |
| --- | --- |
| DB9 is RS-232 | **RS-485**, connector `X300`, pins 2/3/8 |
| PC sent G-code text with CTS handshaking | PC used a **memory-mapped ISA card**; no UART, no text stream |
| Brain board is "AC95" `Y4B081000` | Axis controller / CPU is **`Y4A012000`** (+ `Y4A015000`) |
| 8-station turret | **6 stations** (3 OD + 3 ID) |
| Berger Lahr 5-phase steppers | **Confirmed** — `VRDM 564/50 LN` |
| `Y4A018000` stepper card, `Y4A400000` spindle | **Confirmed** |
| `X104` carries step/dir | **Confirmed**, and now with a pinout — both axes, differential |
| Unknown whether a spindle encoder exists | **It exists** — board `Y4A020000` → CPU `X109` |
| No home switches | Reference sensors **do** exist; end-of-travel limits do **not** |

## 11. Still unknown — take this list to the machine

1. Machine serial number and build year (type plate photograph is illegible).
2. Spindle encoder resolution and channel count.
3. Pin-by-pin assignment within each `X104` axis group, and the differential polarity.
4. Whether the optional 6-station turret and its SPS card are actually fitted to **this** unit.
5. Turret reverse-lock wiring.
6. Stop-bit count for ISO file transfer (one source illegible, one says 1).
7. Whether the original EMCO DOS software and the `Y4A013000` ISA card still exist with the machine.
