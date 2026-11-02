"""Package the exact clean GitHub Actions revision with versioned assets and provenance."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def output(command, root):
    return subprocess.check_output(command, cwd=root, text=True, encoding="utf-8").strip()


def provenance(root, exe, environment):
    if environment.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("CI release packaging requires GitHub Actions context")
    revision = output(["git", "rev-parse", "HEAD"], root)
    if len(revision) != 40 or revision != environment.get("GITHUB_SHA"):
        raise RuntimeError("The checkout does not match the requested GitHub Actions revision")
    if output(["git", "status", "--porcelain", "--untracked-files=normal"], root):
        raise RuntimeError("CI release packaging requires a clean committed checkout")
    package = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    version = package["package"]["version"]
    executable_version = output([str(exe), "--version"], root)
    if executable_version != f"scrutivue {version}":
        raise RuntimeError("The executable version does not match Cargo.toml")
    repository = environment.get("GITHUB_REPOSITORY", "")
    run_id = environment.get("GITHUB_RUN_ID", "")
    attempt = environment.get("GITHUB_RUN_ATTEMPT", "")
    if repository.count("/") != 1 or not run_id.isdecimal() or not attempt.isdecimal():
        raise RuntimeError("Incomplete GitHub Actions provenance")
    pins = {
        name: json.loads((root / "config" / name).read_text(encoding="utf-8"))
        for name in ("libraw.json", "onnxruntime.json", "libjxl.json")
    }
    native = {
        name: hashlib.sha256((root / "native" / name).read_bytes()).hexdigest()
        for name in ("raw.cpp", "parallel_raw.cpp", "parallel_raw.h")
    }
    return {
        "version": version,
        "source_commit": revision,
        "source_clean": True,
        "executable": {"name": exe.name, "bytes": exe.stat().st_size,
                       "sha256": hashlib.sha256(exe.read_bytes()).hexdigest()},
        "build": {"profile": "release", "target": "x86_64-pc-windows-msvc",
                  "default_features": package.get("features", {}).get("default", []),
                  "command": "cargo build --release --locked --bin scrutivue",
                  "rustc": output(["rustc", "--version", "--verbose"], root)},
        "ci": {"provider": "github-actions", "repository": repository, "run_id": run_id,
               "run_attempt": attempt, "ref": environment.get("GITHUB_REF", ""),
               "run_url": f"https://github.com/{repository}/actions/runs/{run_id}"},
        "pins": pins,
        "native_source_sha256": native,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, default=ROOT / "target/release/scrutivue.exe")
    parser.add_argument("--output", type=Path, default=ROOT / "target/dist")
    args = parser.parse_args()
    exe = args.exe.resolve(strict=True)
    report = provenance(ROOT, exe, os.environ)
    directory = args.output.resolve()
    directory.mkdir(parents=True, exist_ok=True)
    info = directory / "release.json"
    archive = directory / f"Scrutivue-{report['version']}-windows-x64.zip"
    checksums = directory / "SHA256SUMS.txt"
    if any(path.exists() for path in (info, archive, checksums)):
        raise SystemExit("Refusing to replace existing CI release assets")
    info.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    subprocess.run([sys.executable, str(ROOT / "scripts/package_windows.py"), "--exe", str(exe),
                    "--output", str(archive), "--provenance", str(info)], cwd=ROOT, check=True)
    subprocess.run([sys.executable, str(ROOT / "scripts/test_package.py"), "--archive", str(archive)],
                   cwd=ROOT, check=True)
    checksums.write_text("".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
        for path in (archive, info)
    ), encoding="utf-8")
    print(json.dumps({"archive": str(archive), "checksums": str(checksums),
                      "source_commit": report["source_commit"], "ci": report["ci"]}, indent=2))


if __name__ == "__main__":
    main()
