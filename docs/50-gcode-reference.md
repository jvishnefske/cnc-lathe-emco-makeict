# 50 — G-code reference: the EMCO Fanuc-0T dialect

What this control accepts, recovered from the photographed EMCO manuals (Software Description
EN 0091 v2.0, the Columbus start-up guide, the classroom guide and the shop's own procedure
binder — see [`90-photo-index.md`](90-photo-index.md)). The dialect is EMCO's emulation of
**GE Fanuc Series 0-T**, "G-code system C", programs per DIN 66025. [MANUAL]

Two audiences, one page:

- **Writing for the simulator today** (`linuxcnc/sim/`): LinuxCNC's dialect is close but not
  identical — §6 lists the traps, and the `gcode/` examples plus `gcode/README.md` are the
  house style. Project convention: **millimetres (G21 in LinuxCNC), diameter mode (G7 in
  LinuxCNC), feed per rev (G95)**.
- **Reading the original programs and binders**, and being ready if the original control ever
  runs again: everything else on this page.

---

## 1. Program structure and address words

Programs are ASCII text, `Oxxxx` numbered (1–9999), blocks numbered `Nxxxx`, ending in `M30`.
[MANUAL]

| Address | Meaning |
| --- | --- |
| O | program number |
| N | block number |
| G / M | preparatory / miscellaneous function |
| X, Z | absolute coordinates (X as **diameter** — §2) |
| U, W | **incremental** X / Z moves (U also = dwell time in G04) |
| R | radius; also taper distance in cycles, retract amounts |
| I, K | arc centre offsets (from arc start point) |
| F | feedrate — or **thread pitch** in G21/G33/G78 |
| S | spindle speed (rpm under G97, surface speed under G96) |
| T | tool: `Txxyy` = turret station xx + offset register yy (e.g. `T0404`); 16 registers, 32 optional |
| P, Q | block labels for contour cycles; also packed parameters in G76/G78 |

## 2. Diameter vs radius programming

The X axis physically moves a **radius** (48 mm of travel), but the control is set up for
**diameter programming**: an X word is the diameter of the resulting part, and the control
halves it internally. That matches how turned parts are drawn and measured, and it is this
project's convention everywhere — the examples in `gcode/` all run in diameter mode.

- Original control: diameter is the default mode; G07/G08 switch between diameter and radius
  input. [MANUAL — noted in `_machine-facts.md`; the exact page should be re-checked]
- LinuxCNC simulator: `G7` = diameter mode (lathe only), `G8` = radius mode. The sim configs and
  examples set `G7` explicitly.
- Things that stay radius regardless: the machine's travel figures, and arc I/K words. When a
  cut is 1 mm deep into the side of the part, the **diameter** shrinks by 2 mm. Keep that ×2 in
  your head; it is the most common beginner error on any lathe.

## 3. G codes

The full set the EMCO Fanuc-0T control accepts. [MANUAL] Plain-English notes; cycles are
detailed in §4.

| G | Function | Plain English |
| --- | --- | --- |
| G00 | rapid traverse | full-speed positioning move — never cut with it |
| G01 | linear interpolation | straight cutting move at feedrate F |
| G02 / G03 | circular interpolation CW / CCW | arcs; centre via I/K measured from the arc start |
| G04 | dwell | pause (time via U/X/P) |
| G20 | **longitudinal turning cycle** | one-block rough pass along Z (NOT "inch" — see §6!) |
| G21 | **thread cutting cycle** | one-block threading pass; **F = pitch** (NOT "metric" — §6!) |
| G24 | facing cycle | one-block rough pass across the face |
| G28 | return to reference point | sends axes home via an intermediate point |
| G33 | thread cutting, single block | one threading pass, no cycle logic |
| G40 / G41 / G42 | cutter radius compensation off / left / right | corrects for the tool nose radius; needs R and tip type in the offset table |
| G70 | **inch data input** | all words read as inches (modern Fanuc/LinuxCNC would call this G20) |
| G71 | **metric data input** | all words read as mm |
| G72 | finishing cycle | runs the contour between blocks P and Q once, at finish allowance zero |
| G73 | stock removal in turning (contour) | roughs a P–Q contour with repeated Z-parallel passes |
| G74 | stock removal in facing (contour) | roughs a P–Q contour with X-parallel passes |
| G75 | pattern repeating | roughs by repeating the contour shape, offset pass by pass (castings/forgings) |
| G76 | deep-hole / peck drilling | two-block format: R = retract, Q = peck depth |
| G77 | cut-in (grooving) cycle, X | plunge grooving on the diameter |
| G78 | **multiple threading cycle** | complete thread in one two-block command — §5 |
| G80 | cancel canned cycle | ends G83–G85 modality |
| G83 | drilling cycle | drill to depth on centreline |
| G84 | tapping cycle | taps; spindle auto-reverses (M03↔M04) at bottom |
| G85 | reaming cycle | feed in, feed out |
| G90 | absolute programming | X/Z are positions |
| G91 | incremental programming | words are distances (or just use U/W in any mode) |
| G92 | coordinate setting / **spindle speed limit** | with S: clamps max rpm — essential with G96 |
| G94 | feed per minute | F in mm/min [in/min] |
| G95 | feed per revolution | F in mm/rev [in/rev] — the natural turning unit, project default |
| G96 | constant surface speed | S = m/min [ft/min]; rpm rises as diameter shrinks — set G92 S-limit first |
| G97 | direct rpm | S = rpm; required mode for threading |
| G98 | return to initial plane | canned-cycle retract behaviour (NOT feed/min — §6!) |
| G99 | return to R plane | canned-cycle retract behaviour |

Power-on modal defaults observed on the MDI screen: G00, G97, G90, G95, G40 (plus G70/G71 as
configured). [MANUAL]

## 4. Canned cycles in brief

The one-block cycles (G20/G21/G24) each execute a four-move sequence — approach, cut, retract,
return — from a single block; the operator repeats the block with a smaller X (or Z) for each
pass. [MANUAL]

The contour cycles (G72–G75) take `P` and `Q` block numbers bracketing a finished-contour
description later in the program, plus depth-of-cut, retract and allowance words (G73: U =
depth per pass, R = retract, U/W = finish allowances, then F/S/T). G73 roughs it, G72 runs the
same contour once as the finish pass. The classroom guide's worked example (ball guide,
programs O100/O101) uses exactly this G73 → G72 → G78 sequence. [MANUAL]

G76 (deep-hole drilling) is a **two-block** command: first block sets retraction R, second gives
depth and peck Q. [MANUAL]

## 5. Threading, and the G78 two-block format

Hard limits first, both from EMCO's own pages: [MANUAL]

- **Never thread above 400 rpm.** The control cannot synchronise faster.
- The **belt position (1/2/3) must be entered in the control** — threading synchronises to the
  spindle sync mark, and a wrong belt entry gives alarm **8010** ("main drive synchronising
  mark not found"). Alarm **8020** fires when thread feed would exceed 80 % of maximum feed:
  lower the rpm or the pitch.
- Start the tool at least **2.5 mm [0.100 in] before the thread** in Z, so the control can
  synchronise before the flank engages.
- Use G97 (direct rpm) while threading.

G78 cuts a complete multi-pass thread from two consecutive blocks:

```
Block 1:  G78  Pxxyyzz     Qq       Rr
              │ xx = number of finishing cuts
              │ yy = chamfer (thread run-out), in 0.1×pitch units
              │ zz = flank (infeed) angle, degrees
              Q  = minimum cutting depth (µm, or 0.0001 in)
              R  = finishing allowance

Block 2:  G78  X(U)… Z(W)…  Rr      Pp      Qq      Ff
              X/Z = core diameter and thread end point
              R  = taper (incremental, radius value; 0 for straight threads)
              P  = total thread depth (µm / 0.0001 in)
              Q  = depth of first cut (µm / 0.0001 in)
              F  = thread pitch
```

Worked inch example from the start-up guide (½-20 UNF-class thread): [MANUAL]

```
N100 G78 P020060 Q0020 R.001
N105 G78 X.406 Z-1.2 R0 P0470 Q0120 F.047
```

with the guide's rules of thumb for inch threads: pitch = 1/TPI, thread depth ≈ 0.6495/TPI.

The control repeats passes automatically, each at decreasing depth (constant chip section),
then the finishing cuts. G33 exists for single hand-managed passes; G21 is the simpler
one-block-per-pass cycle.

**In the LinuxCNC simulator** the equivalent is G76 (LinuxCNC's threading cycle) — different
code, different parameter set. Do not translate blindly; re-derive from pitch and depth.

## 6. Differences that will bite you

### Coming from modern Fanuc or LinuxCNC

| You expect | This control does | Bite |
| --- | --- | --- |
| G20/G21 = inch/metric | **G20 = turning cycle, G21 = threading cycle**; units are **G70/G71** | a "G21 metric" habit line starts a threading cycle on the original control |
| G70/G71 = finish/rough cycles (modern Fanuc lathe) | **G70 = inch, G71 = metric**; the cycles are G72/G73 | swapped meanings in both directions |
| G98/G99 = feed per min/rev (modern Fanuc lathe) | **G94/G95 are the feed modes; G98/G99 are canned-cycle return planes** | wrong feed units by ×rpm |
| G54–G59 work offsets | none — **work shift** (one global Z shift) plus G92 coordinate setting | there is exactly one work coordinate system |
| G76 = threading (modern Fanuc & LinuxCNC) | **G76 = deep-hole drilling; threading is G78/G21/G33** | a modern G76 block means something completely different here |
| decimal-point safety | old Fanuc conventions: packed P-words (`P020060`), µm depths | magnitude errors; write the µm words without decimal points exactly as documented |
| tool change `T1 M6` (LinuxCNC) | `Txxyy` word alone indexes turret and applies offset | on LinuxCNC the sim needs `M6`; on the original it must NOT have one |

### Coming from the original control to the simulator

- LinuxCNC: G21 = mm, G20 = inch, G7/G8 = diameter/radius, G76 = threading, G54+ work offsets
  exist. The EMCO cycles (G20/G21/G24/G72–G78) do not exist in LinuxCNC form — the example
  programs write roughing passes out longhand or with subroutines instead
  (`gcode/stepped-shaft.ngc`).
- The simulator enforces soft limits the real machine does not have, and does not model belt
  ranges or the 400 rpm threading ceiling — respect them anyway.

## 7. M codes

[MANUAL]

| M | Function | Note |
| --- | --- | --- |
| M00 | program stop | unconditional; continue with cycle start |
| M01 | optional stop | stops only if the operator armed it |
| M02 | program end | prefer M30 |
| M03 / M04 | spindle on CW / CCW | with S word |
| M05 | spindle stop | before every tool change |
| M08 / M09 | coolant on / off | **PC TURN 120 only** — this machine has no coolant pump |
| M20 / M21 | tailstock quill back / forward | automated tailstock accessory only |
| M25 / M26 | open / close clamping device | automated chuck accessory only |
| M30 | program end + rewind | the normal last line |
| M71 / M72 | puff blowing on / off | chip blow-off accessory (PC TURN 50 mapping) |
| M98 / M99 | subprogram call / return | P = program, repeat counts supported |

Whether any of the automated accessories (M20–M26, M71/M72) are fitted to this unit is
[UNVERIFIED] — see [`10-machine-reference.md`](10-machine-reference.md) §9.

## 8. House style for new programs

Follow `gcode/README.md`; in short: mm, diameter mode, G95 feed-per-rev; open with
`G53 G0 X96` (retract before anything — X96 because G7 makes the X word a diameter, even on a
`G53` move; it is X48 only in radius mode); retract in X before Z and before tool changes;
spindle off before indexing; slow down for parting; end at `G53 G0 X96 Z0`. Prove everything in the
simulator first — every time.
