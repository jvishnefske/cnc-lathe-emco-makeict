# tools/ — protocol discovery and simulation

The EMCO PC Turn 50's control box talks to its (long-gone) DOS PC over
**RS-485** — the rear DB9 is bulkhead connector **`X300`**: pin 2 GND,
pin 3 DATA−, pin 8 DATA+, a 3-wire half-duplex multidrop bus. The PC side
was a proprietary **memory-mapped ISA card** (`Y4A013000`, a 4 KB
dual-port RAM window at CC000–CFFFF), **not a UART**, so the wire protocol
is unknown and is not a text byte stream. See
[`../docs/_machine-facts.md`](../docs/_machine-facts.md) and
[`../docs/30-serial-protocol.md`](../docs/30-serial-protocol.md).

> **Never connect an RS-232 adapter or PC COM port to `X300`.** RS-232 is
> the wrong electrical standard (single-ended, up to ±12 V into a 5 V
> differential bus) and risks destroying irreplaceable transceivers.
> USB-RS485 hardware only, and meter the connector first
> (doc 30 §7).

These tools exist to climb the discovery ladder below, and to let the rest
of the project develop against a simulated machine in the meantime.

## The escalation order

Escalate only as far as you need to. Each step is costlier or riskier than
the one before it, and every session — null results included — gets a
findings-log entry in doc 30 §8.

### 1. Find the original software and ISA card (zero risk, highest value)

Search everything stored with the machine for the `FANOT`/`PCNC` floppies,
the `Y4A013000` ISA card (full-length 8-bit card, two 9-pin sockets, a
terminator plug), and any old PC carcass. Ask the EMCO user communities
and EMCO Austria. This is the only known route to a reference
implementation; nothing else on this ladder replaces it.

### 2. `dosbox-capture/` — emulate the ISA card's memory window

If the software turns up (even without the card), run it under an emulator
that fakes the card's 4 KB dual-port RAM window and **log every read and
write to CC000–CFFFF**. That reveals the command semantics — message
layouts, polling, what "go" looks like — which is most of the
reverse-engineering battle. Nobody has done this yet; it is the most
promising avenue. See [`dosbox-capture/README.md`](dosbox-capture/README.md).
(Plain DOSBox COM1 capture does **not** work for the machine link — the
link never touches a COM port.)

### 3. `emco-listen.py` — passive RS-485 capture (transmit-free)

```sh
python3 tools/emco-listen.py --port /dev/ttyUSB0 --sweep   # bit rate unknown
python3 tools/emco-listen.py --port /dev/ttyUSB0 --baud 9600
```

A USB-RS485 adapter on `X300` (A/D+ → pin 8, B/D− → pin 3, GND → pin 2),
receive-only. Logs everything heard (hex + ASCII, timestamped) and can
sweep candidate bit rates and framings. Expectations: the old PC card was
the bus master, so the slaves are probably silent without it — but
power-up behaviour is unknown and the CPU↔PLC daisy chain might chatter.
A logic analyser across DATA+/DATA− is better than a UART sweep, because
the framing may not be async at all. Silence is a real finding — log it.

### 3b. The capture bridge — timestamped, direction-tagged, always attached

```sh
python3 tools/emco-capture.py --port /dev/ttyACM1          # the record port
python3 tools/emco-listen.py  --port /dev/ttyACM0          # the raw port, unchanged
python3 tools/emco-capture.py --replay capture.bin         # no hardware
```

For any capture longer than a few minutes, use
[`../firmware/`](../firmware/README.md) rather than a bare dongle. It is an
STM32F4 that presents two USB serial ports: a **raw passthrough**, which
`emco-listen.py` reads unchanged, and a **record stream** carrying a hardware
microsecond timestamp per byte, a direction tag, and the UART's own
framing/parity/noise/overrun flags.

The three things a dongle cannot give are the three this ladder needs. Host
timestamps are taken at `read()`, after USB's 1 ms frames and `cdc_acm`
buffering, and at 9600 baud a character is 1042 µs — so host timing destroys
the inter-byte signature that would identify a polled protocol. On 3-wire
half-duplex, an echo and a reply are the same bytes. And `pyserial` cannot see
a framing error at all, which matters because the wire may not be async-UART
framed (doc 30 §2.2, A1): an error at a known timestamp is a **positive
finding**.

It is listen-only by default as a compile-time property — the transmit path is
not in the binary — and every `STATUS` record names the build variant, so a
transcript proves which firmware produced it. It does **not** replace a logic
analyser across `DATA±`, which is still the better first instrument.

`tools/virtual-bridge.py` puts both ports on ptys with a fake clock, so all of
this is exercisable with no hardware at all — see below.

### 4. `emco-probe.py` — guarded active probing (last resort)

```sh
python3 tools/emco-probe.py --list       # see the probe catalogue
python3 tools/emco-probe.py --dry-run    # exact bytes, nothing sent
python3 tools/emco-probe.py --port /dev/ttyUSB0 --i-am-at-the-machine
```

Sends curated candidate frames one at a time, safest-first, recording
every byte both ways. Be honest about the odds: the original framing was
generated by ISA-card hardware, not by software emitting text, so **blind
ASCII probing is unlikely to elicit anything**. Its realistic use is
replaying frames captured in steps 2–3. The interlocks: refuses to run
without `--i-am-at-the-machine`, requires a typed safety-checklist
acknowledgement, asks before every probe, and double-locks anything
motion-adjacent behind `--allow-motion` plus a typed per-probe
confirmation. Only with an empty chuck, clear tooling, closed guard, and a
hand on the mains switch.

### 5. Escalate to the retrofit

If steps 1–4 are exhausted (suggested budget: one season of archive
hunting alongside other work) the non-invasive path has no remaining
moves. Take the decision through
[`../docs/adr/0001-serial-vs-retrofit.md`](../docs/adr/0001-serial-vs-retrofit.md)
and execute [`../docs/60-retrofit-design.md`](../docs/60-retrofit-design.md).

## Development against no hardware

### `virtual-emco.py` — simulated PC Turn 50 on a pty

```sh
python3 tools/virtual-emco.py
# virtual EMCO PC Turn 50 control box up
#   pty: /dev/pts/7
```

Implements the `PlainGcode` dialect from
[`../sender/INTERFACE.md`](../sender/INTERFACE.md) — a **development and
training fiction**, explicitly not the real protocol. What it does model
faithfully is the machine: real travels (X 0–48 mm radius, Z 0 to
−228 mm), the **6-station turret** (3 external + 3 internal), **no
end-of-travel limit switches** (over-travel stalls the stepper and
silently loses position, like the real stops), and **reference lost at
every power-off** (`G28` required; SIGUSR1 simulates a power cycle). It
speaks G0/G1/G4/G7/G8/G18/G20/G21/G28/G40/G53/G54/G90/G91/G94/G95, M2–M6
and M30, plus the realtime bytes `?` `!` `~` Ctrl-X, with XON/XOFF pacing.
Stdlib only — no pyserial needed. Point `sender/cli/send.py`, the web
sender, or plain `picocom` at the printed pty.

If the real protocol is ever discovered, findings land here so everything
downstream can be tested against them.

### `test_virtual_emco.py` — unit tests

```sh
python3 tools/test_virtual_emco.py
```

Exercises the simulator's parsing, motion timing, coordinate modes
(diameter, work offsets, feed-per-rev), flow control and alarm behaviour
with a fake clock: no hardware, no pty, no pyserial.

### `virtual-bridge.py` — a simulated capture bridge on two ptys

```sh
python3 tools/virtual-bridge.py
# virtual RS-485 capture bridge up
#   raw     : /dev/pts/7     (point tools/emco-listen.py here)
#   records : /dev/pts/9     (point tools/emco-capture.py here)
```

Both USB ports of [`../firmware/`](../firmware/README.md), on a fake clock, so
the framing specification is executable before a board exists. The bus traffic
is an **invented** polled-multidrop pattern — a development fiction, like
`virtual-emco.py`'s dialect — chosen to exercise every record kind and every
flag. Stdlib only.

### `test_emco_bridge.py` — decoder tests and the cross-language corpus

```sh
python3 tools/test_emco_bridge.py
```

Two kinds of case. The first is ordinary decoder testing: timestamps, flags,
direction, clock un-wrapping, and resynchronisation swept across every possible
cut point. The second matters more — there are **two** implementations of the
record format, a Rust encoder on the bridge and this Python decoder, and two
implementations of a wire format drift silently. So
`firmware/bridge-core/tests/golden.rs` writes a fixed corpus, a Rust test
asserts its bytes have not changed, and these cases assert the Python decoder
reproduces exactly the records the scenario describes. Neither side can move
without the other going red.

### `image-size.py` — what would actually be flashed

```sh
python3 tools/image-size.py firmware.elf --min-bytes 16384
```

Sums an ELF's `PT_LOAD` segments. It exists because of one silent failure:
without `-Tlink.x` the linker keeps no vector table, garbage-collects the
program to nothing, and **still succeeds**. Build, flash and verify all report
success and the board does nothing.

## Related

- [`../firmware/README.md`](../firmware/README.md) — the capture bridge these
  two new tools talk to, and the honest list of what has not been verified on
  it.
- `../sender/cli/send.py` — headless G-code sender (streams a file with
  ack pacing and flow control); the counterpart these tools are tested
  against, and the fallback when no Web Serial browser is available.
- `../sender/INTERFACE.md` — the dialect contract everything shares.
- `../docs/30-serial-protocol.md` — the living protocol spec, safety
  rules, and findings log these tools feed.

## Dependencies

Python 3 standard library, plus **pyserial** for the two tools that open
real serial ports (`emco-listen.py`, `emco-probe.py`; `send.py` prefers it
too). Both are in the repo's nix dev shell: `nix develop` from the repo
root. Otherwise `pip install pyserial`. `socat` and `picocom` (also in the
dev shell) are useful for the capture and manual-poking workflows.

## Safety, once more

Nothing here is a safety function. The probe tool's interlocks reduce the
chance of commanding motion by accident; they do not make motion safe.
The only real stop is the **mains switch on the control box**, operated by
a person standing at the machine. Never leave any of these tools running
against the real machine unattended. Full rules: doc 30 §5.
