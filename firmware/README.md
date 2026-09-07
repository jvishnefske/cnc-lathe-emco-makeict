# `firmware/` — the USB↔RS-485 capture bridge

An STM32F4 with a USB connector and an RS-485 transceiver, used as a **safe,
always-attached, timestamped, direction-tagged** capture device on the EMCO PC
Turn 50's `X300` bus.

> **The bus is RS-485 and its protocol is unknown.** Read
> [`../docs/30-serial-protocol.md`](../docs/30-serial-protocol.md) before
> connecting anything, and meter the DB9 first (§7). Never connect an RS-232
> adapter or PC COM port to `X300`. **Do not fit a 120 Ω terminator** — doc 30
> §1.1 records the bus as already terminated and biased at both ends, and a
> third terminator is a real hazard. Leave pin 9 (+5 V) unconnected.
>
> Nothing here is a safety function. The only stop is the mains switch at the
> machine, operated by a person standing at it (doc 30 §5).

## Why build hardware rather than buy a dongle

`tools/emco-listen.py` with a USB-RS485 dongle is the right first move and it
stays the right first move. What it cannot do is the reason for this:

- **Timing.** Host timestamps are taken at `read()`, which is after USB's 1 ms
  frames, after `cdc_acm` buffering and after scheduler latency. At 9600 baud a
  character is 1042 µs, so host timing destroys exactly the inter-byte and
  inter-frame signature that identifies a polled multidrop protocol.
- **Direction.** On a 3-wire half-duplex bus, once transmit has ever been
  armed, your own echo and a reply are the same bytes on the same conductors.
  Only the bridge knows which.
- **Errors.** `pyserial` cannot see a framing, parity, noise or overrun
  condition at all. Doc 30 §2.2 (A1) records that the wire **may not be
  async-UART framed at all**, which makes an error at a known timestamp a
  *positive finding* rather than noise.

### It does not replace a logic analyser

Doc 30 rung 2 says an analyser across `DATA+`/`DATA−` is the better **first**
instrument, precisely because the framing may not be async. That is still true.
Capture edges, decide what the framing is, and *then* leave this attached for
the long, safe, repeatable captures — through power cycles, across sessions,
with the transcript proving which firmware produced it. Use both.

An optional edge-capture mode (timer input capture → run-length `EDGES`
records) would close the gap, and is deliberately **not** built: a £10
logic-analyser clone plus `sigrok` is cheaper than the firmware, and until that
turns out not to be enough, building it would be inventing a requirement.

## The two crates, and why they are two

| Crate | What is in it | How it is tested |
|---|---|---|
| [`bridge-core/`](bridge-core/) | The record format, idle-gap segmentation, the outbound ring, and the overflow accounting. No HAL, no clock, no allocation — it takes `(micros, byte, flags)` in and gives framed bytes out. | `cargo test` on your laptop, every time, with nothing installed |
| [`rs485-bridge/`](rs485-bridge/) | embassy, pins, DMA, USB, DE. A thin shell around the crate above. | compile-only; bench calibration by hand |

The split is the same one `sender/core` uses, and for the same reason: the part
with the decisions in it should be testable where you are, not where the board
is. Everything that could be wrong about *what a record means* is in
`bridge-core` and is exercised by 30 tests that need no hardware.

## Two USB ports

The bridge enumerates as one composite USB device with two CDC-ACM interfaces:

| Port | Contents | Tool |
|---|---|---|
| first `/dev/ttyACM*` | **raw passthrough** — every byte, nothing added, best-effort | `tools/emco-listen.py`, **unchanged** |
| second `/dev/ttyACM*` | **framed records** — hardware microsecond timestamp, direction tag, UART error flags, with accounting for anything dropped | `tools/emco-capture.py` |

The first port exists so the safety-reviewed tool with the RS-232 warnings
does not have to be touched. The second is the one worth having; the format is
specified in [`bridge-core/src/record.rs`](bridge-core/src/record.rs).

## Listen-only is a property, not a mode

Three independent layers, and the point of three is that no single mistake
defeats them:

1. **Compile time.** `listen-only` is a default cargo feature. With it on, the
   transmit task is not in the binary, the USART is constructed receive-only,
   and DE is an input with a pull-down. A bug cannot assert a driver whose code
   does not exist. `firmware/check.sh` greps the built ELF to prove the task is
   absent — and greps the *transmitting* build to prove the absence means
   something.
2. **Hardware.** DE pulled to GND on the adapter, reaching the MCU only through
   a link that is not fitted by default.
3. **Evidence.** Every `STATUS` record names the build variant, so a transcript
   proves which firmware produced it — which is what a findings-log entry
   (doc 30 §8) needs and what nobody can reconstruct afterwards.

Building the transmitting variant is a deliberate act:

```sh
cd firmware/rs485-bridge
cargo build --release --no-default-features --features board-f407-discovery
```

and everything in doc 30 §5 and rung 3 applies to using it.

## Board pins

One file per board, containing a table and no logic:
[`rs485-bridge/src/board/f407_discovery.rs`](rs485-bridge/src/board/f407_discovery.rs).
Correcting a pin is a one-line edit in a file with nothing else in it, and
getting one wrong is a type error rather than a device that enumerates and
never receives.

**Every pin in it is provisional.** Nobody has yet confirmed which board this
is, which USART the transceiver is on, or which pin drives DE. The file assumes
a stock F407VG Discovery so that the firmware could be written and compiled at
all. Two traps are named in it: the VBUS-sense wiring that stops several
Discovery revisions enumerating until `vbus_detection` is off, and the
`RE`/`DE` strapping question.

## Building and checking

```sh
rustup target add thumbv7em-none-eabihf
./firmware/check.sh          # everything that does not need a board
```

That runs `bridge-core`'s tests, both firmware builds, the image-size
assertion, the listen-only symbol check, the Python decoder tests including the
cross-language corpus, and the simulator end to end.

To flash, with the board on its ST-LINK:

```sh
cd firmware/rs485-bridge
cargo run --release          # probe-rs run --chip STM32F407VGTx
```

### The image-size assertion, and why it is there

Without `-C link-arg=-Tlink.x` the linker keeps no `.vector_table`,
garbage-collects the program as unreachable, and **still produces a valid
ELF**. Build succeeds, flash succeeds, board does nothing.
`tools/image-size.py` sums the `PT_LOAD` segments and fails below a plausible
floor, so that failure announces itself.

## Testing without hardware

```sh
python3 tools/virtual-bridge.py                    # two ptys, a fake clock
python3 tools/emco-capture.py --port /dev/pts/N    # the record port
python3 tools/emco-listen.py  --port /dev/pts/M    # the raw port, unchanged
```

`virtual-bridge.py` emits an **invented** polled-multidrop pattern — a
development fiction in the same spirit as `virtual-emco.py`'s `PlainGcode`
dialect, and nothing it produces is a claim about the real machine. What it
does exercise is every record kind and every flag, so the host half of the
toolchain is testable before a board exists.

The cross-language corpus is the guard against the two implementations of the
record format drifting: `bridge-core/tests/golden.rs` writes
`bridge-core/tests/golden.bin` and asserts it has not changed;
`tools/test_emco_bridge.py` asserts the Python decoder reproduces exactly the
records the scenario describes. Neither side can move without the other going
red. After a deliberate format change, bump `PROTOCOL_VERSION` in
`bridge-core/src/lib.rs` **and** `tools/emco-capture.py`, then:

```sh
cd firmware/bridge-core && BRIDGE_WRITE_GOLDEN=1 cargo test --test golden
```

## Not verified

Nobody has run this on a board. Said plainly, because the rest of this project
is careful about the difference between known and assumed:

| | Status |
|---|---|
| USB enumeration | **Untested.** See the VBUS trap in the board file. |
| Timestamp accuracy | **Untested.** See the bench experiment below. |
| DE turnaround at 9600 and 115200 | **Untested.** |
| The pin table | **Provisional.** Nobody has said what the hardware is. |

### The bench experiment that settles the timestamps

The stamp is taken in the task woken by the byte, not in the interrupt, so it
carries executor and DMA-completion latency. The measurement that matters is
the **gap** between two bytes, and a constant latency cancels out of a
difference — only jitter survives. How much jitter is empirical:

1. Loop the bridge's RX to a USB-RS485 dongle transmitting a known pattern at a
   known bit rate (9600, then 115200).
2. Capture with `tools/emco-capture.py`.
3. Compare the reported inter-byte deltas against the theoretical character
   time: 10 bits at the bit rate, so 1042 µs at 9600 and 87 µs at 115200. The
   summary line already prints the median and the bit rate it implies.

No lathe required, and nothing in this repository should claim a timing figure
until it has been done.

## What is deliberately not here

- **USB networking (RNDIS/ECM).** There is no mature Rust device stack for
  STM32F4, and a browser cannot open raw sockets, so it would not help Web
  Serial either.
- **DMA with IDLE-line segmentation.** It is the conventional answer and it is
  the wrong one here: DMA gives one timestamp per burst, and the inter-byte
  gaps inside that burst would then have to be reconstructed from an *assumed*
  bit rate — which is the number we are trying to find out. Reconstructing it
  would be fabricating the measurement. Per-byte error attribution goes the
  same way. The cost is one wake per byte, 11 520/s at 115200 baud, which a
  168 MHz part with nothing else to do can afford.
