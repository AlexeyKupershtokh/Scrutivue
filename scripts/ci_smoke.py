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
    print("Standalone CLI, native statistics and embedded LPIPS passed.")


if __name__ == "__main__":
    main()
