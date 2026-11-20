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
#   claim link             (claimLink.enable, W4b) each node's local serial
#                          link (MW_POSIX_NODE_LOCAL_SERIAL, where a node is
#                          claimed) is a PTY made inside its own unit by
#                          scripts/claim-link.py, at
#                          /run/emco-mesh-claim-{unit,process}/tty, and
#                          always drained; root reaches it on the socket
#                          beside it (scripts/claim.sh: mwctl on the laptop,
#                          ssh, socat). Needs a posix-node that opens a PTY
#                          (middleware bc80998): flake.nix kd240Mesh.
#   emco-mesh-uplink       (uplink.enable, W4b) a SECOND zenohd, with no
#                          listener, linking the local router to the cloud
#                          mesh over mTLS (tls/mesh.updates.<site>:443), its
#                          default-deny ACL exporting the payloads' sys keys
#                          and the process state and nothing of app/unit/**.
#                          The local zenohd above is the same with it on or
#                          off, so a bad certificate never touches the board's
#                          own mesh. Its certificate: emco-mesh-enroll
#                          (BRINGUP.md step 8).
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

  # The uplink (emco-mesh-uplink, uplink.enable): a router of its own with no
  # listener (a router's default is tcp/[::]:7447), linked to the local
  # router over loopback tcp and to the cloud mesh over mTLS. A failed
  # connect never ends it: zenohd retries in the background, and again after
  # a link drops, reading the TLS files at each attempt.
  uplinkConfig = {
    mode = "router";
    listen.endpoints = [ ];
    connect = {
      endpoints = [ router up.endpoint ];
      exit_on_failure = false;
      timeout_ms = -1;
      retry = { period_init_ms = 1000; period_max_ms = 60000; period_increase_factor = 2; };
    };
    scouting.multicast.enabled = false;
    scouting.gossip.enabled = false;
    plugins_loading.enabled = false;
    # Name verification stays on: the router's SAN must be the endpoint's host.
    transport.link.tls = {
      root_ca_certificate = "${up.tlsDir}/ca";
      connect_certificate = "${up.tlsDir}/cert";
      connect_private_key = "${up.tlsDir}/key";
      enable_mtls = true;
    };
    access_control = uplinkAcl;
  };
  # Default deny. A link matches subjects by its protocol: the loopback
  # link to the local router (tcp) may carry anything, the mesh link (tls)
  # only what is listed. `**` does not match a verbatim `@` chunk, so the
  # local rule names @/** (admin space) and @mw/** (sys keys) as well.
  # Ingress on the tls link is what the mesh sends the board; egress is what
  # the board shows the mesh.
  #   in   queries for the payloads' read-only and signed sys keys (no
  #        trust/claim, board, peers, fw/*, log, status), and the mesh's
  #        subscriptions to the process state (so the state's puts flow)
  #   out  the payloads' sys queryables and their replies, and the process
  #        state; nothing of the local clients' other declarations
  #   never  app/unit/** either way: a unit's commands come from the board
  uplinkAcl =
    let
      all = [ "put" "delete" "declare_subscriber" "query" "reply" "declare_queryable" "liveliness_token" "declare_liveliness_subscriber" "liveliness_query" ];
      sys = k: "@mw/node/*/sys/${k}";
    in
    {
      enabled = true;
      default_permission = "deny";
      subjects = [
        { id = "local"; link_protocols = [ "tcp" ]; }
        { id = "uplink"; link_protocols = [ "tls" ]; }
      ];
      rules = [
        { id = "local-all"; permission = "allow"; flows = [ "ingress" "egress" ]; messages = all; key_exprs = [ "**" "@/**" "@mw/**" ]; }
        { id = "no-unit"; permission = "deny"; flows = [ "ingress" "egress" ]; messages = [ "put" "delete" "declare_subscriber" "query" "reply" "declare_queryable" ]; key_exprs = [ "app/unit/**" ]; }
        {
          id = "cloud-queries-in";
          permission = "allow";
          flows = [ "ingress" ];
          messages = [ "query" ];
          key_exprs = map sys [
            "info"
            "trust/info"
            "manifest"
            "manifest/cm"
            "payload/binding"
            "payload/start"
            "payload/stop"
            "payload/bind"
            "loader/begin"
            "loader/chunk"
            "loader/commit"
          ];
        }
        { id = "cloud-subscribes-in"; permission = "allow"; flows = [ "ingress" ]; messages = [ "declare_subscriber" ]; key_exprs = [ "app/proc/*/state" ]; }
        { id = "board-queryables-out"; permission = "allow"; flows = [ "egress" ]; messages = [ "declare_queryable" ]; key_exprs = [ (sys "**") ]; }
        { id = "board-replies-out"; permission = "allow"; flows = [ "egress" ]; messages = [ "reply" ]; key_exprs = [ "@mw/node/**" ]; }
        { id = "board-state-out"; permission = "allow"; flows = [ "egress" ]; messages = [ "put" ]; key_exprs = [ "app/proc/*/state" ]; }
      ];
      policies = [
        { id = "local"; rules = [ "local-all" ]; subjects = [ "local" ]; }
        { id = "uplink-deny"; rules = [ "no-unit" ]; subjects = [ "uplink" ]; }
        { id = "uplink-allow"; rules = [ "cloud-queries-in" "cloud-subscribes-in" "board-queryables-out" "board-replies-out" "board-state-out" ]; subjects = [ "uplink" ]; }
      ];
    };
  uplinkConfigFile = pkgs.writeText "zenohd-uplink.json5" (builtins.toJSON uplinkConfig);
  enroll = pkgs.callPackage ./enroll.nix { inherit (up) tlsDir; };
  expiry = pkgs.writeShellScript "emco-mesh-uplink-expiry" ''
    if ! ${enroll}/bin/emco-mesh-enroll check --dir ${up.tlsDir}; then
      echo "emco-mesh-uplink-expiry: the uplink's certificate fails its check: stopping emco-mesh-uplink (emco-mesh-enroll status)" >&2
      systemctl stop emco-mesh-uplink.service
      exit 1
    fi
  '';
  announce = pkgs.writeShellScript "emco-mesh-announce" ''
    set -eu
    . ${run}/ids
    exec ${pkgs.avahi}/bin/avahi-publish -s "EMCO lathe on %h" _emco-lathe._tcp ${toString cfg.uiPort} \
      "unit=$UNIT" "process=$PROCESS" "zenoh=loopback"
  '';
  posixService = config.systemd.services.mw-posix-node;
  up = cfg.uplink;
  claim = cfg.claimLink.enable;
  claimDir = which: "emco-mesh-claim-${which}";
  # The node's command as middleware's module has it.
  nodeExec = lib.escapeShellArgs ([ (lib.getExe cfg.posixNodePackage) ] ++ config.services.mw-posix-node.args);
  # ExecStart of both node units with the claim link: the daemon in the
  # background (same unit, same cgroup: it stops with the node), then the
  # node, exec'd. A daemon that fails to come up never keeps the node down:
  # the node then starts without a local link, and says so in the journal.
  claimWrapper = pkgs.writeShellScript "emco-mesh-claim-link" ''
    dir=''${RUNTIME_DIRECTORY%%:*}
    ${pkgs.python3}/bin/python3 -I ${../scripts/claim-link.py} &
    i=0
    while [ "$i" -lt 50 ] && ! { [ -e "$dir/tty" ] && [ -S "$dir/sock" ]; }; do
      ${pkgs.coreutils}/bin/sleep 0.1
      i=$((i + 1))
    done
    if [ -e "$dir/tty" ] && [ -S "$dir/sock" ]; then
      echo "emco-mesh-claim-link: local link $dir/tty, claim with kd240/scripts/claim.sh" >&2
    else
      echo "emco-mesh-claim-link: NO CLAIM LINK (the daemon did not come up in 5 s): starting the node without MW_POSIX_NODE_LOCAL_SERIAL" >&2
      unset MW_POSIX_NODE_LOCAL_SERIAL
    fi
    exec "$@"
  '';
  claimUnit = which: lib.optionalAttrs claim {
    RuntimeDirectory = claimDir which;
    RuntimeDirectoryMode = "0700";
  };
  claimEnv = which: lib.optionalAttrs claim {
    MW_POSIX_NODE_LOCAL_SERIAL = "/run/${claimDir which}/tty";
  };
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

    uplink = {
      enable = mkEnableOption "the router-to-router mTLS uplink to the cloud mesh (a second zenohd, emco-mesh-uplink)";

      endpoint = mkOption {
        type = types.str;
        default = "tls/mesh.updates.vishnefske.com:443";
        description = ''
          The cloud mesh router, `tls/<host>:<port>`. The host must be the
          name in the router certificate's SAN (zenoh verifies it).
        '';
      };

      tlsDir = mkOption {
        type = types.str;
        default = "/var/lib/emco-mesh/tls";
        description = ''
          The uplink's certificate: `ca` (the Zenoh CA), `cert` (this
          board's, CN kd240-<unit node id>) and `key` (made on the board by
          `emco-mesh-enroll csr`; it never leaves it).
        '';
      };
    };

    claimLink.enable = mkOption {
      type = types.bool;
      default = true;
      description = ''
        A local serial link for each node, so it can be claimed: a PTY in
        the node's unit, always drained, with a root-only socket beside it
        (/run/emco-mesh-claim-{unit,process}/sock) for kd240/scripts/claim.sh.
        Needs a mw-posix-node that accepts a PTY as its local serial port
        (middleware bc80998): an older one fails to open it and exits.
      '';
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
      } // lib.optionalAttrs up.enable {
        inherit uplinkConfig;
      };

      assertions = [
        {
          assertion = !up.enable || lib.match "tls/[^/ ]+:[0-9]+" up.endpoint != null;
          message = "services.emco-mesh.uplink.endpoint must be tls/<host>:<port> (mTLS only), not ${up.endpoint}";
        }
        {
          assertion = !up.enable || lib.hasPrefix "/" up.tlsDir;
          message = "services.emco-mesh.uplink.tlsDir must be an absolute path, not ${up.tlsDir}";
        }
        {
          assertion = !up.enable || lib.versionAtLeast cfg.zenohdPackage.version "1.10";
          message = "services.emco-mesh.uplink needs zenohd 1.10 or later (its ACL and TLS settings); zenohdPackage is ${cfg.zenohdPackage.version}";
        }
      ];

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
        environment = claimEnv "unit" // {
          MW_POSIX_NODE_DRIVER = cfg.driver;
          # Phase commands and segments for this unit (the payload
          # subscribes to its own node's keys only).
          MW_GATEWAY_DOWNLINK = "app/unit/**";
        } // lib.optionalAttrs (cfg.kd240Inputs != "") {
          MW_POSIX_NODE_KD240_INPUTS = cfg.kd240Inputs;
        };
      };
      systemd.services.mw-posix-node = needsIds // {
        serviceConfig = {
          EnvironmentFile = lib.mkForce [
            "${run}/unit.env"
            "-/var/lib/mw-robot/robot.env"
          ];
        } // lib.optionalAttrs claim ({
          ExecStart = lib.mkForce "${claimWrapper} ${nodeExec}";
        } // claimUnit "unit");
      };

      # A node hosts one payload, so the process model gets a node of its
      # own: middleware's unit, with its own state, port, id and downlinks.
      systemd.services.mw-posix-node-process = needsIds // {
        description = "Linux payload host: the lathe's process model";
        wantedBy = [ "multi-user.target" ];
        environment = builtins.removeAttrs posixService.environment [ "PATH" "MW_POSIX_NODE_LOCAL_SERIAL" ] // claimEnv "process" // {
          MW_PORT = toString (config.services.mw-posix-node.port + 1);
          MW_POSIX_NODE_STATE = "/var/lib/mw-posix-node-process";
          MW_POSIX_NODE_DRIVER = "none";
          # Its operator's commands and programs, and its unit's reports.
          MW_GATEWAY_DOWNLINK = "app/proc/**,app/unit/**";
        };
        unitConfig.StartLimitIntervalSec = 0;
        # ExecStart (the claim link's wrapper, or the node) is the unit
        # node's; the wrapper finds its own RuntimeDirectory.
        serviceConfig = builtins.removeAttrs posixService.serviceConfig [ "ExecStartPre" ] // claimUnit "process" // {
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

    (mkIf up.enable {
      users.users.emco-mesh-uplink = { isSystemUser = true; group = "emco-mesh-uplink"; };
      users.groups.emco-mesh-uplink = { };
      # The key is root's, readable by the uplink's group (emco-mesh-enroll).
      systemd.tmpfiles.rules = [
        "d /var/lib/emco-mesh 0755 root root -"
        "d ${up.tlsDir} 0750 root emco-mesh-uplink -"
      ];
      environment.systemPackages = [ enroll ];

      # The KD240 has no RTC: until timesyncd has synced, the clock is the
      # last shutdown's (or the build's) and a certificate may look "not
      # yet valid". time-sync.target waits for the sync, but not for ever:
      # with no NTP for 5 minutes this unit fails (visibly) and the uplink's
      # check says to look at the clock, retried every 2 minutes.
      systemd.additionalUpstreamSystemUnits = [ "systemd-time-wait-sync.service" ];
      systemd.services.systemd-time-wait-sync = {
        wantedBy = [ "time-sync.target" ];
        serviceConfig.TimeoutStartSec = "5min";
      };

      systemd.services.emco-mesh-uplink = {
        description = "Zenoh uplink: the lathe's router to the cloud mesh (mTLS)";
        wantedBy = [ "multi-user.target" ];
        wants = [ "network-online.target" "time-sync.target" "zenohd.service" ];
        after = [ "network-online.target" "time-sync.target" "zenohd.service" ];
        unitConfig.StartLimitIntervalSec = 0;
        serviceConfig = {
          # As root (the key is 0640): no certificate, or a bad one, and
          # the uplink does not start; the local zenohd is not touched.
          ExecStartPre = "+${enroll}/bin/emco-mesh-enroll check --dir ${up.tlsDir}";
          ExecStart = "${lib.getExe' cfg.zenohdPackage "zenohd"} --config ${uplinkConfigFile}";
          User = "emco-mesh-uplink";
          Group = "emco-mesh-uplink";
          Restart = "on-failure";
          RestartSec = "2min";
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ProtectHome = true;
          PrivateTmp = true;
          PrivateDevices = true;
          RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" "AF_NETLINK" ];
          MemoryMax = "128M";
        };
      };

      # A TLS session outlives its certificate, and once it drops the
      # reconnects fail for ever with the unit still active. So once a day
      # the same check runs, and a failing certificate stops the uplink
      # (and fails this unit) instead: a visible stop, not a silent retry.
      # `emco-mesh-enroll install` starts it again.
      systemd.services.emco-mesh-uplink-expiry = {
        description = "Stop the uplink when its certificate fails emco-mesh-enroll check";
        serviceConfig = {
          Type = "oneshot";
          ExecStart = expiry;
        };
      };
      systemd.timers.emco-mesh-uplink-expiry = {
        wantedBy = [ "timers.target" ];
        timerConfig.OnCalendar = "daily";
      };
    })

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
