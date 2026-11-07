# LinuxCNC configurations

Two configurations live here, for two quite different purposes.

## `sim/emco-pcturn50/` — the training simulator

A model of the PC Turn 50 that runs on any computer and controls nothing. This is what
members learn on. See [`../docs/70-simulator.md`](../docs/70-simulator.md) for how to run
it, and [`../sim/`](../sim/) for the container that puts it in a browser.

| File | What it is |
| --- | --- |
| `emco-pcturn50.ini` | Machine data, travels, homing, units. Every machine-specific number is in one commented block at the top. |
| `qtvcp/screens/emco_lathe/` | The simulator's own screen. It enforces the referencing, belt, touch-off and proving routine of the operator guide; see [`../docs/70-simulator.md`](../docs/70-simulator.md#the-screen). All its decisions are in `emco_rules.py`, which has no Qt or LinuxCNC in it. |
| `tests/test_emco_rules.py` | Tests for those rules. `python3 linuxcnc/sim/emco-pcturn50/tests/test_emco_rules.py`, no LinuxCNC needed. |
| `turret.hal` | The 6-station turret, as a tool-change handshake with an index delay |
| `emco-pcturn50.tbl` | Tool table: 3 external stations, 3 internal |
| `emco-pcturn50.var` | Parameters, shipping with G54 preset to Z −50 so the example programs have somewhere to cut. LinuxCNC rewrites this on exit. |

## `retrofit/emco-pcturn50-7i96s/` — the future option

A configuration for the day the machine is actually converted to LinuxCNC: a Mesa 7i96S
driving the existing EMCO stepper power card through connector `X104`, keeping the Berger
Lahr 5-phase motors. **Nothing has been built or tested against hardware.** Read
[`../docs/60-retrofit-design.md`](../docs/60-retrofit-design.md) before touching it, and meter
`X104` before connecting anything to it — its pin-by-pin assignment is still unverified.

## Do not confuse the two

The simulator cannot drive the machine, and no amount of editing will make it. The machine's
DB9 is a differential RS-485 bus into a complete CNC control, not a step/direction port.
[`../docs/20-control-architecture.md`](../docs/20-control-architecture.md) explains why, and
[`../docs/adr/0001-serial-vs-retrofit.md`](../docs/adr/0001-serial-vs-retrofit.md) records
the decision.

## Version note

The simulator is developed and tested against **LinuxCNC 2.9.10** (what the container
installs). There is a full LinuxCNC **2.10-dev** source checkout at `/home/j/src/linuxcnc` on
the machine this was written on, which is useful for reference but is *not* the version this
config targets — several things differ between them, including the `stdglue` remap helpers.
