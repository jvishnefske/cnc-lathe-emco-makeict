# kd240/fleet: telemetry and signed software updates for the KD240

A small server and a board agent. The board **polls** the server over
outbound HTTPS (it has no inbound port for this). It reports telemetry, and
it fetches **owner-signed** orders: update manifests and `stage`, `switch` and
`reboot` commands. The system closure itself comes from a Nix binary cache
the board trusts. The server stores and relays. It never holds a signing
key.

> **No machine commands, by design.** There's no G-code, no jog, no run, and
> nothing for the RS-485 bus. The agent's only contact with the lathe
> service is reading `GET /api/state`'s `session.run`, for the interlock.
> The message types can't express anything else: `msg::Action` has three
> variants, and unknown ones fail to parse.

```
  build host                          owner's host (docker, Traefik)           KD240 (NixOS)
  ──────────                          ──────────────────────────────           ─────────────
  nix build …toplevel                 emco-fleet-server                        emco-fleet-agent (timer, root)
  nix copy --to file://…  ──rsync──►  /cache/  (static binary cache)  ◄─HTTPS─ nix-store --realise (Nix checks
  emco-fleet publish/command ─HTTPS─► /api/v1  stores signed envelopes ◄─HTTPS─  the cache signature)
    (ed25519 key stays here)          + telemetry, bounded history     poll    verifies order signature, device,
                                                                               seq, expiry, toplevel; interlock
                                                                               on GET 127.0.0.1:8080/api/state
```

## Trust model

* **Orders are signed offline.** Every manifest and command is an ed25519
  signature, made by `emco-fleet` on the build host, over
  `"emco-fleet order v1\n" ‖ payload`. The server stores the envelope as it
  came. The board verifies it against `trustedKeys`, then checks its own
  device id, a sequence number above the last one it accepted (persisted in
  `/var/lib/emco-fleet-agent/state.json`, so a replay is refused), the
  command's expiry (`not_after`, default 1 h), and the toplevel. Signed
  payloads reject unknown fields, so a restriction added by a newer signer
  can't be silently ignored by an older board.
* **The toplevel must be a NixOS system for this host.** The path must have
  the form `/nix/store/<hash>-nixos-system-<host>-…`. It is never a `.drv`,
  because realising one would build it. After the fetch, its `system` must
  be `aarch64-linux`, its `etc/hostname` must be the host, and it must have
  `bin/switch-to-configuration`, `init` and `nixos-version`.
* **Closure integrity is Nix's.** The cache is signed with a
  `nix-store --generate-binary-cache-key` key, and the board has that key in
  `trusted-public-keys` (`require-sigs` stays on). The server's `/cache/` is
  plain static files.
* **A compromised server** can withhold orders, replay them (refused), or
  serve junk (refused). It can also delay a command until it expires, or
  read the telemetry. It cannot make the board run anything the owner didn't
  sign. That takes **both** keys, the order key and the cache key. Both stay
  on the build host.
* **Tokens.** Each device has a bearer token for its own poll endpoint
  only. A separate admin token covers publishing and reading. The server
  keeps tokens as `sha256:<hex>`, or hashes raw tokens on load, and compares
  them in constant time. Secrets live only in files named by options or env.
  None are in the repository or the Nix store.

## The safety interlock (board side)

The policy is in `proto/src/policy.rs`, as pure functions with unit tests.
`agent/src/lib.rs` carries it out, and `agent/tests/end_to_end.rs` checks it
end to end.

| Step | Allowed when | |
|---|---|---|
| fetch (`nix-store --realise`) | always | changes nothing |
| `stage` (`nix-env -p /nix/var/nix/profiles/system --set` + `switch-to-configuration boot`) | always | takes effect at the next boot only |
| `switch` (`switch-to-configuration switch`) | `run` is `idle` or `disconnected` | checked when taken up, and **again immediately before activating** |
| `reboot` (`systemctl reboot`, optionally staging first) | `run` is `idle` or `disconnected` | checked, then saved and reported, then **checked again immediately before** |

* If the service is busy (`running`, `hold`, `alarm`, `homing`,
  `toolchange`, or anything unknown) or **unreachable**, switch and reboot
  are **deferred**. Deferred means reported (`Outcome::Deferred`) and left
  queued. The agent retries at the next poll until the command expires, and
  never forces it. Nothing queued after a deferred order runs first. If the
  second check fails after the profile was already set, the switch becomes a
  stage, so the boot menu agrees with the profile, and is reported as
  deferred.
* **The previous generation is kept.** Before a new generation is made
  (`policy::plan_profile`), the running system is re-recorded as a fresh
  generation if it is not among the newest four (the boot menu shows five)
  or if its generation is more than 7 days old (the weekly GC deletes after
  30). So it stays selectable in the U-Boot menu.
* **Never** the QSPI, the U-Boot environment, or the FAT partition. The
  executor (`agent/src/system.rs`) runs exactly `nix-store`, `nix-env`,
  `systemd-run` (to run `switch-to-configuration`) and `systemctl reboot`. A
  test reads that file and fails if it spawns anything else or names the
  boot-firmware tools. The NixOS unit also gives the process no devices
  (`PrivateDevices`), no `/boot/firmware`, and a read-only `/`.
* **A reboot can't loop.** The order is used up on disk before the reboot.
  The report just before it still gives the server the previous sequence
  number, so if the last check defers, the order stays queued.
* A `switch` is at most once. If activation restarts the agent, the order is
  already used up, and the next telemetry shows the new `current_system`.
* Failed orders (fetch failed, wrong system) are used up, not retried. Sign
  a new one.

## Layout

| | |
|---|---|
| `proto/` (`emco-fleet-proto`) | keys and envelopes, messages, tokens, policy |
| `server/` (`emco-fleet-server`) | axum. State is one JSON file (`<data>/state.json`, atomic writes): latest sample, bounded history, published manifest, queued commands per device |
| `agent/` (`emco-fleet-agent`) | the board's poller. `System` trait, `RealSystem` |
| `cli/` (`emco-fleet`) | `keygen`, `pubkey`, `token`, `publish`, `command`, `cancel`, `devices` |
| `Dockerfile`, `nix/image.nix` | the server image, from Docker or from Nix |
| `nix/package.nix` | the workspace, or selected `members` |
| `deploy/compose.yaml`, `deploy/env.example` | the server behind Traefik |
| `../nix/fleet.nix` | `services.emco-fleet-agent`, the NixOS module for the board (off by default) |

```sh
cd kd240/fleet
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test        # 58 tests: proto 33, server 7, agent 5 + 13 end to end
```

## One-time setup (build host)

```sh
cd kd240/fleet && cargo build --release && export PATH=$PWD/target/release:$PATH
install -d -m 700 ~/.config/emco-fleet && cd ~/.config/emco-fleet

emco-fleet keygen --name emco-fleet-1 --out orders.sec > orders.pub   # order-signing key; orders.pub goes to the board
nix-store --generate-binary-cache-key emco-fleet-cache-1 cache.sec cache.pub   # cache key; cache.pub goes to the board
emco-fleet token --device kd240 > kd240.token-pair                   # line 2: the board's token; line 4: the server's line
head -c 32 /dev/urandom | base64 > admin.token && chmod 600 *       # the admin token (also onto the server)
```

Back up `orders.sec` and `cache.sec`. Without them the board can only be
updated over SSH (`nixos-rebuild`), as now.

## Deploy the server (owner's host)

```sh
# Copy kd240/fleet to the host. Then, in kd240/fleet/deploy:
cp env.example .env            # FLEET_HOST=fleet.vishnefske.com, TRAEFIK_NETWORK, TRAEFIK_ENTRYPOINT, TRAEFIK_CERTRESOLVER
install -d -m 700 secrets cache
cp ~/.config/emco-fleet/admin.token secrets/admin.token       # from the build host
sed -n 4p kd240.token-pair > secrets/device-tokens             # `kd240 sha256:…` (the hash, not the token)
cp ~/.config/emco-fleet/orders.pub secrets/trusted-keys        # optional: refuse mis-signed orders at publish
sudo chown -R 65532:65532 secrets cache && chmod 600 secrets/*
docker compose up -d --build   # or: nix build the image (nix/image.nix), docker load, FLEET_IMAGE=…
curl https://$FLEET_HOST/healthz
```

All three secret files must exist before `up`. Otherwise Docker makes
directories in their place. The router serves the API and `/cache/` on one
host name. `/cache/` is public (store paths, signed). The system closure
must therefore hold no secrets, which is already the rule for this repo.
The data volume holds `state.json`. Back it up if you care about history.

## Enable the agent on the board (once, over SSH as today)

`nix/fleet.nix` is not imported anywhere yet. To turn it on, add this to
`kd240/nix/configuration.nix`, then `just deploy` as usual:

```nix
  imports = [ … ./service.nix ./fleet.nix ];

  services.emco-fleet-agent = {
    enable = true;
    serverUrl = "https://fleet.vishnefske.com";
    tokenFile = "/var/lib/emco-fleet-agent/token";
    trustedKeys = [ "emco-fleet-1:…" ];                 # orders.pub
    substituter = "https://fleet.vishnefske.com/cache";
    substituterPublicKey = "emco-fleet-cache-1:…";      # cache.pub
  };
```

Then put the token on the board:
`ssh admin@kd240.local 'sudo install -D -m 600 /dev/stdin /var/lib/emco-fleet-agent/token' < <(sed -n 2p kd240.token-pair)`.
The agent polls every minute, from two minutes after boot.

The first update has to go through `nixos-rebuild`, because the board must
already trust the cache key before it can fetch from the cache.

## Publish an update (build host, every release)

```sh
cd <repo>
export FLEET_SERVER=https://fleet.vishnefske.com
export FLEET_ADMIN_TOKEN_FILE=~/.config/emco-fleet/admin.token FLEET_KEY_FILE=~/.config/emco-fleet/orders.sec
CACHE=~/.cache/emco-fleet/cache            # local mirror of the server's /cache

# 1. Build the system (x86-64 host: the kernel is cross-compiled; see ../README.md).
toplevel=$(nix build --no-link --print-out-paths .#nixosConfigurations.kd240.config.system.build.toplevel)

# 2. Put its closure in the signed static cache, and upload: NARs first, then narinfos,
#    so the board never sees a narinfo whose NAR isn't there yet.
nix copy --to "file://$CACHE?compression=zstd&secret-key=$HOME/.config/emco-fleet/cache.sec" "$toplevel"
rsync -a --ignore-existing "$CACHE/nar/" owner-host:/path/to/kd240/fleet/deploy/cache/nar/
rsync -a "$CACHE/" owner-host:/path/to/kd240/fleet/deploy/cache/

# 3. Sign and publish the manifest. The board fetches and checks it; --stage also
#    makes it the next boot's default (allowed while the lathe is busy).
emco-fleet publish --device kd240 --toplevel "$toplevel" --stage --note "$(git rev-parse --short HEAD)"

# 4. Optionally, activate now or reboot into it (interlocked; valid for --ttl-s, default 1 h).
emco-fleet command --device kd240 switch --toplevel "$toplevel"
emco-fleet command --device kd240 reboot                       # or: reboot --toplevel "$toplevel"

# 5. Watch it.
emco-fleet devices                  # or --device kd240 for the history size, --json for everything
emco-fleet cancel --device kd240    # drop queued commands (a deferred switch you no longer want)
```

`emco-fleet publish` reads the toplevel locally and runs the board's own
toplevel check before signing. `--print` writes the signed envelope to
stdout instead of sending it, for carrying it over by hand. Sequence numbers
default to the time in milliseconds. A clock that goes backwards gets the
order refused (409 from the server, a replay on the board), never accepted.

## Server API

| Route | Token | |
|---|---|---|
| `POST /api/v1/devices/{id}/poll` | that device's | `PollRequest {last_seq, telemetry}` in, `PollResponse {manifest, commands}` (only those with seq > `last_seq`) out |
| `PUT /api/v1/devices/{id}/manifest` | admin | `Envelope`. 409 unless the seq is above the current manifest's and the device's |
| `POST /api/v1/devices/{id}/commands` | admin | `Envelope`. 409 for an old or duplicate seq, 429 past `FLEET_MAX_PENDING` |
| `DELETE /api/v1/devices/{id}/commands` | admin | drop the queue |
| `GET /api/v1/devices`, `GET /api/v1/devices/{id}` | admin | summaries, or one device with its history |
| `GET /cache/…` | none | the binary cache |
| `GET /healthz` | none | `ok` |

Telemetry covers: host name, `/run/current-system`, `/run/booted-system`,
the profile's system, the newest fetched manifest, `uname -r`/`-v` (from
`/proc/sys/kernel`), uptime, load, memory, up to eight temperatures (hwmon,
and IIO such as the ZynqMP AMS), the `systemctl --failed` count, the lathe's
`run` state, and the last ten results. That is about 1 KiB. The server keeps
the latest sample on every poll, and a history sample every
`FLEET_HISTORY_EVERY_S` (300 s), up to `FLEET_HISTORY` (500) samples, about
42 hours.

## Not verified yet

* **Nothing has run on the board.** In particular: `ProtectSystem=strict`
  with `ReadWritePaths=/nix/var/nix` for `nix-env` on the system profile;
  `systemd-run --wait --pipe` for `switch-to-configuration` from inside the
  sandboxed unit; the hwmon/IIO temperature paths on the KD240; and
  `nix-store --realise` from the HTTPS cache through Traefik.
* The compose file was checked with `docker compose config` only. It has
  not run behind a real Traefik.
