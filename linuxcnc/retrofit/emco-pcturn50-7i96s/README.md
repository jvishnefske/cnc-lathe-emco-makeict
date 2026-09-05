# EMCO PC Turn 50 — Mesa 7i96S retrofit config (SKELETON)

**Do not connect this to the machine.** This is the paper half of a
contingency plan. The standing decision is non-invasive
([`docs/adr/0001-serial-vs-retrofit.md`](../../../docs/adr/0001-serial-vs-retrofit.md));
the entry conditions, signal plan, commissioning sequence and risks are in
[`docs/60-retrofit-design.md`](../../../docs/60-retrofit-design.md).

The one non-negotiable: the `X104` ribbon's pin-by-pin assignment and the
`DIR`/`CK` differential-pair polarity are **UNVERIFIED**. Meter them before
anything is wired (doc 60 §9 R1). Every other unmeasured number is tagged
`UNVERIFIED` in the INI, where all machine numbers live — the HAL only plumbs.

Files:

- `emco-pcturn50-7i96s.ini` — machine numbers; conventions match the verified
  simulator (`linuxcnc/sim/emco-pcturn50/`): X radius −0.5..48 mm, Z 0..−228 mm,
  machine zero at the reference (X48/Z0), metric, `LATHE = 1`,
  `trivkins coordinates=xz`, `EMCIO = iov2`.
- `emco-pcturn50-7i96s.hal` — `hm2_eth` + 7i96S plumbing: two stepgens (via
  differential line drivers into `X104`), reference switches, new end-of-travel
  limits, hardware E-stop chain observer, spindle enable/dir/0–10 V + encoder,
  turret I/O reserved with the tool-change handshake looped back (manual tool
  changes until ClassicLadder lands).
- `tool.tbl` — copied from the simulator.

Bench smoke test (no machine): with a 7i96S on the bench at the configured
`BOARD_IP`, `linuxcnc emco-pcturn50-7i96s.ini` should load and the stepgen
outputs should toggle differentially at the breakout's `X104` end on a scope —
doc 60 §8 step 2. Derived from upstream
`configs/by_interface/mesa/hm2-stepper/7i96s.ini` (a 2.10-dev tree; re-check
pin names against the 2.9.x you run).
