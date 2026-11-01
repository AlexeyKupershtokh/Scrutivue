"""Package the built Windows x64 application, runtime, pins and dependency notices."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import zipfile
from prepare_jxl_runtime import load_pin, validate_bundle

ROOT = Path(__file__).resolve().parents[1]


def copy_file(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def required_jxl_runtime(exe, bundle):
    source = exe.parent / "libjxl"
    pin = load_pin()
    try:
        validate_bundle(source, pin)
    except RuntimeError as error:
        raise SystemExit(f"Cannot package without the pinned JPEG XL runtime and notices: {error}. Run scripts/prepare_jxl_runtime.py first.") from error
    for name in pin["files"]:
        copy_file(source / name, bundle / "libjxl" / name)
    for name in pin["licenses"]:
        copy_file(source / "licenses" / name, bundle / "libjxl/licenses" / name)
    (bundle / "libjxl/bundle-pin.json").write_text(json.dumps(pin, indent=2) + "\n", encoding="utf-8")


def required_lpips_model(exe, bundle):
    pin = json.loads((ROOT / "assets/models/lpips-squeeze-v0.1.json").read_text(encoding="utf-8"))
    source = exe.parent / "models" / pin["model"]
    if not source.is_file():
        raise SystemExit(f"Cannot package without the LPIPS model beside the executable: {source}. Restore it from assets/models/ before packaging.")
    if hashlib.sha256(source.read_bytes()).hexdigest() != pin["sha256"]:
        raise SystemExit(f"LPIPS model checksum mismatch: {source}. Restore it from assets/models/ before packaging.")
    copy_file(source, bundle / "models" / pin["model"])


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
    parser.add_argument("--output", type=Path, default=ROOT / "target/dist/Scrutivue-windows-x64.zip")
    parser.add_argument("--provenance", type=Path, help="Verified release JSON to include as build-info/release.json")
    args = parser.parse_args()
    exe = args.exe.resolve(strict=True)
    runtime = exe.with_name("onnxruntime.dll")
    if not runtime.is_file():
        raise SystemExit("Matching onnxruntime.dll must be beside scrutivue.exe")
    archive = args.output.resolve()
    archive.parent.mkdir(parents=True, exist_ok=True)
    if args.provenance:
        provenance = json.loads(args.provenance.read_text(encoding="utf-8"))
        version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        if (provenance["version"] != version or provenance["source_commit"] != revision
                or provenance["executable"]["sha256"] != hashlib.sha256(exe.read_bytes()).hexdigest()):
            raise SystemExit("Release provenance does not match this executable, version and source revision")
    with tempfile.TemporaryDirectory(prefix="package-", dir=ROOT / "target") as temporary:
        bundle = Path(temporary) / "Scrutivue"
        copy_file(exe, bundle / "scrutivue.exe")
        copy_file(runtime, bundle / "onnxruntime.dll")
        required_lpips_model(exe, bundle)
        required_jxl_runtime(exe, bundle)
        copy_file(ROOT / "LICENSE", bundle / "LICENSE")
        shutil.copytree(ROOT / "LICENSES", bundle / "LICENSES")
        for name in ("LICENSE", "ThirdPartyNotices.txt"):
            copy_file(ROOT / "vendor/onnxruntime" / name, bundle / "LICENSES/ONNXRuntime" / name)
        for pin in ("libraw.json", "onnxruntime.json", "libjxl.json"):
            copy_file(ROOT / "config" / pin, bundle / "build-info" / pin)
        copy_file(ROOT / "assets/models/lpips-squeeze-v0.1.json", bundle / "build-info/lpips-squeeze-v0.1.json")
        for name in ("parallel_raw.cpp", "parallel_raw.h"):
            copy_file(ROOT / "native" / name, bundle / "build-info/native" / name)
        if args.provenance:
            copy_file(args.provenance, bundle / "build-info/release.json")
        rust_notices(bundle / "LICENSES")
        (bundle / "README.txt").write_text(
            "Scrutivue — photograph and RAW comparison\n\n"
            "Extract this entire folder, then run scrutivue.exe. Keep onnxruntime.dll beside it.\n"
            "Keep models/lpips-squeeze-v0.1.onnx for offline LPIPS statistics.\n"
            "Keep the included libjxl subdirectory and its licenses for native JPEG XL DNG loading.\n"
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
