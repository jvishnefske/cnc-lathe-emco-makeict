# 90 — Photo index: the 301 reference photographs

All 301 photographs (IMG_0614–IMG_0914) of the machine, its binders and its electrical
documentation, merged from the three per-range catalogues and regrouped **by subject** so you
can find a source fast. Full page-by-page transcriptions and confidence notes remain in
[`_photos-0614-0713.md`](_photos-0614-0713.md), [`_photos-0714-0813.md`](_photos-0714-0813.md)
and [`_photos-0814-0914.md`](_photos-0814-0914.md).

**The photographs are not in git.** They live in `reference/iCloud Photos/` (filenames
`IMG_xxxx.JPEG`). A curated, downscaled selection is committed in `docs/img/` — see §11.

Orientation warning: most binder pages were photographed rotated — Binder A sideways, Binder B
and the training binders upside-down. The curated copies are rotated upright.

---

## 1. Fast finder

| Looking for… | Photo(s) | Curated copy |
| --- | --- | --- |
| **RS-485 wiring schematic** (PC card → DB9 X300 → CPU → SPS, pinout, termination) | IMG_0902 | `img/c-rs485-comm-schematic.jpg` |
| The DB9 / control-box rear itself | IMG_0615, IMG_0914 | `img/control-box-rear-type-plate.jpg`, `img/c-control-box-rear.jpg` |
| ISA card (PCCOM) memory map and jumpers | IMG_0791 (also 0629, 0630, 0676) | `img/b-interface-card-jumpers.jpg` |
| "The interface is RS 485" statement, card + terminator install | IMG_0740 (also 0736, 0737, 0741) | `img/b-rs485-pc-connection.jpg` |
| **X104 step/direction ribbon pinout** | IMG_0906 (X), IMG_0908 (Z) | `img/c-xaxis-control-x104.jpg` |
| Board part numbers / mounting-panel layout | IMG_0897, IMG_0901 | `img/c-mounting-panel-layout.jpg` |
| **Alarm lists (DOS software, complete)** | IMG_0792, IMG_0793, IMG_0794 | `img/b-alarm-list-rs485.jpg` (0793) |
| Alarm lists (WinNC) | IMG_0631–0632, IMG_0657–0661 | — |
| **G78 threading format + example, 400 rpm limit** | IMG_0714, IMG_0715, IMG_0782 | — |
| Belt speed ranges (130–1300 / 200–2000 / 300–3000) | IMG_0750 (also 0639, 0807) | — |
| Machine technical data tables | IMG_0735 (metric), IMG_0682 (US), IMG_0809 | `img/b-technical-data-page.jpg`, `img/manual-machine-technical-data.jpg` |
| No-limit-switch / "tack-tack" statement | IMG_0744 | `img/b-step-motors-page.jpg` |
| Robotic interface digital I/O table | IMG_0662 | `img/manual-robotic-interface-io.jpg` |
| Serial file-transfer settings (9600/7/E — PC COM ports, not the machine link) | IMG_0882 (also 0640, 0772) | `img/c-serial-params-page.jpg`, `img/manual-rs232-settings.jpg` |
| Stepper / sensor / turret-motor part numbers | IMG_0894 (also 0891–0893) | `img/c-parts-list-steppers.jpg` |
| Spindle encoder schematic (X109 STROBE/SYNC) | IMG_0904 | `img/c-spindle-control-encoder.jpg` |
| Work-shift offset procedure (local binder) | IMG_0687–0688, 0700 (also 0721–0722, 0830) | — |
| Maintenance / slideway oil page | IMG_0756 | — |

## 2. Machine hardware (photographs of the actual machine)

| File | Contents |
| --- | --- |
| IMG_0614 | Front view: enclosure, chuck, 6-station turret behind guard, "PC TURN 50" logo |
| IMG_0615 | Control box rear: type plate (110/230 V, 5.5/2.6 A, 600 W), keyswitch, DB9 female, fused IEC inlet |
| IMG_0617 | Control box interior, power section: 2× ABB BC6-30-01 contactors, transformer, fuse bank F1–F4, terminal strip |
| IMG_0618 | Card rack: STEPMOTORBOARD Y4A 018 000, MD CONTROL board, SPS Y4A 016 000 — silkscreens legible |
| IMG_0624, IMG_0625 | EMCO quick-change toolholders (0625 stamping blurred) |
| IMG_0914 | Control box rear again: key switch, DB9, IEC inlet, type plate (values unreadable) |

## 3. Non-EMCO items photographed alongside (context only)

| File | Contents |
| --- | --- |
| IMG_0616 | Breadboard with five MAX485 RS-485 modules — a member's interface experiment |
| IMG_0619 | "D&M Computing CONTROLLER M4+4-1" PCB — not EMCO |
| IMG_0620 | HobbyCNC 3-axis unipolar stepper driver board — replacement-drive candidate, not original |
| IMG_0621, IMG_0623 | MTS Systems letter / unrelated PCB and software box |
| IMG_0622 | Box with EMCO binder, DB25 cable, wall-wart PSU, toolholders, backplane-style PCB strip |
| IMG_0626 | Condor 12 VDC 500 mA plug-in transformer |
| IMG_0767, IMG_0889 | Blank pages / binder dividers |

## 4. Binder A — EMCO WinNC software description (factory, 1996) — IMG_0627–0680

The later Windows-era software manual; useful for the PCCOM card, alarms and the robotic
interface. Pages photographed sideways.

| Files | Topic |
| --- | --- |
| IMG_0627–0628 | Title; version notes (PC TURN 50 under WinNC v3; minimum PC specs) |
| IMG_0629–0630 | **PCCOM ISA card**: memory-area jumper table CC000–CFFFF, IRQ configuration, emm386 note |
| IMG_0631–0632 | WinNC alarm lists 8101–8211 (incl. threading alarms 8194–8197, 8211) |
| IMG_0633–0635 | Contents; key descriptions; PC-keyboard mapping |
| IMG_0636–0638 | Reference points M/N/R/W; zero offset; tool measurement (PC TURN 50 "ZN + 30 mm") |
| IMG_0639 | Operating modes; **belt/gear position entry: 130–1300 / 200–2000 / 300–3000 rpm** |
| IMG_0640–0642 | **RS-232 serial parameter page**; program I/O; part counter, graphics |
| IMG_0643–0656 | G/M-code chapters: survey, G00–G99 details, M-codes, subprograms, drawing input |
| IMG_0657–0661 | Alarm chapters: startup 0001–0040, RS-485 ORDxx errors, **machine alarms 6000–7053** |
| IMG_0662–0664 | **Robotic interface I/O table (A0.x/E3.x)**; PC TURN 120 variant (blurry); DNC, turret, puff blowing |
| IMG_0665–0670 | WinConfig chapters (interfaces incl. RS485 device list, MSD data, accessory activation) |
| IMG_0671–0675 | Software installation (Fanuc 0 "command group C" selection, licences, network) |
| IMG_0676 | **PCCOM ISA card** jumpers ST1–ST3 + RS-485 alarms 2523/983 |
| IMG_0677–0679 | EMCO control keyboard accessory: ref. numbers, assembly, **DB9 RS-232/RS-485 pinouts** |
| IMG_0680 | WinNC start screen |

## 5. Binder B — locally written US operating procedure (DOS `PCNC`/`FANOT`) — IMG_0681–0713

The shop's own procedure manual for the software that actually ran this machine. Photographed
upside-down. This is the source of the operator workflow modernised in
[`40-operator-guide.md`](40-operator-guide.md).

| Files | Topic |
| --- | --- |
| IMG_0681 | Table of contents |
| IMG_0682 | **Machine technical data table (US units)** |
| IMG_0683 | Software install: ISA card, boot order, alarm 2520, incoming-power set to 100 |
| IMG_0684–0686 | Machine logic (zero points, turret stampings); initial start-up (`CD\PCNC`, `FANOT`); F1 screen + **jog keypad map, F7 = reference** |
| IMG_0687–0688 | **Work-shift procedure steps 11–28**: CONTROL+1 turret index, OFFSET → WORK SHIFT, program load, keypad 0 = E-stop, * = single block |
| IMG_0689–0691 | Editor: ASCII programs, program numbers, word insert/delete |
| IMG_0692–0693 | MDI mode: modal defaults screen; S/T commands; scrape method for Z work offset |
| IMG_0694–0697 | Repeats of boot screen / keypad / work-shift pages |
| IMG_0698–0703 | **Turret tooling (6 stations, 0.50 in stickout)**; tool geometry offsets and setting procedure |
| IMG_0704–0713 | Programming chapter: DIN 66025 addresses; **G-code and M-code summaries**; G00–G76 formats and examples; G21 thread cycle (F = pitch) |

## 6. EMCO PC TURN 50 Start-Up Guide (EMCO Maier Corp, Columbus OH) — IMG_0714–0730

Photographed upside-down; pages out of order (G78 pages photographed before the cover).

| Files | Topic |
| --- | --- |
| IMG_0714–0715 | **G78 two-block threading format + worked example; "don't thread above 400 RPM"** |
| IMG_0716–0717 | G80, G83, G84, G85 cycles |
| IMG_0718–0719 | Cover; install steps, alarm 2520, incoming-power adjustment |
| IMG_0720–0726 | Operating modes; **work-shift concept and procedure (with handwritten worked numbers)**; tool geometry offsets, tip types T1–T9, X/Z tool-length procedures (Ctrl+I turret index) |
| IMG_0727–0728 | G/M-code overview; cutter radius compensation |
| IMG_0729–0730 | PC keyboard function map; softkey menu tree (blurry) |

## 7. Machine-description manual (valid from 1.1.1993) — IMG_0731–0766

The hardware manual: the authoritative metric specifications.

| Files | Topic |
| --- | --- |
| IMG_0731–0734 | Intro, contents, warranty, safety |
| IMG_0735 | **TECHNICAL DATA table** (travels, spindle, feeds, electrics) |
| IMG_0736–0741 | PC requirements; scope of supply (RS-485 card + terminator + cable); dimensions; **electrical connection / voltage selector**; **"the interface is a serial interface RS 485"**; machine connection and start-up |
| IMG_0742–0744 | Machine layout; tailstock + safety package (guard/wheel-cover switches, E-stop); **step motors — no limit switches, "tack-tack"** |
| IMG_0745–0749 | Toolholders, cutting-diameter ranges, centre-height (30 mm), **turret (6 stations, no retrofit)**, internal tools |
| IMG_0750 | **Belt speed ranges + "enter belt position in software"** |
| IMG_0751–0755 | Clamping devices: chucks, **ESX 25 collets 1.5–14 mm**, clamping plate |
| IMG_0756 | **Maintenance: slideway oil ISO VG 68; bearings and ballscrews maintenance-free** |
| IMG_0757–0760 | Cutting data, nomograms, spindle torque curve (~7 Nm) |
| IMG_0761–0766 | Coordinate points M/W/R/N; optical presetter; accessories (turret needs SPS card); tools; tailstock accessories |

## 8. Software Description GE Fanuc Series 0-TC, EN 0091 v2.0 (1993) — IMG_0768–0801

The software manual for the DOS control, including the complete alarm list and the MS-DOS annex.

| Files | Topic |
| --- | --- |
| IMG_0768–0770 | Covers, contents |
| IMG_0771–0772 | Key descriptions; **serial-interface parameters (COM1/COM2 9600/E/7, F9/F10 transfer)** |
| IMG_0773–0775 | Reference points (re-approach after power loss); tool calls, offset registers; tool measuring |
| IMG_0776–0785 | Program structure; **G-code details G00–G99** incl. **G78 parameter breakdown**; chamfers/radii (blurry) |
| IMG_0786 | **Data input/output panel** (device selection, import/export) |
| IMG_0787–0791 | Installation; call-up parameters (RS-485 keyboard addresses); `FANOT` start; **ISA card jumper table ST1–ST3** |
| IMG_0792–0794 | **The complete alarm list: 1–510, 520–9004 (RS-485 2520–2528, PLC 6xxx, 8010/8020), 9005–9540** |
| IMG_0795–0801 | MS-DOS annex, PC troubleshooting, ergonomics |

## 9. Classroom Guide (Edition 1995, US 9905) — IMG_0802–0888

A complete worked training course (the "ball guide" part): planning, offsets, programming,
simulation, optimisation, data transfer.

| Files | Topic |
| --- | --- |
| IMG_0802–0806 | Covers, contents, machine overview |
| IMG_0807–0813 | Spindle (belt steps), tailstock, working area/travels, reference points, control panel, screen layout, operating modes (re-reference after power-off) |
| IMG_0814–0816 | G-code and M-code lists |
| IMG_0817–0836 | The ball-guide project: drawing, planning, tools, **tool corrections and optical measurement steps**, clamping and zero shifts, adjustment/production sheets |
| IMG_0837–0854 | Program structure and full example (O100/O101); editor operations |
| IMG_0855–0866 | **Graphic simulation; program test: dry run, single block, feed override 0–120 %** |
| IMG_0867–0876 | Production checklist; tool-data and cutting-data optimisation; **rapid = 29.5 in/min** |
| IMG_0877–0888 | Program management; **data transmission (devices, serial parameters 9600/E/1/7, load/unload)**; index |

## 10. Electrical documentation VS 4301, Edition 93-5 — IMG_0890–0913

The complete schematic set — the highest-value material in the archive.

| Files | Topic |
| --- | --- |
| IMG_0890 | Cover |
| IMG_0891–0894 | **Parts lists**: PSU components, fuses, boards (Y4A013000/Y4A014000), contactors, switches, ELIN spindle motor, FRC105, **Berger Lahr VRDM 564/50LN steppers, Balluff BES 516-343, Maxon turret gearmotor** |
| IMG_0895–0896 | Sheet registers |
| IMG_0897 | **Mounting-panel layout** — every board designator and fuse ("former CPU being replaced" note) |
| IMG_0898–0900 | Main-drive power; **24 V supply distribution**; transformer/rectifier detail |
| IMG_0901 | Board part-number sheet + DC/DC pinouts |
| IMG_0902 | **KOMMUNIKATION RS485** — the DB9 X300 pinout and bus termination |
| IMG_0903–0904 | Main drive power (blurry) and **control incl. spindle encoder → X109** |
| IMG_0905–0908 | **Axis control X and Z: stepper board, motors, X104 ribbon pinout, reference + Sync-Bero switches** |
| IMG_0909–0910 | Terminal-strip wiring lists (PE strip blurry) |
| IMG_0911 | **Emergency-off circuit** |
| IMG_0912 | **SPS/PLC connection map** (turret I/O, encoder-board supply) |
| IMG_0913 | 230 V consumers (fans) |

## 11. Curated images committed in `docs/img/`

Downscaled ≤1600 px, rotated upright. Source photo in brackets.

| File | Source | Shows |
| --- | --- | --- |
| `machine-front-view.jpg` | 0614 | the machine |
| `control-box-rear-type-plate.jpg` | 0615 | type plate, DB9, inlet, keyswitch |
| `control-box-interior-power-section.jpg` | 0617 | contactors, transformer, fuses |
| `control-box-card-rack-boards.jpg` | 0618 | the three-board card rack |
| `manual-rs232-settings.jpg` | 0640 | WinNC serial parameter page |
| `manual-robotic-interface-io.jpg` | 0662 | A0.x/E3.x I/O table |
| `manual-keyboard-pinouts.jpg` | 0679 | accessory keyboard DB9 pinouts |
| `manual-machine-technical-data.jpg` | 0682 | US spec table |
| `b-technical-data-page.jpg` | 0735 | metric spec table |
| `b-rs485-pc-connection.jpg` | 0740 | RS-485 card install page |
| `b-electrical-connection.jpg` | 0739 | mains/voltage selector |
| `b-step-motors-page.jpg` | 0744 | no-limit-switch statement |
| `b-serial-interface-settings.jpg` | 0772 | COM1/COM2 settings |
| `b-interface-card-jumpers.jpg` | 0791 | ISA memory-window jumpers |
| `b-alarm-list-rs485.jpg` | 0793 | alarms 2520–2528, 6xxx, 8010 |
| `b-data-io-procedure.jpg` | 0786 | data I/O panel |
| `c-parts-list-steppers.jpg` | 0894 | stepper/sensor/gearmotor parts |
| `c-mounting-panel-layout.jpg` | 0897 | board layout |
| `c-24v-supply-schematic.jpg` | 0899 | 24 V distribution |
| `c-rs485-comm-schematic.jpg` | 0902 | the RS-485 bus schematic |
| `c-spindle-control-encoder.jpg` | 0904 | spindle control + encoder |
| `c-xaxis-control-x104.jpg` | 0906 | X104 signals, homing sensors |
| `c-serial-params-page.jpg` | 0882 | serial parameters 9600/E/1/7 |
| `c-control-box-rear.jpg` | 0914 | control box rear |

## 12. Missing or illegible — the re-shoot list

Take a camera (macro mode, straight-on, good light) on the next shop visit:

1. **The type plate, close up.** The serial number and build year are illegible in both attempts
   (IMG_0615, IMG_0914). The machine-number plate is below the key switch. This is the single
   most wanted photograph.
2. The COM1/COM2 stop-bits row in Software Description ch. A (IMG_0772 — row unreadable).
3. Alarm texts 7020–7053 (IMG_0661) and the 6000/7000-series details (IMG_0659–0661) — read at
   low confidence.
4. The US spec-table pairings (IMG_0682) — re-shoot right-side-up to settle which value is Z
   travel vs distance between centres.
5. IMG_0730 (softkey menu tree) and IMG_0785 (chamfers/radii) — too blurry to transcribe.
6. IMG_0625 toolholder stamping; IMG_0617 small EMCO PCB part number (Y4A8…).
7. IMG_0663 robotic interface PC TURN 120 table (blurry — low priority, wrong model).
8. IMG_0903 main-drive power schematic and IMG_0909 PE terminal list (blurry).
9. Not yet photographed at all: the machine's own oil points / lubrication chart if one exists,
   the belt and pulleys under the wheel cover (with the belt size), the turret with tools
   removed, and the inside of the tailstock barrel.
