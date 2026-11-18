# The KD240 on an A/B card: two raw slots, each holding one release's
# store as a squashfs, U-Boot trying a new slot once and falling back to the
# committed one, and the fleet agent (kd240/fleet) writing the inactive slot.
# Imported on top of configuration.nix by `nixosConfigurations.kd240-ab`.
#
# NOT YET BOOTED ON A BOARD. The pieces the board side depends on are tested
# on the host (the boot-state format in kd240/fleet/proto/src/bootstate.rs,
# the agent against a fake card in kd240/fleet/agent/tests/end_to_end.rs);
# the U-Boot script below and the initrd's store mount are not. The first
# card made from this must be tried with the serial console attached, and
# the ext4 layout of `nixosConfigurations.kd240` stays the board's default
# until it has.
#
# The card (MBR, disk id `diskId`):
#
#   gap        0..8 MiB  the two boot-state copies, LBA 4096 and 6144
#                        (bootstate.rs COPY_LBA)
#   p1  FAT    KD240BOOT  boot.scr, a/{Image,initrd,dtb,cmdline}, b/{…}
#   p2  raw    slot a     rootfs.squashfs: the release's /nix/store
#   p3  raw    slot b
#   p4  ext4   KD240DATA  /: state, logs, /var/lib; grown to the card
#
# boot.scr reads the newest valid boot-state copy. With a slot to try and
# tries left it counts one down (writing the other copy), then boots that
# slot; otherwise it boots the committed one. The kernel gets the slot's
# `cmdline` plus `emco.slot=a|b`, and the initrd mounts that slot's squashfs
# as the read-only store (with a tmpfs overlay over it). A trial slot that
# never commits (the agent commits it once the lathe service answers) is
# left after its one boot: the next reset boots the committed slot again.
#
# Nothing here, as in the rest of kd240/, drives the lathe.
{ config, lib, pkgs, modulesPath, ... }:

let
  # The card's MBR disk id: the slots are /dev/disk/by-partuuid/<id>-02/-03.
  # Every card made from this image has it; one board holds one card.
  diskId = "4b443234"; # "KD24"
  slotMiB = 3072;
  dataMiB = 1024; # at first; grown to the card at first boot
  bootMiB = 256;
  dtb = config.hardware.deviceTree.name;
  toplevel = config.system.build.toplevel;

  bootScr = pkgs.runCommand "kd240-ab-boot.scr" { nativeBuildInputs = [ pkgs.buildPackages.ubootTools ]; } ''
    cat > boot.cmd <<'EOF'
    # The A/B boot state: kd240/fleet/proto/src/bootstate.rs. Two 512-byte
    # copies, [CRC-32 big-endian][env export -t text, NUL padded], at LBA
    # 0x1000 and 0x1800 of this disk. U-Boot's crc32 stores big-endian, so
    # a copy is valid when its first word equals the CRC of the rest.
    setenv ramdisk_addr_r 0x18000000
    setenv ab_buf 0x20000000
    setenv ab_text 0x20000004
    setenv ab_crc 0x20001000
    setenv ab_ok0 0
    setenv ab_ok1 0
    for c in 0 1; do
      if test $c = 0; then setenv ab_lba 0x1000; else setenv ab_lba 0x1800; fi
      setenv ab_magic
      if ''${devtype} read ''${ab_buf} ''${ab_lba} 1 && crc32 ''${ab_text} 0x1fc ''${ab_crc} && cmp.l ''${ab_buf} ''${ab_crc} 1; then
        env import -t ''${ab_text} 0x1fc
        if test "''${ab_magic}" = "emco-ab-1"; then
          setenv ab_ok$c 1
          setenv ab_seq$c ''${ab_seq}
          setenv ab_active$c ''${ab_active}
          setenv ab_try$c ''${ab_try}
          setenv ab_tries$c ''${ab_tries}
        fi
      fi
    done
    # The newest valid copy; the one to write is the other.
    setenv ab_use none
    if test ''${ab_ok0} = 1; then setenv ab_use 0; fi
    if test ''${ab_ok1} = 1; then
      if test ''${ab_use} = none; then setenv ab_use 1; elif itest ''${ab_seq1} > ''${ab_seq0}; then setenv ab_use 1; fi
    fi
    if test ''${ab_use} = none; then
      echo "KD240: no valid A/B boot state; booting slot a"
      setenv ab_boot a
    else
      if test ''${ab_use} = 0; then
        setenv ab_active ''${ab_active0}; setenv ab_try ''${ab_try0}; setenv ab_tries ''${ab_tries0}; setenv ab_seq ''${ab_seq0}; setenv ab_wlba 0x1800
      else
        setenv ab_active ''${ab_active1}; setenv ab_try ''${ab_try1}; setenv ab_tries ''${ab_tries1}; setenv ab_seq ''${ab_seq1}; setenv ab_wlba 0x1000
      fi
      setenv ab_boot ''${ab_active}
      if test "''${ab_try}" != "-" && itest ''${ab_tries} > 0; then
        # One try used up before it is booted: a hang means fallback.
        setexpr ab_tries ''${ab_tries} - 1
        setexpr ab_seq ''${ab_seq} + 1
        setenv ab_magic emco-ab-1
        mw.b ''${ab_buf} 0 0x200
        env export -t ''${ab_text} ab_active ab_magic ab_seq ab_tries ab_try
        crc32 ''${ab_text} 0x1fc ''${ab_buf}
        if ''${devtype} write ''${ab_buf} ''${ab_wlba} 1; then
          setenv ab_boot ''${ab_try}
          echo "KD240: trying slot ''${ab_boot} (once)"
        else
          echo "KD240: could not record the try; booting committed slot ''${ab_boot}"
        fi
      fi
    fi
    echo "KD240: booting slot ''${ab_boot} from ''${devtype} ''${devnum}:1"
    load ''${devtype} ''${devnum}:1 ''${scriptaddr} /''${ab_boot}/cmdline && env import -t ''${scriptaddr} ''${filesize}
    setenv bootargs "''${bootargs} emco.slot=''${ab_boot}"
    load ''${devtype} ''${devnum}:1 ''${kernel_addr_r} /''${ab_boot}/Image
    load ''${devtype} ''${devnum}:1 ''${fdt_addr_r} /''${ab_boot}/dtb
    load ''${devtype} ''${devnum}:1 ''${ramdisk_addr_r} /''${ab_boot}/initrd
    booti ''${kernel_addr_r} ''${ramdisk_addr_r}:''${filesize} ''${fdt_addr_r}
    EOF
    mkimage -A arm64 -O linux -T script -C none -n kd240-ab-boot -d boot.cmd $out
  '';

  squashfs = pkgs.callPackage "${modulesPath}/../lib/make-squashfs.nix" {
    storeContents = [ toplevel ];
    comp = "zstd -Xcompression-level 6";
  };

  cmdline = "init=${toplevel}/init ${toString config.boot.kernelParams}";

  bundle = pkgs.runCommand "kd240-bundle" { nativeBuildInputs = [ pkgs.buildPackages.jq ]; } ''
    mkdir $out
    cp ${squashfs} $out/rootfs.squashfs
    cp ${toplevel}/kernel $out/Image
    cp ${toplevel}/initrd $out/initrd
    cp ${toplevel}/dtbs/${dtb} $out/dtb
    printf 'bootargs=%s\n' ${lib.escapeShellArg cmdline} > $out/cmdline
    test $(stat -c %s $out/cmdline) -le 4096
    files=$(for f in rootfs.squashfs Image initrd dtb cmdline; do
      jq -n --arg name $f --argjson size $(stat -c %s $out/$f) \
            --arg sha256 $(sha256sum $out/$f | cut -d' ' -f1) \
            '{name: $name, size: $size, sha256: $sha256}'
    done | jq -s .)
    jq -n --arg version ${toplevel} --argjson files "$files" \
      '{format: "emco-bundle-1", class: "kd240", version: $version, created: 0, files: $files}' \
      > $out/bundle.json
  '';

  # The whole card, slot a holding this release, slot b empty.
  abImage = pkgs.runCommand "emco-kd240-ab.img" {
    nativeBuildInputs = with pkgs.buildPackages; [ util-linux dosfstools mtools e2fsprogs zstd ];
  } ''
    img=emco-kd240-ab.img
    MiB=$((1024 * 1024))
    total=$((8 + ${toString bootMiB} + 2 * ${toString slotMiB} + ${toString dataMiB} + 1))
    truncate -s $((total * MiB)) $img
    sfdisk --no-reread --no-tell-kernel $img <<EOF
    label: dos
    label-id: 0x${diskId}
    start=8MiB, size=${toString bootMiB}MiB, type=c, bootable
    size=${toString slotMiB}MiB, type=83
    size=${toString slotMiB}MiB, type=83
    size=${toString dataMiB}MiB, type=83
    EOF
    test $(stat -c %s ${bundle}/rootfs.squashfs) -le $((${toString slotMiB} * MiB)) || {
      echo "error: the store squashfs does not fit a ${toString slotMiB} MiB slot" >&2; exit 1; }

    # The boot state: slot a committed, nothing to try (bootstate.rs
    # BootState::initial()), copy 0; copy 1 zeroed, so invalid.
    text=$(printf 'ab_active=a\nab_magic=emco-ab-1\nab_seq=1\nab_tries=0\nab_try=-\n')
    { printf '%s\n' "$text"; } > text.bin
    truncate -s 508 text.bin
    crc=$(gzip -c < text.bin | tail -c8 | head -c4 | od -An -tx4 -v --endian=little | tr -d ' ')
    printf "\\x''${crc:0:2}\\x''${crc:2:2}\\x''${crc:4:2}\\x''${crc:6:2}" > state.bin
    cat text.bin >> state.bin
    test $(stat -c %s state.bin) = 512
    dd if=state.bin of=$img bs=512 seek=4096 conv=notrunc
    dd if=/dev/zero of=$img bs=512 seek=6144 count=1 conv=notrunc

    mkfs.vfat -C -n KD240BOOT boot.fat $((${toString bootMiB} * 1024))
    mcopy -i boot.fat ${bootScr} ::boot.scr
    mmd -i boot.fat ::a ::b
    for f in Image initrd dtb cmdline; do mcopy -i boot.fat ${bundle}/$f ::a/$f; done
    dd if=boot.fat of=$img bs=$MiB seek=8 conv=notrunc
    dd if=${bundle}/rootfs.squashfs of=$img bs=$MiB seek=$((8 + ${toString bootMiB})) conv=notrunc

    mkfs.ext4 -L KD240DATA -E offset=$(((8 + ${toString bootMiB} + 2 * ${toString slotMiB}) * MiB)) $img ${toString dataMiB}M

    mkdir -p $out/sd-image
    zstd -T0 --rm $img -o $out/sd-image/$img.zst
    cp ${bundle}/bundle.json $out/sd-image/slot-a.bundle.json
  '';
in
{
  # services.emco-fleet-agent, off until a server, token and key are set.
  imports = [ ./fleet.nix ];

  # configuration.nix's card layout and extlinux menu do not apply.
  boot.loader.generic-extlinux-compatible.enable = lib.mkForce false;

  # The store is the booted slot's squashfs, read-only, under a tmpfs
  # overlay (as the NixOS installer ISO does). The initrd finds the slot
  # from `emco.slot=` on the command line, which boot.scr appends.
  boot.initrd.availableKernelModules = [ "squashfs" "overlay" ];
  boot.initrd.kernelModules = [ "squashfs" "overlay" "loop" ];
  boot.initrd.postDeviceCommands = ''
    slot=
    for w in $(cat /proc/cmdline); do
      case "$w" in emco.slot=a) slot=02 ;; emco.slot=b) slot=03 ;; esac
    done
    if [ -z "$slot" ]; then
      echo "emco: no emco.slot= on the command line; trying slot a"
      slot=02
    fi
    ln -sfn /dev/disk/by-partuuid/${diskId}-$slot /dev/emco-store
  '';

  fileSystems = lib.mkForce {
    "/" = {
      device = "/dev/disk/by-label/KD240DATA";
      fsType = "ext4";
      autoResize = true;
    };
    "/nix/.ro-store" = {
      device = "/dev/emco-store";
      fsType = "squashfs";
      neededForBoot = true;
    };
    "/nix/.rw-store" = {
      fsType = "tmpfs";
      options = [ "mode=0755" ];
      neededForBoot = true;
    };
    "/nix/store" = {
      overlay = {
        lowerdir = [ "/nix/.ro-store" ];
        upperdir = "/nix/.rw-store/store";
        workdir = "/nix/.rw-store/work";
      };
      neededForBoot = true;
    };
    # The FAT partition: boot.scr and both slots' boot files. The agent
    # writes the inactive slot's directory here.
    "/boot" = {
      device = "/dev/disk/by-label/KD240BOOT";
      fsType = "vfat";
      options = [ "nofail" "umask=0077" ];
    };
  };
  boot.growPartition = true;

  # A release is the slot; nothing is built or switched on the board.
  nix.enable = false;
  nix.gc.automatic = lib.mkForce false;
  system.switch.enable = false;
  documentation.enable = false;

  # The card's id, for the fleet agent (services.emco-fleet-agent.diskId).
  system.build.abDiskId = diskId;
  system.build.bundle = bundle;
  system.build.abBootScr = bootScr;
  system.build.abImage = abImage;
}
