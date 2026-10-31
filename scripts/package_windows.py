"""Package the built Windows x64 application, runtime, pins and dependency notices."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def copy_file(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def rust_notices(destination):
    """Keep each published crate's metadata and supplied license/notice files."""
    supplemental_root = ROOT / "LICENSES/Rust-supplemental"
    supplemental = json.loads((supplemental_root / "index.json").read_text(encoding="utf-8"))
    recorded_hashes = dict(supplemental["authored_file_sha256"])
    for upstream in supplemental["upstreams"].values():
        recorded_hashes.update({name: notice["sha256"] for name, notice in upstream["notice_files"].items()})
    for name, expected in recorded_hashes.items():
        actual = hashlib.sha256((supplemental_root / name).read_bytes()).hexdigest()
        if actual != expected:
            raise SystemExit(f"Supplemental notice checksum mismatch: {name}")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--filter-platform", "x86_64-pc-windows-msvc", "--format-version", "1"],
        cwd=ROOT, text=True, encoding="utf-8",
    ))
    packages = []
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["id"] in metadata["workspace_members"] or package["id"] not in resolved:
            continue
        source = Path(package["manifest_path"]).parent
        target = destination / "Rust" / f'{package["name"]}-{package["version"]}'
        supplied = set()
        for path in source.rglob("*"):
            if not path.is_file() or any(p in {".git", "target"} for p in path.relative_to(source).parts):
                continue
            if path.name.lower().startswith(("license", "licence", "copying", "copyright", "notice")):
                supplied.add(path)
        if package.get("license_file"):
            supplied.add(source / package["license_file"])
        for path in sorted(supplied):
            copy_file(path, target / path.relative_to(source))
        # The manifest records authors, SPDX expression and upstream source even
        # for a published crate that supplies no standalone license document.
        copy_file(Path(package["manifest_path"]), target / "Cargo.toml")
        entry = {
            key: package.get(key) for key in ("name", "version", "license", "repository", "authors")
        } | {"notice_files": [str(p.relative_to(source)).replace("\\", "/") for p in sorted(supplied)]}
        if not supplied:
            key = f'{package["name"]}@{package["version"]}'
            supplement = supplemental["packages"].get(key)
            if not supplement:
                raise SystemExit(f"Missing dependency notices: {key}; review the pinned upstream before packaging")
            for name in supplement["notice_files"]:
                if not (supplemental_root / name).is_file():
                    raise SystemExit(f"Missing supplemental notice file: {name}")
            entry.update(
                supplemental_notice_files=["Rust-supplemental/" + name for name in supplement["notice_files"]],
                selected_license=supplement["selected_license"],
                review_required=supplement.get("review_required", False),
                provenance="Rust-supplemental/index.json",
            )
        packages.append(entry)
    (destination / "rust-dependencies.json").write_text(
        json.dumps(packages, indent=2, ensure_ascii=False) + "\n", encoding="utf-8",
    )
    review = [p["name"] for p in packages if p.get("review_required")]
    if review:
        print("Documented upstream notice-text gaps (canonical terms and exact attribution included): " + ", ".join(review))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "target/release/scrutivue.exe")
    args = parser.parse_args()
    exe = args.exe.resolve(strict=True)
    runtime = exe.with_name("onnxruntime.dll")
    if not runtime.is_file():
        raise SystemExit("Matching onnxruntime.dll must be beside scrutivue.exe")
    output = ROOT / "target/dist"
    output.mkdir(parents=True, exist_ok=True)
    archive = output / "Scrutivue-windows-x64.zip"
    with tempfile.TemporaryDirectory(prefix="package-", dir=ROOT / "target") as temporary:
        bundle = Path(temporary) / "Scrutivue"
        copy_file(exe, bundle / "scrutivue.exe")
        copy_file(runtime, bundle / "onnxruntime.dll")
        copy_file(ROOT / "LICENSE", bundle / "LICENSE")
        shutil.copytree(ROOT / "LICENSES", bundle / "LICENSES")
        for name in ("LICENSE", "ThirdPartyNotices.txt"):
            copy_file(ROOT / "vendor/onnxruntime" / name, bundle / "LICENSES/ONNXRuntime" / name)
        for pin in ("libraw.json", "onnxruntime.json"):
            copy_file(ROOT / "config" / pin, bundle / "build-info" / pin)
        copy_file(ROOT / "assets/models/lpips-squeeze-v0.1.json", bundle / "build-info/lpips-squeeze-v0.1.json")
        rust_notices(bundle / "LICENSES")
        (bundle / "README.txt").write_text(
            "Scrutivue — photograph and RAW comparison\n\n"
            "Extract this entire folder, then run scrutivue.exe. Keep onnxruntime.dll beside it.\n"
            "Use Help / F1 for controls. Comparison runs offline; source images are never changed.\n"
            "Source and build instructions: https://github.com/AlexeyKupershtokh/Scrutivue\n"
            "Native runtime and published Rust dependency notices are under LICENSES.\n"
            "See LICENSES/Rust-supplemental/README.md for documented upstream notice-text gaps.\n"
            "This unsigned portable build may trigger Windows SmartScreen.\n"
            "The Microsoft Visual C++ x64 runtime may be needed on a clean Windows installation.\n",
            encoding="utf-8",
        )
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as zip_file:
            for path in sorted(bundle.rglob("*")):
                if path.is_file():
                    zip_file.write(path, path.relative_to(bundle.parent))
    print(archive)


if __name__ == "__main__":
    main()
