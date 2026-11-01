"""Read-only Windows statistics profile using existing optimized debug dependencies.

Builds an isolated test harness, never the application or a release executable.
Uses process Working Set (RSS) and Private Bytes, sampled approximately every 2 ms.
Example: python scripts/profile_statistics.py --edge 512 D:/Photos/halo/a.dng D:/Photos/halo/b.dng
"""
import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument("--edge", type=int, choices=[512, 1536], default=512)
p.add_argument("--single", action="store_true")
p.add_argument("--compare-arena", action="store_true")
p.add_argument("--tests", action="store_true")
p.add_argument("--output", type=Path)
p.add_argument("paths", nargs="*", type=Path)
args = p.parse_args()
assert args.tests or len(args.paths) == 2, "Pass the requested two paths (only A is decoded with --single)"
out = ROOT / "target/statistics-profile"
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
# Use the real metric/decode sources. The extracted mailbox has no GPU/UI dependencies.
services = (ROOT / "src/services.rs").read_text(encoding="utf-8")
mailbox = services[services.index("pub struct Mailbox<T>"):services.index("pub enum RenderJob")]
harness = out / "harness.rs"
harness.write_text(r'''#![allow(dead_code)]
#[path = "../../src/decode.rs"] mod decode;
#[path = "../../src/dng.rs"] mod dng;
#[path = "../../src/lfu.rs"] mod lfu;
#[path = "../../src/noise.rs"] mod noise;
#[path = "../../src/metrics.rs"] mod metrics;
mod model {
    pub type PhotoId = u64;
    pub fn is_raw(p: &std::path::Path) -> bool {
        p.extension().is_some_and(|e| ["dng", "nef", "arw", "cr3"].iter().any(|s| e.eq_ignore_ascii_case(s)))
    }
}
mod services {
    use std::{sync::{Mutex, Condvar}, thread};
    use crossbeam_channel::{unbounded, Sender};
''' + mailbox + "\n}\n", encoding="utf-8")
deps = ROOT / "target/debug/deps"
lib = max((ROOT / "target/debug/build").glob("*/out/diff_raw.lib"), key=lambda x: x.stat().st_mtime)
z = max((ROOT / "target/debug/build").glob("libz-sys-*/out/lib/z.lib"), key=lambda x: x.stat().st_mtime)
cmd = [str(Path.home() / ".cargo/bin/rustc.exe"), "--edition=2021", "--test", str(harness),
       "-L", "dependency=" + str(deps), "-L", "native=" + str(lib.parent), "-L", "native=" + str(z.parent),
       "-l", "static=diff_raw", "-l", "static=z", "-l", "ws2_32", "-C", "opt-level=2",
       "-C", "link-arg=" + str(out / "raw.obj")]
for name in ["anyhow", "image", "memmap2", "rayon", "jxl_oxide", "sha2", "bytemuck", "ort",
             "serde", "serde_json", "crossbeam_channel", "ssimulacra2"]:
    rlib = max(deps.glob("lib" + name + "-*.rlib"), key=lambda x: x.stat().st_mtime)
    cmd += ["--extern", name + "=" + str(rlib)]
exe = out / "tests.exe"
subprocess.run(cmd + ["-o", str(exe)], env=env, check=True)
shutil.copy2(ROOT / "vendor/onnxruntime/lib/onnxruntime.dll", out / "onnxruntime.dll")
(out / "models").mkdir(exist_ok=True)
shutil.copy2(ROOT / "assets/models/lpips-squeeze-v0.1.onnx", out / "models/lpips-squeeze-v0.1.onnx")
if args.tests:
    raise SystemExit(subprocess.run([str(exe), "--nocapture"], env=env).returncode)
env.update(SCRUTIVUE_METRICS_PROFILE="1", SCRUTIVUE_PROFILE_METRIC_EDGE=str(args.edge),
           SCRUTIVUE_PROFILE_PAIR_A=str(args.paths[0].resolve()),
           SCRUTIVUE_PROFILE_PAIR_B=str(args.paths[1].resolve()))
if args.single:
    env["SCRUTIVUE_PROFILE_SINGLE"] = "1"
else:
    env.pop("SCRUTIVUE_PROFILE_SINGLE", None)
if args.compare_arena:
    env["SCRUTIVUE_PROFILE_COMPARE_ARENA"] = "1"
else:
    env.pop("SCRUTIVUE_PROFILE_COMPARE_ARENA", None)

class ProcessMemory(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
        (name, ctypes.c_size_t) for name in ["PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
        "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage",
        "PeakPagefileUsage", "PrivateUsage"]]

kernel = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel.OpenProcess.restype = wintypes.HANDLE
kernel.CloseHandle.argtypes = [wintypes.HANDLE]
psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(ProcessMemory), wintypes.DWORD]
process = subprocess.Popen([str(exe), "metrics::tests::profile_dng_statistics", "--exact", "--ignored",
                            "--nocapture", "--test-threads=1"], env=env, stdout=subprocess.PIPE,
                           stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace", bufsize=1)
handle = kernel.OpenProcess(0x0400 | 0x0010, False, process.pid)
assert handle, ctypes.get_last_error()
lock = threading.Lock()
stack, completed, results = [], [], {}
peak = {"rss_bytes": 0, "private_bytes": 0, "samples": 0}
done = threading.Event()
sample_times = []
winmm = ctypes.WinDLL("winmm")
timer_resolution_changed = winmm.timeBeginPeriod(1) == 0

def memory():
    info = ProcessMemory()
    info.cb = ctypes.sizeof(info)
    if psapi.GetProcessMemoryInfo(handle, ctypes.byref(info), info.cb):
        return {"rss_bytes": info.WorkingSetSize, "private_bytes": info.PrivateUsage}
    return {"rss_bytes": 0, "private_bytes": 0}

def sample():
    while not done.wait(0.002):
        m = memory()
        with lock:
            sample_times.append(time.perf_counter())
            peak["samples"] += 1
            for name, value in m.items():
                peak[name] = max(peak[name], value)
                for stage in stack:
                    stage["peak_" + name] = max(stage["peak_" + name], value)

sampler = threading.Thread(target=sample, daemon=True)
sampler.start()
for line in process.stdout:
    if "STATISTICS_PROFILE\t" in line:
        fields = line[line.index("STATISTICS_PROFILE\t"):].rstrip().split("\t")
        m = memory()
        with lock:
            for name, value in m.items():
                peak[name] = max(peak[name], value)
                for active in stack:
                    active["peak_" + name] = max(active["peak_" + name], value)
            if fields[1] == "begin":
                stack.append(dict(label=fields[2], start=m, **{"peak_" + k: v for k, v in m.items()}))
            else:
                stage = stack.pop()
                assert stage["label"] == fields[2], (stage, line)
                stage.update(ms=float(fields[3]), end=m)
                completed.append(stage)
        if fields[2] != "source_lookup":
            print(line, end="", flush=True)
    elif line.startswith("STATISTICS_RESULT\t"):
        _, label, value = line.rstrip().split("\t", 2)
        results[label] = json.loads(value)
        print(line, end="", flush=True)
    else:
        print(line, end="", flush=True)
returncode = process.wait()
done.set()
sampler.join()
kernel.CloseHandle(handle)
if timer_resolution_changed:
    winmm.timeEndPeriod(1)
intervals = [(b - a) * 1000 for a, b in zip(sample_times, sample_times[1:])]
report = dict(edge=args.edge, single=args.single, interval_ms=2,
              actual_mean_interval_ms=sum(intervals) / len(intervals) if intervals else None,
              actual_max_interval_ms=max(intervals) if intervals else None,
              peak=peak, stages=completed, results=results, returncode=returncode)
destination = args.output or out / f"profile-{'single' if args.single else 'pair'}-{args.edge}.json"
destination.write_text(json.dumps(report, indent=2), encoding="utf-8")
print("Profile saved to", destination)
raise SystemExit(returncode)
