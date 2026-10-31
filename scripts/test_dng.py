"""Developer-only independent libjxl reference (imagecodecs 2026.3.6).

--fixture regenerates the tiny synthetic lossless JXL used by native tests.
--source compares a JPEG XL DNG with an independently unpacked copy in target/.
No source image is modified. Application runtime does not use this script.
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import imagecodecs
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument("--fixture", action="store_true")
p.add_argument("--source", type=Path)
p.add_argument("--exe", type=Path, default=ROOT / "target/debug/scrutivue.exe")
args = p.parse_args()
if args.fixture:
    values = ((np.arange(16*16*3, dtype=np.uint32) * 17431 + 50123) & 65535).astype(np.uint16).reshape(16, 16, 3)
    out = ROOT / "assets/tests/dng-jxl-tile.jxl"
    out.parent.mkdir(parents=True, exist_ok=True)
    encoded = imagecodecs.jpegxl_encode(values, lossless=True)
    assert np.array_equal(imagecodecs.jpegxl_decode(encoded), values)
    out.write_bytes(encoded)
    print(out.name, len(encoded), hashlib.sha256(encoded).hexdigest())
if args.source:
    source = args.source.read_bytes()
    assert source[:4] == b"II*\0", "Reference tool expects little-endian classic TIFF"
    offset, = struct.unpack_from("<I", source, 4)
    n, = struct.unpack_from("<H", source, offset)
    entries = [source[offset+2+i*12:offset+14+i*12] for i in range(n)]
    tags = {struct.unpack_from("<H", entry)[0]: entry for entry in entries}
    def values(tag):
        _, kind, count, value = struct.unpack("<HHII", tags[tag])
        code, size = {1: ("B", 1), 3: ("H", 2), 4: ("I", 4)}[kind]
        data = tags[tag][8:] if count * size <= 4 else source[value:value+count*size]
        return struct.unpack("<" + code * count, data[:count*size])
    w, h, tw, th, channels = [values(tag)[0] for tag in [256, 257, 322, 323, 277]]
    assert values(259) == (52546,) and values(258) == (16, 16, 16) and channels == 3
    nx = (w + tw - 1) // tw
    pixels = np.zeros((h, w, 3), dtype="<u2")
    for i, (start, length) in enumerate(zip(values(324), values(325))):
        x, y = (i % nx) * tw, (i // nx) * th
        tile = imagecodecs.jpegxl_decode(source[start:start+length])
        cw, ch = min(tw, w-x), min(th, h-y)
        pixels[y:y+ch, x:x+cw] = tile[:ch, :cw]
    raw_start = (len(source) + 3) & ~3
    replacement = [e for e in entries if struct.unpack_from("<H", e)[0] not in [259, 273, 278, 279, 317, 322, 323, 324, 325]]
    for tag, kind, value in [(259, 3, 1), (273, 4, raw_start), (278, 4, h), (279, 4, pixels.nbytes)]:
        replacement.append(struct.pack("<HHII", tag, kind, 1, value))
    replacement.sort(key=lambda e: struct.unpack_from("<H", e)[0])
    virtual = bytearray(source)
    rewritten = struct.pack("<H", len(replacement)) + b"".join(replacement) + source[offset+2+n*12:offset+6+n*12]
    virtual[offset:offset+len(rewritten)] = rewritten
    root = ROOT / "target/dng-reference"
    root.mkdir(parents=True, exist_ok=True)
    out = root / "uncompressed.dng"
    with out.open("wb") as f:
        f.write(virtual)
        f.write(bytes(raw_start-len(virtual)))
        f.write(pixels.tobytes())
    result = json.loads(subprocess.check_output([str(args.exe), "--probe", str(args.source), str(out)], text=True, encoding="utf-8"))
    for name in ["dimensions", "rgb16_bytes", "sample_sum", "rgb16_le_sha256"]:
        assert result[0][name] == result[1][name], (name, result)
    assert hashlib.sha256(args.source.read_bytes()).digest() == hashlib.sha256(source).digest(), "Source was changed"
    (root / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps({"independent_libjxl_matches": True, "results": result}, indent=2))
