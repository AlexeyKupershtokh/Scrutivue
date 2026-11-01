"""Bounded native JPEG reduced-IDCT/ROI experiment, independent of the app.

All generated fixtures, builds and results stay under target/preview-jpeg.
Photographs are read-only and SHA-256 checked before and after the run.
"""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "target" / "preview-jpeg"
SOURCE = Path.home() / ".cargo/registry/src/index.crates.io-1949cf8c6b5b557f/jpegli-sys-0.1.0+0.10.2/libjxl/third_party/libjpeg-turbo"
VSWHERE = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"


def digest(path):
    with open(path, "rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def memory():
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (key, ctypes.c_size_t) for key in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
                "PagefileUsage", "PeakPagefileUsage", "PrivateUsage")]

    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    if not psapi.GetProcessMemoryInfo(kernel.GetCurrentProcess(), ctypes.byref(counters), counters.cb):
        raise ctypes.WinError(ctypes.get_last_error())
    return {"peak_working_set_bytes": counters.PeakWorkingSetSize,
            "working_set_bytes": counters.WorkingSetSize,
            "peak_private_commit_bytes": counters.PeakPagefileUsage}


def pillow_worker(path, denominator):
    from PIL import Image, features
    encoded = Path(path).read_bytes()
    before = memory()
    started = time.perf_counter()
    image = Image.open(io.BytesIO(encoded))
    original = image.size
    draft = image.draft("RGB", (original[0] // denominator, original[1] // denominator))
    prepared = time.perf_counter()
    image.load()
    finished = time.perf_counter()
    after = memory()
    print(json.dumps({"backend": "Pillow/libjpeg-turbo " + features.version_feature("libjpeg_turbo"),
                      "denominator": denominator, "source_width": original[0], "source_height": original[1],
                      "width": image.width, "height": image.height, "draft": draft,
                      "header_draft_ms": (prepared-started)*1000,
                      "load_ms": (finished-prepared)*1000, "decode_ms": (finished-started)*1000,
                      "output_rgb_bytes": image.width*image.height*3,
                      "working_set_before_bytes": before["working_set_bytes"], **after}))


def checked(command, **kwargs):
    subprocess.run([str(item) for item in command], check=True, **kwargs)


def build():
    if not (SOURCE / "CMakeLists.txt").is_file():
        raise SystemExit("The expected read-only libjpeg-turbo 2.1.5.1 source is unavailable")
    install = Path(subprocess.check_output([str(VSWHERE), "-latest", "-products", "*", "-property", "installationPath"], text=True).strip())
    cmake = install / "Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe"
    build_dir = OUT / "build"
    started = time.perf_counter()
    with open(OUT / "build.log", "w", encoding="utf-8") as log:
        checked([cmake, "-S", SOURCE, "-B", build_dir, "-G", "Visual Studio 17 2022", "-A", "x64",
                 "-DWITH_SIMD=OFF", "-DENABLE_SHARED=OFF", "-DENABLE_STATIC=ON", "-DWITH_TURBOJPEG=OFF",
                 "-DCMAKE_POLICY_VERSION_MINIMUM=3.5"], stdout=log, stderr=subprocess.STDOUT)
        checked([cmake, "--build", build_dir, "--config", "Release", "--target", "jpeg-static", "--parallel", "2"],
                stdout=log, stderr=subprocess.STDOUT)
        env_output = subprocess.check_output(
            f'cmd /d /s /c ""{install / "VC/Auxiliary/Build/vcvars64.bat"}" >nul && set"', text=True)
        env = dict(os.environ)
        for line in env_output.splitlines():
            if "=" in line:
                key, value = line.split("=", 1)
                env[key] = value
        compiler = Path(env["VCToolsInstallDir"]) / "bin/Hostx64/x64/cl.exe"
        checked([compiler, "/nologo", "/std:c++17", "/O2", "/EHsc", "/MT",
                 "/I" + str(SOURCE), "/I" + str(build_dir), ROOT / "experiments/preview/jpeg_probe.cpp",
                 build_dir / "Release/jpeg-static.lib", "psapi.lib", "/Fe:" + str(OUT / "jpeg_probe.exe"),
                 "/Fo:" + str(OUT / "jpeg_probe.obj")], env=env, cwd=OUT, stdout=log, stderr=subprocess.STDOUT)
    elapsed = time.perf_counter() - started
    (OUT / "build-metadata.json").write_text(json.dumps({"seconds": elapsed, "parallel": 2, "simd": False,
            "libjpeg_turbo": "2.1.5.1", "jpeglib_sha256": digest(SOURCE / "jpeglib.h"),
            "jdapistd_sha256": digest(SOURCE / "jdapistd.c")}, indent=2) + "\n", encoding="utf-8")
    print(f"JPEG standalone build complete: {elapsed:.2f} seconds, maximum 2 build jobs", flush=True)


def fixtures():
    import numpy as np
    from PIL import Image
    baseline, progressive = OUT / "synthetic-baseline.jpg", OUT / "synthetic-progressive.jpg"
    if not baseline.exists() or not progressive.exists():
        # A bounded 12.6 MP deterministic texture exercises 4:2:0 and chroma edges.
        y, x = np.indices((3072, 4096), dtype=np.uint32)
        pixels = np.empty((3072, 4096, 3), dtype=np.uint8)
        pixels[:, :, 0] = ((x*7 + y*3) ^ (x//17*57)) % 256
        pixels[:, :, 1] = ((x*2 + y*11) ^ (y//19*63)) % 256
        pixels[:, :, 2] = ((x*5 + y*5) ^ ((x+y)//13*71)) % 256
        image = Image.fromarray(pixels)
        image.save(baseline, quality=92, subsampling=2, progressive=False)
        image.save(progressive, quality=92, subsampling=2, progressive=True)
    return baseline, progressive


def native(path, denominator, rectangle=None, dump=None):
    command = [OUT / "jpeg_probe.exe", path, denominator, "roi" if rectangle else "full"]
    if rectangle:
        command.extend(rectangle)
        if dump:
            command.append(dump)
    elif dump:
        command.append(dump)
    return json.loads(subprocess.check_output([str(item) for item in command], text=True))


def compare(full_path, roi_path, full_width, full_height, rectangle, decoded_rectangle=None):
    import numpy as np
    x, y, width, height = rectangle
    full = np.memmap(full_path, dtype=np.uint8, mode="r", shape=(full_height, full_width, 3))
    dx, dy, dw, dh = decoded_rectangle or rectangle
    decoded = np.memmap(roi_path, dtype=np.uint8, mode="r", shape=(dh, dw, 3))
    roi = decoded[y-dy:y-dy+height, x-dx:x-dx+width]
    delta = np.abs(roi.astype(np.int16) - full[y:y+height, x:x+width].astype(np.int16))
    interior = delta[:, 2:-2, :]
    result = {"max_absolute_error": int(delta.max()), "mean_absolute_error": float(delta.mean()),
              "different_channel_values": int(np.count_nonzero(delta)),
              "interior_max_error_excluding_two_edge_columns": int(interior.max()),
              "interior_different_channel_values": int(np.count_nonzero(interior))}
    del full, roi, decoded
    return result


def edge_checks():
    results = []
    for path in fixtures():
        full_dump = OUT / "edge-full.rgb"
        native(path, 1, dump=full_dump)
        for rectangle in ((1536, 1155, 1024, 65), (1536, 1155, 1025, 65), (1537, 1155, 1023, 65)):
            x, y, width, height = rectangle
            for halo in (0, 16):
                decoded = (x-halo, y, width+2*halo, height)
                roi_dump = OUT / "edge-roi.rgb"
                result = native(path, 1, decoded, roi_dump)
                results.append({"fixture": path.stem, "denominator": 1, "requested_rectangle": rectangle,
                    "decoded_rectangle": decoded, "horizontal_halo": halo, "decode_ms": result["decode_ms"],
                    **compare(full_dump, roi_dump, 4096, 3072, rectangle, decoded)})
                roi_dump.unlink()
        full_dump.unlink()
    (OUT / "edge-correctness.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(f"Finished {len(results)} aligned-edge and halo comparisons", flush=True)


def run(photo, repeats):
    from PIL import Image, __version__ as pillow_version, features
    before = digest(photo)
    baseline, progressive = fixtures()
    records, correctness = [], []
    manifest = {"platform": platform.platform(), "python": sys.version, "pillow": pillow_version,
                "pillow_libjpeg_turbo": features.version_feature("libjpeg_turbo"),
                "repeats": repeats, "timing_scope": "Fresh process per decode; compressed bytes preloaded; no output-file I/O in decode timing",
                "sources": []}
    for label, path in [("photo-45mp", photo), ("synthetic-baseline", baseline), ("synthetic-progressive", progressive)]:
        with Image.open(path) as image:
            source_width, source_height = image.size
            manifest["sources"].append({"label": label, "width": source_width, "height": source_height,
                "bytes": path.stat().st_size, "sha256": digest(path), "progressive": bool(image.info.get("progressive")),
                "sampling": image.layer})
        for denominator in (1, 2, 4, 8):
            full_dump = OUT / f"{label}-{denominator}-full.rgb"
            width, height = (source_width + denominator-1)//denominator, (source_height + denominator-1)//denominator
            roi_width, roi_height = min(1024, width//3), min(768, height//3)
            x = (width-roi_width)//2 + 3
            rectangles = {"top": (x, 0, roi_width, roi_height),
                          "middle": (x, (height-roi_height)//2+3, roi_width, roi_height),
                          "bottom": (x, height-roi_height-3, roi_width, roi_height)}
            for repeat in range(repeats):
                result = native(path, denominator, dump=full_dump if repeat == 0 else None)
                records.append({"fixture": label, "mode": "full", "repeat": repeat, **result})
                pillow = json.loads(subprocess.check_output([sys.executable, __file__, "--pillow-worker", str(path),
                                                            "--denominator", str(denominator)], text=True))
                records.append({"fixture": label, "mode": "full", "repeat": repeat, **pillow})
                for position, rectangle in rectangles.items():
                    roi_dump = OUT / "correctness-roi.rgb"
                    result = native(path, denominator, rectangle, roi_dump if repeat == 0 else None)
                    records.append({"fixture": label, "mode": "roi-"+position, "repeat": repeat, **result})
                    if repeat == 0:
                        correctness.append({"fixture": label, "denominator": denominator, "position": position,
                            "rectangle": rectangle, **compare(full_dump, roi_dump, width, height, rectangle)})
                        roi_dump.unlink()
            full_dump.unlink()
            print(f"Finished {label} at 1/{denominator}", flush=True)
    manifest["photo_sha256_before"] = before
    manifest["photo_sha256_after"] = digest(photo)
    if before != manifest["photo_sha256_after"]:
        raise RuntimeError("Source photograph hash changed")
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    (OUT / "measurements.json").write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
    (OUT / "correctness.json").write_text(json.dumps(correctness, indent=2) + "\n", encoding="utf-8")
    groups = {}
    for record in records:
        key = (record["fixture"], record["backend"], record["denominator"], record["mode"])
        groups.setdefault(key, []).append(record)
    summary = []
    for (fixture, backend, denominator, mode), group in groups.items():
        item = {"fixture": fixture, "backend": backend, "denominator": denominator, "mode": mode,
                "decode_ms_median": statistics.median(row["decode_ms"] for row in group),
                "decode_ms_min": min(row["decode_ms"] for row in group),
                "decode_ms_max": max(row["decode_ms"] for row in group),
                "peak_working_set_mib_median": statistics.median(row["peak_working_set_bytes"] for row in group)/2**20,
                "peak_private_commit_mib_median": statistics.median(row["peak_private_commit_bytes"] for row in group)/2**20,
                "working_set_before_mib_median": statistics.median(row["working_set_before_bytes"] for row in group)/2**20}
        for field in ("start_ms", "skip_ms", "consumed_bytes", "encoded_bytes", "output_bytes", "aligned_x", "aligned_width"):
            if field in group[0]:
                item[field+"_median"] = statistics.median(row[field] for row in group)
        summary.append(item)
    (OUT / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(f"JPEG measurements complete: {len(records)} fresh workers; {len(correctness)} ROI comparisons; source SHA-256 unchanged", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--photo", type=Path)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--edge-check", action="store_true")
    parser.add_argument("--pillow-worker", type=Path)
    parser.add_argument("--denominator", type=int, default=1)
    args = parser.parse_args()
    if args.pillow_worker:
        pillow_worker(args.pillow_worker, args.denominator)
        return
    OUT.mkdir(parents=True, exist_ok=True)
    if args.build:
        build()
    if args.edge_check:
        edge_checks()
    if args.photo:
        if not 1 <= args.repeats <= 5:
            parser.error("Repeat count must be between 1 and 5")
        run(args.photo.resolve(), args.repeats)
    elif not args.build and not args.edge_check:
        parser.error("Select --build, --photo and/or --edge-check")


if __name__ == "__main__":
    main()
