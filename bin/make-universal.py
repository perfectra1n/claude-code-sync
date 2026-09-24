#!/usr/bin/env python3
"""Combine thin Mach-O binaries into one universal (fat) binary.

Stands in for Apple's `lipo`, which does not exist on Linux. The fat container is a
big-endian header followed by one record per slice; each slice is the unmodified thin
binary, page-aligned. Format: mach-o/fat.h.
"""
import struct
import sys

FAT_MAGIC = 0xCAFEBABE
MH_MAGIC_64 = 0xFEEDFACF
ALIGN_EXPONENT = 14  # 16 KiB, the alignment Apple uses for arm64 slices


def read_thin(path):
    data = open(path, "rb").read()
    magic, cpu_type, cpu_subtype = struct.unpack("<3I", data[:12])
    if magic != MH_MAGIC_64:
        raise SystemExit(f"{path}: not a 64-bit Mach-O (magic {magic:#x})")
    return cpu_type, cpu_subtype, data


def main(output, inputs):
    slices = [read_thin(path) for path in inputs]
    alignment = 1 << ALIGN_EXPONENT

    offset = 8 + 20 * len(slices)
    placed = []
    for cpu_type, cpu_subtype, data in slices:
        offset = (offset + alignment - 1) // alignment * alignment
        placed.append((cpu_type, cpu_subtype, offset, len(data), data))
        offset += len(data)

    out = bytearray()
    out += struct.pack(">2I", FAT_MAGIC, len(placed))
    for cpu_type, cpu_subtype, start, size, _ in placed:
        out += struct.pack(">5I", cpu_type, cpu_subtype, start, size, ALIGN_EXPONENT)
    for _, _, start, _, data in placed:
        out += b"\0" * (start - len(out))
        out += data

    open(output, "wb").write(out)
    print(f"{output}: {len(placed)} architectures, {len(out)} bytes")
    for cpu_type, _, start, size, _ in placed:
        print(f"  cputype {cpu_type:#010x}  offset {start}  size {size}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2:])
