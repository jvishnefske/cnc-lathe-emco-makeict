# Programmable logic from NixOS (doc 65 §3 S2): load a bitstream into the
# K24's PL at boot through the kernel's FPGA manager, with a device-tree
# overlay for whatever the design puts on the bus, so PL designs ship with
# `nixos-rebuild` like everything else. Off by default.
#
# How. The KD240 device tree has an FPGA region (`fpga_full`, /fpga-region)
# on the ZynqMP FPGA manager (zynqmp-fpga, which programs the PL through the
# PMU firmware), and the kernel is built with OF_OVERLAY and keeps the tree's
# __symbols__. Applying an overlay to that region that names a bitstream in
# `firmware-name` makes the region driver program the PL from the firmware
# path; removing the overlay unloads it. Mainline has no userspace switch to
# apply an overlay, so a twenty-line module (pl-overlay/) does just that,
# from a systemd unit:
#
#   emco-pl.service   modprobe emco_pl_overlay overlay=emco-pl.dtbo
#                     (stop: modprobe -r, which removes the overlay)
#
# The overlay and the bitstream go into hardware.firmware, so they are in
# the firmware path of the generation that booted, and a rollback rolls the
# PL back with it.
#
# Bitstreams come from Vivado (none on the build host yet), converted for the
# FPGA manager with
#   bootgen -arch zynqmp -process_bitstream bin -image design.bif
# which writes design.bit.bin.
#
#   services.emco-pl = {
#     enable = true;
#     bitstream = ./pl/blink.bit.bin;
#     overlay = ''
#       &amba { gpio_pl: gpio@a0000000 { compatible = "xlnx,xps-gpio-1.00.a"; … }; };
#     '';
#   };
#
# With `bitstream = null` (the default) the unit still applies an overlay,
# with no firmware-name: it checks the whole path (module, firmware lookup,
# overlay against the live tree, the region's notifier) without touching the
# PL, and leaves /proc/device-tree/fpga-region/emco-pl-check behind as proof.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.emco-pl;
  # The board's kernel is cross-compiled (configuration.nix); its modules
  # must be too.
  cross = import pkgs.path {
    localSystem = "x86_64-linux";
    crossSystem = "aarch64-linux";
  };
  module = (cross.linuxPackagesFor config.boot.kernelPackages.kernel).callPackage ./pl-overlay { };
  fwName = "${cfg.name}.bit.bin";
  dts = pkgs.writeText "${cfg.name}.dts" ''
    /dts-v1/;
    /plugin/;

    &fpga_full {
    ${if cfg.bitstream != null then ''
      firmware-name = "${fwName}";
    '' else ''
      emco-pl-check {
        compatible = "emco,kd240-pl-check";
        status = "disabled";
      };
    ''}
    };

    ${cfg.overlay}
  '';
  firmware = pkgs.runCommand "emco-pl-firmware" { nativeBuildInputs = [ pkgs.buildPackages.dtc ]; } ''
    mkdir -p $out/lib/firmware
    dtc -@ -I dts -O dtb -o $out/lib/firmware/${cfg.name}.dtbo ${dts}
    ${lib.optionalString (cfg.bitstream != null) "cp ${cfg.bitstream} $out/lib/firmware/${fwName}"}
  '';
in
{
  options.services.emco-pl = {
    enable = lib.mkEnableOption "loading the PL design at boot through the FPGA manager";
    name = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9_-]+";
      default = "emco-pl";
      description = "Base name of the overlay and bitstream files in the firmware path.";
    };
    bitstream = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        The PL design as a .bit.bin (bootgen -process_bitstream bin). null
        applies a check overlay that leaves the PL alone.
      '';
    };
    overlay = lib.mkOption {
      type = lib.types.lines;
      default = "";
      description = ''
        Further overlay fragments (dts source, after /plugin/) for the
        design's PL peripherals, e.g. under &amba.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    boot.extraModulePackages = [ module ];
    hardware.firmware = [ firmware ];
    systemd.services.emco-pl = {
      description = "Load the PL design (FPGA manager + DT overlay)";
      wantedBy = [ "multi-user.target" ];
      before = [ "emco-kd240.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${pkgs.kmod}/bin/modprobe emco_pl_overlay overlay=${cfg.name}.dtbo";
        ExecStop = "${pkgs.kmod}/bin/modprobe -r emco_pl_overlay";
      };
    };
  };
}
