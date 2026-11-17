#!/usr/bin/env python3
"""Extract .NET assemblies from a .NET for Android assembly store (XABA v2/v3).

Usage: uv run --with lz4 scripts/extract_assembly_store.py <libassemblies.*.blob.so> <out_dir>

The store sits in the `payload` section of an ELF wrapper. Each assembly is
either raw PE or an XALZ container (LZ4 block). Output stays in work/ (git-ignored).
"""
import struct
import sys
from pathlib import Path

import lz4.block

MAGIC = b"XABA"
INDEX_ENTRY_SIZE = {2: 12, 3: 13}  # 64-bit stores: u64 hash + u32 index (+ u8 ignore in v3)


def extract(blob_path: Path, out_dir: Path) -> int:
    data = blob_path.read_bytes()
    base = data.find(MAGIC)
    if base < 0:
        raise SystemExit(f"{blob_path}: no XABA assembly store found")
    _, version, entry_count, index_count, index_size = struct.unpack_from("<5I", data, base)
    fmt = version & 0xFF
    if fmt not in INDEX_ENTRY_SIZE or not version & 0x80000000:
        raise SystemExit(f"unsupported store version {version:#x} (only 64-bit v2/v3)")
    assert index_size == index_count * INDEX_ENTRY_SIZE[fmt], "index size mismatch"

    pos = base + 20 + index_size
    descriptors = [struct.unpack_from("<7I", data, pos + i * 28) for i in range(entry_count)]
    pos += entry_count * 28
    names = []
    for _ in range(entry_count):
        (length,) = struct.unpack_from("<I", data, pos)
        names.append(data[pos + 4 : pos + 4 + length].decode())
        pos += 4 + length

    out_dir.mkdir(parents=True, exist_ok=True)
    for name, (_, offset, size, *_rest) in zip(names, descriptors):
        raw = data[base + offset : base + offset + size]
        if raw[:4] == b"XALZ":
            (uncompressed,) = struct.unpack_from("<I", raw, 8)
            raw = lz4.block.decompress(raw[12:], uncompressed_size=uncompressed)
        assert raw[:2] == b"MZ", f"{name}: not a PE file after extraction"
        target = out_dir / Path(name).name  # names can carry a culture dir; flatten safely
        if "/" in name:
            target = out_dir / name.replace("/", "__")
        target.write_bytes(raw)
    return entry_count


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    print(f"extracted {extract(Path(sys.argv[1]), Path(sys.argv[2]))} assemblies")
