"""Sequential actual-JPEG corpus probe; never extracts an embedded RAW preview.

All evidence and temporary RGB dumps stay in a new target/preview-jpeg-corpus run.
Use --plan for the inventory and workload, or --build to build and then measure.
No application binaries, production dependencies, source images or old evidence
are modified. DNG and NEF decoding belong to the independent RAW experiment.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
from datetime import datetime, timezone
import hashlib
import io
import json
import mmap
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
import traceback

ROOT = Path(__file__).resolve().parents[2]
CORPUS = Path("E:/Documents/AI/corpus/3 этаж single/halo")
OUTPUT = ROOT / "target/preview-jpeg-corpus"
DENOMINATORS = (1, 2, 4, 8)
SIZES = (512, 1024)
HALO = 16  # Output pixels, clamped horizontally; correctness is checked, not assumed.
SOURCE_NAME = re.compile(r"(?P<scene>.+)_halo(?P<halo>0|24|48|96|256)$", re.IGNORECASE)


def digest(path):
    with Path(path).open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def write_json(path, value):
    # Every run has a unique directory, and no prior run is opened for writing.
    Path(path).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def jpeg_metadata(path):
    """Read marker headers only; do not invoke a decoder or read an EXIF thumbnail."""
    frame = None
    with path.open("rb") as handle:
        if handle.read(2) != b"\xff\xd8":
            raise ValueError(f"Not a JPEG codestream: {path}")
        while True:
            if handle.read(1) != b"\xff":
                raise ValueError(f"Invalid JPEG marker before first scan: {path}")
            marker_byte = handle.read(1)
            while marker_byte == b"\xff":
                marker_byte = handle.read(1)
            if not marker_byte:
                raise ValueError(f"Truncated JPEG header: {path}")
            marker = marker_byte[0]
            if marker in (0xDA, 0xD9):
                break
            if marker == 0x01 or 0xD0 <= marker <= 0xD8:
                continue
            length_bytes = handle.read(2)
            if len(length_bytes) != 2:
                raise ValueError(f"Truncated JPEG segment length: {path}")
            length = int.from_bytes(length_bytes, "big") - 2
            if length < 0:
                raise ValueError(f"Invalid JPEG segment length: {path}")
            # SOF markers exclude DHT, JPG and DAC. Payloads are at most 65533 bytes.
            if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
                payload = handle.read(length)
                if len(payload) < 6 or len(payload) != length or len(payload) != 6 + 3 * payload[5]:
                    raise ValueError(f"Invalid JPEG frame header: {path}")
                components = [{"id": payload[i], "h": payload[i + 1] >> 4,
                               "v": payload[i + 1] & 15, "quantization_table": payload[i + 2]}
                              for i in range(6, len(payload), 3)]
                if not components or any(not c["h"] or not c["v"] for c in components):
                    raise ValueError(f"Invalid JPEG component sampling: {path}")
                frame = {"sof_marker": f"FF{marker:02X}", "baseline": marker == 0xC0,
                         "progressive": marker in (0xC2, 0xC6, 0xCA, 0xCE),
                         "sample_precision_bits": payload[0],
                         "height": int.from_bytes(payload[1:3], "big"),
                         "width": int.from_bytes(payload[3:5], "big"), "components": components}
            else:
                handle.seek(length, os.SEEK_CUR)
    if frame is None or not frame["width"] or not frame["height"]:
        raise ValueError(f"No bounded JPEG frame dimensions before first scan: {path}")
    if frame["sof_marker"] not in ("FFC0", "FFC1", "FFC2") or frame["sample_precision_bits"] != 8:
        raise ValueError(f"Only 8-bit baseline/extended sequential/progressive JPEGs are supported: {path}")
    sampling = [(c["h"], c["v"]) for c in frame["components"]]
    frame["sampling_factors"] = ":".join(f"{h}x{v}" for h, v in sampling)
    frame["sampling_label"] = ({((1, 1), (1, 1), (1, 1)): "4:4:4",
                                ((2, 1), (1, 1), (1, 1)): "4:2:2",
                                ((2, 2), (1, 1), (1, 1)): "4:2:0"}.get(tuple(sampling),
                                "grayscale" if len(sampling) == 1 else "other; see component factors"))
    return frame


def inventory():
    files = sorted((p for p in CORPUS.iterdir() if p.is_file() and p.suffix.lower() in (".jpg", ".jpeg")),
                   key=lambda p: p.name.casefold())
    if len(files) != 30:
        raise ValueError(f"Expected all 30 actual JPEGs in {CORPUS}; found {len(files)}")
    sources = []
    scenes = defaultdict(set)
    for path in files:
        match = SOURCE_NAME.fullmatch(path.stem)
        if not match:
            raise ValueError(f"Unexpected corpus JPEG name: {path.name}")
        scene, halo = match["scene"], int(match["halo"])
        scenes[scene].add(halo)
        stat = path.stat()
        if not 0 < stat.st_size <= 512 * 2**20:
            raise ValueError(f"JPEG exceeds the native worker's 512 MiB input bound: {path}")
        sources.append({"id": path.stem, "path": str(path.resolve()), "scene": scene,
                        "source_halo": halo, "comprehensive": halo == 0,
                        "bytes": stat.st_size, "mtime_ns_before": stat.st_mtime_ns,
                        **jpeg_metadata(path)})
    if len(scenes) != 6 or any(halos != {0, 24, 48, 96, 256} for halos in scenes.values()):
        raise ValueError("Expected six scenes, each with source halo variants 0/24/48/96/256")
    return sources


def scaled_size(source, denominator):
    return tuple((source[key] + denominator - 1) // denominator for key in ("width", "height"))


def roi_cases(source, denominator):
    """Main centers on all 30; four-scale positions/aligned edges on six halo0 files."""
    width, height = scaled_size(source, denominator)
    positions = ["center"] if denominator in (1, 4) or source["comprehensive"] else []
    if source["comprehensive"]:
        positions.extend(("top", "bottom", "center-aligned"))
    cases, skipped = [], []
    for position in positions:
        for size in SIZES:
            rw, rh = min(size, width), min(size, height)
            x = (width - rw) // 2
            y = {"top": 0, "bottom": height - rh}.get(position, (height - rh) // 2)
            if position == "center-aligned":
                # Deliberately exercise an iMCU-aligned fancy-upsampling boundary.
                unit = max(c["h"] for c in source["components"]) * (8 // denominator)
                x = x // unit * unit
            case = {"case": f"{position}-{size}", "position": position,
                    "requested_edge": size, "requested_rectangle": [x, y, rw, rh],
                    "clamped_to_image": (rw, rh) != (size, size)}
            if rw == width and rh == height:
                skipped.append({**case, "reason": "Requested ROI covers the entire scaled image; not partial output"})
            elif padded_rectangle(case["requested_rectangle"], width, HALO) == [0, 0, width, height]:
                skipped.append({**case, "reason": "Horizontal padding would cover the entire scaled image; not partial output"})
            else:
                cases.append(case)
    return cases, skipped


def padded_rectangle(rectangle, width, halo):
    x, y, rw, rh = rectangle
    left, right = max(0, x - halo), min(width, x + rw + halo)
    return [left, y, right - left, rh]


def make_plan(sources, repeats, pillow):
    roi_count, omissions = 0, []
    max_roi, max_full, total_full_dumps, total_roi_dumps = 0, 0, 0, 0
    for source in sources:
        for denominator in DENOMINATORS:
            width, height = scaled_size(source, denominator)
            full_bytes = width * height * 3
            max_full = max(max_full, full_bytes)
            total_full_dumps += full_bytes
            cases, skipped = roi_cases(source, denominator)
            roi_count += len(cases)
            omissions.extend({"source": source["id"], "denominator": denominator, **case} for case in skipped)
            for case in cases:
                for halo in (0, HALO):
                    _, _, rw, rh = padded_rectangle(case["requested_rectangle"], width, halo)
                    max_roi = max(max_roi, rw * rh * 3)
                    total_roi_dumps += rw * rh * 3
    full_workers = len(sources) * len(DENOMINATORS) * repeats
    return {"actual_jpeg_sources": len(sources), "comprehensive_sources": [s["id"] for s in sources if s["comprehensive"]],
            "repeats": repeats, "denominators": DENOMINATORS, "native_full_workers": full_workers,
            "pillow_full_workers": full_workers if pillow else 0,
            "native_padded_roi_workers": roi_count * repeats,
            "native_unpadded_correctness_workers": roi_count,
            "total_workers": full_workers * (2 if pillow else 1) + roi_count * (repeats + 1),
            "correctness_comparisons": roi_count * 2, "skipped_roi_cases": omissions,
            "source_bytes_per_hash_pass": sum(s["bytes"] for s in sources),
            "maximum_full_rgb_dump_bytes": max_full, "maximum_roi_rgb_dump_bytes": max_roi,
            "scratch_rgb_upper_bound_bytes": max_full + max_roi,
            "total_rgb_dump_bytes_written": total_full_dumps + total_roi_dumps,
            "disk_reserve_bytes": max_full + max_roi + 64 * 2**20,
            "execution": "Strictly sequential fresh worker processes; at most one full-scale dump and one ROI dump exist"}


def memory():
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (key, ctypes.c_size_t) for key in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
                "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage", "PrivateUsage")]

    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    if not psapi.GetProcessMemoryInfo(kernel.GetCurrentProcess(), ctypes.byref(counters), counters.cb):
        raise ctypes.WinError(ctypes.get_last_error())
    return {"working_set_bytes": counters.WorkingSetSize, "private_commit_bytes": counters.PrivateUsage,
            "peak_working_set_bytes": counters.PeakWorkingSetSize,
            "peak_private_commit_bytes": counters.PeakPagefileUsage}


def pillow_worker(path, denominator):
    from PIL import Image, __version__ as pillow_version, features

    snapshots = {"process_before_input": memory()}
    begin = time.perf_counter()
    encoded = Path(path).read_bytes()
    read_end = time.perf_counter()
    snapshots["input_read"] = memory()
    header_begin = time.perf_counter()
    stream = io.BytesIO(encoded)
    image = Image.open(stream)
    original = image.size
    draft = image.draft("RGB", tuple(max(1, side // denominator) for side in original))
    header_end = time.perf_counter()
    snapshots["header_draft"] = memory()
    load_begin = time.perf_counter()
    image.load()
    load_end = time.perf_counter()
    snapshots["output_decoded"] = memory()
    if image.mode != "RGB":
        raise ValueError(f"Pillow full-image comparison path expects RGB, got {image.mode}")
    result = {"backend": "Pillow/libjpeg-turbo " + str(features.version_feature("libjpeg_turbo")),
              "pillow_version": pillow_version, "libjpeg_turbo_enabled": features.check_feature("libjpeg_turbo"),
              "denominator": denominator, "source_width": original[0], "source_height": original[1],
              "scaled_width": image.width, "scaled_height": image.height,
              "width": image.width, "height": image.height, "draft": draft,
              "encoded_bytes": len(encoded), "output_rgb_bytes": image.width * image.height * 3,
              "input_read_and_allocation_ms": (read_end - begin) * 1000,
              "header_ms": (header_end - header_begin) * 1000,
              "load_ms": (load_end - load_begin) * 1000,
              "decode_ms": (load_end - header_begin) * 1000,
              "decode_stage_sum_ms": (header_end - header_begin + load_end - load_begin) * 1000,
              "working_set_before_bytes": snapshots["input_read"]["working_set_bytes"],
              **{key: snapshots["output_decoded"][key] for key in ("peak_working_set_bytes", "peak_private_commit_bytes")},
              "memory": snapshots,
              "allocation_scope": "Pillow load allocates internally; no separate decoder-allocation observation or true ROI API here"}
    result["read_allocate_decode_stage_sum_ms"] = result["input_read_and_allocation_ms"] + result["decode_stage_sum_ms"]
    image.close()
    print(json.dumps(result))


def worker(command, timeout):
    started = time.perf_counter()
    result = subprocess.run([str(item) for item in command], capture_output=True, encoding="utf-8", timeout=timeout)
    elapsed = (time.perf_counter() - started) * 1000
    if result.returncode:
        raise RuntimeError(f"Worker failed ({result.returncode}): {command!r}\n{result.stderr}\n{result.stdout}")
    try:
        record = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"Worker did not return one JSON record: {result.stdout!r}\n{result.stderr}") from error
    record["worker_wall_ms"] = elapsed
    record["worker_stderr"] = result.stderr
    return record


def native(probe, source, denominator, timeout, rectangle=None, dump=None):
    command = [probe, source["path"], denominator, "roi" if rectangle is not None else "full"]
    if rectangle is not None:
        command.extend(rectangle)
    if dump is not None:
        command.append(dump)
    record = worker(command, timeout)
    if record.get("probe_schema_version") != 2:
        raise RuntimeError("Native probe lacks schema-v2 stage instrumentation; use this runner's --build in a fresh run")
    if record.get("backend") != "libjpeg-turbo 2.1.5.1 scalar" or record["denominator"] != denominator:
        raise RuntimeError("The selected native probe is not the expected scalar libjpeg-turbo 2.1.5.1 backend/scale")
    if (record["source_width"], record["source_height"]) != (source["width"], source["height"]):
        raise RuntimeError("Native source dimensions disagree with the marker inventory")
    if record["progressive"] != source["progressive"] or record["encoded_bytes"] != source["bytes"]:
        raise RuntimeError("Native source process/size disagrees with the marker inventory")
    expected = scaled_size(source, denominator)
    if (record["scaled_width"], record["scaled_height"]) != expected:
        raise RuntimeError(f"Native output dimensions disagree with JPEG header: {source['id']} at 1/{denominator}")
    actual_rect = [record[key] for key in ("x", "y", "width", "height")]
    if actual_rect != (rectangle if rectangle is not None else [0, 0, *expected]):
        raise RuntimeError(f"Native output rectangle disagrees with request: {actual_rect}")
    if record["output_bytes"] != record["width"] * record["height"] * 3:
        raise RuntimeError("Native packed-RGB output byte count disagrees with dimensions")
    if dump is not None and dump.stat().st_size != record["output_bytes"]:
        raise RuntimeError("Native RGB dump size disagrees with reported output")
    record["input_stage_sum_ms"] = sum(record[key] for key in ("input_open_ms", "input_allocation_ms", "input_read_ms"))
    record["read_allocate_decode_stage_sum_ms"] = record["input_stage_sum_ms"] + record["decode_stage_sum_ms"]
    record["rgb_dump_written"] = dump is not None
    return record


def compare(full_path, roi_path, full_size, requested, decoded):
    """Bounded chunk comparison; reference and ROI are from the SAME native scale."""
    import numpy as np

    width, height = full_size
    x, y, rw, rh = requested
    dx, dy, dw, dh = decoded
    if not (dx <= x and dy <= y and x + rw <= dx + dw and y + rh <= dy + dh):
        raise ValueError("Requested rectangle is not contained in partial decoded output")
    sums = {"absolute_error_sum": 0, "different_channel_values": 0,
            "interior_different_channel_values_excluding_two_columns": 0,
            "max_absolute_error": 0, "interior_max_error_excluding_two_columns": 0}
    full_hash, roi_hash = hashlib.sha256(), hashlib.sha256()
    with full_path.open("rb") as full_handle, roi_path.open("rb") as roi_handle:
        with mmap.mmap(full_handle.fileno(), 0, access=mmap.ACCESS_READ) as full_map, \
                mmap.mmap(roi_handle.fileno(), 0, access=mmap.ACCESS_READ) as roi_map:
            full = np.ndarray((height, width, 3), dtype=np.uint8, buffer=full_map)
            partial = np.ndarray((dh, dw, 3), dtype=np.uint8, buffer=roi_map)
            try:
                for row in range(0, rh, 64):
                    count = min(64, rh - row)
                    reference = full[y + row:y + row + count, x:x + rw].copy()
                    actual = partial[y - dy + row:y - dy + row + count, x - dx:x - dx + rw].copy()
                    full_hash.update(reference.tobytes())
                    roi_hash.update(actual.tobytes())
                    delta = np.abs(reference.astype(np.int16) - actual.astype(np.int16))
                    sums["absolute_error_sum"] += int(delta.sum())
                    sums["different_channel_values"] += int(np.count_nonzero(delta))
                    sums["max_absolute_error"] = max(sums["max_absolute_error"], int(delta.max()))
                    if rw > 4:
                        interior = delta[:, 2:-2]
                        key = "interior_different_channel_values_excluding_two_columns"
                        sums[key] += int(np.count_nonzero(interior))
                        key = "interior_max_error_excluding_two_columns"
                        sums[key] = max(sums[key], int(interior.max()))
            finally:
                # No exported ndarray views may survive closing the Windows mappings.
                del full, partial
    return {**sums, "mean_absolute_error": sums["absolute_error_sum"] / (rw * rh * 3),
            "compared_channel_values": rw * rh * 3, "full_crop_sha256": full_hash.hexdigest(),
            "roi_trim_sha256": roi_hash.hexdigest(), "byte_identical": full_hash.digest() == roi_hash.digest(),
            "reference": "same native backend, same denominator, full scaled output; no cross-scale resize or Pillow pixel reference"}


def summarize(records):
    groups = defaultdict(list)
    for record in records:
        if record["purpose"] == "measurement":
            groups[(record["source"], record["backend"], record["denominator"], record["case"], record["horizontal_halo"])].append(record)
    result = []
    for (source, backend, denominator, case, halo), rows in groups.items():
        item = {"source": source, "backend": backend, "denominator": denominator,
                "case": case, "horizontal_halo": halo, "samples": len(rows)}
        for key in rows[0]:
            if key.endswith("_ms") or key in ("encoded_bytes", "consumed_bytes", "output_bytes", "output_rgb_bytes",
                                              "row_scratch_bytes", "aligned_x", "aligned_width", "width", "height",
                                              "peak_working_set_bytes", "peak_private_commit_bytes"):
                values = [row[key] for row in rows]
                item[key] = {"min": min(values), "median": statistics.median(values), "max": max(values)}
        item["memory_boundary_medians_bytes"] = {
            stage: {key: statistics.median(row["memory"][stage][key] for row in rows) for key in values}
            for stage, values in rows[0]["memory"].items()}
        item["worker_wall_scope"] = "Includes process startup and, on first native repeat, RGB dump I/O; not a pure decode comparison"
        result.append(item)
    return result


class Evidence:
    def __init__(self, directory):
        self.records = []
        self.correctness = []
        self.measurements_file = (directory / "measurements.jsonl").open("x", encoding="utf-8")
        self.correctness_file = (directory / "correctness.jsonl").open("x", encoding="utf-8")

    def add(self, record, correctness=False):
        target = self.correctness if correctness else self.records
        handle = self.correctness_file if correctness else self.measurements_file
        target.append(record)
        handle.write(json.dumps(record, ensure_ascii=False) + "\n")
        handle.flush()

    def close(self):
        self.measurements_file.close()
        self.correctness_file.close()


def benchmark(probe, sources, args, directory, evidence):
    scratch = directory / "scratch"
    scratch.mkdir()
    full_dump, roi_dump = scratch / "reference-full.rgb", scratch / "partial-roi.rgb"
    for index, source in enumerate(sources, 1):
        for denominator in DENOMINATORS:
            full_size = scaled_size(source, denominator)
            base = {"source": source["id"], "scene": source["scene"], "source_halo": source["source_halo"],
                    "comprehensive": source["comprehensive"], "denominator": denominator}
            try:
                for repeat in range(args.repeats):
                    record = native(probe, source, denominator, args.worker_timeout,
                                    dump=full_dump if repeat == 0 else None)
                    evidence.add({**base, "case": "full", "repeat": repeat, "purpose": "measurement",
                                  "horizontal_halo": 0, **record})
                    if not args.native_only:
                        record = worker([sys.executable, Path(__file__).resolve(), "--pillow-worker", source["path"],
                                         "--denominator", denominator], args.worker_timeout)
                        if (record["scaled_width"], record["scaled_height"]) != full_size:
                            raise RuntimeError(f"Pillow draft selected unexpected dimensions for {source['id']}")
                        evidence.add({**base, "case": "full", "repeat": repeat, "purpose": "measurement",
                                      "horizontal_halo": 0, **record})
                cases, _ = roi_cases(source, denominator)
                for case in cases:
                    requested = case["requested_rectangle"]
                    # Padded ROI is the measured, exactness-checked integration candidate.
                    # Unpadded output is a diagnostic, never folded into 3-repeat timings.
                    for halo in (HALO, 0):
                        decoded = padded_rectangle(requested, full_size[0], halo)
                        if decoded == [0, 0, *full_size]:
                            raise RuntimeError("Padding would turn an alleged partial decode into full RGB output")
                        count = args.repeats if halo else 1
                        for repeat in range(count):
                            record = native(probe, source, denominator, args.worker_timeout, decoded,
                                            roi_dump if repeat == 0 else None)
                            evidence.add({**base, **case, "decoded_rectangle": decoded, "repeat": repeat,
                                          "purpose": "measurement" if halo else "unpadded-correctness-diagnostic",
                                          "horizontal_halo": halo, **record})
                            if repeat == 0:
                                compared = compare(full_dump, roi_dump, full_size, requested, decoded)
                                evidence.add({**base, **case, "decoded_rectangle": decoded,
                                              "horizontal_halo": halo, "padded_exactness_required": bool(halo),
                                              **compared}, correctness=True)
                                roi_dump.unlink()
            finally:
                # Only files created in this new run's scratch directory are removed.
                full_dump.unlink(missing_ok=True)
                roi_dump.unlink(missing_ok=True)
            print(f"[{index:02d}/30] {source['id']} 1/{denominator} complete", flush=True)


def run(args, sources, plan):
    import numpy
    if os.name != "nt":
        raise ValueError("This probe records Windows process memory and requires Windows")
    run_id = args.run_id or datetime.now(timezone.utc).strftime("actual-jpeg-%Y%m%dT%H%M%S-%fZ")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", run_id):
        raise ValueError("Run ID must be 1-80 ASCII letters, digits, underscores, periods or hyphens, starting with a letter/digit")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    directory = OUTPUT / run_id
    directory.mkdir(exist_ok=False)
    write_json(directory / "plan.json", plan)
    manifest = {"status": "preparing", "run_id": run_id, "started_utc": datetime.now(timezone.utc).isoformat(),
                "platform": platform.platform(), "python": sys.version, "numpy": numpy.__version__,
                "corpus": str(CORPUS), "sources": sources, "plan": plan,
                "runner_sha256": digest(__file__), "native_source_sha256": digest(Path(__file__).with_name("jpeg_probe.cpp")),
                "build_recipe_sha256": digest(Path(__file__).with_name("jpeg_probe.py")),
                "notes": ["Actual .jpg codestreams only; no DNG/NEF embedded JPEG is an actual RAW decode",
                          "All workers are sequential; no explicit cold-cache eviction or warmup; record execution order",
                          "Native phase timers exclude inter-stage memory queries; decode_ms is instrumentation-inclusive wall time",
                          "Parent worker_wall_ms includes startup/capture and optional dump I/O",
                          "Pillow cannot expose separate internal output allocation; load_ms is combined allocation/decode",
                          "Working set and private commit are process counters, not codec-only allocations",
                          "Source hash passes touch the cache; results are not cold-storage latency measurements"]}
    write_json(directory / "manifest.json", manifest)
    evidence = Evidence(directory)
    failure = None
    started = time.perf_counter()
    try:
        if args.build:
            # Reuse the existing build recipe, not its output directory or evidence.
            import jpeg_probe
            jpeg_probe.OUT = directory / "native"
            jpeg_probe.OUT.mkdir()
            jpeg_probe.build()
            probe = jpeg_probe.OUT / "jpeg_probe.exe"
        else:
            probe = args.probe.resolve()
        if not probe.is_file():
            raise ValueError(f"Native probe is unavailable: {probe}")
        manifest["native_probe"] = {"path": str(probe), "sha256": digest(probe), "expected_schema": 2}
        if shutil.disk_usage(directory).free < plan["disk_reserve_bytes"]:
            raise ValueError("Insufficient free space for the bounded full/ROI RGB scratch files and evidence reserve")
        for source in sources:
            source["sha256_before"] = digest(source["path"])
        manifest["status"] = "running"
        write_json(directory / "manifest.json", manifest)
        benchmark(probe, sources, args, directory, evidence)
        if len(evidence.records) != plan["total_workers"] or len(evidence.correctness) != plan["correctness_comparisons"]:
            raise RuntimeError("Completed evidence counts disagree with the inventory plan")
        padded_failures = [row for row in evidence.correctness if row["padded_exactness_required"] and not row["byte_identical"]]
        if padded_failures:
            raise RuntimeError(f"{len(padded_failures)} padded same-backend ROI comparisons are not byte-identical; inspect correctness.jsonl")
    except BaseException as error:
        failure = error
        write_json(directory / "failure.json", {"error": str(error), "type": type(error).__name__,
                                               "traceback": traceback.format_exc()})
    finally:
        identity_failures = []
        for source in sources:
            if "sha256_before" not in source:
                continue
            try:
                path = Path(source["path"])
                source["sha256_after"] = digest(path)
                source["mtime_ns_after"] = path.stat().st_mtime_ns
                source["bytes_after"] = path.stat().st_size
                source["identity_unchanged"] = source["sha256_before"] == source["sha256_after"] and source["bytes"] == source["bytes_after"]
                if not source["identity_unchanged"]:
                    identity_failures.append(source["id"])
            except OSError as error:
                source["identity_check_error"] = str(error)
                identity_failures.append(source["id"])
        manifest.update({"status": "failed" if failure or identity_failures else "complete",
                         "finished_utc": datetime.now(timezone.utc).isoformat(),
                         "elapsed_seconds_including_build_and_identity_checks": time.perf_counter() - started,
                         "measurement_records": len(evidence.records), "correctness_records": len(evidence.correctness),
                         "identity_failures": identity_failures,
                         "all_source_hashes_checked": all("sha256_after" in source for source in sources)})
        evidence.close()
        write_json(directory / "manifest.json", manifest)
        write_json(directory / "summary.json", summarize(evidence.records))
        write_json(directory / "correctness-summary.json", {
            "comparisons": len(evidence.correctness),
            "padded_nonidentical": [row for row in evidence.correctness if row["padded_exactness_required"] and not row["byte_identical"]],
            "unpadded_nonidentical": [row for row in evidence.correctness if not row["padded_exactness_required"] and not row["byte_identical"]],
            "note": "Unpadded differences are observations, not accepted tolerance; padded output must be byte-identical"})
    print(f"Evidence: {directory}", flush=True)
    if failure:
        raise failure
    if manifest["identity_failures"]:
        raise RuntimeError(f"Source identity verification failed: {manifest['identity_failures']}")
    print(f"Complete: {len(evidence.records)} workers, {len(evidence.correctness)} comparisons, all 30 JPEG hashes unchanged", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", action="store_true", help="Read only JPEG headers and print the planned workload; no decodes/build")
    parser.add_argument("--build", action="store_true", help="Build the instrumented standalone probe inside this new run before measuring")
    parser.add_argument("--probe", type=Path, help="Reuse an explicitly selected schema-v2 probe instead of building")
    parser.add_argument("--run-id", help="New evidence subdirectory; existing runs are never overwritten")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--native-only", action="store_true", help="Omit the independent Pillow full-image timings")
    parser.add_argument("--worker-timeout", type=float, default=120, help="Timeout in seconds for each sequential worker")
    parser.add_argument("--pillow-worker", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--denominator", type=int, choices=DENOMINATORS, default=1, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.pillow_worker:
        pillow_worker(args.pillow_worker, args.denominator)
        return
    if args.repeats != 3:
        parser.error("The actual-corpus evidence protocol requires exactly three measurement repeats")
    if args.worker_timeout <= 0:
        parser.error("Worker timeout must be positive")
    if args.build and args.probe:
        parser.error("Select either --build or --probe, not both")
    if not args.plan and not args.build and not args.probe:
        parser.error("Select --plan, or supply --build / --probe for a sequential run")
    sources = inventory()
    plan = make_plan(sources, args.repeats, not args.native_only)
    if args.plan:
        print(json.dumps({"corpus": str(CORPUS), "sources": sources, **plan}, ensure_ascii=False, indent=2))
        return
    run(args, sources, plan)


if __name__ == "__main__":
    main()
