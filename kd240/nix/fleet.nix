# services.emco-fleet-agent: the board's side of the fleet server
# (kd240/fleet/README.md). OFF by default; for the A/B card layout
# (ab.nix), which is not imported by configuration.nix.
#
# Every poll (a timer) it reports telemetry and fetches the owner-signed
# orders for this device. It acts only on orders that verify against
# `trustedKeys` (middleware's verifier: ed25519, or P-256 from the owner's
# card), name this device, carry a sequence number above the last one it
# accepted (kept in /var/lib/emco-fleet-agent), and carry a well-formed
# release bundle for this device class.
#
# What it writes: the inactive slot (its raw partition and its directory on
# the FAT boot partition, each file checked by size and SHA-256 and read
# back from the card), and the two A/B boot-state sectors. Never the QSPI,
# its U-Boot environment, the partition table, boot.scr, or the data
# partition beyond its own state directory.
#
# Interlock: `reboot` runs only while emco-kd240's GET /api/state says
# run = idle or disconnected, checked when the order is taken up and again
# immediately before acting; otherwise it is deferred and reported, never
# forced. Staging is always allowed: nothing running uses the inactive
# slot. A trial boot of a new slot is committed once the lathe service
# answers; until then U-Boot falls back at the next reset. It sends the
# lathe service nothing.
#
# It runs as root because it must write block devices; it sees the rest of
# the system read-only, and only the SD card's block devices.
{ config, lib, pkgs, middlewareSrc, ... }:

let
  cfg = config.services.emco-fleet-agent;
  inherit (lib) mkEnableOption mkIf mkOption types escapeShellArg concatMapStringsSep;
  state = "/var/lib/emco-fleet-agent";
in
{
  options.services.emco-fleet-agent = {
    enable = mkEnableOption "the KD240 fleet agent (telemetry and owner-signed A/B updates)";

    package = mkOption {
      type = types.package;
      default = pkgs.callPackage ../fleet/nix/package.nix { inherit middlewareSrc; members = [ "emco-fleet-agent" ]; };
      defaultText = lib.literalExpression ''pkgs.callPackage ../fleet/nix/package.nix { inherit middlewareSrc; members = [ "emco-fleet-agent" ]; }'';
      description = "The emco-fleet-agent package.";
    };

    serverUrl = mkOption {
      type = types.str;
      example = "https://fleet.example.com";
      description = "The fleet server, https:// (the agent refuses plain HTTP off loopback).";
    };

    deviceId = mkOption {
      type = types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = "This board's id, as in the server's device tokens file and the signed orders.";
    };

    tokenFile = mkOption {
      type = types.str;
      example = "/var/lib/emco-fleet-agent/token";
      description = ''
        File holding this device's bearer token (`emco-fleet token`). Put it
        on the board by hand (root only, mode 0600). A string, not a path,
        so it is never copied into the Nix store.
      '';
    };

    trustedKeys = mkOption {
      type = types.listOf types.str;
      example = [ "emco-fleet-1:jNhg+lbZCc6qcNDwI77A2C3xVUEzcrHwfLdK/fryN8Q=" ];
      description = "Public keys whose signed manifests and commands the board obeys (`emco-fleet keygen`).";
    };

    diskId = mkOption {
      type = types.strMatching "[0-9a-f]{8}";
      default = config.system.build.abDiskId or "4b443234";
      defaultText = lib.literalExpression "config.system.build.abDiskId";
      description = "The SD card's MBR disk id: the slots are /dev/disk/by-partuuid/<id>-02 and -03.";
    };

    bootDir = mkOption {
      type = types.str;
      default = "/boot";
      description = "Where the FAT boot partition (KD240BOOT, the slots' boot files) is mounted.";
    };

    localStateUrl = mkOption {
      type = types.str;
      default = "http://127.0.0.1:8080/api/state";
      description = "emco-kd240's state endpoint, read for the interlock. Nothing is ever sent to the service.";
    };

    interval = mkOption {
      type = types.str;
      default = "1min";
      description = "Time between polls (systemd time span).";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.trustedKeys != [ ];
        message = "services.emco-fleet-agent.trustedKeys is empty: every order would be refused.";
      }
      {
        assertion = lib.hasPrefix "/" cfg.tokenFile && !lib.hasPrefix builtins.storeDir cfg.tokenFile;
        message = "services.emco-fleet-agent.tokenFile must be an absolute path outside the Nix store.";
      }
    ];

    systemd.services.emco-fleet-agent = {
      description = "KD240 fleet agent (telemetry, owner-signed A/B updates)";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      # systemctl (reboot, and the failed-unit count).
      path = [ config.systemd.package ];
      environment.HOME = state;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.concatStringsSep " " ([
          (lib.getExe' cfg.package "emco-fleet-agent")
          "--once"
          "--server" (escapeShellArg cfg.serverUrl)
          "--device" (escapeShellArg cfg.deviceId)
          "--token-file" (escapeShellArg cfg.tokenFile)
          "--disk-id" cfg.diskId
          "--boot-dir" (escapeShellArg cfg.bootDir)
          "--state-dir" state
          "--local-state-url" (escapeShellArg cfg.localStateUrl)
        ] ++ map (k: "--trusted-key ${escapeShellArg k}") cfg.trustedKeys);
        StateDirectory = "emco-fleet-agent";
        StateDirectoryMode = "0700";
        # A root file system is a few hundred MiB over the network.
        TimeoutStartSec = "2h";
        # Root, because it writes the card's block devices; the rest is
        # fenced in. Only SCSI disks (the USB card reader) are reachable.
        ProtectSystem = "strict";
        ReadWritePaths = [ cfg.bootDir ];
        ProtectHome = true;
        PrivateTmp = true;
        DevicePolicy = "closed";
        DeviceAllow = [ "block-sd rw" ];
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        NoNewPrivileges = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
      };
    };

    systemd.timers.emco-fleet-agent = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnUnitInactiveSec = cfg.interval;
        RandomizedDelaySec = "10s";
      };
    };
  };
}
