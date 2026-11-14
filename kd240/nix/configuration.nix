# The KD240 system: NixOS for the Kria KD240 drives starter kit, on an SD
# card, updated afterwards over the network with
#
#   nixos-rebuild switch --flake .#kd240 --target-host admin@kd240.local --sudo
#
# (kd240/README.md, "The SD card"). Build the card image with
#
#   nix build .#nixosConfigurations.kd240.config.system.build.sdImage
#
# Boot chain (checked on the board 2026-11-13). The K24 SOM always boots its
# QSPI first: AMD's boot firmware (here U-Boot 2025.01 of Nov 2025) is
# factory-programmed there, and this image does not touch it, nor its saved
# environment. That U-Boot reads the SD card through the KD240's USB card
# reader (`usb 0`; Linux sees it as /dev/sda) and runs the first boot
# method it finds on the card's *bootable* partition: extlinux, then
# boot.scr.
#
# Its extlinux cannot boot NixOS directly. U-Boot loads the initrd at
# ramdisk_addr_r = 0x02100000, which lies inside the generic arm64 kernel
# (~62 MB, loaded at 0x200000): "RD image overlaps OS image". So the
# bootable partition holds only a boot.scr that moves ramdisk_addr_r up
# and then runs the extlinux menu that nixos-rebuild keeps on the root
# partition:
#
#   partition 1, FAT, label KD240BOOT, bootable: boot.scr only, written once
#   partition 2, ext4, label NIXOS_SD: the system, with /boot/extlinux and
#       a kernel, initrd and device tree per generation in /boot/nixos;
#       grown to fill the card at first boot
#
# `nixos-rebuild` writes each new generation and its menu entry into
# /boot, exactly where boot.scr points U-Boot, and the previous generations
# stay selectable at the U-Boot menu (serial console) as the way back from
# a bad update.
{ config, lib, pkgs, modulesPath, ... }:

let
  bootLabel = "KD240BOOT";
  # The device tree for the starter-kit K24 SOM on the KD240 carrier, as
  # Buildroot's zynqmp_kria_kd240_defconfig uses it. In mainline Linux from
  # 6.15ish; 6.12 does not have it, hence the kernel choice below.
  dtb = "xilinx/zynqmp-smk-k24-revA-sck-kd-g-revA.dtb";
  # U-Boot runs this from the bootable FAT partition. ${devtype} and
  # ${devnum} are the device distro boot is scanning (usb 0); extlinux.conf
  # is read to pxefile_addr_r, not scriptaddr, where this script itself is.
  bootScr = pkgs.runCommand "kd240-boot.scr" { nativeBuildInputs = [ pkgs.buildPackages.ubootTools ]; } ''
    cat > boot.cmd <<'EOF'
    echo KD240: initrd at 0x18000000, clear of the kernel; booting the NixOS menu on ''${devtype} ''${devnum}:2
    setenv ramdisk_addr_r 0x18000000
    sysboot ''${devtype} ''${devnum}:2 any ''${pxefile_addr_r} /boot/extlinux/extlinux.conf
    EOF
    mkimage -A arm64 -O linux -T script -C none -n kd240-boot -d boot.cmd $out
  '';
  keys = lib.filter (l: l != "" && !lib.hasPrefix "#" l)
    (lib.splitString "\n" (builtins.readFile ./authorized_keys));
in
{
  imports = [
    "${modulesPath}/installer/sd-card/sd-image.nix"
    ./service.nix
  ];

  nixpkgs.hostPlatform = "aarch64-linux";
  system.stateVersion = "25.11";
  networking.hostName = "kd240";

  # ------------------------------------------------------------------ boot

  # Mainline 6.18 LTS with one option the NixOS kernel lacks:
  # CONFIG_USB_ONBOARD_DEV_USB5744. The KD240's SD slot is a USB-SD
  # converter behind a Microchip USB5744 hub that stays in its configuration
  # stage until told over SMBus to attach. Without the option, the
  # onboard-usb-dev driver's attach is a stub that fails, and it then holds
  # the hub in reset, so the root file system never appears (seen on the
  # board; the Kconfig help says the option is a must on Kria KR260 for
  # this reason). Rust support is turned off because nothing here needs it
  # and it would mean building a cross rustc.
  #
  # The kernel is cross-compiled on an x86-64 host. A modified kernel is not
  # in the binary cache, and compiling it under qemu emulation takes hours.
  # Everything else stays the native aarch64 build from the cache. So the
  # system is built (and `nixos-rebuild --target-host` is run) on an x86-64
  # Linux host.
  boot.kernelPackages =
    let
      cross = import pkgs.path {
        localSystem = "x86_64-linux";
        crossSystem = "aarch64-linux";
      };
      kernel = cross.linux_6_18.override {
        # Without Rust, nixpkgs' Rust-only options (the NOVA GPU drivers,
        # the QR-code panic screen) have nothing to apply to; unset them so
        # its unused-option check passes.
        structuredExtraConfig = with lib.kernel; {
          USB_ONBOARD_DEV_USB5744 = yes;
          RUST = lib.mkForce unset;
          DRM_NOVA = lib.mkForce unset;
          NOVA_CORE = lib.mkForce unset;
          DRM_PANIC_SCREEN_QR_CODE = lib.mkForce unset;
        };
      };
    in
    pkgs.linuxPackagesFor kernel;
  boot.loader.grub.enable = false;
  boot.loader.generic-extlinux-compatible = {
    enable = true;
    # Each generation keeps its own kernel, initrd and device tree in /boot.
    configurationLimit = 5;
  };
  hardware.deviceTree = {
    enable = true;
    name = dtb;
    # Copy only the K24 trees into each generation, not every arm64 board's.
    filter = "zynqmp-*k24*.dtb";
  };
  # The KD240's USB-UART carries the PS UART1, as for Buildroot (ttyPS1).
  # clk_ignore_unused keeps Linux from gating clocks the boot firmware set up
  # for the PL, as Kria Ubuntu does.
  boot.kernelParams = [ "console=ttyPS1,115200" "earlycon" "clk_ignore_unused" ];
  # The root file system is on the SD card behind the USB card reader, and
  # the USB controller waits for a chain of devices (seen on the board: with
  # these missing, ff9d0000.usb deferred "wait for supplier
  # /axi/i2c@ff030000/gpio@11" and the root never appeared):
  #   gpio-zynq      the PS GPIO controller; the I2C1 controller needs it
  #                  for its bus-recovery pins
  #   i2c-cadence    I2C1, where the carrier's reset chip and hub sit
  #   gpio-pca9570   the SLG7XL45106 reset chip at 0x11, which releases
  #                  USB0_PHY_RESET_B, USB0_HUB_RESET_B, SD_RESET_B and
  #                  PS_GEM0_RESET_B (Ethernet)
  #   phy-zynqmp     the PS-GTR lane for USB 3
  #   onboard_usb_dev the USB5744 hub
  # The DWC3 controller, xhci-plat, usb-storage and sd_mod are built in.
  boot.initrd.availableKernelModules = [
    "gpio-zynq" "i2c-cadence" "gpio-pca9570" "phy-zynqmp" "onboard_usb_dev" "uas"
  ];

  image.baseName = "emco-kd240";
  sdImage = {
    firmwarePartitionName = bootLabel;
    firmwareSize = 64;
    populateFirmwareCommands = ''
      cp ${bootScr} ./firmware/boot.scr
    '';
    populateRootCommands = ''
      mkdir -p ./files/boot
      ${config.boot.loader.generic-extlinux-compatible.populateCmd} -c ${config.system.build.toplevel} -d ./files/boot
    '';
    # sd-image marks the root partition bootable. U-Boot's distro boot scans
    # only bootable partitions, and it must find boot.scr on the FAT one,
    # not the extlinux.conf on the root it cannot boot unaided.
    postBuildCommands = ''
      ${pkgs.util-linux}/bin/sfdisk --activate $img 1
    '';
  };
  # The extlinux entry names this file; a typo or a kernel without the tree
  # would boot with no device tree at all, so fail the build instead.
  system.systemBuilderCommands = ''
    test -e ${config.hardware.deviceTree.package}/${dtb} || {
      echo "error: ${dtb} is not in ${config.hardware.deviceTree.package}" >&2; exit 1; }
  '';

  # ------------------------------------------------------------- network

  # DHCP on whichever Ethernet port is plugged in.
  networking.useDHCP = true;
  # kd240.local on the shop LAN.
  services.avahi = {
    enable = true;
    nssmdns4 = true;
    publish = {
      enable = true;
      addresses = true;
    };
  };
  # avahi leaves /run/avahi-daemon/pid behind when it is stopped, so the
  # restart every `nixos-rebuild switch` does failed with "Failed to create
  # PID file: File exists" (seen on the board). A stale file is safe to drop:
  # systemd has already stopped the old daemon.
  systemd.services.avahi-daemon.serviceConfig.ExecStartPre = [ "-${pkgs.coreutils}/bin/rm -f /run/avahi-daemon/pid" ];
  services.timesyncd.enable = true;

  # Updates arrive over SSH: key only, no root login.
  services.openssh = {
    enable = true;
    settings = {
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
      PermitRootLogin = "no";
    };
  };
  users.users.admin = {
    isNormalUser = true;
    description = "Deploys updates (nixos-rebuild --target-host)";
    extraGroups = [ "wheel" "dialout" ];
    openssh.authorizedKeys.keys = keys;
  };
  # `nixos-rebuild --target-host … --sudo` activates without a terminal, so
  # sudo cannot ask. The only way in is an SSH key, so this is the key's
  # privilege, not a password's.
  security.sudo.wheelNeedsPassword = false;
  nix.settings = {
    experimental-features = [ "nix-command" "flakes" ];
    # Lets nixos-rebuild copy a closure built elsewhere onto the board.
    trusted-users = [ "@wheel" ];
  };
  # Old generations go after a while; the boot menu keeps its last five.
  nix.gc = {
    automatic = true;
    dates = "weekly";
    options = "--delete-older-than 30d";
  };

  assertions = [
    {
      assertion = keys != [ ];
      message = "kd240/nix/authorized_keys has no SSH key: the board could only be updated by reflashing it.";
    }
  ];

  # ------------------------------------------------------------- the service

  services.emco-kd240 = {
    enable = true;
    package = pkgs.callPackage ./package.nix { };
    # PS UART0. The KD240 device tree runs it in RS-485 mode
    # (linux,rs485-enabled-at-boot-time) with the transceiver's DE on
    # rts-gpios, so Linux asserts DE only while transmitting, and the
    # service opens it read-only. The capture runs only when started from
    # the UI.
    rs485 = "/dev/ttyPS0";
  };

  environment.systemPackages = with pkgs; [ picocom usbutils pciutils i2c-tools python3 ];
}
