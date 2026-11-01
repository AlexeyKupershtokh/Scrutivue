"""Replay djxl's public-API downsampling policy on retained actual tiles/controls.

No djxl executable is invoked. The isolated Rust mode reproduces its
kLastPasses/get-ratio/one-flush/stop loop for single-frame RGB16 inputs.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus-results", type=Path, default=ROOT / "target/preview-dng-corpus/halo30-20261101")
    parser.add_argument("--controls", type=Path, default=ROOT / "target/preview-native-probe")
    parser.add_argument("--library", type=Path, default=Path("E:/Documents/AI/REA_DXO/pipeline-graph/resources/native/libjxl-0.12.0/jxl.dll"))
    parser.add_argument("--output", type=Path, default=ROOT / "target/jxl-downsampling-review-20261101/results")
    parser.add_argument("--control-repeats", type=int, default=3)
    args = parser.parse_args()
    assert 1 <= args.control_repeats <= 10
    checks = json.loads((args.corpus_results / "checks.json").read_text(encoding="utf-8"))
    assert len(checks) == 30 and all(len(item["tiles"]) == 3 for item in checks)
    args.output.mkdir(parents=True, exist_ok=False)
    executable = ROOT / "target/preview-experiments/preview_capabilities.exe"
    records, identities = [], {}

    def run(path, category, expected_hash=None, **labels):
        before = digest(path)
        identities[str(path)] = before
        if expected_hash is not None:
            assert before == expected_hash
        repeats = args.control_repeats if category == "synthetic" else 1
        for ratio in (1, 2, 4, 8):
            for repeat in range(repeats):
                command = [str(executable), "--mode", "djxl-downsampling", "--library", str(args.library),
                           "--downsampling", str(ratio), "--threads", "1", str(path)]
                process = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", timeout=120)
                if process.returncode:
                    raise RuntimeError(process.stderr + process.stdout)
                result = json.loads(process.stdout.strip().splitlines()[-1])
                assert result["success"] and result["source_sha256"] == before
                assert result["work"]["requested_maximum_ratio"] == ratio
                if not result["work"]["is_partial"]:
                    assert result["validation"]["matches_full_baseline"]
                result.update(category=category, requested_ratio=ratio, repeat=repeat, **labels)
                records.append(result)
                with (args.output / "measurements.jsonl").open("a", encoding="utf-8") as file:
                    file.write(json.dumps(result) + "\n")
        assert digest(path) == before

    for item in checks:
        for tile in item["tiles"]:
            identity = tile["identity"]
            run(Path(identity["path"]), "actual_dng_tile", identity["sha256"],
                source_id=item["id"], source_dng=item["source"], tile_label=tile["label"], tile_index=tile["tile_index"])
    for progressive in (False, True):
        path = args.controls / f"synthetic-4096x3072-{str(progressive).lower()}.jxl"
        run(path, "synthetic", spectral_progressive=progressive)
    actual = [row for row in records if row["category"] == "actual_dng_tile"]
    assert len(actual) == 360
    headers = Counter(json.dumps(row["validation"]["frame_header"], sort_keys=True) for row in actual if row["requested_ratio"] == 1)
    controls = []
    for progressive in (False, True):
        for ratio in (1, 2, 4, 8):
            rows = [row for row in records if row["category"] == "synthetic"
                    and row["spectral_progressive"] == progressive and row["requested_ratio"] == ratio]
            controls.append({"spectral_progressive": progressive, "requested_ratio": ratio, "n": len(rows),
                "median_ready_ms": statistics.median(row["work"]["elapsed_ms"] for row in rows),
                "median_flush_ms": statistics.median(sum(event["flush_ms"] or 0 for event in row["work"]["events"]) for row in rows),
                "median_consumed_bytes": statistics.median(row["work"]["consumed_bytes_at_stop"] for row in rows),
                "output_bytes": sorted({row["work"]["full_output_bytes"] for row in rows}),
                "partial": sorted({row["work"]["is_partial"] for row in rows}),
                "intended_ratio": sorted({row["work"]["intended_ratio"] for row in rows}),
                "flush_counts": sorted({sum(event["flush_status"] == 0 for event in row["work"]["events"]) for row in rows})})
    summary = {"success": True, "records": len(records), "actual_tiles": 90, "actual_dng_inputs": 30,
        "actual_runs": len(actual), "actual_partial_outputs": sum(row["work"]["is_partial"] for row in actual),
        "actual_progression_events": sum(event["event"] == "frame_progression" for row in actual for event in row["work"]["events"]),
        "actual_exact_full_outputs": sum(row["validation"]["matches_full_baseline"] for row in actual),
        "actual_frame_headers": [{"header": json.loads(header), "tiles": count} for header, count in headers.items()],
        "controls": controls, "source_tile_and_control_hashes_unchanged": True,
        "inputs": identities, "library_sha256": digest(args.library), "executable_sha256": digest(executable),
        "scope": "Exact djxl downsampling selection loop via public C API, not the djxl executable; RGB16/original profile, one native thread; baseline/header inspection outside ready timing; no embedded previews, no source photograph modification"}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
