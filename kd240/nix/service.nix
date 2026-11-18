# services.emco-kd240: the service as a NixOS module (kd240/README.md).
#
# The bearer token is made on the board at first start, not baked into the
# image: an image (or a repository) holding the token would hand it to
# everyone who has a copy. Read it with
#   ssh admin@kd240.local sudo cat /var/lib/emco-kd240/token
{ config, lib, pkgs, ... }:

let
  cfg = config.services.emco-kd240;
  inherit (lib) mkEnableOption mkIf mkOption types optionals optionalString;
  port = lib.toInt (lib.last (lib.splitString ":" cfg.listen));
  state = "/var/lib/emco-kd240";
in
{
  options.services.emco-kd240 = {
    enable = mkEnableOption "the EMCO PC Turn 50 sender service";

    package = mkOption {
      type = types.package;
      description = "The emco-kd240 package.";
    };

    listen = mkOption {
      type = types.str;
      default = "0.0.0.0:8080";
      description = "Address and port the web UI and API are served on.";
    };

    rs485 = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/dev/ttyPS0";
      description = ''
        The RS-485 tty, opened READ-ONLY for the listen-only capture. Null
        turns the capture off. The service gets read access to this one
        device and no other.
      '';
    };

    openFirewall = mkOption {
      type = types.bool;
      default = true;
      description = "Open the service's port in the firewall.";
    };
  };

  config = mkIf cfg.enable {
    users.users.emco = {
      isSystemUser = true;
      group = "emco";
      extraGroups = [ "dialout" ];
    };
    users.groups.emco = { };

    systemd.services.emco-kd240 = {
      description = "EMCO PC Turn 50 sender service (virtual machine + listen-only RS-485 capture)";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      path = [ pkgs.coreutils ];
      preStart = ''
        if [ ! -s ${state}/token ]; then
          umask 077
          head -c 24 /dev/urandom | base64 | tr -d '/+=\n' > ${state}/token
          echo "emco-kd240: made a new bearer token in ${state}/token"
        fi
      '';
      serviceConfig = {
        User = "emco";
        Group = "emco";
        SupplementaryGroups = [ "dialout" ];
        StateDirectory = "emco-kd240";
        StateDirectoryMode = "0750";
        ExecStart =
          "${lib.getExe cfg.package} --listen ${cfg.listen} --data ${state} --token-file ${state}/token"
          + optionalString (cfg.rs485 != null) " --rs485 ${cfg.rs485}";
        Restart = "on-failure";
        RestartSec = 2;
        # It reads one tty and writes only its state directory.
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        DevicePolicy = "closed";
        DeviceAllow = optionals (cfg.rs485 != null) [ "${cfg.rs485} r" ];
      };
    };

    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [ port ];
  };
}
