#!/usr/bin/env python3
"""How much of an ELF would actually be written to flash.

Exists because of a specific, silent failure. Without `-C link-arg=-Tlink.x`
the linker keeps no `.vector_table`, garbage-collects the whole program as
unreachable, and STILL PRODUCES A VALID ELF. The build succeeds, the flash
succeeds, and the board does nothing at all — a failure that costs an
afternoon because every step reports success.

So the build asserts a minimum plausible size. The idea is stolen from the
`boards.toml` in a sibling project, which learned it the same way.

    python3 tools/image-size.py FIRMWARE.elf --min-bytes 16384

Stdlib only: this runs in CI next to a cross-compile, and adding a dependency
to check the output of a build is a way to make the check the thing that
breaks.
"""

import argparse
import struct
import sys


def loadable_bytes(path):
    """The sum of the PT_LOAD segments' file sizes: what a flasher writes."""
    with open(path, "rb") as handle:
        data = handle.read()
    if data[:4] != b"\x7fELF":
        raise SystemExit(f"{path} is not an ELF file")
    is64 = data[4] == 2
    endian = "<" if data[5] == 1 else ">"
    if is64:
        phoff, = struct.unpack_from(endian + "Q", data, 0x20)
        phentsize, phnum = struct.unpack_from(endian + "HH", data, 0x36)
        filesz_at = 0x20
    else:
        phoff, = struct.unpack_from(endian + "I", data, 0x1C)
        phentsize, phnum = struct.unpack_from(endian + "HH", data, 0x2A)
        filesz_at = 0x10
    total = 0
    for i in range(phnum):
        at = phoff + i * phentsize
        kind, = struct.unpack_from(endian + "I", data, at)
        if kind != 1:                       # PT_LOAD
            continue
        size, = struct.unpack_from(endian + ("Q" if is64 else "I"), data,
                                   at + filesz_at)
        total += size
    return total


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("elf")
    parser.add_argument("--min-bytes", type=int, default=16384,
                        help="fail below this (default 16384)")
    parser.add_argument("--max-bytes", type=int, default=1024 * 1024,
                        help="fail above this; the F407VG has 1 MiB of flash")
    args = parser.parse_args()

    size = loadable_bytes(args.elf)
    print(f"{args.elf}: {size:,} bytes would be written to flash")
    if size < args.min_bytes:
        print(f"error: below {args.min_bytes:,} bytes. The usual cause is a "
              "missing `-Tlink.x`: the linker keeps no vector table, "
              "garbage-collects the program to nothing, and still succeeds.",
              file=sys.stderr)
        return 1
    if size > args.max_bytes:
        print(f"error: above {args.max_bytes:,} bytes, which is more flash "
              "than the part has.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
