# services.emco-mesh: the lathe's S88 payloads on middleware, on the board
# (plan M6; cnc/README.md). Off unless enabled (configuration.nix enables
# it with the KD240's input-only driver).
#
#   zenohd                 a Zenoh router on LOOPBACK ONLY (tcp/127.0.0.1:7447):
#                          the node's gateways, the lathe service and the fleet
#                          agent meet here. Nothing off the board can publish
#                          on it, so nothing off the board can command a unit
#                          or forge the process model's state (publications
#                          are not authenticated; cnc/README.md). Reaching it
#                          from a browser (M7) needs a listener of its own,
#                          with access control.
#   emco-mesh-ids          once per boot: the two node ids, from the machine id,
#                          into /run/emco-mesh/*.env
#   mw-posix-node          the PHYSICAL model's node (middleware's module): its
#                          control-module driver is `driver` (kd240: the board's
#                          inputs, read only, no outputs at all; sim: a
#                          simulated PC Turn 50). Its id is the one
#                          mw-posix-node would derive itself.
#   mw-posix-node-process  the PROCESS model's node: a second mw-posix-node (a
#                          node hosts one payload), no driver, its own state
#                          directory and id
#   emco-mesh-announce     mDNS: `_emco-lathe._tcp` on the UI's port, its TXT
#                          naming both node ids (for M7's autoprobe)
#
# The payloads themselves are not in the image: they are signed and
# installed per node with mwctl (cnc/README.md), so a fresh card runs both
# nodes empty. `ui` puts the lathe service on them (--mesh) instead of its
# virtual machine; `fleetInterlock` makes the fleet agent's reboot interlock
# need the process model's state too. Both are off by default: with no
# payload installed the UI would show "disconnected", and the interlock,
# which fails closed, would defer every update.
#
# RS-485 capture is unchanged (services.emco-kd240.rs485, read only).
{ config, lib, pkgs, options, ... }:

let
  cfg = config.services.emco-mesh;
  inherit (lib) mkEnableOption mkIf mkOption types;
  router = "tcp/127.0.0.1:${toString cfg.port}";
  run = "/run/emco-mesh";
  # posix-node/src/node_id.rs: the first 8 bytes of SHA-256 over DOMAIN
  # (with its NUL) and the 32 digits of the machine id. The unit's id is
  # exactly what mw-posix-node derives by default; the process node's uses
  # a domain of its own.
  ids = pkgs.writeShellScript "emco-mesh-ids" ''
    set -eu
    mid=$(tr -d '\n' < /etc/machine-id)
    id() { printf '%s\0%s' "$1" "$mid" | ${pkgs.coreutils}/bin/sha256sum | cut -c1-16; }
    unit=$(id mw-pi-node/node-id/v1)
    proc=$(id emco-kd240/process-node/v1)
    mkdir -p ${run}
    echo "MW_POSIX_NODE_NODE_ID=$unit" > ${run}/unit.env
    echo "MW_POSIX_NODE_NODE_ID=$proc" > ${run}/process.env
    printf 'EMCO_MESH_UNIT=%s\nEMCO_MESH_PROCESS=%s\n' "$unit" "$proc" > ${run}/emco.env
    printf 'FLEET_ZENOH_ROUTER=%s\nFLEET_PROCESS_NODE=%s\n' ${lib.escapeShellArg router} "$proc" > ${run}/fleet.env
    printf 'UNIT=%s\nPROCESS=%s\n' "$unit" "$proc" > ${run}/ids
    chmod 0644 ${run}/*
    echo "emco-mesh: unit $unit, process $proc"
  '';
  localConfig = {
    mode = "router";
    listen.endpoints = [ router ];
    scouting.multicast.enabled = false;
    scouting.gossip.enabled = false;
    # No plugins (no REST, no storage): a router and nothing else.
    plugins_loading.enabled = false;
  };
  zenohdConfig = pkgs.writeText "zenohd.json5" (builtins.toJSON localConfig);
  announce = pkgs.writeShellScript "emco-mesh-announce" ''
    set -eu
    . ${run}/ids
    exec ${pkgs.avahi}/bin/avahi-publish -s "EMCO lathe on %h" _emco-lathe._tcp ${toString cfg.uiPort} \
      "unit=$UNIT" "process=$PROCESS" "zenoh=loopback"
  '';
  posixService = config.systemd.services.mw-posix-node;
  needsIds = {
    after = [ "emco-mesh-ids.service" "zenohd.service" ];
    requires = [ "emco-mesh-ids.service" ];
    wants = [ "zenohd.service" ];
  };
in
{
  options.services.emco-mesh = {
    enable = mkEnableOption "the lathe's payload nodes on middleware (zenohd on loopback, two mw-posix-node)";

    posixNodePackage = mkOption {
      type = types.package;
      description = "mw-posix-node (middleware's packages.<system>.mw-posix-node).";
    };

    zenohdPackage = mkOption {
      type = types.package;
      description = "zenohd, of the zenoh 1.x release the node's gateway is built with (1.10).";
    };

    port = mkOption {
      type = types.port;
      default = 7447;
      description = "zenohd's port, on 127.0.0.1 only.";
    };

    driver = mkOption {
      type = types.enum [ "kd240" "sim" "none" ];
      default = "kd240";
      description = ''
        The physical model's control-module driver: `kd240` reads the
        board's inputs and has no outputs at all; `sim` is a simulated
        PC Turn 50 (the M6 bench test: the UI drives the simulated lathe
        through the payloads).
      '';
    };

    kd240Inputs = mkOption {
      type = types.str;
      default = "";
      example = "x_home=gpiochip2:12:low,z_home=gpiochip2:13:low";
      description = ''
        MW_POSIX_NODE_KD240_INPUTS: `point=chip:offset[:low]`, comma-separated.
        Empty: the driver has only `pl_present`. Lines also need the node to
        reach /dev/gpiochip* (supplementaryGroups and a udev rule), not set
        up here yet.
      '';
    };

    ui = mkOption {
      type = types.bool;
      default = false;
      description = "Run the lathe service on the payloads (--mesh) instead of its virtual machine.";
    };

    belt = mkOption {
      type = types.ints.between 1 3;
      default = 2;
      description = "The belt position fitted on the lathe, for the process model's preflight (--mesh-belt).";
    };

    uiPort = mkOption {
      type = types.port;
      default = 8080;
      description = "The lathe service's port, as announced over mDNS.";
    };

    generated = mkOption {
      internal = true;
      readOnly = true;
      type = types.attrs;
      description = "The zenohd configurations this module generates, for the eval check (nix/tests/uplink-eval.nix).";
    };

    fleetInterlock = mkOption {
      type = types.bool;
      default = false;
      description = ''
        The fleet agent's reboot interlock also needs the process model's
        state, read over zenohd. It fails closed: with no process payload
        installed every reboot is deferred.
      '';
    };
  };

  config = mkIf cfg.enable (lib.mkMerge [
    {
      services.emco-mesh.generated = {
        inherit localConfig;
      };

      systemd.services.zenohd = {
        description = "Zenoh router for the lathe's payload nodes (loopback only)";
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" ];
        serviceConfig = {
          ExecStart = "${lib.getExe' cfg.zenohdPackage "zenohd"} --config ${zenohdConfig}";
          DynamicUser = true;
          StateDirectory = "zenohd";
          Restart = "always";
          RestartSec = "2s";
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          PrivateDevices = true;
          RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" "AF_NETLINK" ];
          MemoryMax = "128M";
        };
      };

      systemd.services.emco-mesh-ids = {
        description = "The lathe's payload node ids";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          ExecStart = ids;
        };
      };

      services.mw-posix-node = {
        enable = true;
        package = cfg.posixNodePackage;
        zenohRouter = router;
        environment = {
          MW_POSIX_NODE_DRIVER = cfg.driver;
          # Phase commands and segments for this unit (the payload
          # subscribes to its own node's keys only).
          MW_GATEWAY_DOWNLINK = "app/unit/**";
        } // lib.optionalAttrs (cfg.kd240Inputs != "") {
          MW_POSIX_NODE_KD240_INPUTS = cfg.kd240Inputs;
        };
      };
      systemd.services.mw-posix-node = needsIds // {
        serviceConfig.EnvironmentFile = lib.mkForce [
          "${run}/unit.env"
          "-/var/lib/mw-robot/robot.env"
        ];
      };

      # A node hosts one payload, so the process model gets a node of its
      # own: middleware's unit, with its own state, port, id and downlinks.
      systemd.services.mw-posix-node-process = needsIds // {
        description = "Linux payload host: the lathe's process model";
        wantedBy = [ "multi-user.target" ];
        environment = builtins.removeAttrs posixService.environment [ "PATH" ] // {
          MW_PORT = toString (config.services.mw-posix-node.port + 1);
          MW_POSIX_NODE_STATE = "/var/lib/mw-posix-node-process";
          MW_POSIX_NODE_DRIVER = "none";
          # Its operator's commands and programs, and its unit's reports.
          MW_GATEWAY_DOWNLINK = "app/proc/**,app/unit/**";
        };
        unitConfig.StartLimitIntervalSec = 0;
        serviceConfig = builtins.removeAttrs posixService.serviceConfig [ "ExecStartPre" ] // {
          StateDirectory = "mw-posix-node-process";
          EnvironmentFile = [ "${run}/process.env" "-/var/lib/mw-robot/robot.env" ];
        };
      };

      systemd.services.emco-mesh-announce = {
        description = "mDNS: the lathe's payload nodes";
        wantedBy = [ "multi-user.target" ];
        after = [ "emco-mesh-ids.service" "avahi-daemon.service" ];
        requires = [ "emco-mesh-ids.service" "avahi-daemon.service" ];
        serviceConfig = {
          ExecStart = announce;
          DynamicUser = true;
          Restart = "always";
          RestartSec = "5s";
        };
      };
    }

    (mkIf cfg.ui {
      services.emco-kd240.mesh = {
        enable = true;
        inherit router;
        inherit (cfg) belt;
        environmentFile = "${run}/emco.env";
      };
      systemd.services.emco-kd240 = needsIds;
    })

    (mkIf cfg.fleetInterlock (lib.optionalAttrs (options.services ? emco-fleet-agent) {
      systemd.services.emco-fleet-agent = needsIds // {
        serviceConfig.EnvironmentFile = "${run}/fleet.env";
      };
    }))
  ]);
}
