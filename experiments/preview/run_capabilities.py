"""Run sequential preview/ROI capability probes without changing source images.

Requires both isolated experiment executables and generated monolithic controls.
All references, manifests and measurements are written under target/.
"""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def invoke(command):
    start = time.perf_counter()
    process = subprocess.run([str(part) for part in command], capture_output=True, text=True, encoding="utf-8")
    if process.returncode:
        raise RuntimeError(f"Probe failed ({process.returncode}): {process.stderr}\n{process.stdout}")
    result = json.loads(process.stdout.strip().splitlines()[-1])
    assert result["success"], result
    result["external_wall_ms"] = (time.perf_counter() - start) * 1000
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dng", type=Path, action="append", required=True)
    parser.add_argument("--library", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--output", type=Path, default=ROOT / "target/preview-capability-results")
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10:
        parser.error("Expected 1..10 repeats")
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    executable = ROOT / "target/preview-experiments/preview_capabilities.exe"
    full_executable = ROOT / "target/dng-jxl-experiments/dng_jxl_bench.exe"
    controls = sorted((ROOT / "target/preview-native-probe").glob("synthetic-4096x3072-*.jxl"))
    assert len(controls) == 2, "Generate the two monolithic JXL controls first"
    tiles = sorted((ROOT / "target/dng-jxl-results-20261101").glob("*.jxl"))
    assert len(tiles) == 6, "Extract tiles 0/363/725 from each DNG into target/dng-jxl-results-20261101 first"
    sources = [path.resolve() for path in args.dng] + controls
    identities = {str(path): digest(path) for path in sources}
    records = []
    references = []

    def record(command, group, repeat):
        result = invoke(command)
        result.update(group=group, repeat=repeat)
        records.append(result)
        save(out / "measurements.json", records)
        return result

    for index, path in enumerate(args.dng):
        reference = out / f"dng-{index}-full-raw.u16"
        assert not reference.exists(), f"Use a new results directory: {reference}"
        result = invoke([full_executable, "--backend", "libjxl", "--library", args.library,
                         "--threads", 6, "--transport-only", "--raw-output", reference, path])
        assert result["source_sha256"] == identities[str(path.resolve())]
        references.append(result)
        save(out / "reference-decodes.json", references)
        regions = {"center512": "3884,2504,512,512", "single128": "3904,2400,128,128", "edge": "8200,5460,80,60"}
        for repeat in range(args.repeats):
            record([executable, "--mode", "embedded", path], f"dng-{index}/embedded", repeat)
            for label, region in regions.items():
                for mode in ["dng-roi-native", "dng-roi-oxide", "dng-roi-oxide-crop"]:
                    result = record([executable, "--mode", mode, "--library", args.library,
                                     "--reference", reference, "--roi", region, path], f"dng-{index}/{label}/{mode}", repeat)
                    assert result["validation"]["exact_match"]
        print(f"Completed DNG {index}: embedded preview and three ROI cases", flush=True)

    for path in controls:
        reference = out / f"{path.stem}-full.u16"
        for repeat in range(args.repeats):
            extra = ["--output", reference] if repeat == 0 else ["--reference", reference]
            full = record([executable, "--mode", "standalone-full", *extra, path], f"{path.stem}/full", repeat)
            for mode in ["standalone-roi", "standalone-expand"]:
                result = record([executable, "--mode", mode, "--roi", "1788,1240,512,512",
                                 "--reference", reference, path], f"{path.stem}/{mode}", repeat)
                assert result["work"]["validation"]["exact_match"]
                if mode == "standalone-expand":
                    assert result["work"]["full_sha256"] == full["work"]["sha256"]
            record([executable, "--mode", "oxide-stream", path], f"{path.stem}/oxide-stream", repeat)
        print(f"Completed monolithic control: {path.name}", flush=True)

    for path in tiles:
        record([executable, "--mode", "oxide-stream", path], f"tile/{path.name}/oxide-stream", 0)

    for path in sources:
        assert digest(path) == identities[str(path)], f"Source changed: {path}"
    for result in records:
        matching = [expected for path, expected in identities.items() if Path(path).name == result["source_name"]]
        if matching:
            assert result["source_sha256"] in matching
    save(out / "manifest.json", {"source_identities": identities, "source_hashes_unchanged": True,
         "library_sha256": digest(args.library), "repeats": args.repeats, "sequential_processes": True,
         "cache_scope": "OS-cache-warm; no physical cold-drive claim; mmap faults are included in work",
         "timing_scope": "Use inner ready/ROI times; external wall includes validation and full source hashing"})
    summary = []
    for group in sorted({result["group"] for result in records}):
        values = [result for result in records if result["group"] == group]
        row = {"group": group, "n": len(values)}
        for key in ["open_map_ms", "library_setup_ms", "pool_setup_ms", "work_ms"]:
            row[key] = statistics.median(value[key] for value in values)
        for key in ["ready_ms", "first_roi_ms", "expansion_to_full_ms", "parse_ms", "reconstruction_ms", "stream_ms",
                    "parse_and_plan_ms", "output_alloc_ms", "selected_tiles_ms", "assembly_ms", "total_ms", "final_ready_ms"]:
            if key in values[0]["work"]:
                row[key] = statistics.median(value["work"][key] for value in values)
        memory = [value["work"].get("memory_at_first_ready", value["work"].get("first_roi_memory",
                  value["work"].get("memory_at_final_ready", value["memory_at_decode_ready"]))) for value in values]
        row["peak_ws_mib_at_ready"] = statistics.median(value["peak_rss_bytes"] / 2**20 for value in memory)
        row["private_mib_at_ready"] = statistics.median(value["private_bytes"] / 2**20 for value in memory)
        summary.append(row)
    save(out / "summary.json", summary)
    print(json.dumps({"success": True, "records": len(records), "output": str(out)}))


if __name__ == "__main__":
    main()
