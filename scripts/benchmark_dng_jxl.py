"""Read-only DNG/JXL experiments: codecs, I/O, allocator probe and optional IPX recognition.

All output belongs to a new directory under target/. Runs use fresh processes;
Windows file cache is not flushed. No UI, source writes or DLL redistribution.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import ctypes as c
from ctypes import wintypes as w
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / "target/dng-jxl-experiments"


class Memory(c.Structure):
    _fields_ = [("cb", w.DWORD), ("faults", w.DWORD)] + [(name, c.c_size_t) for name in (
        "peak_rss", "rss", "peak_paged", "paged", "peak_nonpaged", "nonpaged", "pagefile", "peak_pagefile", "private")]


def execute(command, timeout=240):
    """Measure child memory outside its hot path; preserve OS-reported peak RSS too."""
    process = subprocess.Popen([str(arg) for arg in command], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    kernel = c.WinDLL("kernel32", use_last_error=True)
    psapi = c.WinDLL("psapi", use_last_error=True)
    kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
    kernel.OpenProcess.restype = w.HANDLE
    kernel.CloseHandle.argtypes = [w.HANDLE]
    psapi.GetProcessMemoryInfo.argtypes = [w.HANDLE, c.POINTER(Memory), w.DWORD]
    handle = kernel.OpenProcess(0x410, False, process.pid)
    stop = threading.Event()
    samples = []
    observed = {"sampled_peak_rss_bytes": 0, "sampled_peak_private_bytes": 0, "os_peak_rss_bytes": 0, "sample_period_ms": 2}

    def sample():
        while not stop.is_set():
            counters = Memory(cb=c.sizeof(Memory))
            if handle and psapi.GetProcessMemoryInfo(handle, c.byref(counters), counters.cb):
                observed["sampled_peak_rss_bytes"] = max(observed["sampled_peak_rss_bytes"], counters.rss)
                observed["sampled_peak_private_bytes"] = max(observed["sampled_peak_private_bytes"], counters.private)
                observed["os_peak_rss_bytes"] = max(observed["os_peak_rss_bytes"], counters.peak_rss)
                samples.append((time.perf_counter(), counters.rss, counters.private))
            stop.wait(0.002)

    thread = threading.Thread(target=sample, daemon=True)
    thread.start()
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        process.kill()
        stdout, stderr = process.communicate()
        raise RuntimeError(f"Experiment exceeded {timeout} seconds: {command[0]}")
    finally:
        stop.set()
        thread.join()
        if handle:
            kernel.CloseHandle(handle)
        samples.append((time.perf_counter(), 0, 0))
    text = stdout.decode("utf-8", errors="replace")
    try:
        result = json.loads(text.strip())
    except json.JSONDecodeError:
        result = {"success": False, "error": "No valid JSON result", "stdout": text[-8000:]}
    result["exit_code"] = process.returncode
    result["parent_observed_memory"] = observed
    if stderr:
        result["stderr"] = stderr.decode("utf-8", errors="replace")[-16000:]
    return result, samples


def digest(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def command(path, backend, threads, library, mode="buffered"):
    native = backend.startswith("libjxl")
    result = [BIN / "dng_jxl_bench.exe", "--backend", "libjxl" if native else backend,
              "--threads", str(threads), "--io", mode]
    if native:
        if not library:
            raise SystemExit("Native backends require --library ABSOLUTE_TRUSTED_JXL_DLL")
        result += ["--library", library]
        if backend == "libjxl-reuse":
            result.append("--reuse")
    return result + [path]


def pair_peak(traces):
    state = [(0, 0)] * len(traces)
    events = sorted((timestamp, index, rss, private) for index, trace in enumerate(traces) for timestamp, rss, private in trace)
    peaks = [0, 0]
    for _, index, rss, private in events:
        state[index] = rss, private
        peaks = [max(peaks[column], sum(value[column] for value in state)) for column in (0, 1)]
    return {"sampled_combined_peak_rss_bytes": peaks[0], "sampled_combined_peak_private_bytes": peaks[1], "sample_period_ms": 2}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("paths", nargs="+", type=Path)
    parser.add_argument("--library", type=Path)
    parser.add_argument("--tico-framework", type=Path)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--threads", default="1,6")
    parser.add_argument("--backends", default="production,oxide,libjxl,libjxl-reuse")
    parser.add_argument("--pair", action="store_true", help="Also measure first two inputs in two fresh processes, 3 workers each")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if os.name != "nt":
        raise SystemExit("Windows-only measurement harness")
    if args.runs < 1:
        raise SystemExit("Runs must be positive")
    threads = [int(value) for value in args.threads.split(",")]
    if not threads or any(not 1 <= value <= 64 for value in threads):
        raise SystemExit("Threads must be between 1 and 64")
    paths = [path.resolve(strict=True) for path in args.paths]
    if len({path.name for path in paths}) != len(paths):
        raise SystemExit("Input basenames must be distinct for unambiguous result grouping")
    if args.pair and len(paths) < 2:
        raise SystemExit("--pair requires two inputs")
    backends = args.backends.split(",")
    if any(name not in {"production", "oxide", "libjxl", "libjxl-reuse"} for name in backends):
        raise SystemExit("Unknown backend")
    if "production" not in backends or len(set(backends)) != len(backends):
        raise SystemExit("The matrix needs one production baseline and distinct backend names")
    library = args.library.resolve(strict=True) if args.library else None
    output = args.output or ROOT / f"target/dng-jxl-results-{time.time_ns()}"
    output = output.resolve()
    if not output.is_relative_to((ROOT / "target").resolve()):
        raise SystemExit("Measurement outputs must stay below this repository's target directory")
    output.mkdir(parents=True, exist_ok=False)
    results = {"schema": 1, "date": time.strftime("%Y-%m-%d"), "platform": platform.platform(),
               "logical_cpus": os.cpu_count(), "build": json.loads((BIN / "build.json").read_text()),
               "cache_boundary": "Fresh process for every case, source hashing warms OS cache before trials; no cache flush; no UI/GPU",
               "runs": [], "io_runs": [], "allocator_probes": [], "tico_probes": [], "pair_runs": []}
    identities = {str(path): {"name": path.name, "bytes": path.stat().st_size, "sha256_before": digest(path)} for path in paths}
    if library:
        results["library"] = {"name": library.name, "sha256": digest(library)}
        manifest = library.with_name("bundle.json")
        if manifest.is_file():
            bundle = json.loads(manifest.read_text())
            checked = []
            for entry in bundle["files"]:
                file = library.parent / Path(entry["path"]).relative_to("native/libjxl-0.12.0")
                if file.stat().st_size != entry["bytes"] or digest(file) != entry["sha256"]:
                    raise SystemExit(f"Local libjxl bundle integrity failure: {file.name}")
                checked.append(file.name)
            results["library"]["verified_bundle_files"] = checked

    def save():
        (output / "results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")

    for run in range(args.runs):
        order = backends[run % len(backends):] + backends[:run % len(backends)]
        for path in paths:
            for count in threads:
                for backend in order:
                    result, _ = execute(command(path, backend, count, library))
                    result.update(experiment_backend=backend, run=run + 1)
                    results["runs"].append(result)
                    save()
                    print(f'{path.name} {backend} threads={count} run={run + 1}: {result.get("pipeline_ms_excluding_setup_validation", "FAILED")} ms', flush=True)

    for path in paths:
        for mode in ["buffered", "mmap", "direct"]:
            for run in range(args.runs):
                result, _ = execute([BIN / "dng_io_bench.exe", mode, path])
                result["run"] = run + 1
                results["io_runs"].append(result)
        if library:
            result, _ = execute(command(path, "libjxl-reuse", max(threads), library)[:-1] + ["--trace-alloc", path])
            results["allocator_probes"].append(result)
        if args.tico_framework:
            first = output / f"{len(results['tico_probes'])}-first.jxl"
            extracted, _ = execute([BIN / "dng_jxl_bench.exe", "--extract-tile", first, path])
            if not extracted.get("success"):
                raise RuntimeError(f"Could not extract representative tile: {extracted}")
            count = extracted["layout"]["tiles"]
            for index in sorted({0, count // 2, count - 1}):
                tile = first
                if index:
                    tile = output / f"{len(results['tico_probes'])}-tile-{index}.jxl"
                    extracted, _ = execute([BIN / "dng_jxl_bench.exe", "--extract-tile", tile, "--tile-index", index, path])
                    if not extracted.get("success"):
                        raise RuntimeError(f"Could not extract tile {index}: {extracted}")
                probe, _ = execute([BIN / "tico_probe.exe", "--framework", args.tico_framework.resolve(strict=True), "--payload", tile])
                probe.update(source_name=path.name, tile_index=index)
                results["tico_probes"].append(probe)
        save()

    if args.pair:
        for run in range(args.runs):
            for backend in [name for name in backends if name != "oxide"]:
                start = time.perf_counter()
                with ThreadPoolExecutor(max_workers=2) as workers:
                    futures = [workers.submit(execute, command(path, backend, 3, library)) for path in paths[:2]]
                    completed = [future.result() for future in futures]
                wall_ms = (time.perf_counter() - start) * 1000
                results["pair_runs"].append({"backend": backend, "run": run + 1,
                    "pair_wall_ms_including_setup_validation": wall_ms, "workers_per_process": 3, "worker_processes": 2,
                    "scope": "Two separate 3-worker pools, not the application's shared pool; wall includes process startup, validation hashes and cleanup",
                    "children": [value for value, _ in completed], "memory": pair_peak([trace for _, trace in completed])})
                save()
                print(f'Pair {backend} 3+3 workers run={run + 1}: {wall_ms:.3f} ms', flush=True)

    references = {}
    for result in results["runs"]:
        if result.get("backend") == "production" and result.get("success"):
            identity = tuple(result[key] for key in ["source_sha256", "canvas_sha256", "rgb16_sha256"])
            old = references.setdefault(result["source_name"], identity)
            if identity != old:
                raise RuntimeError("Production reference is not deterministic")
    mismatches = []
    for result in results["runs"] + results["allocator_probes"] + [child for pair in results["pair_runs"] for child in pair["children"]]:
        name = result.get("source_name")
        identity = tuple(result.get(key) for key in ["source_sha256", "canvas_sha256", "rgb16_sha256"])
        result["matches_production_hashes"] = bool(name in references and identity == references[name])
        if not result.get("success") or not result["matches_production_hashes"]:
            mismatches.append({"source": name, "backend": result.get("backend"), "error": result.get("error"), "identity": identity})
    for path in paths:
        identity = identities[str(path)]
        identity["sha256_after"] = digest(path)
        identity["source_unchanged"] = identity["sha256_before"] == identity["sha256_after"]
    expected_sources = {value["name"]: value["sha256_before"] for value in identities.values()}
    io_errors = [value for value in results["io_runs"] if not value.get("success") or
                 value.get("sha256") != expected_sources.get(value.get("source_name"))]
    probe_errors = [value for value in results["tico_probes"] if not value.get("probe_completed") or value.get("exit_code") != 0]
    results["sources"] = list(identities.values())
    results["validation"] = {"mismatches": mismatches, "io_errors": io_errors, "compatibility_probe_errors": probe_errors,
                             "all_sources_unchanged": all(value["source_unchanged"] for value in identities.values())}
    save()
    rows = []
    for path in paths:
        for count in threads:
            for backend in backends:
                group = [value for value in results["runs"] if value.get("source_name") == path.name and value["experiment_backend"] == backend and value["threads"] == count and value.get("success")]
                if not group:
                    continue
                median = lambda fn: statistics.median(fn(value) for value in group)
                rows.append({"source": path.name, "backend": backend, "threads": count, "n": len(group),
                    "pipeline_ms": median(lambda r: r["pipeline_ms_excluding_setup_validation"]),
                    "transport_ms": median(lambda r: r["transport"]["total_ms"]),
                    "parallel_tiles_ms": median(lambda r: r["transport"]["parallel_tiles_ms"]),
                    "development_ms": median(lambda r: r["development"]["total_ms"]),
                    "io_ms": median(lambda r: r["io"]["total_ms"]),
                    "peak_rss_bytes": median(lambda r: r["memory"]["peak_rss_bytes"]),
                    "sampled_peak_private_bytes": median(lambda r: r["parent_observed_memory"]["sampled_peak_private_bytes"]),
                    "hashes_match": all(r["matches_production_hashes"] for r in group)})
    if rows:
        with (output / "summary.csv").open("w", encoding="utf-8", newline="") as file:
            writer = csv.DictWriter(file, fieldnames=list(rows[0]))
            writer.writeheader()
            writer.writerows(rows)
    print(output)
    if mismatches or io_errors or probe_errors or not results["validation"]["all_sources_unchanged"]:
        raise SystemExit("Validation failed; inspect results.json; no backend is approved for production")


if __name__ == "__main__":
    main()
