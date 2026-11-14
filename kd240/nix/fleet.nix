# services.emco-fleet-agent: the board's side of the fleet server
# (kd240/fleet/README.md). OFF by default, and not imported by
# configuration.nix yet.
#
# Every poll (a timer) it reports telemetry and fetches the owner-signed
# orders for this device. It acts only on orders that verify against
# `trustedKeys`, name this device, carry a sequence number above the last
# one it accepted (kept in /var/lib/emco-fleet-agent), and point at a NixOS
# system for this host, fetched from a binary cache Nix trusts.
#
# Interlock: `switch` and `reboot` run only while emco-kd240's
# GET /api/state says run = idle or disconnected, checked when the order is
# taken up and again immediately before acting; otherwise they are deferred
# and reported, never forced. `stage` (the next boot's default) is always
# allowed. It never writes the QSPI, the U-Boot environment or the FAT
# boot partition, and it sends the lathe service nothing.
#
# It runs as root because it must: it sets the system profile and asks
# systemd to activate and reboot. The process itself sees /, /boot and the
# rest read-only (except /nix/var/nix and its state), no devices, and no
# /boot/firmware. Activation runs as its own transient unit
# (systemd-run), as nixos-rebuild does.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.emco-fleet-agent;
  inherit (lib) mkEnableOption mkIf mkOption types escapeShellArg concatMapStringsSep;
  state = "/var/lib/emco-fleet-agent";
in
{
  options.services.emco-fleet-agent = {
    enable = mkEnableOption "the KD240 fleet agent (telemetry and owner-signed NixOS updates)";

    package = mkOption {
      type = types.package;
      default = pkgs.callPackage ../fleet/nix/package.nix { members = [ "emco-fleet-agent" ]; };
      defaultText = lib.literalExpression ''pkgs.callPackage ../fleet/nix/package.nix { members = [ "emco-fleet-agent" ]; }'';
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

    substituter = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "https://fleet.example.com/cache";
      description = "The binary cache the published closures come from (added to nix.settings.extra-substituters).";
    };

    substituterPublicKey = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "emco-fleet-cache-1:…";
      description = "That cache's signing key, from `nix-store --generate-binary-cache-key` (added to extra-trusted-public-keys).";
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
      {
        assertion = (cfg.substituter == null) == (cfg.substituterPublicKey == null);
        message = "services.emco-fleet-agent: set substituter and substituterPublicKey together.";
      }
    ];

    nix.settings = mkIf (cfg.substituter != null) {
      extra-substituters = [ cfg.substituter ];
      extra-trusted-public-keys = [ cfg.substituterPublicKey ];
    };

    systemd.services.emco-fleet-agent = {
      description = "KD240 fleet agent (telemetry, owner-signed NixOS updates)";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      # nix-store, nix-env; systemd-run, systemctl.
      path = [ config.nix.package config.systemd.package ];
      environment.HOME = state;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.concatStringsSep " " ([
          (lib.getExe' cfg.package "emco-fleet-agent")
          "--once"
          "--server" (escapeShellArg cfg.serverUrl)
          "--device" (escapeShellArg cfg.deviceId)
          "--token-file" (escapeShellArg cfg.tokenFile)
          "--host" (escapeShellArg config.networking.hostName)
          "--system" (escapeShellArg pkgs.stdenv.hostPlatform.system)
          "--state-dir" state
          "--local-state-url" (escapeShellArg cfg.localStateUrl)
        ] ++ map (k: "--trusted-key ${escapeShellArg k}") cfg.trustedKeys);
        StateDirectory = "emco-fleet-agent";
        StateDirectoryMode = "0700";
        # A first closure can be large.
        TimeoutStartSec = "2h";
        # Root, because profile and activation need it; the process itself
        # is fenced in. switch-to-configuration runs in its own unit.
        ProtectSystem = "strict";
        ReadWritePaths = [ "/nix/var/nix" ];
        InaccessiblePaths = [ "-/boot/firmware" ];
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
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
