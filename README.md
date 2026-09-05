# EMCO PC Turn 50 — MakeICT

Documentation, control software and a training simulator for MakeICT's **EMCO PC TURN 50**
benchtop CNC lathe.

![The machine](docs/img/machine-overview.jpg)

## What this machine is

An EMCO PC TURN 50: a small Austrian CNC lathe from the 1990s (EMCO Maier Ges.m.b.H., A-5400
Hallein), in two parts — the lathe itself, and a separate red control cabinet with a **DB9 serial
port** and a mains inlet.

It was sold to be driven by a DOS PC running EMCO's Fanuc-0T control emulation, connected to the
control box by **RS-485 through a proprietary memory-mapped ISA card**. **That DOS host, and the
card, are gone.** The paper binders on the shelf describe a system nobody can run, which is why
they are no longer enough on their own. This repository is the replacement.

## Read this first: the serial port is not a LinuxCNC port

The single most important thing to understand, and the reason this project is shaped the way it is:

> **LinuxCNC cannot drive this lathe over the serial port.**

Two independent reasons, either of which alone is fatal:

1. **That DB9 is not a serial port.** It is control-box connector `X300`, carrying a **differential
   RS-485 bus** (pin 2 GND, pin 3 DATA−, pin 8 DATA+). Plugging an RS-232 adapter into it is
   electrically wrong and risks damage. The DOS PC reached it through a proprietary EMCO ISA card
   that was **memory-mapped**, not a UART — so there was never a byte-stream protocol to speak in
   the first place.
2. **The red box is not a motor amplifier.** It contains a complete CNC control — axis-controller
   board `Y4A012000` — with its own interpreter and motion planner. LinuxCNC is itself a motion
   controller: it closes a loop roughly a thousand times a second and emits step pulses. It has no
   way to hand coordinated motion to somebody else's black box, and was never designed to.

The machine's step/direction signals do exist, on a 16-pin ribbon header called `X104` inside the
box. That, not the DB9, is where a LinuxCNC retrofit would tap in — see
[`docs/60-retrofit-design.md`](docs/60-retrofit-design.md).

So the split here is:

| Job | What does it |
| --- | --- |
| Driving the actual lathe | The browser G-code sender in [`sender/`](sender/), over RS-232 |
| Learning, and preparing programs | The LinuxCNC simulator in [`linuxcnc/sim/`](linuxcnc/sim/) |
| Future option, or if the control dies | The retrofit design in [`docs/60-retrofit-design.md`](docs/60-retrofit-design.md) |

The full reasoning is recorded in [`docs/adr/0001-serial-vs-retrofit.md`](docs/adr/0001-serial-vs-retrofit.md).

## Where to start

**I want to use the lathe.** → [`docs/40-operator-guide.md`](docs/40-operator-guide.md)

**I have never run a CNC lathe.** → [`docs/70-simulator.md`](docs/70-simulator.md), then come back.
Learn on the simulator first. It is free to crash.

**I want to understand how it works.** → [`docs/00-overview.md`](docs/00-overview.md) →
[`docs/20-control-architecture.md`](docs/20-control-architecture.md)

**I am working on the serial protocol.** → [`docs/30-serial-protocol.md`](docs/30-serial-protocol.md)
and [`tools/README.md`](tools/README.md). Read the safety rules before you plug anything in.

**Something is broken.** → [`docs/80-maintenance.md`](docs/80-maintenance.md)

## Layout

```
docs/          the documentation set; start at 00-overview.md
sender/        browser G-code sender — Rust/WASM core + TypeScript UI + a Python CLI fallback
linuxcnc/      simulator config, and a config for the future retrofit option
sim/           containerised LinuxCNC — `docker compose -f sim/compose.yml up`
tools/         serial discovery and testing tools
gcode/         example programs, including the tutorial part used by the operator guide
reference/     the original 301 reference photographs (not in git — see docs/90-photo-index.md)
```

## Quick start

```sh
# Development shell with Rust, wasm-pack, Node, Python+pyserial, socat, ImageMagick
nix develop

# The simulator, in a browser at http://localhost:8080
docker compose -f sim/compose.yml up

# The sender, in a browser — pick "Simulated machine" to try it with no hardware
cd sender/web && npm ci && npm run dev
```

The sender needs **Chrome or Edge** — it uses the Web Serial API, which Firefox and Safari do not
implement. Reaching the real machine would additionally need a **USB-RS485** adapter; a USB-RS232
cable is the wrong electrical standard for this port.

## Status, honestly

This documentation was reconstructed from photographs of the machine and its binders, plus the
EMCO and LinuxCNC community record. It has **not** all been confirmed against the machine.

- Most specifications are now **documented** — they were recovered from EMCO's own manuals and
  schematics, photographed page by page. Those carry a source tag ([PLATE], [SCHEM], [MANUAL]) in
  [`docs/10-machine-reference.md`](docs/10-machine-reference.md). They have not been re-measured at
  the machine.
- Anything still tagged **UNVERIFIED** needs somebody with a rule and a meter. The machine's serial
  number, the spindle encoder's resolution, and the pin-by-pin detail of `X104` are the main ones.
- **The RS-485 protocol is not known**, and no public reverse-engineering of it exists. It is
  harder than it first looked, because the framing was generated by ISA card hardware rather than
  by software writing text to a port. Until the work in
  [`docs/30-serial-protocol.md`](docs/30-serial-protocol.md) succeeds, the sender talks to a
  simulated machine, not a real one. That is the project's critical path — and it may not be
  winnable, in which case the retrofit is the answer.
- The simulator and the sender's simulated mode both work today and are useful today, because
  neither depends on the protocol.

If you confirm a value at the machine, please correct it here and drop the **UNVERIFIED** tag. That
is the single most useful contribution anyone can make to this repository.

## Safety

This is a machine tool with a powered chuck. It can take a finger off.

- A software stop sent over a serial cable is **advisory, not a safety function**. The real stop is
  the switch on the control box, with a person standing at the machine.
- Never run it unattended, and never run it remotely.
- Get checked out by a member who knows the machine before running it alone, per MakeICT's rules
  for the shop.
