# Example programs

All of these are written for the EMCO PC Turn 50 and assume:

- **Millimetres** (`G21`) and **diameter mode** (`G7`) — an `X` value is a
  diameter, not a radius. This is how the original EMCO control worked and how
  drawings are dimensioned.
- **Feed per revolution** (`G95`), in mm/rev — the natural unit for turning.
- **Work zero (`G54`) at the face of the stock, on the centreline.** You set
  this by touching off; see [`../docs/40-operator-guide.md`](../docs/40-operator-guide.md).
  If you have not touched off, these programs will cut air or crash.
- Tooling as laid out in `../linuxcnc/sim/emco-pcturn50/emco-pcturn50.tbl`:
  T1 right-hand turning, T3 parting blade.

| File | Makes | Stock | Teaches |
| --- | --- | --- | --- |
| `tutorial-part.ngc` | 8 mm × 10 mm stepped pin with a chamfer | 12 mm alu, 30 mm proud | facing, roughing and finishing, chamfering, parting off |
| `stepped-shaft.ngc` | two-diameter shaft | 14 mm alu, 45 mm proud | order of operations, and subroutines for roughing passes |

## Run them in the simulator first

Every time, not just the first time.

```sh
docker compose -f ../sim/compose.yml up
```

Then home both axes, open the program, and step through it — the simulator's
screen makes the first run of any new or edited program single block anyway. The
backplot will show you a crash before the machine does.

## Conventions worth copying into your own programs

- **Start with a retract.** `G53 G0 X96` moves to the retracted position in
  machine coordinates, whatever work offset happens to be active. It means a
  program can never begin by ploughing into the part. It is X96, not X48,
  because diameter mode (`G7`) applies to `G53` moves too: the machine's 48 mm
  of X travel is a radius.
- **Retract in X before moving in Z**, and before any tool change. The
  6-station turret sweeps a wide circle and will hit the chuck if it indexes
  while it is down near the work.
- **Stop the spindle before changing tools** (`M5` then `T_ M6`).
- **Slow down for parting.** It is the operation most likely to grab.
- **Finish with `G53 G0 X96 Z0`** so the machine is left clear and predictable
  for the next person.
