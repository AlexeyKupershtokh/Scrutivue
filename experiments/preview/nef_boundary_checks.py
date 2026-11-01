"""Exact source-level Nikon stripe edge/seam checks against retained full CFA."""
import argparse
import hashlib
import json
from pathlib import Path

from nef_corpus import ROOT, invoke


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--references", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads((args.references / "manifest.json").read_text())
    assert manifest["status"] == "complete" and manifest["source_hashes_unchanged"]
    records = []
    executable = ROOT / "target/preview-nef-build/nef_libraw.exe"
    args.output.mkdir(parents=True, exist_ok=False)
    for name, identity in manifest["sources"].items():
        path = args.directory / name
        assert digest(path) == identity["sha256"]
        metadata = invoke(executable, "metadata", path)
        width, height = int(metadata["raw_width"]), int(metadata["raw_height"])
        x, y = width // 2, height // 2
        cases = [[0, 0, 1, 1], [width - 9, 1, 9, 7], [0, 62, 9, 7],
                 [width - 9, 63, 9, 7], [x, 64, 7, 9], [x + 1, 65, 13, 11],
                 [x + 1, y + 1, 17, 19], [width - 1, height - 1, 1, 1],
                 [width - 13, height - 11, 13, 11], [0, y, 32, 32],
                 [width - 32, y, 32, 32], [0, 63, width, 2]]
        reference = args.references / f"{path.stem}-full-cfa.u16"
        for roi in cases:
            result = invoke(executable, "stripe-roi", path, "--roi", *roi, "--reference", reference)
            result.update(source_name=name, roi=roi)
            records.append(result)
            (args.output / "checks.json").write_text(json.dumps(records, indent=2) + "\n", encoding="utf-8")
            assert result["different_samples"] == 0 and result["maximum_absolute_error"] == 0, result
        assert digest(path) == identity["sha256"]
    result = {"success": True, "exact_region_checks": len(records), "source_hashes_unchanged": True,
              "scope": "one-pixel, odd-size, physical-edge and 64-row-seam raw CFA comparisons; no RGB equivalence claim"}
    (args.output / "manifest.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
