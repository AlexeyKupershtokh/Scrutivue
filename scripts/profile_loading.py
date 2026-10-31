"""Isolated, read-only full RGB16 pair profile using existing debug dependencies.

Never builds or replaces the application EXE. Windows/MSVC developer tool.
Example: python scripts/profile_loading.py --threads 2 path/to/a.dng path/to/b.dng
Process-cold, not OS-cache-cold: no standby-list flush or source writes.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import shutil

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument("--threads", type=int, default=6)
p.add_argument("--runs", type=int, default=1)
p.add_argument("--tests", action="store_true")
p.add_argument("paths", nargs="*", type=Path)
args = p.parse_args()
assert args.tests or len(args.paths) == 2, "Pass exactly two existing image paths"
assert 1 <= args.threads <= 12 and args.runs > 0
out = ROOT / "target/loading-profile"
out.mkdir(parents=True, exist_ok=True)
vswhere = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
vs = json.loads(subprocess.check_output([str(vswhere), "-latest", "-products", "*", "-format", "json"]))[0]
vcvars = Path(vs["installationPath"]) / "VC/Auxiliary/Build/vcvars64.bat"
text = subprocess.check_output(f'cmd /d /s /c ""{vcvars}" >nul && set"', text=True)
env = dict(os.environ)
env.update(line.split("=", 1) for line in text.splitlines() if "=" in line and not line.startswith("="))
env["CARGO_MANIFEST_DIR"] = str(ROOT)
compiler = shutil.which("cl.exe", path=env.get("Path", env.get("PATH")))
subprocess.run([compiler, "/nologo", "/std:c++17", "/EHsc", "/MD", "/O2", "/c", "/DLIBRAW_NODLL",
                "/D_CRT_SECURE_NO_WARNINGS", "/I" + str(ROOT / "vendor/libraw"),
                str(ROOT / "native/raw.cpp"), "/Fo" + str(out / "raw.obj")], env=env, check=True)
harness = out / "harness.rs"
harness.write_text(r'''#![allow(dead_code)]
#[path = "../../src/decode.rs"] mod decode;
#[path = "../../src/dng.rs"] mod dng;
mod model {
    pub fn is_raw(p: &std::path::Path) -> bool {
        p.extension().is_some_and(|e| ["dng", "nef", "arw", "cr3"].iter().any(|s| e.eq_ignore_ascii_case(s)))
    }
}
fn main() -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    let mut args = std::env::args_os().skip(1);
    let threads: usize = args.next().unwrap().to_string_lossy().parse()?;
    let paths: Vec<_> = args.map(std::path::PathBuf::from).collect();
    anyhow::ensure!(paths.len() == 2, "Exactly two paths required");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    let start = std::time::Instant::now();
    let frames = std::thread::scope(|scope| {
        let handles: Vec<_> = paths.iter().map(|path| {
            let pool = &pool;
            scope.spawn(move || pool.install(|| decode::decode(path)))
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect::<anyhow::Result<Vec<_>>>()
    })?;
    println!("pair_decode_ms={:.3} threads={threads}", start.elapsed().as_secs_f64()*1000.);
    for (path, frame) in paths.iter().zip(frames) {
        let mut hash = Sha256::new();
        for block in frame.rgb.chunks(16384) {
            let bytes: Vec<_> = block.iter().flat_map(|v| v.to_le_bytes()).collect();
            hash.update(bytes);
        }
        println!("{} {:?} rgb16_sha256={:x} source_sha256={:x}", path.display(), frame.size(), hash.finalize(), Sha256::digest(std::fs::read(path)?));
    }
    Ok(())
}
''', encoding="utf-8")
deps = ROOT / "target/debug/deps"
lib = max((ROOT / "target/debug/build").glob("scrutivue-*/out/diff_raw.lib"), key=lambda x: x.stat().st_mtime)
z = max((ROOT / "target/debug/build").glob("libz-sys-*/out/lib/z.lib"), key=lambda x: x.stat().st_mtime)
cmd = [str(Path.home() / ".cargo/bin/rustc.exe"), "--edition=2021", str(harness),
       "-L", "dependency=" + str(deps), "-L", "native=" + str(lib.parent), "-L", "native=" + str(z.parent),
       "-l", "static=diff_raw", "-l", "static=z", "-l", "ws2_32", "-C", "opt-level=2",
       "-C", "link-arg=" + str(out / "raw.obj")]
for name in ["anyhow", "image", "memmap2", "rayon", "jxl_oxide", "sha2"]:
    rlib = max(deps.glob("lib" + name + "-*.rlib"), key=lambda x: x.stat().st_mtime)
    cmd += ["--extern", name + "=" + str(rlib)]
exe = out / ("tests.exe" if args.tests else "profile.exe")
subprocess.run(cmd + (["--test"] if args.tests else []) + ["-o", str(exe)], env=env, check=True)
env["SCRUTIVUE_DECODE_TRACE"] = "1"
for _ in range(args.runs):
    subprocess.run([str(exe)] + (["--nocapture"] if args.tests else [str(args.threads)] + [str(x.resolve()) for x in args.paths]), env=env, check=True)
