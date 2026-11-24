"""Read-only inspector for standard PE headers. Does not execute input."""
import argparse
import json
import struct
from pathlib import Path

def inspect(data: bytes) -> dict:
    if len(data) < 64 or data[:2] != b"MZ":
        raise ValueError("Invalid DOS header")
    offset = struct.unpack_from("<I", data, 60)[0]
    if offset < 64 or offset + 24 > len(data) or data[offset:offset+4] != b"PE\0\0":
        raise ValueError("Invalid PE header")
    machine, sections, _, _, _, optional_size, flags = struct.unpack_from("<HHIIIHH", data, offset+4)
    start = offset+24
    if optional_size < 2 or start+optional_size+sections*40 > len(data):
        raise ValueError("Truncated headers")
    magic = struct.unpack_from("<H", data, start)[0]
    minimum = {0x10b: 96, 0x20b: 112}.get(magic)
    if minimum is None or optional_size < minimum:
        raise ValueError("Invalid optional header")
    return {"machine": hex(machine), "sections": sections, "format": "PE32+" if magic == 0x20b else "PE32", "characteristics": hex(flags)}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file", type=Path)
    args = parser.parse_args()
    try:
        if args.file.stat().st_size > 64*1024*1024:
            raise ValueError("Input exceeds 64 MiB inspection limit")
        with args.file.open("rb") as stream:
            data = stream.read(64*1024*1024+1)
        if len(data) > 64*1024*1024:
            raise ValueError("Input exceeds inspection limit")
        print(json.dumps(inspect(data), indent=2))
    except (OSError, ValueError) as exc:
        parser.exit(1, f"error: {exc}\n")

if __name__ == "__main__":
    main()
