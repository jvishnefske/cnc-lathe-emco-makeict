# tools/ — AC95 protocol discovery and simulation

The EMCO PC Turn 50's red control box (AC95 board, `Y4B081000`) speaks an
**undocumented protocol** over its DB9 RS-232 port. These tools exist to
discover that protocol and to let the rest of the project develop against a
simulated machine in the meantime.

## Use them in this order

Escalate only as far as you need to. Each step is riskier than the one
before it.

### 0. `dosbox-capture/` — capture the original software (no machine at all)

If the original EMCO DOS software can be found (floppies stored with the
machine, or archives), running it under DOSBox-X with COM1 bridged to a
logged pty answers most questions in one session, **with the lathe
disconnected**. Zero risk, highest information. See
[`dosbox-capture/README.md`](dosbox-capture/README.md). Depends on finding
the software — start the search now, do the steps below meanwhile.

### 1. `ac95-listen.py` — passive listening (transmit-free)

```sh
python3 tools/ac95-listen.py --port /dev/ttyUSB0 --sweep   # find the baud
python3 tools/ac95-listen.py --port /dev/ttyUSB0 --baud 9600
```

Opens the port **without transmitting a single byte**, logs everything
received (hex + ASCII, timestamped), watches the modem control lines
(CTS/DSR/DCD/RI — community reports say the box gates transfers on CTS,
so line transitions are evidence), and can sweep baud rates and framings
scoring each for printable output. Power-cycle the control box and press
panel keys while it listens: boot banners and keypress chatter are common
on machines of this era. Always writes a transcript.

Safe with the machine powered; it cannot command anything.

### 2. `ac95-probe.py` — guarded active probing (transmits!)

```sh
python3 tools/ac95-probe.py --list       # see the probe catalogue
python3 tools/ac95-probe.py --dry-run    # exact bytes, nothing sent
python3 tools/ac95-probe.py --port /dev/ttyUSB0 --i-am-at-the-machine
```

Sends curated candidate strings **one at a time, safest-first** (pure
enquiry bytes → status/identify text → motion-adjacent commands) and
records every byte both ways. It refuses to run without
`--i-am-at-the-machine`, makes you type an acknowledgement of a physical
safety checklist, asks before every single probe, and keeps anything that
could conceivably move the machine double-locked behind `--allow-motion`
plus a typed per-probe confirmation.

Only reach for this after steps 0–1; only unlock motion probes with an
empty chuck, clear tooling, closed guard, and a hand on the mains switch.

## Development against no hardware

### `virtual-ac95.py` — simulated AC95 on a pty

```sh
python3 tools/virtual-ac95.py
# virtual AC95 up
#   pty: /dev/pts/7
```

Implements the `PlainGcode` dialect from
[`../sender/INTERFACE.md`](../sender/INTERFACE.md): CR-terminated G-code
lines, XON/XOFF flow control (the pty stand-in for the real CTS wire),
`ok`/`error:`/`ALARM:` responses, timed X/Z motion, an 8-station turret
with an index delay, spindle state, soft limits with latched alarms, and
the realtime bytes `?` (status), `!` (hold), `~` (resume), Ctrl-X (abort).
Stdlib only — no pyserial needed. Point `sender/cli/send.py`, the web
sender, or plain `picocom` at the printed pty.

As the real protocol gets discovered, this model is the place findings
land so everything downstream can be tested against them.

### `test_virtual_ac95.py` — unit tests

```sh
python3 tools/test_virtual_ac95.py
```

Exercises the simulator's parsing, motion timing, flow control and alarm
behaviour with a fake clock: no hardware, no pty, no pyserial.

## Related

- `../sender/cli/send.py` — headless G-code sender (streams a file with
  ack pacing and flow control); the counterpart these tools are tested
  against, and the fallback when no Web Serial browser is available.
- `../sender/INTERFACE.md` — the dialect contract everything shares.

## Dependencies

Python 3 standard library, plus **pyserial** for the two tools that open
real serial ports (`ac95-listen.py`, `ac95-probe.py`; `send.py` prefers it
too). Both are in the repo's nix dev shell: `nix develop` from the repo
root. Otherwise `pip install pyserial`. `socat` and `picocom` (also in the
dev shell) are useful for the capture and manual-poking workflows.

## Safety, once more

Nothing here is a safety function. The probe tool's interlocks reduce the
chance of commanding motion by accident; they do not make motion safe.
The only real stop is the **mains switch on the control box**, operated by
a person standing at the machine. Never leave any of these tools running
against the real machine unattended.
