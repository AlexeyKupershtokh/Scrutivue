"""Make a 10,000-file load fixture, or fetch verified CC0 Nikon HE / HE* samples."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import urllib.request
import zlib

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("kind", choices=["library", "raw"])
args = parser.parse_args()
if args.kind == "raw":
    manifest = json.loads((ROOT / "vendor/libraw/redlamp/bench/samples.json").read_text())
    destination = ROOT / "target/fixtures/raw"
    destination.mkdir(parents=True, exist_ok=True)
    for sample in manifest["samples"][:2]:
        path = destination / (sample["name"] + ".NEF")
        if not path.exists():
            print("Downloading CC0", sample["name"], flush=True)
            urllib.request.urlretrieve(sample["url"], path)
        if hashlib.sha256(path.read_bytes()).hexdigest() != sample["sha256"]:
            raise SystemExit("Fixture SHA-256 mismatch: " + str(path))
        print(path)
else:
    destination = ROOT / "target/fixtures/library-10000"
    destination.mkdir(parents=True, exist_ok=True)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    width, height = 320, 216
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        for x in range(width):
            rows.extend((int(210 * x / width), int(180 * y / height), 80 + ((x ^ y) % 80)))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
    first = destination / "Frame_00000.png"
    first.write_bytes(png)
    for i in range(1, 10000):
        path = destination / f"Frame_{i:05d}.png"
        if not path.exists():
            try:
                path.hardlink_to(first)
            except OSError:
                path.write_bytes(png)
    print(destination)
