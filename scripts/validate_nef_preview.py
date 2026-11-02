"""Read-only, separate-process NEF preview timing and authoritative pixel checks."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess


def identity(path):
    stat = path.stat()
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"sha256": digest, "bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--corpus", required=True, type=Path)
    parser.add_argument("--baseline", required=True, type=Path, help="Saved 0.3.1 --probe JSON")
    parser.add_argument("--output", required=True, type=Path, help="Fresh directory; never overwrite")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--cpu", action="store_true")
    args = parser.parse_args()
    assert args.repetitions > 0
    exe = args.exe.resolve(strict=True)
    paths = sorted(path for path in args.corpus.iterdir() if path.suffix.lower() == ".nef")
    assert paths, "No NEFs in the requested corpus"
    reference = {Path(row["path"]).name: row for row in json.loads(args.baseline.read_text(encoding="utf-8"))}
    assert all(path.name in reference for path in paths), "Baseline does not cover every source"
    args.output.mkdir(parents=True, exist_ok=False)
    before = {str(path): identity(path) for path in paths}
    binary_before = identity(exe)
    records = []
    environment = os.environ.copy()
    environment.pop("SCRUTIVUE_NEF_UI_TRACE", None)
    for repetition in range(args.repetitions):
        for path in paths:
            result = subprocess.run([str(exe), "--nef-preview-probe", str(path)] + (["--cpu"] if args.cpu else []),
                                    capture_output=True, encoding="utf-8", timeout=120, env=environment)
            stem = f"{path.stem}-{repetition}"
            (args.output / f"{stem}.stdout.json").write_text(result.stdout, encoding="utf-8")
            (args.output / f"{stem}.stderr.txt").write_text(result.stderr, encoding="utf-8")
            result.check_returncode()
            row = json.loads(result.stdout)
            assert row["success"] and row["first_preview_ms"] is not None, row
            assert 0 < row["first_preview_ms"] < row["full_decode_ms"] <= row["full_render_ready_ms"], row
            assert row["snapshots"] and row["snapshots"][0]["covered_sampled_pixels"][0] > 0, row
            assert row["dimensions"] == reference[path.name]["dimensions"], row
            assert row["full_rgb16_sha256"] == reference[path.name]["rgb16_le_sha256"], row
            records.append({"file": path.name, "repetition": repetition, **row})
    after = {str(path): identity(path) for path in paths}
    assert before == after, "Source identity changed during validation"
    assert binary_before == identity(exe), "Executable changed during validation"
    medians = []
    for path in paths:
        runs = [row for row in records if row["file"] == path.name]
        medians.append({"file": path.name, **{key: statistics.median(row[key] for row in runs)
            for key in ("first_preview_ms", "full_decode_ms", "full_render_ready_ms", "startup_ms")}})
    report = {"success": True, "executable": str(exe), "executable_identity": binary_before,
              "sources_unchanged": True, "sources": after, "all_final_hashes_match_031": True,
              "repetitions": args.repetitions, "medians": medians, "records": records,
              "scope": "Warm OS cache; new process and Services per observation; excludes GPU startup, GPUI upload and screen presentation. First preview is a ready display buffer, not an observed screen image."}
    (args.output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"success": True, "medians": medians}, indent=2))


if __name__ == "__main__":
    main()
