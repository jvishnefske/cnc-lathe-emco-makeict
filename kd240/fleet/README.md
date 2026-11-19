# kd240/fleet: telemetry and signed A/B updates for the KD240

A small server and a board agent. The board **polls** the server over
outbound HTTPS (it has no inbound port for this). It reports telemetry, and
it fetches **owner-signed** orders: release manifests, and `stage` and
`reboot` commands. A release is a **bundle**: one A/B slot's worth of files,
built by `nix build .#kd240-bundle`. The agent writes it into the slot the
board is not running from, reads it back, and on a `reboot` order has U-Boot
try it once. The new system commits itself once healthy; if it never does,
the next reset boots the old slot. The server stores and relays. It never
holds a signing key.

> **No machine commands, by design.** There's no G-code, no jog, no run, and
> nothing for the RS-485 bus. The agent's only contact with the lathe
> service is reading `GET /api/state`'s `session.run`, for the interlock and
> the health check. `msg::Action` has two variants, `stage` and `reboot`,
> and unknown ones fail to parse.

```
  build host                             owner's host (docker, Traefik)        KD240 (NixOS, A/B card)
  ──────────                             ──────────────────────────────        ───────────────────────
  nix build .#kd240-bundle               emco-fleet-server                     emco-fleet-agent (timer, root)
  emco-fleet publish ──HTTPS──►  /api/v1/files/<sha256>  ◄──HTTPS── download into the inactive slot,
    (signs with the card via mwctl,      /api/v1/devices/…        poll      check size + SHA-256, read back;
     or a software key)                  signed envelopes,                  verify order (middleware trust),
                                         telemetry, history                 interlock on GET :8080/api/state
```

## Signing: middleware's keys

The order signature is **middleware's** (`middleware::loader::trust`): the
same key types (Ed25519, or ECDSA P-256 as raw `r ‖ s`) and the same
`verify_signature` that check a signed WASM payload on a node. The CLI signs
through **mwctl**'s signers, so **one hardware key** (the owner's smart
card, a TPM, anything PKCS#11) signs both middleware payloads
(`mwctl upload --pkcs11 LABEL …`) and KD240 orders (`emco-fleet --pkcs11 LABEL`).
A software ed25519 key (`emco-fleet keygen`) still works, for tests and for
a fleet without a card.

What stays the fleet's own is the domain: a signature covers
`"emco-fleet order v1\n" ‖ payload`, so a signature this key made over a
payload commit (`mw-payload-v1\0…`) can never be passed off as an order,
nor the reverse. The board's trusted keys are text, `<name>:<base64>`
(ed25519) or `<name>:p256:<base64 SPKI>`, and the key id is the SPKI's
SHA-256 (`emco-fleet pubkey` prints both), as `openssl pkey -pubin -outform
der | sha256sum` gives for the card's certificate.

## Trust model

* **Orders are signed offline.** The server stores the envelope as it came.
  The board verifies it against `trustedKeys`, then checks its own device
  id, a sequence number above the last one it accepted (persisted in
  `/var/lib/emco-fleet-agent/state.json`, so a replay is refused), a
  command's expiry (`not_after`, default 1 h), and the bundle: format
  `emco-bundle-1`, this device class, exactly `rootfs.squashfs`, `Image`,
  `initrd`, `dtb`, `cmdline` in that order, each with a size and SHA-256.
  Signed payloads reject unknown fields, so a restriction added by a newer
  signer can't be silently ignored by an older board.
* **File integrity is the signed bundle's.** The server keeps files by
  SHA-256 and checks each upload against its name, but the board does not
  rely on that: it hashes every file as it writes it, refuses one that is
  longer, shorter or different, and then reads the whole slot back from the
  card (page cache dropped) and checks it again.
* **A compromised server** can withhold orders, replay them (refused),
  serve junk files (refused, the slot is invalidated), delay a command until
  it expires, or read the telemetry. It cannot make the board boot anything
  the owner didn't sign.
* **Tokens.** Each device has a bearer token for its own poll endpoint and
  for downloading bundle files (which are the same for every board). A
  separate admin token covers publishing, uploading and reading. The server
  keeps tokens as `sha256:<hex>`, or hashes raw tokens on load, and compares
  them in constant time.

## The slots and the interlock (board side)

The policy is in `proto/src/policy.rs` and the boot-state format in
`proto/src/bootstate.rs`, as pure functions with unit tests.
`agent/src/lib.rs` carries them out, and `agent/tests/end_to_end.rs` checks
them end to end against a fake card that also plays U-Boot's part.

| Step | Allowed when | |
|---|---|---|
| download and write the inactive slot (`stage`, or a manifest with `--stage`) | always, while this boot is committed | nothing running uses that slot |
| `reboot` with a bundle | `run` is `idle` or `disconnected` | stage it (or re-check it if it is there), mark its slot to be tried once, report, **check again immediately before**, `systemctl reboot` |
| `reboot` without one | the same | the committed slot boots again |
| commit a trial boot | the lathe service answers `GET /api/state` | `ab_active` becomes the booted slot |

* **The only slot written is the other one**, and only while this boot is
  the committed one (`policy::stage_target`). During a trial boot the other
  slot is the way back, so staging is refused until the trial commits.
* **The boot state** is two 512-byte copies in the gap before partition 1
  (LBA 4096 and 6144), each a CRC-32 and U-Boot `env export -t` text. A
  writer, U-Boot or the agent, always overwrites the older copy, so a write
  cut by a power loss leaves the previous state readable (tested at every
  byte). U-Boot counts a try down **before** booting it, so a hang anywhere
  later means the next reset boots the committed slot.
* If the service is busy (`running`, `hold`, `alarm`, `homing`,
  `toolchange`, or anything unknown) or **unreachable**, a reboot is
  **deferred**: reported (`Outcome::Deferred`), left queued, retried at the
  next poll until it expires, never forced. Nothing queued after it runs
  first. If the second check fails after the try was marked, the mark is
  taken back.
* **A reboot can't loop.** The order is used up on disk before the reboot.
  The report just before it still gives the server the previous sequence
  number, so if the last check defers, the order stays queued.
* After the reboot the agent reports the trial's outcome as a `boot` result
  for the reboot's sequence number: `done` (committed) or `failed` (it did
  not come up; back on the old slot).
* **What it writes, and nothing else** (`agent/src/system.rs`): the inactive
  slot's raw partition (`/dev/disk/by-partuuid/<disk id>-02` or `-03`), its
  directory on the FAT boot partition (`/boot/a/` or `/boot/b/`), and the two
  boot-state sectors. It runs one program, `systemctl`. Never the QSPI, its
  U-Boot environment, the partition table or `boot.scr`. A test reads
  `system.rs` and fails if it spawns anything else or names the
  boot-firmware tools. The NixOS unit gives it a read-only `/`, `/boot`
  writable, and only the SD card's block devices.
* Failed orders (download failed, a file that does not match, a slot that
  does not read back) are used up, not retried. Sign a new one.

## Layout

| | |
|---|---|
| `proto/` (`emco-fleet-proto`) | keys (on middleware's trust types) and envelopes, bundles, the boot state, messages, tokens, policy |
| `server/` (`emco-fleet-server`) | axum. State is one JSON file (`<data>/state.json`, atomic writes); bundle files in `<data>/files/<sha256>` |
| `agent/` (`emco-fleet-agent`) | the board's poller. `System` trait, `RealSystem` |
| `cli/` (`emco-fleet`) | `pubkey`, `keygen`, `token`, `publish`, `command`, `cancel`, `devices` |
| `Dockerfile`, `nix/image.nix` | the server image, from Docker or from Nix (`nix build .#emco-fleet-image`) |
| `nix/package.nix` | the workspace, or selected `members` (`nix build .#emco-fleet`) |
| `deploy/compose.yaml`, `deploy/env.example` | the server behind Traefik |
| `../nix/fleet.nix` | `services.emco-fleet-agent`, the NixOS module for the board (off by default) |
| `../nix/ab.nix` | the A/B card: layout, `boot.scr`, the slot's squashfs store, the bundle and the card image |

The workspace takes middleware as git dependencies pinned to a commit
(github.com/jvishnefske/middleware, stack/posix-node); the flake pins the
same commit.

```sh
cd kd240/fleet
cargo fmt -p emco-fleet-proto -p emco-fleet-server -p emco-fleet-agent -p emco-fleet --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace   # proto 53, server 7, agent 6 + 16 end to end, cli 6 (+ SoftHSM2 when installed)
```

(`cargo fmt --all` would also format middleware, through the path
dependency: name the packages.)

## One-time setup (build host)

```sh
nix build .#emco-fleet -o result-fleet && export PATH=$PWD/result-fleet/bin:$PATH
install -d -m 700 ~/.config/emco-fleet && cd ~/.config/emco-fleet

# The signing key: in the card (as for mwctl; PKCS11_MODULE, PKCS11_TOKEN,
# PKCS11_PIN in the environment or ~/.env) ...
emco-fleet pubkey --pkcs11 emco-fleet > orders.pub       # the board's trustedKeys line
# ... or a software key:
emco-fleet keygen --name emco-fleet-1 --out orders.sec > orders.pub

emco-fleet token --device kd240 > kd240.token-pair       # line 2: the board's token; line 4: the server's line
head -c 32 /dev/urandom | base64 > admin.token && chmod 600 *
```

A card key is made with `mwctl key new --pkcs11 emco-fleet` (P-256, not
extractable). The same label can sign middleware payloads.

## Deploy the server (owner's host)

```sh
nix build .#emco-fleet-image && docker load < result   # emco-fleet-server:latest
# In kd240/fleet/deploy:
cp env.example .env            # FLEET_HOST, TRAEFIK_NETWORK, TRAEFIK_ENTRYPOINT, TRAEFIK_CERTRESOLVER
install -d -m 700 secrets
cp ~/.config/emco-fleet/admin.token secrets/admin.token
sed -n 4p kd240.token-pair > secrets/device-tokens       # `kd240 sha256:…` (the hash, not the token)
cp ~/.config/emco-fleet/orders.pub secrets/trusted-keys  # optional: refuse mis-signed orders at publish
sudo chown -R 65532:65532 secrets && chmod 600 secrets/*
docker compose up -d
curl https://$FLEET_HOST/healthz
```

All three secret files must exist before `up`. Otherwise Docker makes
directories in their place. The data volume holds `state.json` and the
bundle files (a few hundred MiB per release; delete old ones by hash when
no board needs them).

## The board

The agent runs on the A/B card layout, `nixosConfigurations.kd240-ab`
(`../nix/ab.nix`), which imports `../nix/fleet.nix`. That layout **has not
booted on a board yet**; the ext4 `nixosConfigurations.kd240` stays the
default until it has. To enable the agent there:

```nix
  services.emco-fleet-agent = {
    enable = true;
    serverUrl = "https://fleet.example.com";
    tokenFile = "/var/lib/emco-fleet-agent/token";
    trustedKeys = [ "emco-fleet:p256:MFkw…" ];   # orders.pub
  };
```

Build and write the card (`nix build
.#nixosConfigurations.kd240-ab.config.system.build.abImage`, then as
`just flash`), put the token on it
(`/var/lib/emco-fleet-agent/token`, mode 0600), and watch the first boot on
the serial console. The agent polls every minute, from two minutes after
boot.

## Publish a release (build host, every release)

```sh
export FLEET_SERVER=https://fleet.example.com
export FLEET_ADMIN_TOKEN_FILE=~/.config/emco-fleet/admin.token FLEET_PKCS11_KEY=emco-fleet

# 1. Build the bundle (x86-64 host: the kernel is cross-compiled; see ../README.md).
nix build .#kd240-bundle -o result-bundle

# 2. Upload its files and publish the signed manifest. --stage also writes
#    it into the inactive slot now (allowed while the lathe is busy).
emco-fleet publish --device kd240 --bundle result-bundle --stage --note "$(git rev-parse --short HEAD)"

# 3. Boot it once, when the lathe is idle (valid for --ttl-s, default 1 h).
#    It commits itself if healthy; otherwise the next reset falls back.
emco-fleet command --device kd240 reboot --bundle result-bundle

# 4. Watch it.
emco-fleet devices                  # slots, results; --device kd240 for history size, --json for everything
emco-fleet cancel --device kd240    # drop queued commands (a deferred reboot you no longer want)
```

`emco-fleet` checks every file against `bundle.json` before signing.
`--print` writes the signed envelope to stdout instead of sending it.
Sequence numbers default to the time in milliseconds; a clock that goes
backwards gets the order refused (409 from the server, a replay on the
board), never accepted.

## Server API

| Route | Token | |
|---|---|---|
| `POST /api/v1/devices/{id}/poll` | that device's | `PollRequest {last_seq, telemetry}` in, `PollResponse {manifest, commands}` (only those with seq > `last_seq`) out |
| `PUT /api/v1/devices/{id}/manifest` | admin | `Envelope`. 409 unless the seq is above the current manifest's and the device's |
| `POST /api/v1/devices/{id}/commands` | admin | `Envelope`. 409 for an old or duplicate seq, 429 past `FLEET_MAX_PENDING` |
| `DELETE /api/v1/devices/{id}/commands` | admin | drop the queue |
| `GET /api/v1/devices`, `GET /api/v1/devices/{id}` | admin | summaries, or one device with its history |
| `PUT /api/v1/files/{sha256}` | admin | a bundle file, streamed to disk; 400 unless its SHA-256 is the name, 201 stored, 204 already there |
| `GET /api/v1/files/{sha256}` | any device, admin | download |
| `GET /healthz` | none | `ok` |

Telemetry covers: host name, `/run/current-system`, the slots (booted,
committed, to be tried, the release in each, the newest manifest's and
whether it is staged), `uname -r`/`-v`, uptime, load, memory, up to eight
temperatures (hwmon, and IIO such as the ZynqMP AMS), the `systemctl
--failed` count, the lathe's `run` state, and the last ten results. That is
about 1 KiB. The server keeps the latest sample on every poll, and a
history sample every `FLEET_HISTORY_EVERY_S` (300 s), up to
`FLEET_HISTORY` (500) samples, about 42 hours.

Telemetry is HTTPS polling for now; moving it onto the middleware mesh is
later work.

## Not verified yet

* **Nothing of the A/B path has run on the board**: `boot.scr`'s boot-state
  handling (`usb read`/`write`, `crc32` + `cmp.l`, `env import -t`), the
  initrd's mount of the slot's squashfs as the store, the agent writing
  `/dev/sda2`/`sda3` and `/boot/{a,b}` through the hardened unit, and
  `fadvise(DONTNEED)` making the read-back come from the card.
* The hwmon/IIO temperature paths on the KD240.
* The compose file was checked with `docker compose config` only. It has
  not run behind a real Traefik.
