# 80 — Maintenance and troubleshooting

Keeping a 1993 training lathe alive. Two kinds of guidance live on this page, and they are
labelled: **[EMCO]** — quoted from EMCO's own photographed documentation (see
[`90-photo-index.md`](90-photo-index.md)); **[PRACTICE]** — general good machine-shop practice
that EMCO does not spell out. Where neither exists, it says so. Specifications and alarm codes
referenced here live in [`10-machine-reference.md`](10-machine-reference.md).

**Before opening the control cabinet:** it contains unobtainable boards and a charged 15000 µF
reservoir capacitor. Key switch off, mains unplugged, and read
[`20-control-architecture.md`](20-control-architecture.md) first. The project's current policy
is non-invasive — the box stays shut unless there is a concrete, agreed reason.

---

## 1. Lubrication

**[EMCO]** The maintenance page of the machine-description manual (IMG_0756) is short and
specific:

- **Spindle bearings: maintenance-free.** Do not grease, do not adjust.
- **Recirculating ball screws: maintenance-free.**
- **Slideways: oil, ISO VG 68** (CGLP per DIN 51 502). Named brands: BP Maccurat 68, Castrol
  Magnaglide D 68, Esso Febis K 68 — any equivalent slideway oil (e.g. Mobil Vactra No. 2) is
  the same class. **[PRACTICE]** for the equivalence.

**[PRACTICE]** Routine: wipe the exposed ways clean of chips and old oil, then apply a light
film of way oil — every session on shutdown, and before use if the machine has sat. Oil the
visible leadscrew surfaces lightly at the same time. Never lubricate belts or pulleys.

EMCO gives no oiling interval or oil-point diagram in the photographed pages; if an oil-nipple
chart exists in the binders it was not captured — check the physical manual (open question).

## 2. Ways and leadscrews

**[PRACTICE]**

- Chips left on the ways grind into the slides — brush toward the tray after every use, never
  with compressed air (it drives grit under the wipers and into the screws).
- Check the Z leadscrew's exposed length for embedded chips; clean with a brush and re-oil.
- Signs of trouble worth logging: a tight spot at one place along travel (bent screw or damaged
  way), new noise, or visible bright wear lines on the ways.

## 3. Backlash — checking, and what is adjustable

**[PRACTICE]** Check per axis with a dial indicator (magnetic base on the bed):

1. Indicator against the carriage (Z) or cross slide (X), preloaded ~1 mm.
2. Jog 0.5 mm one way, zero the indicator, jog 0.1 mm further, then 0.1 mm back.
3. The difference between commanded and indicated return is the lost motion. Repeat mid-travel
   and near both ends.

Interpretation for this machine:

- The ball screws are preloaded and **[EMCO]** maintenance-free; EMCO documents **no user
  backlash adjustment** for them in the photographed pages. Meaningful screw backlash on a
  ballscrew machine indicates wear or a loose screw-mounting/nut bolt — investigate, don't
  compensate blindly.
- **[PRACTICE]** Loose gib strips or motor-coupling set screws mimic backlash; check coupling
  grub screws on the stepper couplings first, since a slipping coupling also causes silent
  position loss on a stepper machine.
- The original control has no documented backlash-compensation parameter in the photographed
  material; a LinuxCNC retrofit could compensate in software, but mechanical repair beats
  compensation. Acceptable figure: a healthy machine of this class should show ≤0.02 mm
  [0.001 in] **[PRACTICE — target, not an EMCO spec]**.

## 4. Spindle belt — the three speed ranges

**[EMCO]** The spindle is belt-driven with a three-step pulley (IMG_0750):

| Position | Range |
| --- | --- |
| 1 | 130–1300 rpm |
| 2 | 200–2000 rpm |
| 3 | 300–3000 rpm |

Changing the range:

1. **Key switch off.** The wheel cover carries an interlock switch **[EMCO]** — the machine
   will not run with it open (alarm 6025), but power off anyway. **[PRACTICE]**
2. Open the wheel (belt) cover.
3. Move the V-belt to the wanted pulley step. **[PRACTICE]:** walk it off the smaller pulley
   first, rotating the spindle by hand; never lever it with a screwdriver.
4. Check tension — about 10 mm of finger deflection mid-span is right for a small V-belt
   **[PRACTICE]**; EMCO documents no tension spec or adjuster in the photographed pages.
5. Check alignment by eye along the belt line; close the cover.
6. **Enter the new belt position in the control software.** **[EMCO]** — it is used for speed
   monitoring and threading synchronisation; a wrong entry means wrong speeds and alarm 8010
   when threading.

Belt size is not recorded anywhere in the photographed documentation — measure the belt and
write it here when the cover is next open ([UNVERIFIED], open question in doc 10 §13).

Torque note **[EMCO]**: the motor gives ~7 Nm at the spindle at best (IMG_0760); low range (1)
is for larger diameters and heavier cuts, high range (3) for small-diameter finishing. Threading
stays ≤400 rpm in any range.

## 5. Chip handling

**[PRACTICE]**

- Brush and scrape chips into the tray after every job; empty the tray before it overflows into
  the Z ways.
- Aluminium chips + steel chips + oil = a galling paste; don't let materials accumulate mixed.
- No compressed air (§2), no bare hands, no vacuuming of oily steel wool (fire).
- Long stringy swarf during a job means wrong feed/speed — fix the cut, don't just clear the
  birds-nest.

## 6. Periodic inspection

**[PRACTICE — suggested schedule; EMCO provides none in the photographed pages]**

Every session (part of the operator's pre-start checks, [`40-operator-guide.md`](40-operator-guide.md) §3):
guards and interlocks, chuck condition, ways clean and oiled, E-stop reachable.

Monthly / 10 running hours:

1. E-stop test: with the spindle running empty, hit it. Motors must drop instantly (the chain
   physically drops the contactors — [`20-control-architecture.md`](20-control-architecture.md) §4.8).
2. Interlock test: open chip guard and wheel cover in turn; machine must refuse to run
   (alarms 6024/6025).
3. Backlash spot-check per §3; log the numbers so drift is visible.
4. Belt condition: glazing, cracking, dust under the cover.
5. Turret: index through all six stations (when a control path exists); check each locks solidly
   with no rock.
6. Tailstock quill: full 35 mm travel smooth, MT1 taper socket clean.
7. Electrical, external only: cords, strain reliefs, the DB9 protective state ("RS-485 ONLY"
   label present), fan airflow at the cabinet vents audible/felt.

Annually, or on any electrical fault: fuses and their ratings (mains 6.3 A T; F1–F3 2.5 A T,
F4 6.3 A T **[EMCO/SCHEM]**), and a visual check for capacitor distress (bulging, leakage) —
the 15000 µF reservoir and every board electrolytic are 30+ years old; plan for eventual
replacement by someone competent with mains equipment. **[PRACTICE]**

## 7. Troubleshooting

Keyed to the alarm table in [`10-machine-reference.md`](10-machine-reference.md) §11. "Alarm"
columns apply when a control path exists to display them; the mechanical symptoms apply today.

| Symptom / alarm | Likely cause | Action |
| --- | --- | --- |
| Loud "tack-tack" from an axis, position now wrong | Axis ran into its mechanical stop — **this machine has no limit switches** and the stepper stalled | Stop. Re-reference the machine before any further motion ([`40-operator-guide.md`](40-operator-guide.md) §5). Recurring at the same spot mid-travel: check for chips/damage on that way |
| Alarm 315/325/335 "step motor fell out of step" | Stall from overload, too-high feed, or mechanical bind | Re-reference; reduce cut; find the bind per §2/§3 |
| Alarm 6010/6011/6012 "drive not ready" | Stepper board fault or unpowered — check its SERVO READY LEDs; check fuse F3 (2.5 A T) and the 24 V rail (F4) | Fuses first. If the board is genuinely dead: it is unobtainable — see doc 10 §12 and [`60-retrofit-design.md`](60-retrofit-design.md) |
| Alarm 6013 / spindle won't start | Main drive not ready: FRC105 inverter, its on-card fuses (4 A FF, 100 mA T), contactors K1/K2, or an open interlock | Check interlocks (6024/6025) before suspecting electronics |
| Alarm 6000 won't clear | E-stop chain open: button latched, chip guard, or wheel cover switch | Release/close each; the chain is in series — one open switch stops everything |
| Alarm 6024 / 6025 | Chip guard / wheel cover open (or its switch failed) | Close it; if closed and alarm persists, test the switch for continuity |
| Alarm 8010 during threading | Belt position entered wrong, or spindle sync sensor not delivering its mark | Verify belt entry first (§4); then the encoder path (CPU `X109`) |
| Alarm 8020 during threading | Pitch × rpm exceeds 80 % of max feed | Lower rpm (≤400 anyway) or coarse pitch in more passes |
| Alarm 6040/6041/6043–6045 / turret hunts or won't lock | Turret stuck, tool over-length striking the bed, or Strobe/Sync sensing fault | Check stickout ≤12.7 mm; hand-rotate check; then the Maxon motor and encoder board `Y4A020000` |
| Alarm 2520/2521/2523/983 | RS-485 device absent / comms failure — with the ISA card gone, **2520 is the expected state** if the original software were ever run | Not a machine fault. See [`30-serial-protocol.md`](30-serial-protocol.md) |
| Chatter, poor finish | Stickout too long (tool or stock), dull insert, wrong speed for diameter, loose gib/coupling | Shorten, sharpen, re-speed; then §3 mechanical checks |
| Diameter drifts over a job | Tool wear, or heat growth; on this machine also silent step loss | If steps are being lost the *position* jumps, not drifts — drift is tooling/thermal |
| Machine dead, no fans | Mains, IEC fuse drawer (6.3 A T), key switch | Meter at the inlet before opening anything |

When you fix something — or fail to — write it in the shop's machine log and, if it changes a
fact in these documents, correct the document. The alarm texts above 7020 were read from
photographs at low confidence; if the control ever displays one that disagrees with doc 10's
table, trust the screen and fix the table.
