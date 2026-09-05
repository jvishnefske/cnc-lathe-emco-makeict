# Capturing the original EMCO software — the memory-window route

The machine's original control software (`FANOT`, installed at `C:\PCNC`
on a DOS PC) drove the lathe through a proprietary **memory-mapped 8-bit
ISA card** (`Y4A013000` "PCCOM"): a **4 KB dual-port RAM window at
CC000–CFFFF** (default CC000–CCFFF, jumpers ST1–ST3, selectable IRQ). The
card's own hardware serialised that shared RAM onto the RS-485 bus
(control-box connector `X300`). See
[`../../docs/_machine-facts.md`](../../docs/_machine-facts.md).

**Consequence, stated plainly: capturing DOSBox's COM1 will NOT capture
the machine link.** The machine link never touches a COM port. An earlier
version of this directory was built around COM1 capture; that approach is
kept below only for the *file-transfer* side channel. The two avenues that
can actually work are:

1. **Emulate the memory window** (software required, card not required) —
   log what `FANOT` reads and writes at CC000–CFFFF. This reveals the
   *command semantics*: message layouts, the polling structure, what a
   "start", "jog" or "stop" looks like in RAM. The wire *encoding* the
   card hardware generates stays hidden, but semantics are most of the
   reverse-engineering battle. **This is the most promising avenue and
   nobody has done it.**
2. **Tap the wire with the real card** (software *and* card *and* an ISA
   PC required) — run the real stack against the real machine and capture
   DATA+/DATA− with a logic analyser or a receive-only USB-RS485 adapter
   (`../emco-listen.py`). This gives the actual wire framing.

**Honest caveat up front:** both avenues depend on artefacts we do not
currently have. Everything below is ready for the day they turn up.

## 0. Go look for these in the shop

- **The floppies**: `FANOT` / `PCNC` disks, the "machine-data disk", the
  SPS/turret disk, anything labelled EMCO. Check drawers, binders, the
  bottom of the accessories box. Image them raw (`dd` or Greaseweazle)
  before doing anything else — they are 30+ years old.
- **The ISA card `Y4A013000`**: full-length 8-bit ISA card with two 9-pin
  connectors and a terminator plug, possibly still inside a dead beige PC.
- **Any old PC carcass** that came with the machine — even a dead one may
  hold the card and a hard disk with the installed software.
- Failing the shop: EMCO user communities (CNCzone, CNC-Ecke,
  PraktikerForum), abandonware archives (search `EMCO PC TURN`,
  `EMCO Fanuc 0-T`, `FANOT`), and a polite email to EMCO in Hallein,
  Austria — they still support the education line.

## 1. The memory-window route (86Box / PCem / QEMU)

The idea: boot DOS in an emulator, install the EMCO software, and add a
fake device that claims the CC000–CFFFF window and logs every access.

```
DOS in emulator ── FANOT writes/reads CC000-CFFFF ──> fake dual-port RAM
                                                        │
                                              append-only access log
                                              (offset, byte, r/w, time)
```

Expected behaviour without a responding "machine": `FANOT` should raise
alarm **"2520 ORDxx RS485 device absent"** (or 983/2523). Even that is
informative — the accesses made *before* it gives up show the init
handshake. The next step is teaching the fake RAM to answer (a scripted
responder, morally the memory-window sibling of `../virtual-emco.py`), and
iterating until the software gets further. Each answered step decodes more
of the protocol.

Concrete steps, in order of increasing fidelity:

1. **QEMU with a custom ISA device** (most scriptable). Write a small
   device model (C, ~150 lines) registering a `MemoryRegion` with
   read/write callbacks over 0xCC000–0xCCFFF backed by a 4 KB buffer, and
   log every access with an instruction-count or virtual-time stamp.
   Build against QEMU's source tree (`hw/isa/`), boot FreeDOS or MS-DOS
   6.22 with `-M isapc`, run `FANOT`. QEMU's `-d`/tracing infrastructure
   can also log memory-region accesses without writing much code —
   start with `trace-events` on the region before writing a responder.
2. **86Box / PCem** (most period-faithful). Both emulate real ISA
   machines and take custom device code in C; add a device that maps the
   window and logs. Better odds than QEMU if `FANOT` does timing-sensitive
   or IRQ-driven things (the card has a jumper-selectable IRQ — the log
   must eventually show whether the software waits on one).
3. **A dumb first pass with no code at all**: run the software in DOSBox-X
   with a debugger build, set memory watchpoints on CC00:0000–0FFF, and
   note what it touches. DOSBox-X cannot emulate the card properly (no
   custom ISA memory-window devices without patching), but watching the
   first faulting accesses tells you the window really is where the
   manual says, and what gets written first.

What to extract from a log, in priority order: the init sequence and any
IRQ dependence; the layout (command mailboxes? ring buffers? per-device
regions for CPU vs SPS?); what changes when you press cycle-start /
feed-hold / reset in the software; how a part program gets into RAM. Write
findings into `docs/30-serial-protocol.md` §8 (findings log), and mirror
testable ones into a responder script.

## 2. The wire-tap route (real card, real PC)

If both the card and software turn up: build a period ISA PC (386/486, or
an industrial ISA backplane), install both, connect to the machine
(cable `K1000`, DB9 `X300`), and capture:

- **Logic analyser** across DATA+ (pin 8) and DATA− (pin 3), ground to
  pin 2 — preferred, because it captures edges without assuming UART
  framing. Decode afterwards.
- Or `../emco-listen.py` with a USB-RS485 adapter as a third bus device,
  receive-only, if the framing turns out to be async.

Exercise every function while capturing: power-up, reference, jog, MDI, a
small program, feed hold, reset, turret index. One good session makes
everything else in this project easy. Safety rules: doc 30 §5 — the
machine can move; two people, empty chuck, hand on the mains switch.

## 3. Legacy: COM1 capture (file-transfer channel only)

The DOS PC's own COM1/COM2 (9600, 7 data bits, even parity) was used
solely to move part-program and tool-offset *files* to external devices —
**not** the machine link. If you want to decode that file-transfer format
(useful for reading old program disks or feeding a WinNC-era workflow),
the original COM1-capture harness still works:

```sh
mkdir -p captures
socat -d -d -lu -x -v \
    TCP-LISTEN:5555,reuseaddr \
    PTY,link=/tmp/emco-dos-pty,raw,echo=0 \
    2>captures/dos-session-$(date +%Y%m%d-%H%M%S).log
# then, in another terminal:
dosbox-x -conf tools/dosbox-capture/dosbox-x.conf
```

[`dosbox-x.conf`](dosbox-x.conf) bridges COM1 to that socat listener with
`transparent:1` (raw bytes, no telnet escaping). In the EMCO software,
F9 = output / F10 = input with device 1 (COM1). Expect ISO G-code text
with `%` terminators. Do not mistake anything captured here for the
machine protocol.

## Where results go

Captures land in `captures/` (committing them as evidence is encouraged);
conclusions go to [`../../docs/30-serial-protocol.md`](../../docs/30-serial-protocol.md)
— the findings log (§8) and the known/assumed tables (§2). If semantics
become testable, they get scripted into a responder so hypotheses can be
run, not argued about.
