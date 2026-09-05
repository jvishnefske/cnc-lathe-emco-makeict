# DOSBox capture — the cleanest route to the AC95 protocol

The AC95 wire protocol is undocumented. The cleanest way to learn it is not
to poke the box and guess, but to **watch the original EMCO DOS software
talk**, with the machine disconnected. If the software is found, one
captured session answers most of the open questions at zero risk: baud and
framing, whether the wire carries G-code text or lower-level block/motion
commands, what the handshake looks like, and what a start/stop sequence is.

**Honest caveat up front:** this whole route depends on obtaining the
original EMCO DOS software, which we do not currently have. Everything
below is ready to use the day a copy turns up.

## 1. Find the software

The PC Turn 50 was driven by EMCO's DOS training packages, which put a
software control on the PC (the Fanuc 0-T variant is the one reported with
this machine; Sinumerik and Emcotronic variants also existed). Places to
look, roughly in order of likelihood:

- **The floppies stored with the machine.** EMCO shipped the software with
  the lathe; school machines usually kept the box of disks nearby. Check
  drawers, binders, and the PC (or its disk image) if the original host
  survives anywhere.
- Other owners of PC Turn 50/55 machines — CNCzone threads, the
  r/hobbycnc and EMCO user communities have exchanged disk images before.
- Abandonware archives: WinWorld, archive.org's software collections,
  old FTP mirrors. Search terms: `EMCO PC TURN`, `EMCO F1 CNC`,
  `EMCO Fanuc 0-T`, `WinTutor`, `EMCOtronic`.
- EMCO Maier itself (Hallein, Austria) still supports the education line;
  a polite email has worked for other legacy-machine owners.

If you image floppies, image them raw (`dd` or Greaseweazle) before doing
anything else — the disks are 30+ years old.

## 2. The idea

```
DOS EMCO software ──COM1──> DOSBox-X nullmodem ──TCP──> socat ──> pty + hex log
                                                                   │
                                             (optional) tools/virtual-ac95.py
                                             or nothing at all — just log
```

DOSBox-X emulates COM1 as a "nullmodem" that speaks raw bytes over TCP.
socat accepts that TCP connection, mirrors it onto a pty, and — the
important part — logs **every byte in both directions with timestamps**.

The machine is never involved. Do this at a desk.

## 3. Run it

Start the logger first (from the repo root; socat is in the nix dev shell):

```sh
mkdir -p captures
socat -d -d -lu -x -v \
    TCP-LISTEN:5555,reuseaddr \
    PTY,link=/tmp/emco-dos-pty,raw,echo=0 \
    2>captures/dos-session-$(date +%Y%m%d-%H%M%S).log
```

- `-x -v` hex-dumps and text-dumps both directions to stderr,
  `>` vs `<` marking direction; `-lu` adds microsecond timestamps.
- `/tmp/emco-dos-pty` is the "machine side". Leave it unconnected for a
  pure capture — the DOS software will think the cable is plugged in but
  the controller silent, which is itself informative (what does it send
  first? does it retry? does it wait on CTS?).

Then, in another terminal:

```sh
dosbox-x -conf tools/dosbox-capture/dosbox-x.conf
```

Edit the `mount` line in the conf (or at the DOS prompt) to point at the
unpacked EMCO software, start it, and go through the motions: connect,
send a tiny program, single-step it. Everything crossing COM1 lands in the
log file.

Config lines that matter (already in `dosbox-x.conf`):

```ini
[serial]
serial1 = nullmodem server:127.0.0.1 port:5555 transparent:1
```

`transparent:1` is essential — without it the nullmodem speaks telnet and
escapes 0xFF bytes, corrupting a binary protocol capture.

## 4. Variations

- **Talk back:** point the software at our simulator to test hypotheses —
  run `python3 tools/virtual-ac95.py` (it prints its pty path, e.g.
  `/dev/pts/7`) and bridge it instead of a dead-end pty:

  ```sh
  socat -d -d -lu -x -v \
      TCP-LISTEN:5555,reuseaddr \
      /dev/pts/7,raw,echo=0 \
      2>captures/dos-vs-sim-$(date +%Y%m%d-%H%M%S).log
  ```

  If the DOS software gets further against the simulator after a protocol
  tweak, the tweak is probably right. Iterate.

- **Manual poking from the pty side:** `picocom /tmp/emco-dos-pty` lets a
  human play "machine" by hand while the log records everything.

- **QEMU instead of DOSBox-X:** boot a real DOS image with
  `-serial tcp:127.0.0.1:5555` (same socat listener) or `-serial pty`
  (then log that pty with socat). QEMU's 16550 emulation is more faithful
  if the EMCO software does exotic UART programming; DOSBox-X is easier to
  drive.

## 5. Reading the capture

Each socat log line looks like:

```
> 2026/09/04 12:34:56.789012  length=4 from=0 to=3
 25 0d 4e 31                                      %.N1
```

`>` is DOS→machine, `<` is machine→DOS. Things to extract:

1. **First bytes sent** on connect — likely an enquiry or reset sequence;
   prime candidates for `tools/ac95-probe.py`'s curated list.
2. **Framing of a program transfer** — raw G-code lines? `%` tape
   wrapper? binary blocks with checksums?
3. **Pacing** — the timestamps show whether the software waits between
   blocks (suggesting CTS/ack gating) or streams freely.
4. **The stop/abort sequence**, if you press stop in the software —
   this is the single most safety-relevant byte sequence to learn.

Baud rate does not appear in a TCP capture. Get it from the DOS
software's config screen/files (look for `COM1`, `9600`, `MODE` in
`.CFG`/`.INI`/`.BAT` files), or from `tools/ac95-listen.py --sweep`
against the real box.

Drop captures in `captures/` (git-ignored or committed as evidence —
committing them is encouraged) and write findings up in
`docs/30-serial-protocol.md`.
