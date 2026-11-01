"""Sequential production streaming checks on the actual 30 DNGs and two copies."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def identity(path):
    stat = path.stat()
    with path.open("rb") as file:
        digest = hashlib.file_digest(file, "sha256").hexdigest()
    return {"sha256": digest, "bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "target/release/scrutivue.exe")
    parser.add_argument("--corpus", type=Path, default=Path("E:/Documents/AI/corpus/3 этаж single/halo"))
    parser.add_argument("--copies", type=Path, default=ROOT / "target/progressive-dng-samples")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    originals = sorted(args.corpus.glob("*.dng"))
    copies = sorted(args.copies.glob("*.dng"))
    assert len(originals) == 30 and len(copies) == 2
    args.output.mkdir(parents=True, exist_ok=False)
    records = []
    identities = {}
    for path in originals + copies:
        before = identity(path)
        process = subprocess.run([str(args.exe), "--stream-probe", str(path)], capture_output=True, text=True, encoding="utf-8", timeout=180)
        if process.returncode:
            raise RuntimeError(f"{path}: {process.stderr}\n{process.stdout}")
        record = json.loads(process.stdout.strip().splitlines()[-1])
        assert record["success"] and record["matches_fresh_full_decode"] and record["source_unchanged"]
        assert record["total_entropy_tile_decodes"] == record["total_tiles"]
        assert record["views"][2]["new_tile_decodes"] == 0
        assert identity(path) == before
        record["original_corpus"] = path in originals
        identities[str(path)] = before
        records.append(record)
        (args.output / "measurements.json").write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
        print(f"Validated {len(records)}/32: {path.name}", flush=True)
    summary = []
    for original in (True, False):
        rows = [row for row in records if row["original_corpus"] == original]
        summary.append({"original_corpus": original, "files": len(rows),
            "first_visible_median_ms": statistics.median(row["views"][0]["ready_ms"] for row in rows),
            "pan_missing_median_ms": statistics.median(row["views"][1]["ready_ms"] for row in rows),
            "return_to_cached_median_ms": statistics.median(row["views"][2]["ready_ms"] for row in rows),
            "all_tiles_median_ms": statistics.median(row["all_tiles_ready_ms"] for row in rows),
            "full_developed_median_ms": statistics.median(row["full_developed_ready_ms"] for row in rows),
            "exact_full_matches": sum(row["matches_fresh_full_decode"] for row in rows),
            "regional_spot_maximum_error": max(row["regional_maximum_absolute_error"] for row in rows)})
    result = {"success": True, "files": len(records), "source_hashes_unchanged": True,
        "executable": str(args.exe), "executable_sha256": identity(args.exe)["sha256"],
        "summary": summary, "sources": identities,
        "scope": "Fresh sequential workers; six inner tile threads; source hashing warms Windows cache. Visibility is 512x512 at 100%; no GPU upload/presentation timed. Regional development is provisional; final equality is relative to each input, including lossy copies."}
    (args.output / "manifest.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in result.items() if k != "sources"}, indent=2))


if __name__ == "__main__":
    main()
