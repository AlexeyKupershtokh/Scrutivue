"""Prepare or run sequential raw-sample ROI/progression checks on the halo corpus.

Without --run this prints a metadata-only plan and starts no decoder. This never
builds, develops display RGB, decodes an embedded preview, or changes a source.
Only Python's standard library and the two already-built experiment EXEs are used.
"""
from __future__ import annotations

import argparse
from array import array
from collections import Counter, defaultdict
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
OUTPUT_ROOT = ROOT / "target/preview-dng-corpus"
CORPUS = Path("E:/Documents/AI/corpus/3 этаж single/halo")
LIBRARY = Path("E:/Documents/AI/REA_DXO/pipeline-graph/resources/native/libjxl-0.12.0/jxl.dll")
PREVIEW = ROOT / "target/preview-experiments/preview_capabilities.exe"
FULL = ROOT / "target/dng-jxl-experiments/dng_jxl_bench.exe"
HALOS = (0, 24, 48, 96, 256)
MODES = ("dng-roi-native", "dng-roi-oxide", "dng-roi-oxide-crop")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def save(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def metadata(path):
    stat = path.stat()
    return {"bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns,
            "device": stat.st_dev, "file_id": stat.st_ino}


def identity(path):
    started = time.perf_counter()
    before = metadata(path)
    with path.open("rb") as handle:
        sha256 = hashlib.file_digest(handle, "sha256").hexdigest()
    after = metadata(path)
    require(before == after, f"Source metadata changed while hashing: {path}")
    return {"path": str(path), "metadata": after, "sha256": sha256,
            "identity_wall_ms": (time.perf_counter() - started) * 1000}


def discover(corpus, representative_halo):
    require(corpus.is_dir(), f"Missing corpus directory: {corpus}")
    pairs = defaultdict(dict)
    for path in corpus.iterdir():
        extension = path.suffix.lower()
        if extension not in (".dng", ".jpg", ".jpeg"):
            continue
        require(path.is_file(), f"Expected an input file: {path}")
        match = re.fullmatch(r"(.+)_halo(\d+)", path.stem, flags=re.IGNORECASE)
        require(match is not None, f"Unexpected corpus filename: {path.name}")
        scene, halo = match.group(1), int(match.group(2))
        require(halo in HALOS, f"Unexpected halo value: {path.name}")
        kind = "dng" if extension == ".dng" else "jpg"
        require(kind not in pairs[scene, halo], f"Duplicate {kind} input: {path.name}")
        pairs[scene, halo][kind] = path.resolve()
    scenes = sorted({scene for scene, _ in pairs})
    require(len(scenes) == 6 and len(pairs) == 30,
            f"Expected six scenes and 30 pairs; found {len(scenes)} scenes, {len(pairs)} pairs")
    inputs = []
    for scene_index, scene in enumerate(scenes):
        for halo in HALOS:
            pair = pairs.get((scene, halo), {})
            require(set(pair) == {"dng", "jpg"}, f"Missing DNG/JPG pair for {scene}, halo {halo}")
            inputs.append({"id": f"scene-{scene_index:02d}-halo-{halo:03d}", "scene": scene,
                           "halo": halo, "representative": halo == representative_halo, **pair})
    return inputs


def checked_layout(layout):
    for key in ("width", "height", "tile_width", "tile_height", "channels", "tiles", "sample_bytes"):
        require(type(layout.get(key)) is int and layout[key] > 0, f"Invalid layout {key}: {layout}")
    width, height = layout["width"], layout["height"]
    nx = (width + layout["tile_width"] - 1) // layout["tile_width"]
    ny = (height + layout["tile_height"] - 1) // layout["tile_height"]
    require(layout["tiles"] == nx * ny, f"Tile grid mismatch: {layout}")
    require(layout["channels"] in (1, 3) and type(layout.get("little_endian")) is bool,
            f"Unsupported sample layout: {layout}")
    require(layout["sample_bytes"] == width * height * layout["channels"] * 2,
            f"Sample byte count mismatch: {layout}")
    require(layout["sample_bytes"] <= 1024**3, "Reference canvas exceeds the EXE's 1 GiB bound")
    return nx, ny


def regions(layout):
    width, height = layout["width"], layout["height"]

    def centered(size):
        w, h = min(size, width), min(size, height)
        return [(width - w) // 2, (height - h) // 2, w, h]

    crossing = centered(257)
    for axis, (extent, tile_extent) in enumerate(((width, layout["tile_width"]),
                                                (height, layout["tile_height"]))):
        # Prefer an internal tile boundary near the center; otherwise use center.
        boundary = (extent // 2 // tile_extent) * tile_extent
        if boundary == 0 and tile_extent < extent:
            boundary = tile_extent
        if 0 < boundary < extent:
            crossing[axis] = max(0, min(extent - crossing[axis + 2],
                                        boundary - crossing[axis + 2] // 2))
    edge_w, edge_h = min(80, width), min(60, height)
    return {"center512": centered(512), "top_left128": [0, 0, min(128, width), min(128, height)],
            "tile_boundary257": crossing, "center1024": centered(1024),
            "bottom_right80x60": [width - edge_w, height - edge_h, edge_w, edge_h]}


def selected_indices(layout, roi):
    x, y, width, height = roi
    tw, th = layout["tile_width"], layout["tile_height"]
    nx, _ = checked_layout(layout)
    return [ty * nx + tx for ty in range(y // th, (y + height + th - 1) // th)
            for tx in range(x // tw, (x + width + tw - 1) // tw)]


def reference_roi_hash(path, layout, roi):
    """Hash only the requested rows, in the preview EXE's native-u16 byte order."""
    x, y, width, height = roi
    stride = layout["width"] * layout["channels"] * 2
    row_bytes = width * layout["channels"] * 2
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for row in range(height):
            handle.seek((y + row) * stride + x * layout["channels"] * 2)
            data = handle.read(row_bytes)
            require(len(data) == row_bytes, f"Short reference row: {path}")
            if layout["little_endian"] != (sys.byteorder == "little"):
                values = array("H")
                values.frombytes(data)
                values.byteswap()
                data = values.tobytes()
            digest.update(data)
    return digest.hexdigest()


def numeric_metrics(value, prefix=""):
    result = {}
    for key, item in value.items():
        name = f"{prefix}.{key}" if prefix else key
        if isinstance(item, dict):
            result.update(numeric_metrics(item, name))
        elif type(item) in (float, int) and key.endswith(("_ms", "_ns", "_bytes")):
            result[name] = item
    return result


def summarize(records):
    groups = defaultdict(list)
    for record in records:
        if "result" in record:
            groups[record["group"]].append(record)
    summary = []
    for group, rows in sorted(groups.items()):
        metrics = defaultdict(list)
        for row in rows:
            for name, value in numeric_metrics({"external_wall_ms": row["external_wall_ms"],
                                                "derived": row.get("derived", {}),
                                                "result": row["result"]}).items():
                metrics[name].append(value)
        summary.append({"group": group, "n": len(rows), "metrics": {
            key: {"n": len(values), "median": statistics.median(values),
                  "min": min(values), "max": max(values)} for key, values in sorted(metrics.items())}})
    return summary


def workload(args, inputs):
    representatives = sum(item["representative"] for item in inputs)
    native_full = len(inputs) - representatives + representatives * args.full_repeats
    oxide_full = representatives * args.full_repeats
    if args.oxide_full_scope == "all":
        oxide_full += len(inputs) - representatives
    progressive_inputs = len(inputs) if args.progressive_scope == "all" else representatives
    # Tile 0 extraction supplies metadata on every input and is reused for probing.
    extractions = len(inputs) + 2 * progressive_inputs
    progression = 3 * progressive_inputs
    roi = len(inputs) * 5 * len(MODES) * args.repeats
    return {"dng_inputs": len(inputs), "jpg_inventory_only": len(inputs), "scenes": representatives,
            "roi_cases_per_input": 5, "roi_processes": roi, "native_full_processes": native_full,
            "oxide_full_processes": oxide_full, "tile_extraction_processes": extractions,
            "progressive_processes": progression,
            "total_processes": roi + native_full + oxide_full + extractions + progression,
            "concurrent_child_processes": 1, "threads_per_child": args.threads,
            "full_decode_repeats_per_representative_backend": args.full_repeats,
            "full_decode_repeats_per_other_selected_backend": 1,
            "note": "Counts assume distinct first/center/last tiles; duplicate indices are deduplicated"}


class Runner:
    def __init__(self, args, out, identities):
        self.args, self.out, self.identities = args, out, identities
        self.records = []
        self.counter = 0

    def invoke(self, command, item, operation, backend="", label="", repeat=0):
        command = [str(part) for part in command]
        self.counter += 1
        record = {"sequence": self.counter, "source_id": item["id"], "scene": item["scene"],
                  "halo": item["halo"], "representative": item["representative"],
                  "operation": operation, "backend": backend, "label": label, "repeat": repeat,
                  "group": "/".join(filter(None, (item["id"], operation, backend, label))),
                  "command": command}
        started = time.perf_counter()
        try:
            process = subprocess.run(command, capture_output=True, text=True, encoding="utf-8",
                                     errors="replace", timeout=self.args.timeout_seconds)
            record["external_wall_ms"] = (time.perf_counter() - started) * 1000
            record["returncode"] = process.returncode
            record["stderr"] = process.stderr
            # Retain exact stdout as well as its parsed final JSON line.
            record["stdout"] = process.stdout
            require(process.returncode == 0, f"Probe failed ({process.returncode}): {process.stderr}")
            result = json.loads(process.stdout.strip().splitlines()[-1])
            record["result"] = result
            require(result.get("success") is True, f"Probe reported failure: {result}")
            if operation == "roi":
                ready = result["open_map_ms"] + result["work_ms"]
                memory = result["memory_at_decode_ready"]
            elif operation == "full_transport":
                ready = result["io"]["total_ms"] + result["transport"]["total_ms"]
                memory = next((entry for entry in result["checkpoints"]
                               if entry["stage"] == "transport_ready"), {})
            else:
                ready, memory = None, {}
            if ready is not None:
                record["derived"] = {"input_to_samples_ready_ms": ready,
                                     "setup_plus_input_to_samples_ready_ms": ready + result["pool_setup_ms"]
                                     + result["library_setup_ms"], "memory_at_samples_ready": memory}
        except BaseException as error:
            record["external_wall_ms"] = (time.perf_counter() - started) * 1000
            record["error"] = str(error)
            raise
        finally:
            self.records.append(record)
            with (self.out / "measurements.jsonl").open("a", encoding="utf-8") as handle:
                handle.write(json.dumps(record, ensure_ascii=False) + "\n")
        return result

    def source_matches(self, result, path):
        require(result.get("source_sha256") == self.identities[str(path)]["sha256"],
                f"Probe/source SHA-256 mismatch: {path}")

    def extract(self, item, tile_index, label, expected_layout=None):
        tile = self.out / f"{item['id']}-tile-{tile_index}.jxl"
        require(not tile.exists(), f"Refusing to replace tile output: {tile}")
        result = self.invoke([FULL, "--threads", self.args.threads, "--io", "buffered",
                              "--extract-tile", tile, "--tile-index", tile_index, item["dng"]],
                             item, "extract_tile", label=label)
        checked_layout(result["layout"])
        if expected_layout is not None:
            require(result["layout"] == expected_layout, f"Layout changed: {item['dng']}")
        tile_identity = identity(tile)
        require(tile_identity["sha256"] == result["tile_sha256"]
                and tile_identity["metadata"]["bytes"] == result["tile_bytes"], "Extracted tile mismatch")
        return tile, result, tile_identity

    def full(self, item, backend, repeat, layout, reference=None):
        command = [FULL, "--backend", backend, "--threads", self.args.threads, "--io", "buffered",
                   "--transport-only"]
        if backend == "libjxl":
            command += ["--library", self.args.library]
        if reference is not None:
            require(not reference.exists(), f"Refusing to replace reference output: {reference}")
            command += ["--raw-output", reference]
        result = self.invoke([*command, item["dng"]], item, "full_transport", backend=backend, repeat=repeat)
        self.source_matches(result, item["dng"])
        require(result["layout"] == layout, f"Full decode layout mismatch: {item['dng']}")
        require(result["development"] is None and result["rgb16_sha256"] is None,
                "Unexpected display development in transport-only probe")
        return result


def run_one(runner, item):
    args, out = runner.args, runner.out
    tile0, extracted, tile0_identity = runner.extract(item, 0, "first_metadata")
    layout = extracted["layout"]
    nx, _ = checked_layout(layout)
    reference = out / f"{item['id']}-native-full.u16"
    full = runner.full(item, "libjxl", 0, layout, reference)
    raw_identity = identity(reference)
    require(raw_identity["metadata"]["bytes"] == layout["sample_bytes"], "Reference size mismatch")
    require(raw_identity["sha256"] == full["canvas_sha256"], "Exported reference SHA-256 mismatch")
    canvas_hash = full["canvas_sha256"]
    repetitions = args.full_repeats if item["representative"] else 1
    for repeat in range(1, repetitions):
        require(runner.full(item, "libjxl", repeat, layout)["canvas_sha256"] == canvas_hash,
                "Repeated native full canvas changed")
    oxide_full = item["representative"] or args.oxide_full_scope == "all"
    if oxide_full:
        for repeat in range(repetitions):
            require(runner.full(item, "oxide", repeat, layout)["canvas_sha256"] == canvas_hash,
                    f"Oxide full canvas differs from native: {item['dng']}")
    cases = regions(layout)
    case_reports = {}
    for label, roi in cases.items():
        case_reports[label] = {"roi": roi, "selected_tile_indices": selected_indices(layout, roi),
                               "reference_roi_sha256": reference_roi_hash(reference, layout, roi)}
    for repeat in range(args.repeats):
        # Rotate order; this does not make the measurements cold-cache samples.
        rotation = (repeat + item["halo"]) % len(MODES)
        modes = MODES[rotation:] + MODES[:rotation]
        for label, case in case_reports.items():
            for mode in modes:
                roi = case["roi"]
                result = runner.invoke([PREVIEW, "--mode", mode, "--threads", args.threads,
                                        "--library", args.library, "--reference", reference,
                                        "--roi", ",".join(map(str, roi)), item["dng"]],
                                       item, "roi", backend=mode, label=label, repeat=repeat)
                runner.source_matches(result, item["dng"])
                validation, work = result["validation"], result["work"]
                require(validation.get("exact_match") is True and validation["different_samples"] == 0
                        and validation["maximum_absolute_error"] == 0, "ROI sample validation failed")
                require(validation["roi_sha256"] == case["reference_roi_sha256"],
                        f"ROI hash differs from reference crop: {item['dng']}, {label}, {mode}")
                require(work["roi_raw_coordinates"] == roi and work["total_tiles"] == layout["tiles"]
                        and work["selected_tiles"] == len(case["selected_tile_indices"])
                        and work["output_sample_bytes"] == roi[2] * roi[3] * layout["channels"] * 2,
                        "ROI layout/report mismatch")
    tiles = [{"label": "first", "tile_index": 0, "identity": tile0_identity}]
    progression = []
    gaps = []
    if args.progressive_scope == "all" or item["representative"]:
        if layout["channels"] != 3:
            gaps.append("The existing native-progressive CLI hardcodes three channels; this input was not probed")
        else:
            center_index = (layout["height"] // 2 // layout["tile_height"]) * nx + (
                layout["width"] // 2 // layout["tile_width"])
            seen = set()
            for label, index in (("first", 0), ("center", center_index), ("last", layout["tiles"] - 1)):
                if index in seen:
                    continue
                seen.add(index)
                if index == 0:
                    tile, tile_identity = tile0, tile0_identity
                else:
                    tile, _, tile_identity = runner.extract(item, index, label, layout)
                    tiles.append({"label": label, "tile_index": index, "identity": tile_identity})
                result = runner.invoke([PREVIEW, "--mode", "native-progressive", "--threads", args.threads,
                                        "--library", args.library, tile], item, "native_progressive", label=label)
                require(result["source_sha256"] == tile_identity["sha256"], "Progressive tile hash mismatch")
                work = result["work"]
                require(work["matches_full_baseline"] is True and work["full_hash"] == work["baseline_hash"],
                        "Progressive final samples differ from fresh full native tile decode")
                require(work["same_decoder_continued"] is True and work["embedded_preview_decoded"] is False
                        and work["native_scaled_output"] is False, "Unexpected progression test scope")
                iw, ih = work["output_dimensions"]
                visible_w = min(layout["tile_width"], layout["width"] - (index % nx) * layout["tile_width"])
                visible_h = min(layout["tile_height"], layout["height"] - (index // nx) * layout["tile_height"])
                require(visible_w <= iw <= layout["tile_width"] and visible_h <= ih <= layout["tile_height"],
                        "Progressive decoded tile geometry mismatch")
                require(work["full_output_bytes"] == iw * ih * layout["channels"] * 2,
                        "Progressive output byte count mismatch")
                events = [event for event in work["events"] if event["event"] == "frame_progression"]
                progression.append({"label": label, "tile_index": index, "tile_sha256": tile_identity["sha256"],
                                    "progression_event_count": len(events),
                                    "successful_flushes": [event for event in events if event["flush_status"] == 0],
                                    "matches_full_baseline": True, "full_hash": work["full_hash"]})
    report = {"id": item["id"], "scene": item["scene"], "halo": item["halo"],
              "source": str(item["dng"]), "representative": item["representative"], "layout": layout,
              "canvas_sha256": canvas_hash, "native_full_checked": True, "oxide_full_checked": oxide_full,
              "oxide_same_backend_full_reference_equivalence": oxide_full,
              "roi_cases": case_reports, "tiles": tiles, "progression": progression, "gaps": gaps,
              "reference_identity": raw_identity, "reference_retained": False}
    save(out / f"{item['id']}-checks.json", report)
    # This exact path was absent before this run and created by this run's EXE.
    # No directory removal, glob cleanup, source deletion, or previous-result cleanup.
    require(reference.parent == out and reference.is_file(), "Unexpected reference cleanup path")
    reference.unlink()
    return report


def execute_run(args, inputs, plan):
    out = args.output
    require(not out.exists(), f"Use a new output directory, not existing results: {out}")
    out.mkdir(parents=True, exist_ok=False)
    save(out / "plan.json", plan)
    manifest = {"status": "identifying_sources", "plan": plan, "source_hashes_unchanged": None,
                "source_metadata_unchanged": None, "python": sys.version,
                "scope": "Raw sample canvas only; no embedded JPEG or display development; JPG inventory only",
                "cache_scope": "Source hashing and prior probes warm the OS cache; no physical cold-disk claim",
                "timing_scope": "Inner stage timings are primary; external wall includes launch/hash/validation/export/exit"}
    save(out / "manifest.json", manifest)
    before, reports, runner = {}, [], None
    failure = None
    try:
        for item in inputs:
            for kind in ("dng", "jpg"):
                path = item[kind]
                before[str(path)] = {"kind": kind, "source_id": item["id"], **identity(path)}
        save(out / "source-identities-before.json", before)
        manifest["tools"] = [identity(path) for path in (PREVIEW, FULL, args.library)]
        manifest["status"] = "running"
        save(out / "manifest.json", manifest)
        runner = Runner(args, out, before)
        for index, item in enumerate(inputs, start=1):
            reports.append(run_one(runner, item))
            save(out / "checks.json", reports)
            manifest["completed_inputs"] = index
            save(out / "manifest.json", manifest)
            print(f"Completed {index}/{len(inputs)} DNGs: {item['dng'].name}", flush=True)
        manifest["status"] = "complete"
    except BaseException as error:
        failure = error
        manifest["status"], manifest["error"] = "failed", str(error)
    finally:
        after, verification_errors = {}, []
        # Preserve partial pre-run identities too if identification was interrupted.
        save(out / "source-identities-before.json", before)
        for path_string, expected in before.items():
            try:
                actual = identity(Path(path_string))
                actual["sha256_unchanged"] = actual["sha256"] == expected["sha256"]
                actual["metadata_unchanged"] = actual["metadata"] == expected["metadata"]
                after[path_string] = actual
            except (OSError, RuntimeError) as error:
                verification_errors.append({"path": path_string, "error": str(error)})
        complete = len(before) == len(inputs) * 2 and len(after) == len(before) and not verification_errors
        manifest["source_hashes_unchanged"] = complete and all(row["sha256_unchanged"] for row in after.values())
        manifest["source_metadata_unchanged"] = complete and all(row["metadata_unchanged"] for row in after.values())
        manifest["source_verification_errors"] = verification_errors
        save(out / "source-identities-after.json", after)
        if runner is not None:
            save(out / "measurements.json", runner.records)
            save(out / "summary.json", summarize(runner.records))
        manifest["gaps"] = [{"source_id": report["id"], "gaps": report["gaps"]}
                            for report in reports if report["gaps"]]
        progressive = [probe for report in reports for probe in report["progression"]]
        manifest["coverage"] = {"completed_dng_inputs": len(reports),
                                "native_full_inputs": sum(report["native_full_checked"] for report in reports),
                                "oxide_full_inputs": sum(report["oxide_full_checked"] for report in reports),
                                "roi_unique_cases": sum(len(report["roi_cases"]) for report in reports),
                                "roi_processes": sum(record["operation"] == "roi" for record in runner.records)
                                if runner is not None else 0,
                                "progressive_inputs": sum(bool(report["progression"]) for report in reports),
                                "progressive_scenes": len({report["scene"] for report in reports if report["progression"]}),
                                "progressive_tiles": len(progressive),
                                "tiles_without_progression_events": sum(probe["progression_event_count"] == 0
                                                                        for probe in progressive),
                                "progression_events": sum(probe["progression_event_count"] for probe in progressive),
                                "successful_progressive_flushes": sum(len(probe["successful_flushes"]) for probe in progressive),
                                "successful_flush_ratio_counts": dict(Counter(str(event["ratio"]) for probe in progressive
                                                                              for event in probe["successful_flushes"]))}
        if not manifest["source_hashes_unchanged"] or not manifest["source_metadata_unchanged"]:
            manifest["status"] = "failed"
            if failure is None:
                failure = RuntimeError("Source identity verification failed; inspect manifest.json")
        manifest["complete_without_gaps"] = manifest["status"] == "complete" and not manifest["gaps"]
        save(out / "manifest.json", manifest)
    if failure is not None:
        raise failure
    print(json.dumps({"success": True, "output": str(out), "processes": runner.counter,
                      "complete_without_gaps": manifest["complete_without_gaps"], "coverage": manifest["coverage"]}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="Actually start the sequential decoder workload")
    parser.add_argument("--corpus", type=Path, default=CORPUS)
    parser.add_argument("--library", type=Path, default=LIBRARY, help="Explicit trusted libjxl DLL")
    parser.add_argument("--output", type=Path, help="NEW child directory under target/preview-dng-corpus")
    parser.add_argument("--repeats", type=int, choices=range(1, 4), default=3)
    parser.add_argument("--full-repeats", type=int, choices=range(1, 4), default=3)
    parser.add_argument("--threads", type=int, choices=range(1, 7), default=6)
    parser.add_argument("--representative-halo", type=int, choices=HALOS, default=48)
    parser.add_argument("--oxide-full-scope", choices=("representatives", "all"), default="representatives")
    parser.add_argument("--progressive-scope", choices=("all", "representatives"), default="all")
    parser.add_argument("--timeout-seconds", type=int, default=180, help="Per-child timeout, not a performance target")
    args = parser.parse_args()
    require(args.timeout_seconds > 0, "Expected a positive process timeout")
    require(args.library.is_absolute(), "The trusted library path must be absolute")
    args.library, args.corpus = args.library.resolve(), args.corpus.resolve()
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    args.output = (args.output or OUTPUT_ROOT / f"corpus-{stamp}-{uuid.uuid4().hex[:8]}").resolve()
    require(OUTPUT_ROOT.resolve() in args.output.parents, "Output must be a new child of target/preview-dng-corpus")
    require(not args.output.exists(), f"Output already exists: {args.output}")
    for path in (PREVIEW, FULL, args.library):
        require(path.is_file(), f"Required prebuilt tool/library is missing (this runner never builds): {path}")
    inputs = discover(args.corpus, args.representative_halo)
    source_sizes = {str(item[kind]): metadata(item[kind])["bytes"] for item in inputs for kind in ("dng", "jpg")}
    plan = {"run_requested": args.run, "corpus": str(args.corpus), "output": str(args.output),
            "library": str(args.library), "preview_executable": str(PREVIEW), "full_executable": str(FULL),
            "representative_halo": args.representative_halo, "oxide_full_scope": args.oxide_full_scope,
            "progressive_scope": args.progressive_scope, "workload": workload(args, inputs),
            "sources": [{key: str(value) if isinstance(value, Path) else value for key, value in item.items()}
                        for item in inputs], "source_sizes": source_sizes,
            "roi_validation_whole_source_hash_bytes": args.repeats * 5 * len(MODES)
            * sum(source_sizes[str(item["dng"])] for item in inputs),
            "hash_io_note": "The existing ROI EXE hashes its whole source after every timed ROI; these are logical bytes, not cold-drive reads or generated output",
            "disk": {"simultaneous_full_reference_files": 1, "full_reference_bytes_upper_bound": 1024**3,
                     "retained_tiles_bytes_upper_bound": sum(source_sizes[str(item["dng"])] for item in inputs),
                     "note": "Actual reference size comes from each extraction layout; selected distinct tiles are retained; JSON/log overhead is additional"},
            "excluded": ["embedded JPEG/thumbnail decoding", "JPG decode benchmarks", "NEF probes", "builds",
                         "display RGB development", "production code changes", "cold-cache claims"]}
    if args.run:
        execute_run(args, inputs, plan)
    else:
        print(json.dumps(plan, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"DNG corpus runner failed: {error}", file=sys.stderr)
        sys.exit(1)
