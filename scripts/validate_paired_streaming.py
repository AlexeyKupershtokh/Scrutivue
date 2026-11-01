"""Sequential Slider/Diff worker checks on original, progressive and mixed tile pairs."""
import argparse
import hashlib
import json
from pathlib import Path
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
    args.output.mkdir(parents=True, exist_ok=False)
    pairs = {
        "original": [args.corpus / "AK8_0036_halo0.dng", args.corpus / "AK8_0036_halo96.dng"],
        "progressive": [args.copies / "AK8_0005_halo0-lossy-progressive.dng", args.copies / "AK8_0988_halo48-lossy-progressive.dng"],
        "mixed-tiling": [args.corpus / "AK8_0005_halo0.dng", args.copies / "AK8_0005_halo0-lossy-progressive.dng"],
    }
    sources = {str(path): identity(path) for paths in pairs.values() for path in paths}
    records = []
    for kind, paths in pairs.items():
        for mode in (["slider", "diff", "slider-fit", "diff-fit"] if kind != "mixed-tiling" else ["diff", "diff-fit"]):
            name = f"{kind}-{mode}"
            process = subprocess.run([str(args.exe), "--stream-pair-probe", mode,
                "--stream-probe-output", str(args.output / name), *map(str, paths)],
                capture_output=True, text=True, encoding="utf-8", timeout=180)
            if process.returncode:
                raise RuntimeError(f"{name}: {process.stderr}\n{process.stdout}")
            record = json.loads(process.stdout)
            assert record["success"] and record["both_sides_progress_before_completion"] and record["full_frames_match_fresh_decode"]
            assert record["first_both_sides_progress_ms"] < min(record["full_developed_ready_ms"])
            if mode.startswith("diff"):
                assert record["paired_preview_maximum_error_u8"] <= 1
            for path in paths:
                assert identity(path) == sources[str(path)]
            records.append({"case": name, **record})
            (args.output / "measurements.json").write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
            print(f"Validated {len(records)}/10: {name}; first A+B preview {record['first_both_sides_progress_ms']:.1f} ms", flush=True)
    result = {"success": True, "cases": len(records), "executable": str(args.exe),
        "executable_sha256": identity(args.exe)["sha256"], "source_hashes_unchanged": True,
        "maximum_diff_error_u8": max(r["paired_preview_maximum_error_u8"] or 0 for r in records),
        "sources": sources, "scope": "Actual asynchronous worker. 512x512 at 100% and Fit. PNG buffers, not GUI screenshots; encoding them can affect ready timers. Reference decoding/rendering follows worker completion."}
    (args.output / "manifest.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: v for k, v in result.items() if k != "sources"}, indent=2))


if __name__ == "__main__":
    main()
