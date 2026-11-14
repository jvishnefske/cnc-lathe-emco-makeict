# kd240 — the sender as a Linux service on the AMD Kria KD240

A Rust service for the KD240 drives starter kit (K24 SOM, Zynq UltraScale+,
Cortex-A53 under Kria Ubuntu). It serves the operator UI over Ethernet and
gives the shop a permanently attached, listen-only RS-485 capture of the
lathe's `X300` bus.

> **Nothing here moves the real lathe.** The RS-485 protocol is unknown and is
> not G-code ([`docs/30-serial-protocol.md`](../docs/30-serial-protocol.md),
> [`docs/35-rs485-lessons-learned.md`](../docs/35-rs485-lessons-learned.md)).
> The machine this service drives is `sender/core`'s `VirtualMachine`. **No
> code path transmits on the machine bus:** the RS-485 tty is opened read-only.
> Abort is advisory. The stop is the cabinet's EMERGENCY-OFF and mains switch,
> with a person at the machine. Never run it unattended or remotely.

```
  browser (shop LAN) ──HTTP + SSE──►  emco-kd240 on the KD240's A53
                                       │
                                       ├─ machine pump: sender/core Session ⇄ VirtualMachine (10 ms tick)
                                       │     the Dialect seam is where a recovered emco-485 dialect lands
                                       │
                                       └─ capture: RS-485 tty, O_RDONLY ──► bridge-core records ──► captures/*.bin
                                                                                  (tools/emco-capture.py --replay)
```

## What it does

| Part | What | Source |
|---|---|---|
| Machine pump | `Session` against `VirtualMachine`, pumped on a thread as the browser sender's rAF loop does. Commands are the `sender/INTERFACE.md` `Command`s. | `src/machine.rs` |
| Programs | G-code stored as `<data>/programs/<name>.nc`. Loading parses with `sender/core`'s parser and returns diagnostics and the backplot. A program with errors is refused (422). | `src/store.rs`, `src/api.rs` |
| Capture | The KD240's RS-485 port, read-only, at a bit rate and framing from doc 30 §7's sweep. The output is `emco-bridge-core`'s record stream, byte for byte, so `tools/emco-capture.py --replay` decodes it unchanged. UART framing and parity errors arrive in-band (`PARMRK`) and become the records' flags. | `src/capture.rs`, `src/parmrk.rs` |
| UI | Program pane, toolpath (sender/web's renderer ported), DRO with the simulator's true position, controls, training faults, capture panel, log. RxJS streams, plain ES modules, no build step, embedded in the binary. | `ui/` |

### What the capture is and is not

* **Is:** proof of "the bus is silent without a master" (a real finding, doc
  30 §4 rung 2), byte values, and *where* the UART flagged framing or parity
  errors. It is safe to leave attached, because it cannot transmit.
* **Is not:** a timing instrument. Timestamps are host `read()` times: bytes
  from one read share one timestamp, so `emco-capture.py` shows `gaps(us)
  0,0,…` and its baud estimate from gaps is meaningless here. Every `STATUS`
  record says `HOST read() timestamps, not wire`, so a transcript cannot be
  mistaken for a bridge one. Don't quote timing from it in doc 30 §8. Use
  the bridge (`firmware/`) or a logic analyser for timing.
* "Listen-only" here is a software property: the tty has no write handle.
  On Linux a UART asserts its RS-485 transceiver's driver-enable only while
  transmitting, but **check on the board** that the KD240's transceiver DE
  stays low with this service capturing (meter DE, or watch the bus with an
  analyser while it runs) before attaching it to `X300`.

## Wiring to X300 (from doc 30 §1.2 and §7; meter first)

* KD240 RS-485 A/D+ → `X300` pin 8, B/D− → pin 3, signal ground → pin 2.
  **Nothing on pin 9** (+5 V for the machine's own bias network).
* **No terminator.** The bus is already terminated and biased at both ends.
  If the KD240's RS-485 port has a termination jumper or resistor, it must
  be off.
* Label the cable "RS-485 ONLY — NOT RS-232".

Which tty the KD240's RS-485 port is (`/dev/ttyPS1`, a PL UART such as
`/dev/ttyUL*`, …) depends on the board's device tree and the loaded PL
design. Find it on the board and pass it with `--rs485`. It hasn't been
checked yet.

## Build

```sh
cd kd240
cargo test                       # 18 tests: unit, the API through the router, and a capture over a pty
                                 #   decoded by tools/emco-capture.py (needs python3; skipped without)
(cd ui && node --test test/)     # 9 tests: the RxJS stream logic and plot math (Node ≥ 18, no npm)

# For the KD240 (aarch64; needs aarch64-linux-gnu-gcc, glibc ≤ 2.34 required)
rustup target add aarch64-unknown-linux-gnu
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --release --target aarch64-unknown-linux-gnu
```

## Run

```sh
# On a laptop, against the virtual machine only:
head -c 24 /dev/urandom | base64 > token
cargo run -- --listen 127.0.0.1:8240 --data ./data --token-file token
# open http://127.0.0.1:8240/, paste the token in the header

# On the KD240, with the capture:
emco-kd240 --listen 0.0.0.0:8080 --data /var/lib/emco-kd240 \
           --token-file /etc/emco-kd240/token --rs485 /dev/ttyPS1
```

On the NixOS card the service is `services.emco-kd240` (`nix/service.nix`),
already enabled. On another Linux (Kria Ubuntu), `emco-kd240.service` is a
systemd unit for the board: copy the binary to
`/usr/local/bin` and the unit to `/etc/systemd/system`, create a system user
`emco` and the token file `/etc/emco-kd240/token` (readable by it), then
`systemctl enable --now emco-kd240`. The unit gives the service read access
to `ttyS`, `ttyPS` (Zynq PS UART) and `ttyUL` (AXI UART Lite) devices only.
Those device-class names have not been checked on the board yet.

Reads are open. Every change (`POST`, `PUT`, `DELETE`) needs `Authorization:
Bearer <token>`. `--no-auth` exists for a closed bench network. The token
keeps a passer-by on the shop LAN from changing things. It does not make
remote operation acceptable.

## The SD card (NixOS), and updates over the network

`nix/configuration.nix` is the board's whole system: NixOS 25.11 on the SD
card, this service, SSH and mDNS. After the first card, every update goes
over Ethernet with `nixos-rebuild`. There's no reflashing, and each update
is a new boot generation you can roll back to.

```sh
cd kd240
just image                 # result-sd/sd-image/emco-kd240-*.img.zst
just flash /dev/sdX        # whole card; asks before writing
# card into the KD240, Ethernet plugged in, power on; then from the laptop:
ssh admin@kd240.local      # your key from nix/authorized_keys
just token                 # the web UI's bearer token, made on the board
just deploy                # after changing anything: build here, switch the board
```

**What's on the card.** Partition 1 is FAT, labelled `KD240BOOT` and
marked bootable, and holds only `boot.scr`. Partition 2 is the ext4 root,
with `/boot/extlinux/extlinux.conf` and a kernel, initrd and device tree per
generation (the last five). It grows to fill the card on first boot. The
kernel is mainline 6.18 LTS from the NixOS binary cache, with
`xilinx/zynqmp-smk-k24-revA-sck-kd-g-revA.dtb` (U-Boot reports
`board_name=SMK-K24-XCL2G`). 6.12, the release's default, has no KD240
device tree. The console is `ttyPS1` at 115200 on the board's USB-UART,
channel 1 of the "Xilinx KD Carrier Card" FT4232H
(`/dev/serial/by-id/usb-Xilinx_KD_Carrier_Card_*-if01-port0`).

**Boot chain (checked on the board).** The K24 SOM always boots from its
QSPI first. AMD's factory boot firmware (here U-Boot 2025.01, Nov 2025)
lives there, and the card doesn't touch it or its saved environment. That
U-Boot finds the card through the KD240's USB card reader (`usb 0`) and
runs the first boot method on its bootable partition. Its extlinux can't
boot NixOS directly, because it loads the initrd at `0x02100000`, inside the
~62 MB arm64 kernel loaded at `0x200000` ("RD image overlaps OS image"). So
`boot.scr` moves `ramdisk_addr_r` to `0x18000000` and then runs
`sysboot` on the root partition's extlinux menu, the one `nixos-rebuild`
maintains. In the initrd, the USB root needs `gpio-zynq` (the I²C bus's
recovery pins), `i2c-cadence`, and `gpio-pca9570` (the SLG7XL45106 reset
chip that releases the USB PHY, hub, SD and Ethernet resets). Without them
the USB controller defers for ever.

**RS-485.** PS UART0, `/dev/ttyPS0`. The device tree runs it in RS-485
mode with the transceiver's DE on `rts-gpios`, so Linux drives DE only
while transmitting. The service opens it read-only and captures only when
started from the UI.

**Updates.** `just deploy` runs `nixos-rebuild switch --flake
..#nixosConfigurations.kd240 --target-host admin@kd240.local --sudo`. The
system is built on the laptop (aarch64 under qemu, mostly fetched from the
binary cache), copied to the board over SSH, activated, and its boot entry
is written to `/boot`. To undo a bad update, run `nixos-rebuild switch
--rollback` over SSH, or pick an older entry at the U-Boot prompt on the
serial console. `just build-only` installs the new system to take effect
at the next boot. The board runs a weekly `nix-collect-garbage`, and the
boot menu keeps the last five generations.

**Access.** SSH accepts keys only. `admin` gets passwordless sudo, because
the deploy activates with no terminal to type into, so the SSH key is the
credential. `nix/authorized_keys` must list at least one key or the build
stops. With none, the only way back in would be to reflash. Add a second
person's key there and deploy. The web UI's token is made on the board at
first start, so it's in neither the image nor the repository.

**Building on x86-64** needs qemu-user binfmt (Ubuntu: `qemu-user-static`)
and `extra-platforms = aarch64-linux` in the Nix daemon's config
(`/etc/nix/nix.custom.conf` with Determinate Nix), then a daemon restart.

**On the board (2026-11-14):** the KD240 boots NixOS unaided from the
card. It goes through the factory QSPI U-Boot to `boot.scr`, then the root
partition's extlinux menu, then this kernel and initrd, and the root mounts
behind the USB5744 hub. It gets `192.168.1.134` by DHCP on `end0`,
answers SSH as `admin`, and serves the web UI on port 8080, with the
capture on `/dev/ttyPS0`. `nixos-rebuild switch --target-host
admin@192.168.1.134 --sudo` has deployed a configuration change to it
(generation 3). Things that were each found on the board, with what fixed
them: the initrd/kernel address clash (`boot.scr`), the missing
`gpio-zynq`/`gpio-pca9570` in the initrd, the kernel's missing
`CONFIG_USB_ONBOARD_DEV_USB5744`, and avahi's stale PID file on restart.

The card in the board was converted in place over serial and SSH, not
rewritten. An image written fresh from this configuration (`just image`)
is built but hasn't been booted yet.

The kernel is the only part not in the binary cache. It's cross-compiled on
an x86-64 Linux host in about 20 minutes, the first time only, so deploy
from such a host. `kd240.local` needs an mDNS resolver on the deploying
machine. Without one, give the address: `just deploy admin@192.168.1.134`.

## API

| Route | |
|---|---|
| `GET /` | the UI |
| `GET /api/profile` | the `MachineProfile` |
| `GET /api/state` | `{ session: SessionState, sim: Dro, transport: "virtual" }` |
| `GET /api/events` | SSE: `state` (snapshots), `core` (the session's `Event`s), `lagged` (n missed) |
| `GET /api/programs`, `GET/PUT/DELETE /api/programs/{name}` | stored programs (names `[A-Za-z0-9._-]`, ≤ 64, ≤ 1 MiB) |
| `POST /api/programs/{name}/load` | parse and load: `{ name, lines, diagnostics, errors, backplot }`. 422 with errors, 409 while running |
| `GET /api/program` | the loaded program |
| `POST /api/command` | a `Command` (`{"kind":"run"}`, `{"kind":"jog","axis":"x","deltaMm":1,"feedMmMin":200}`, …) |
| `POST /api/sim/fault` | `{"kind": "estop" \| "limit_x" \| "limit_z" \| "turret_jam"}` |
| `GET /api/capture`, `POST /api/capture/start` `{baud, framing}`, `POST /api/capture/stop` | the capture. The port comes from `--rs485` only |
| `GET /api/captures`, `GET /api/captures/{name}` | capture files (download) |

## Not done yet

* **On hardware:** nothing has run on a KD240 yet. Still to check there: the
  binary under Kria Ubuntu, the RS-485 tty and its DE behaviour, and a
  capture at `X300` (doc 30 §7 metering first).
* **The UI in a browser:** the page is served and its stream logic is
  tested under Node, but no browser has rendered it yet.
* **The R5F, evaluated later:** the useful job for the KD240's Cortex-R5F is
  the timing this capture lacks. It would timestamp each byte in hardware
  from the UART interrupt, with direction and error flags, and hand the
  bridge-core records to Linux (RPMsg). That's the STM32 bridge's job done
  on the board, and `emco-bridge-core` is `no_std` with no dependencies, so
  the record code ports as is.
* **A real transport:** only once a recovered `emco-485` dialect exists
  behind `sender/core`'s `Dialect` seam, decided in the ADR, not before.
