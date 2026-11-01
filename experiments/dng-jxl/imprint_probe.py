"""Isolated Windows C++ Imprint native-raw build and read-only C API probe.

Python only downloads, builds, times C API calls and records results. All image
parsing, TicoRAW decoding and rendering run in the pinned native C++ library.
Nothing is installed; application binaries, LibRaw pins and sources are untouched.
"""

import argparse
import ctypes as C
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import urllib.request


ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "target/dng-jxl-imprint"
COMMIT = "3916a114bc32b487de611135cf66514447c541a6"
LIBRAW_COMMIT = "4abfcd2ab82f2211b1f7e7c3cd83974c87e0d812"
LIBRAW_SHA256 = "2ca1d9673531fccabcc7c8bce589444163868bcde625aa1f09eb442df1bcce2d"
SOURCE = OUT / "source"
BUILD = OUT / "build"


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "Scrutivue-isolated-Imprint-probe"})
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def dependency_archives():
    archives = sorted(set(BUILD.glob("_deps/**/archive.tar")) |
                      set(BUILD.glob("_deps/**/*" + LIBRAW_COMMIT + "*")))
    result = [{"path": str(p), "sha256": digest(p)} for p in archives if p.is_file()]
    if not result or any(item["sha256"] != LIBRAW_SHA256 for item in result):
        raise RuntimeError("Missing or mismatched CMake-downloaded LibRaw archive")
    return result


def prepare():
    api = "https://api.github.com/repos/luqie0106/Imprint/git"
    commit = json.loads(fetch(f"{api}/commits/{COMMIT}"))
    tree_url = f"{api}/trees/{commit['tree']['sha']}"
    tree = json.loads(fetch(tree_url))
    if commit.get("sha") != COMMIT or tree.get("truncated") or tree.get("sha") != commit["tree"]["sha"]:
        raise RuntimeError("GitHub returned an unexpected or truncated pinned tree")
    def subtree(parent, name, recursive=False):
        entry = next(item for item in parent["tree"] if item["path"] == name and item["type"] == "tree")
        suffix = "?recursive=1" if recursive else ""
        value = json.loads(fetch(f"{api}/trees/{entry['sha']}{suffix}"))
        if value.get("truncated") or value.get("sha") != entry["sha"]:
            raise RuntimeError("Unexpected or truncated minimal source subtree")
        return value
    native = subtree(tree, "native-raw", recursive=True)
    licenses = subtree(subtree(tree, "third_party"), "licenses")
    files = [item for item in tree["tree"] if item["type"] == "blob" and item["path"] in (
        "LICENSE", "THIRD_PARTY_NOTICES.md")]
    files += [dict(item, path="native-raw/" + item["path"]) for item in native["tree"] if item["type"] == "blob"]
    files += [dict(item, path="third_party/licenses/" + item["path"]) for item in licenses["tree"]
              if item["path"] in ("LIBRAW_COPYRIGHT.txt", "LIBRAW_LGPL_2.1.txt") and item["type"] == "blob"]
    manifest = []
    for item in files:
        relative = item["path"]
        url = f"https://raw.githubusercontent.com/luqie0106/Imprint/{COMMIT}/{relative}"
        destination = SOURCE / relative
        data = destination.read_bytes() if destination.exists() else fetch(url)
        blob = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
        if blob != item["sha"]:
            raise RuntimeError(f"Pinned Git blob mismatch; refusing to overwrite {destination}")
        destination.parent.mkdir(parents=True, exist_ok=True)
        if not destination.exists():
            destination.write_bytes(data)
        manifest.append({"path": relative, "url": url, "git_blob": blob,
                         "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)})
    cmake = (SOURCE / "native-raw/CMakeLists.txt").read_text(encoding="utf-8")
    if LIBRAW_COMMIT not in cmake or LIBRAW_SHA256 not in cmake:
        raise RuntimeError("Unexpected upstream CMake LibRaw dependency pin")
    write_json(OUT / "sources.json", {"commit": COMMIT, "tree_url": tree_url, "files": manifest,
               "libraw_commit": LIBRAW_COMMIT, "libraw_archive_sha256": LIBRAW_SHA256})


def developer_environment():
    vswhere = Path(os.environ["ProgramFiles(x86)"]) / "Microsoft Visual Studio/Installer/vswhere.exe"
    installs = json.loads(subprocess.check_output([
        str(vswhere), "-latest", "-products", "*", "-format", "json"]))
    if not installs:
        raise RuntimeError("No existing Visual Studio installation; this script installs no tools")
    vs = Path(installs[0]["installationPath"])
    vcvars = vs / "VC/Auxiliary/Build/vcvars64.bat"
    text = subprocess.check_output(f'cmd /d /s /c ""{vcvars}" >nul && set"', text=True)
    env = dict(os.environ)
    env.update(line.split("=", 1) for line in text.splitlines() if "=" in line and not line.startswith("="))
    path = env.get("Path", env.get("PATH", ""))
    cmake = shutil.which("cmake.exe", path=path)
    if not cmake:
        candidate = vs / "Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe"
        if not candidate.is_file():
            raise RuntimeError("No existing CMake installation; this script installs no tools")
        cmake = str(candidate)
    return env, cmake, installs[0]


def run_logged(command, env, name):
    started = datetime.now(timezone.utc).isoformat(timespec="milliseconds")
    start = time.perf_counter()
    with (OUT / (name + ".log")).open("w", encoding="utf-8") as log:
        result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, check=False)
    return {"command": command, "returncode": result.returncode,
            "elapsed_ms": (time.perf_counter() - start) * 1000, "log": name + ".log",
            "started_at_utc": started,
            "finished_at_utc": datetime.now(timezone.utc).isoformat(timespec="milliseconds")}


def build():
    env, cmake, vs = developer_environment()
    result = {"commit": COMMIT, "libraw_commit": LIBRAW_COMMIT,
              "libraw_archive_sha256": LIBRAW_SHA256, "configuration": "Release",
              "source_modifications": [], "visual_studio": vs["installationVersion"]}
    result["cmake_version"] = subprocess.check_output([cmake, "--version"], env=env, text=True).splitlines()[0]
    result["configure"] = run_logged([cmake, "-S", str(SOURCE / "native-raw"), "-B", str(BUILD),
        "-G", "Visual Studio 17 2022", "-A", "x64", "-DCMAKE_BUILD_TYPE=Release"], env, "configure")
    if result["configure"]["returncode"] == 0:
        result["build"] = run_logged([cmake, "--build", str(BUILD), "--config", "Release",
            "--target", "imprint_raw", "--parallel", "4"], env, "build")
    library = BUILD / "Release/imprint_raw.dll"
    result["dll_exists"] = library.is_file()
    if library.is_file():
        result["dll_sha256"] = digest(library)
    if result.get("build", {}).get("returncode", 1) == 0:
        abi_source = OUT / "abi.cpp"
        abi_source.write_text('''#include "imprint_raw.h"
#include <cstdio>
int main() {
    std::printf("{\\\"sizes\\\":%zu,\\\"info\\\":%zu,\\\"params\\\":%zu}\\n",
        sizeof(imprint_raw_sizes), sizeof(imprint_raw_info), sizeof(imprint_raw_postprocess_params));
}
''', encoding="utf-8")
        compiler = shutil.which("cl.exe", path=env.get("Path", env.get("PATH", "")))
        result["abi_compile"] = run_logged([compiler, "/nologo", "/std:c++17", "/EHsc", "/MD", "/O2",
            "/I" + str(SOURCE / "native-raw/include"), str(abi_source),
            "/Fo" + str(OUT / "abi.obj"), "/Fe" + str(OUT / "abi.exe")], env, "abi-build")
        if result["abi_compile"]["returncode"] == 0:
            abi = json.loads(subprocess.check_output([str(OUT / "abi.exe")], env=env, text=True))
            write_json(OUT / "abi.json", abi)
            result["native_abi_bytes"] = abi
    result["dependency_archives"] = dependency_archives() if result["configure"]["returncode"] == 0 else []
    cache = BUILD / "CMakeCache.txt"
    if cache.exists():
        result["zlib_cache"] = [line for line in cache.read_text(encoding="utf-8").splitlines()
                                if line.startswith("ZLIB_") and ":" in line]
    write_json(OUT / "build.json", result)
    return result


class Sizes(C.Structure):
    _fields_ = [(name, C.c_uint32) for name in (
        "raw_height", "raw_width", "height", "width", "top_margin", "left_margin", "iheight", "iwidth")] + [
        ("pixel_aspect", C.c_double), ("flip", C.c_int32)] + [(name, C.c_uint32) for name in (
        "crop_left_margin", "crop_top_margin", "crop_width", "crop_height", "raw_pitch_bytes")]


class Info(C.Structure):
    _fields_ = [("sizes", Sizes)] + [(name, C.c_uint32) for name in (
        "compression", "filters", "num_colors", "raw_pattern_size")] + [
        ("raw_pattern", C.c_uint8 * 36), ("color_desc", C.c_char * 5),
        ("make", C.c_char * 64), ("model", C.c_char * 64),
        ("lens_make", C.c_char * 128), ("lens_model", C.c_char * 128), ("artist", C.c_char * 128),
        ("camera_whitebalance", C.c_float * 4), ("daylight_whitebalance", C.c_float * 4),
        ("cam_xyz", C.c_float * 12), ("black_level_per_channel", C.c_uint32 * 4),
        ("white_level", C.c_uint32), ("has_camera_white_level_per_channel", C.c_int32),
        ("camera_white_level_per_channel", C.c_uint32 * 4)] + [(name, C.c_double) for name in (
        "iso_speed", "shutter_speed", "aperture", "focal_length")] + [
        ("timestamp", C.c_int64), ("shot_order", C.c_int32),
        ("lens_min_focal", C.c_double), ("lens_max_focal", C.c_double)]


class Params(C.Structure):
    _fields_ = [(name, C.c_int32) for name in (
        "half_size", "use_camera_wb", "no_auto_bright", "output_bps", "output_color")] + [
        ("gamma", C.c_double * 2), ("has_user_wb", C.c_int32), ("user_wb", C.c_float * 4),
        ("has_user_flip", C.c_int32), ("user_flip", C.c_int32)]


def load_api():
    native_abi = json.loads((OUT / "abi.json").read_text(encoding="utf-8"))
    python_abi = {"sizes": C.sizeof(Sizes), "info": C.sizeof(Info), "params": C.sizeof(Params)}
    if native_abi != python_abi:
        raise RuntimeError(f"Native/ctypes struct-size mismatch: {native_abi} versus {python_abi}")
    # CDLL uses the C calling convention declared by imprint_raw.h.
    dll = C.CDLL(str(BUILD / "Release/imprint_raw.dll"))
    error = [C.c_char_p, C.c_size_t]
    dll.imprint_raw_probe_nikon_he.argtypes = [C.c_char_p, C.POINTER(C.c_int32)] + error
    dll.imprint_raw_probe_nikon_he.restype = C.c_int32
    dll.imprint_raw_open.argtypes = [C.c_char_p, C.POINTER(C.c_void_p), C.POINTER(Info)] + error
    dll.imprint_raw_open.restype = C.c_int32
    dll.imprint_raw_close.argtypes = [C.c_void_p]
    dll.imprint_raw_close.restype = None
    dll.imprint_raw_decoder_backend.argtypes = [C.c_void_p]
    dll.imprint_raw_decoder_backend.restype = C.c_char_p
    dll.imprint_raw_copy_visible.argtypes = [C.c_void_p, C.POINTER(C.c_uint16), C.c_size_t] + error
    dll.imprint_raw_copy_visible.restype = C.c_int32
    dll.imprint_raw_postprocess.argtypes = [C.c_void_p, C.POINTER(Params), C.POINTER(C.c_void_p),
        C.POINTER(C.c_size_t)] + [C.POINTER(C.c_uint32)] * 4 + error
    dll.imprint_raw_postprocess.restype = C.c_int32
    return dll


def timed_call(function, *args):
    start = time.perf_counter_ns()
    result = function(*args)
    return result, (time.perf_counter_ns() - start) / 1e6


def probe_one(dll, path, run):
    path = path.resolve(strict=True)
    before = path.stat()
    source_hash = digest(path)
    native_path = path
    # The C API accepts char*, not wchar_t*. Avoid ambiguous Windows ANSI paths.
    if not str(path).isascii():
        native_path = OUT / "inputs" / (source_hash[:12] + path.suffix.lower())
        native_path.parent.mkdir(exist_ok=True)
        if not native_path.exists():
            shutil.copyfile(path, native_path)
        if digest(native_path) != source_hash:
            raise RuntimeError("ASCII-path input copy hash mismatch")
    result = {"path": str(path), "native_path": str(native_path), "run": run,
              "bytes": before.st_size, "source_sha256": source_hash, "backend_requested": "cpu",
              "abi_bytes": {"sizes": C.sizeof(Sizes), "info": C.sizeof(Info), "params": C.sizeof(Params)},
              "internal_constructor_read_decode_allocation_split": "not exposed by public C API"}
    encoded = os.fsencode(native_path)
    error = C.create_string_buffer(4096)
    selected = C.c_int32()
    code, elapsed = timed_call(dll.imprint_raw_probe_nikon_he, encoded, C.byref(selected), error, len(error))
    result["metadata_probe"] = {"returncode": code, "is_nikon_he": bool(selected.value),
                                "ms": elapsed, "error": error.value.decode("utf-8", errors="replace")}
    error.value = b""
    info, handle = Info(), C.c_void_p()
    code, elapsed = timed_call(dll.imprint_raw_open, encoded, C.byref(handle), C.byref(info), error, len(error))
    result["open_including_unpack"] = {"returncode": code, "ms": elapsed,
                                       "error": error.value.decode("utf-8", errors="replace")}
    try:
        if code == 0 and handle.value:
            result["backend_actual"] = dll.imprint_raw_decoder_backend(handle).decode("ascii")
            result["metadata"] = {"make": info.make.decode(errors="replace"),
                "model": info.model.decode(errors="replace"), "nef_compression": info.compression,
                "sizes": {name: getattr(info.sizes, name) for name, _ in Sizes._fields_}}
            count = int(info.sizes.width) * int(info.sizes.height)
            if not 0 < count <= 200_000_000:
                raise RuntimeError("Unexpected visible CFA size")
            start = time.perf_counter_ns()
            samples = (C.c_uint16 * count)()
            result["caller_cfa_zero_allocation_ms"] = (time.perf_counter_ns() - start) / 1e6
            error.value = b""
            copy_code, elapsed = timed_call(dll.imprint_raw_copy_visible, handle, samples, count, error, len(error))
            result["copy_visible"] = {"returncode": copy_code, "ms": elapsed,
                                      "error": error.value.decode(errors="replace")}
            if copy_code == 0:
                result["copy_visible"]["sha256"] = hashlib.sha256(samples).hexdigest()
            params = Params()
            params.half_size, params.use_camera_wb, params.no_auto_bright = 1, 1, 1
            params.output_bps, params.output_color = 16, 0
            params.gamma[:] = (1.0, 1.0)
            data, size = C.c_void_p(), C.c_size_t()
            width, height, channels, bits = (C.c_uint32() for _ in range(4))
            error.value = b""
            render_code, elapsed = timed_call(dll.imprint_raw_postprocess, handle, C.byref(params),
                C.byref(data), C.byref(size), C.byref(width), C.byref(height), C.byref(channels),
                C.byref(bits), error, len(error))
            result["render_half_camera_linear_rgb16"] = {"returncode": render_code, "ms": elapsed,
                "error": error.value.decode(errors="replace"), "bytes": size.value,
                "width": width.value, "height": height.value, "channels": channels.value, "bits": bits.value}
            if render_code == 0:
                if size.value > 1024 ** 3 or not data.value:
                    raise RuntimeError("Unexpected render output size or pointer")
                start = time.perf_counter_ns()
                pixels = C.string_at(data, size.value)
                result["caller_render_copy_ms"] = (time.perf_counter_ns() - start) / 1e6
                result["render_half_camera_linear_rgb16"]["sha256"] = hashlib.sha256(pixels).hexdigest()
    finally:
        _, elapsed = timed_call(dll.imprint_raw_close, handle)
        result["close_ms"] = elapsed
    after = path.stat()
    result["source_unchanged"] = (before.st_size, before.st_mtime_ns, source_hash) == (
        after.st_size, after.st_mtime_ns, digest(path))
    if not result["source_unchanged"]:
        raise RuntimeError("Input changed during read-only probe")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--action", choices=("prepare", "build", "probe", "all"), default="all")
    parser.add_argument("--input", type=Path, action="append", default=[])
    parser.add_argument("--runs", type=int, default=1)
    args = parser.parse_args()
    if not 1 <= args.runs <= 100:
        parser.error("--runs must be between 1 and 100")
    OUT.mkdir(parents=True, exist_ok=True)
    if args.action in ("prepare", "all"):
        prepare()
    if args.action in ("build", "all"):
        result = build()
        print(json.dumps(result, indent=2))
        if result["configure"]["returncode"] or result.get("build", {}).get("returncode", 1):
            raise SystemExit("Native Release build failed; see isolated logs")
    if args.action in ("probe", "all"):
        os.environ["IMPRINT_TICORAW_BACKEND"] = "cpu"
        inputs = args.input or [ROOT / "target/fixtures/raw" / name for name in (
            "Z9_3x2_HE_be91bac2.NEF", "Z9_3x2_HEstar_e113deb3.NEF")]
        dll = load_api()
        report = {"commit": COMMIT, "codec_language": "C++", "orchestrator": "Python ctypes",
                  "command": [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
                  "libraw_commit": LIBRAW_COMMIT, "backend_requested": "cpu",
                  "dll_sha256": digest(BUILD / "Release/imprint_raw.dll"), "results": []}
        for run in range(1, args.runs + 1):
            for path in inputs:
                result = probe_one(dll, path, run)
                report["results"].append(result)
                write_json(OUT / "probe.json", report)
                print(json.dumps(result, ensure_ascii=True))


if __name__ == "__main__":
    main()
