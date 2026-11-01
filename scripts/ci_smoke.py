"""Headless Windows/CI check: full statistics and embedded ONNX without Python ML."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]


def png(path):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    size = 64
    pixels = b"".join(
        b"\0" + bytes((x * 3 + y * 5 + channel * 31) % 256
                       for x in range(size) for channel in range(3))
        for y in range(size)
    )
    path.write_bytes(b"\x89PNG\r\n\x1a\n"
                     + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0))
                      + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b""))


def dng(path, calibrated=False):
    """Small, generated LinearRaw fixture using the repository's lossless JXL tile."""
    tile = (ROOT / "assets/tests/dng-jxl-tile.jxl").read_bytes()
    count, tiles = 14 + (2 if calibrated else 0), 16
    data = bytearray(b"II*\0" + struct.pack("<I", 8) + bytes(2 + count * 12 + 4))
    bits = len(data)
    data.extend(struct.pack("<HHH", 16, 16, 16))
    offsets = len(data)
    data.extend(bytes(tiles * 4))
    lengths = len(data)
    data.extend(bytes(tiles * 4))
    camera = len(data)
    data.extend(b"TEST-DNG\0")
    neutral = len(data)
    if calibrated:
        data.extend(struct.pack("<IIIIII", 1, 1, 1, 1, 1, 1))
    for index in range(tiles):
        struct.pack_into("<I", data, offsets + index * 4, len(data))
        struct.pack_into("<I", data, lengths + index * 4, len(tile))
        data.extend(tile)
    entries = [(254, 4, 1, 0), (256, 4, 1, 64), (257, 4, 1, 64),
        (258, 3, 3, bits), (259, 3, 1, 52546), (262, 3, 1, 34892),
        (277, 3, 1, 3), (284, 3, 1, 1), (322, 4, 1, 16), (323, 4, 1, 16),
        (324, 4, tiles, offsets), (325, 4, tiles, lengths),
        (50706, 1, 4, 0x701), (50708, 2, 9, camera)]
    if calibrated:
        # The lossless asset's independent generator is documented in dng.rs.
        # Make its actual peak equal the declared white, so no global rescale is needed.
        white = max((i * 17431 + 50123) & 65535 for i in range(16 * 16 * 3))
        entries += [(50717, 4, 1, white), (50728, 5, 3, neutral)]
    struct.pack_into("<H", data, 8, count)
    for index, entry in enumerate(sorted(entries)):
        struct.pack_into("<HHII", data, 10 + index * 12, *entry)
    path.write_bytes(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "target/release/scrutivue.exe")
    args = parser.parse_args()
    exe = args.exe.resolve(strict=True)
    if not exe.with_name("onnxruntime.dll").is_file():
        raise SystemExit("onnxruntime.dll must be beside scrutivue.exe")
    # No UI, network, reference-image download or modification of user preferences.
    subprocess.run([str(exe), "--help"], check=True, stdout=subprocess.DEVNULL, timeout=30)
    (ROOT / "target").mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ci-smoke-", dir=ROOT / "target") as temporary:
        for calibrated in (False, True):
            raw = Path(temporary) / f"native-tiled-{calibrated}.dng"
            dng(raw, calibrated)
            probe = subprocess.run([str(exe), "--stream-probe", str(raw)],
                check=True, capture_output=True, text=True, encoding="utf-8", timeout=30)
            native = json.loads(probe.stdout)
            assert native["success"] and native["native_runtime"], native
            assert native["matches_fresh_full_decode"] and native["source_unchanged"], native
            assert native["matches_legacy_full_development"] and native["legacy_differing_samples"] == 0, native
            assert native["completion"]["path"] == ("developed-tiles" if calibrated else "full-libraw"), native
            if calibrated:
                assert native["completion"]["development_ms"] == 0, native
            assert native["total_entropy_tile_decodes"] == native["total_tiles"], native
            assert native["views"][2]["new_tile_decodes"] == 0, native
        image = Path(temporary) / "identity.png"
        png(image)
        result = subprocess.run(
            [str(exe), "--statistics", str(image), str(image)],
            check=True, capture_output=True, text=True, encoding="utf-8", timeout=120,
        )
        report = json.loads(result.stdout)
        assert report["original_size"] == [64, 64], report
        assert not report["pending"], report
        assert report["pixels"]["mae"] == 0, report
        assert report["pixels"]["psnr_db"] is None, report
        assert abs(report["ssim_y"] - 1) < 1e-6, report
        assert abs(report["neural"]["Ok"]["lpips"]) < 1e-7, report
    print("Standalone CLI, native tile completion/fallback with legacy RGB16 equality, statistics and embedded LPIPS passed.")


if __name__ == "__main__":
    main()
