"""Build isolated Windows DNG experiments using existing optimized Rust dependencies.

No application binary, production source, LibRaw pin or neighboring project is changed.
Run python scripts/prepare.py and cargo test --locked once if dependencies are missing.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "experiments/dng-jxl"
OUT = ROOT / "target/dng-jxl-experiments"


def msvc_environment():
    vswhere = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
    instances = json.loads(subprocess.check_output([str(vswhere), "-latest", "-products", "*", "-format", "json"]))
    if not instances:
        raise SystemExit("MSVC Build Tools are required")
    vcvars = Path(instances[0]["installationPath"]) / "VC/Auxiliary/Build/vcvars64.bat"
    text = subprocess.check_output(f'cmd /d /s /c ""{vcvars}" >nul && set"', text=True)
    env = dict(os.environ)
    env.update(line.split("=", 1) for line in text.splitlines() if "=" in line and not line.startswith("="))
    env["CARGO_MANIFEST_DIR"] = str(ROOT)
    return env


def newest(pattern):
    paths = list(ROOT.glob(pattern))
    if not paths:
        raise SystemExit(f"Missing {pattern}; prepare and test the application dependencies first")
    return max(paths, key=lambda p: p.stat().st_mtime)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tests", action="store_true")
    parser.add_argument("--preview", action="store_true", help="Build the separate preview/ROI capability probe")
    args = parser.parse_args()
    if os.name != "nt":
        raise SystemExit("These developer experiments currently require Windows x64/MSVC")
    output = ROOT / "target/preview-experiments" if args.preview else OUT
    output.mkdir(parents=True, exist_ok=True)
    pin = json.loads((ROOT / "config/libraw.json").read_text())
    if (ROOT / "vendor/libraw/.diff-pin").read_text().strip() != pin["version"]:
        raise SystemExit("The exact pinned Redlamp LibRaw is required")
    env = msvc_environment()
    compiler = shutil.which("cl.exe", path=env.get("Path", env.get("PATH")))
    rustc = Path.home() / ".cargo/bin/rustc.exe"
    # Normalize inner comments and the test fixture's relative path for include!;
    # production decoder logic is unchanged in this ignored build artifact.
    original = (ROOT / "src/dng.rs").read_text(encoding="utf-8")
    original = "\n".join("//" + line[3:] if line.startswith("//!") else line for line in original.splitlines())
    fixture = (ROOT / "assets/tests/dng-jxl-tile.jxl").as_posix()
    original = original.replace('include_bytes!("../assets/tests/dng-jxl-tile.jxl")', f'include_bytes!(r#"{fixture}"#)')
    original = original.replace('pub mod tiles;', f'#[path = r#"{(ROOT / "src/dng/tiles.rs").as_posix()}"#] pub mod tiles;')
    generated = output / "dng_experiment_module.rs"
    transport = (SOURCE / "transport.rs").as_posix()
    extra = f'\ninclude!(r#"{transport}"#);\n'
    if args.preview:
        extra += f'include!(r#"{(ROOT / "experiments/preview/dng_regions.rs").as_posix()}"#);\n'
    generated.write_text(original + extra, encoding="utf-8")
    env["DNG_EXPERIMENT_SOURCE"] = str(generated)
    dependencies = ["anyhow", "serde", "serde_json", "sha2", "memmap2", "rayon", "image", "jxl_oxide", "libloading"]
    deps = ROOT / "target/debug/deps"
    externs = []
    for name in dependencies:
        externs += ["--extern", name + "=" + str(newest(f"target/debug/deps/lib{name}-*.rlib"))]
    common = [str(rustc), "--edition=2021", "-C", "opt-level=3", "-L", "dependency=" + str(deps)] + externs
    subprocess.run([compiler, "/nologo", "/std:c++17", "/EHsc", "/MD", "/O2", "/c", "/DLIBRAW_NODLL", "/D_CRT_SECURE_NO_WARNINGS",
                    "/I" + str(ROOT / "vendor/libraw"), str(ROOT / "native/raw.cpp"), "/Fo" + str(output / "raw.obj")], env=env, check=True)
    lib = newest("target/debug/build/scrutivue-*/out/diff_raw.lib")
    zlib = newest("target/debug/build/libz-sys-*/out/lib/z.lib")
    links = ["-L", "native=" + str(lib.parent), "-L", "native=" + str(zlib.parent), "-l", "static=diff_raw", "-l", "static=z",
             "-l", "ws2_32", "-C", "link-arg=" + str(output / "raw.obj")]
    target = output / ("tests.exe" if args.tests else "preview_capabilities.exe" if args.preview else "dng_jxl_bench.exe")
    main_source = ROOT / "experiments/preview/capabilities.rs" if args.preview else SOURCE / "main.rs"
    subprocess.run(common + [str(main_source)] + links + (["--test"] if args.tests else []) + ["-o", str(target)], env=env, check=True)
    if args.tests:
        subprocess.run([str(target), "--nocapture"], env=env, check=True)
        return
    if args.preview:
        print(output)
        return
    subprocess.run(common + [str(SOURCE / "io_bench.rs"), "-o", str(OUT / "dng_io_bench.exe")], env=env, check=True)
    tico = SOURCE / "tico_probe.cpp"
    if tico.is_file():
        subprocess.run([compiler, "/nologo", "/std:c++17", "/EHsc", "/MD", "/O2", "/D_ITERATOR_DEBUG_LEVEL=0", str(tico),
                        "/Fo" + str(OUT / "tico_probe.obj"), "/Fe" + str(OUT / "tico_probe.exe"), "/link", "bcrypt.lib", "version.lib"], env=env, check=True)
    identity = {"libraw": pin, "rustc": subprocess.check_output([str(rustc), "--version"], text=True).strip(),
                "optimization": "rustc opt-level=3 harness; existing Cargo debug dependencies opt-level=2", "binaries": [p.name for p in OUT.glob("*.exe")]}
    (OUT / "build.json").write_text(json.dumps(identity, indent=2) + "\n", encoding="utf-8")
    print(OUT)


if __name__ == "__main__":
    main()
