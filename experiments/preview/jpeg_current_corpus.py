"""Measure the existing Rust JPEG backend on the actual halo JPEG corpus."""
import argparse
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
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10:
        parser.error("Expected 1..10 repeats")
    paths = sorted(path for path in args.directory.iterdir() if path.suffix.lower() in (".jpg", ".jpeg"))
    assert len(paths) == 30, f"Expected 30 actual JPEGs, found {len(paths)}"
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    executable = ROOT / "target/preview-experiments/preview_capabilities.exe"
    records, summary, identities = [], [], {}
    for path in paths:
        before = digest(path)
        identities[path.name] = before
        values = []
        for repeat in range(args.repeats):
            process = subprocess.run([str(executable), "--mode", "jpeg-current", str(path)],
                                     check=True, capture_output=True, text=True, encoding="utf-8", timeout=120)
            result = json.loads(process.stdout.strip().splitlines()[-1])
            assert result["success"] and result["source_sha256"] == before
            result["repeat"] = repeat
            records.append(result)
            values.append(result)
            (out / "measurements.json").write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
        assert digest(path) == before, f"Source changed: {path}"
        assert len({value["work"]["sha256"] for value in values}) == 1
        row = {"source_name": path.name, "n": len(values)}
        for key in ["header_and_compressed_copy_ms", "output_alloc_ms", "decode_ms", "ready_ms"]:
            row[key] = statistics.median(value["work"][key] for value in values)
        row["peak_ws_mib"] = statistics.median(value["work"]["memory_at_first_ready"]["peak_rss_bytes"] / 2**20 for value in values)
        summary.append(row)
        print(f"Completed current Rust JPEG baseline: {path.name}", flush=True)
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    (out / "manifest.json").write_text(json.dumps({"source_sha256": identities,
        "source_hashes_unchanged": True, "sequential_processes": True, "repeats": args.repeats,
        "scope": "full unrotated decode only; no claimed scaled/ROI support or cross-codec pixel equivalence"}, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"success": True, "records": len(records), "output": str(out)}))


if __name__ == "__main__":
    main()
