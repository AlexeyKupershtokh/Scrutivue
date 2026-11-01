"""Build/run isolated pinned-LibRaw NEF full, reduced and region experiments."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "target/preview-nef-corpus"


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def build():
    spec = importlib.util.spec_from_file_location("experiment_builder", ROOT / "scripts/build_dng_experiments.py")
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
    env = builder.msvc_environment()
    pin = json.loads((ROOT / "config/libraw.json").read_text())
    assert (ROOT / "vendor/libraw/.diff-pin").read_text().strip() == pin["version"]
    import shutil
    compiler = shutil.which("cl.exe", path=env.get("Path", env.get("PATH")))
    raw = builder.newest("target/debug/build/scrutivue-*/out/diff_raw.lib")
    zlib = builder.newest("target/debug/build/libz-sys-*/out/lib/z.lib")
    output = ROOT / "target/preview-nef-build"
    output.mkdir(parents=True, exist_ok=True)
    command = [compiler, "/nologo", "/std:c++17", "/O2", "/EHsc", "/MD", "/DLIBRAW_NODLL",
               "/D_CRT_SECURE_NO_WARNINGS", "/I" + str(ROOT / "vendor/libraw"),
               str(ROOT / "experiments/preview/nef_libraw.cpp"), str(ROOT / "experiments/preview/nef_stripes.cpp"),
               "/Fo" + str(output) + os.sep, "/Fe" + str(output / "nef_libraw.exe"),
               "/link", str(raw), str(zlib), "ws2_32.lib", "psapi.lib"]
    timer = time.perf_counter()
    process = subprocess.run(command, env=env, capture_output=True, text=True)
    save(output / "build.json", {"command": command, "returncode": process.returncode,
         "wall_ms": (time.perf_counter() - timer) * 1000, "stdout": process.stdout,
         "stderr": process.stderr, "production_libraw": pin, "static_archive_sha256": digest(raw)})
    print(process.stdout, process.stderr)
    process.check_returncode()


def invoke(executable, mode, path, *extra):
    timer = time.perf_counter()
    process = subprocess.run([str(executable), mode, str(path), *[str(p) for p in extra]],
                             capture_output=True, text=True, encoding="utf-8", timeout=120)
    if process.returncode:
        raise RuntimeError(process.stderr + process.stdout)
    result = json.loads(process.stdout.strip().splitlines()[-1])
    result["external_wall_ms"] = (time.perf_counter() - timer) * 1000
    return result


def run(args):
    executable = ROOT / "target/preview-nef-build/nef_libraw.exe"
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    paths = sorted(path for path in args.directory.iterdir() if path.suffix.lower() == ".nef")
    assert len(paths) == 6, f"Expected six actual corpus NEFs, found {len(paths)}"
    identities = {path.name: {"sha256": digest(path), "bytes": path.stat().st_size,
                  "mtime_ns": path.stat().st_mtime_ns} for path in paths}
    save(out / "manifest.json", {"status": "running", "sources": identities,
         "repeats": args.repeats, "sequential_processes": True, "no_embedded_preview": True,
         "libraw_pin": json.loads((ROOT / "config/libraw.json").read_text())})
    records = []

    def record(path, mode, label, repeat, *extra):
        result = invoke(executable, mode, path, *extra)
        result.update(source_name=path.name, case=label, repeat=repeat)
        records.append(result)
        save(out / "measurements.json", records)
        return result

    for path in paths:
        metadata = record(path, "metadata", "metadata", 0)
        record(path, "stripe-self-test", "stripe-negative-tests", 0)
        raw = out / f"{path.stem}-full-cfa.u16"
        full = out / f"{path.stem}-full-rgb.u16"
        half = out / f"{path.stem}-half-rgb.u16"
        fixed = out / f"{path.stem}-fixed-full-rgb.u16"
        for reference in [raw, full, half, fixed]:
            assert not reference.exists(), f"Use a new output directory: {reference}"
        record(path, "raw-full", "raw-full-reference", 0, "--output", raw)
        record(path, "full", "full-reference", 0, "--output", full)
        record(path, "half", "half-reference", 0, "--output", half)
        record(path, "full", "fixed-full-reference", 0, "--fixed-maximum", "--output", fixed)
        width, height = int(metadata["active_width"]), int(metadata["active_height"])
        rw, rh = int(metadata["raw_width"]), int(metadata["raw_height"])
        center = [((width - 512) // 2) & ~1, ((height - 512) // 2) & ~1, 512, 512]
        raw_center = [((rw - 512) // 2) & ~1, ((rh - 512) // 2) & ~1, 512, 512]
        stripe_cases = {"center": raw_center, "top": [raw_center[0], 0, 512, 512],
                        "bottom": [raw_center[0], rh - 512, 512, 512],
                        "seam": [raw_center[0] + 1, 63, 127, 130]}
        for repeat in range(args.repeats):
            record(path, "full", "full", repeat)
            record(path, "half", "half", repeat)
            record(path, "quarter", "quarter-half-then-resize", repeat)
            resumed = record(path, "resume", "half-then-full-reuse", repeat, "--reference", full)
            assert resumed["different_samples"] == 0, resumed
            for halo in [0, 16]:
                record(path, "roi", f"roi-halo{halo}", repeat, "--roi", *center,
                       "--halo", halo, "--reference", full)
            fixed_roi = record(path, "roi", "roi-fixed-maximum-halo16", repeat, "--roi", *center,
                               "--halo", 16, "--fixed-maximum", "--reference", fixed)
            assert fixed_roi["different_samples"] == 0, fixed_roi
            half_roi = record(path, "half-roi", "half-roi-halo16", repeat, "--roi", *center,
                              "--halo", 16, "--reference", half)
            for label, region in stripe_cases.items():
                result = record(path, "stripe-roi", f"stripe-roi-{label}", repeat, "--roi", *region, "--reference", raw)
                assert result["different_samples"] == 0, result
        after = {"sha256": digest(path), "bytes": path.stat().st_size, "mtime_ns": path.stat().st_mtime_ns}
        assert after == identities[path.name], f"Source changed: {path}"
        print(f"Completed {path.name}: full, reduced, cropbox, reuse and true stripe regions", flush=True)
    summary = []
    for name, case in sorted({(r["source_name"], r["case"]) for r in records}):
        values = [r for r in records if r["source_name"] == name and r["case"] == case]
        row = {"source_name": name, "case": case, "n": len(values)}
        for key in ["ready_ms", "open_map_ms", "metadata_ms", "unpack_ms", "process_ms", "bitmap_ms",
                    "half_ready_ms", "resume_full_ms", "header_index_ms", "output_internal_alloc_ms",
                    "entropy_idwt_ms", "bayer_ms", "stripe_total_ms", "post_half_box_resize_ms"]:
            if key in values[0]:
                row[key] = statistics.median(r[key] for r in values)
        if "ready_memory" in values[0]:
            row["peak_ws_mib"] = statistics.median(r["ready_memory"]["peak_ws_bytes"] / 2**20 for r in values)
        for key in ["different_samples", "maximum_absolute_error", "mean_absolute_error", "selected_stripes", "total_stripes"]:
            if key in values[0]:
                row[key] = max(r[key] for r in values)
        summary.append(row)
    save(out / "summary.json", summary)
    save(out / "manifest.json", {"status": "complete", "sources": identities, "source_hashes_unchanged": True,
         "repeats": args.repeats, "sequential_processes": True, "no_embedded_preview": True,
         "libraw_pin": json.loads((ROOT / "config/libraw.json").read_text()),
         "cache_scope": "hash-warmed Windows file cache; mmap demand faults inside relevant work stages"})
    print(json.dumps({"success": True, "records": len(records), "output": str(out)}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--output", type=Path, default=OUT)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10:
        parser.error("Expected 1..10 repeats")
    if args.build:
        build()
    if args.directory:
        run(args)
    elif not args.build:
        parser.error("Specify --build and/or --directory")


if __name__ == "__main__":
    main()
