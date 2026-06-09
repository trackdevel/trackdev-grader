#!/usr/bin/env python3
import struct
import zlib
from pathlib import Path


def png_chunk(tag: bytes, data: bytes) -> bytes:
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def main() -> None:
    width, height = 32, 32
    # Tauri requires RGBA (color type 6), not RGB.
    row = b"\x00" + b"\x00\x66\xcc\xff" * width
    raw = row * height
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    out = Path(__file__).with_name("icon.png")
    out.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", ihdr)
        + png_chunk(b"IDAT", zlib.compress(raw, 9))
        + png_chunk(b"IEND", b"")
    )
    print(f"Wrote {out} ({out.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
