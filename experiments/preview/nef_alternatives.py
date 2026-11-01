"""Read-only six-NEF alternatives probe; no downloads, builds, or guessed ABI.

The default action inventories existing sources/runtime without decoding. Probe
children run sequentially with a fresh Imprint handle for each RGB size. The
intoPIX path invokes only the existing hash-pinned helper CLI at original size.
No embedded JPEG/thumbnail entry point is called. See nef-alternatives-notes.md.
"""

import argparse
import ctypes as C
from datetime import datetime, timezone
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[2]
TARGET = ROOT / "target"
IMPRINT = TARGET / "dng-jxl-imprint"
PIPELINE = Path("E:/Documents/AI/REA_DXO/pipeline-graph")
CORPUS = Path("E:/Documents/AI/corpus/3 этаж single/3 этаж single")
NAMES = tuple(f"AK8_{number}.NEF" for number in ("0005", "0036", "0149", "0988", "1588", "1714"))
COMMIT = "3916a114bc32b487de611135cf66514447c541a6"
DLL_SHA = "5523606c669ad499ab1c8ee7571a638d4651d1d42e2fa489ccbc874ba6e8eb06"
HELPER_SHA = "fce4f2e3bd537e5e66995ad4491f8c1ded7e0307ef30a3de8041bb7ffdce44b0"
FRAMEWORK_SHA = "f3064b4bf321402e6cce16388e135bb6b15de824ed717a6074a53784c9c662c0"
MANIFEST_SHA = "59aa78cce27ee5a0b2ac36d767e75da4f5083e6431f2e422f1e060fa0dc4321b"
CAP = b"CONTACT_INTOPIX_\xef\xc0" + bytes(14)
PIH_TAIL = bytes.fromhex("000000002058159000000010040408124410511450887083f01523d149cd3f7f07")
CDT = bytes.fromhex("0e11510e11510e11000e1151")
WGT = bytes.fromhex("0302031302090216010a011502050103011100010007000d000b02060104011000000008000e01140012000f0017000c0018")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")


def timed(function, *args):
    start = time.perf_counter_ns()
    result = function(*args)
    return result, (time.perf_counter_ns() - start) / 1e6


def memory_snapshot():
    from ctypes import wintypes

    class Counters(C.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (key, C.c_size_t) for key in (
                "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
                "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage", "PrivateUsage")]

    counters = Counters()
    counters.cb = C.sizeof(counters)
    kernel, psapi = C.WinDLL("kernel32", use_last_error=True), C.WinDLL("psapi", use_last_error=True)
    kernel.GetCurrentProcess.restype = wintypes.HANDLE
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, C.POINTER(Counters), wintypes.DWORD]
    require(psapi.GetProcessMemoryInfo(kernel.GetCurrentProcess(), C.byref(counters), counters.cb), "Read process memory failed")
    return {"peak_ws_bytes": counters.PeakWorkingSetSize, "ws_bytes": counters.WorkingSetSize,
            "private_bytes": counters.PrivateUsage, "page_faults": counters.PageFaultCount}


def new_output(path):
    path = path.resolve()
    require(path.is_relative_to(TARGET.resolve()) and path != TARGET.resolve(),
            "Outputs must be in a new subdirectory of this repository's target/")
    path.mkdir(parents=True, exist_ok=False)
    return path


class ReadLocks:
    """Deny concurrent write/delete while hashing and using Windows artifacts."""

    def __init__(self):
        self.handles = []

    def __enter__(self):
        require(os.name == "nt", "The existing native artifacts require Windows x64")
        require(C.sizeof(C.c_void_p) == 8, "The existing native artifacts require 64-bit Python")
        self.kernel = C.WinDLL("kernel32", use_last_error=True)
        self.kernel.CreateFileW.argtypes = [C.c_wchar_p, C.c_uint32, C.c_uint32, C.c_void_p,
                                           C.c_uint32, C.c_uint32, C.c_void_p]
        self.kernel.CreateFileW.restype = C.c_void_p
        self.kernel.CloseHandle.argtypes = [C.c_void_p]
        self.kernel.CloseHandle.restype = C.c_int
        return self

    def add(self, path):
        handle = self.kernel.CreateFileW(str(Path(path).resolve(strict=True)), 0x80000000,
                                         1, None, 3, 0x80, None)
        require(handle not in (None, C.c_void_p(-1).value),
                f"Cannot acquire read-only artifact lock: {path}; Windows error {C.get_last_error()}")
        self.handles.append(handle)

    def __exit__(self, *_):
        for handle in reversed(self.handles):
            self.kernel.CloseHandle(handle)


def imprint_module():
    spec = importlib.util.spec_from_file_location("existing_imprint_probe", ROOT / "experiments/dng-jxl/imprint_probe.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def imprint_identity(locks=None):
    manifest_path = IMPRINT / "sources.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    require(manifest["commit"] == COMMIT, "Unexpected Imprint source commit")
    verified = []
    for item in manifest["files"]:
        path = IMPRINT / "source" / item["path"]
        if locks:
            locks.add(path)
        require(digest(path) == item["sha256"], f"Pinned source differs: {path}")
        verified.append({"path": str(path), "sha256": item["sha256"]})
    dll = IMPRINT / "build/Release/imprint_raw.dll"
    if locks:
        locks.add(dll)
    require(digest(dll) == DLL_SHA, "Existing Imprint DLL differs from the recorded Release build")
    build = json.loads((IMPRINT / "build.json").read_text(encoding="utf-8"))
    require(build["dll_sha256"] == DLL_SHA and build["commit"] == COMMIT,
            "Existing Imprint build provenance differs")
    return {"commit": COMMIT, "dll": str(dll), "dll_sha256": DLL_SHA,
            "libraw_commit": build["libraw_commit"], "libraw_archive_sha256": build["libraw_archive_sha256"],
            "verified_sources": verified, "source_modifications": []}


def ipx_identity(pipeline, locks=None):
    helper = pipeline / "resources/native/ipx/dxo_ipx_helper.exe"
    framework = pipeline / "resources/native/ipx/dxo-6.8.0.7/DxOFramework.dll"
    manifest = framework.parent / "manifest.json"
    for path, expected in ((helper, HELPER_SHA), (framework, FRAMEWORK_SHA), (manifest, MANIFEST_SHA)):
        if locks:
            locks.add(path)
        require(digest(path) == expected, f"Certified intoPIX artifact differs: {path}")
    runtime = json.loads(manifest.read_text(encoding="utf-8"))
    for item in runtime["files"]:
        name = item["name"]
        require(Path(name).name == name and not any(c in name for c in "/\\:"), "Unsafe runtime manifest name")
        path = framework.parent / name
        if locks:
            locks.add(path)
        require(path.stat().st_size == item["bytes"] and digest(path) == item["sha256"],
                f"Certified runtime file differs: {path}")
    source_files = ("native/dxo_ipx_helper.cpp", "crates/raw/src/ipx.rs", "crates/raw/src/tiff.rs",
                    "crates/raw/src/ipx_header.rs")
    return {"helper": str(helper), "helper_sha256": HELPER_SHA, "framework": str(framework),
            "framework_sha256": FRAMEWORK_SHA, "manifest_sha256": MANIFEST_SHA,
            "runtime_files": runtime["files"],
            "adapter_sources": [{"path": str(pipeline / p), "sha256": digest(pipeline / p)} for p in source_files],
            "abi_scope": "Existing helper executable and CLI only; no new private ABI calls"}


def inspect_tiff(stream, length):
    """Bounded public TIFF tags, matching the existing adapter's RAW strip route."""
    budget = 0

    def read_at(offset, size):
        nonlocal budget
        require(0 <= offset <= length and 0 <= size <= length - offset, "Truncated TIFF metadata")
        budget += size
        require(budget <= 8 * 1024 * 1024, "TIFF metadata read budget exceeded")
        stream.seek(offset)
        data = stream.read(size)
        require(len(data) == size, "Short TIFF metadata read")
        return data

    head = read_at(0, 8)
    require(head[:2] in (b"II", b"MM"), "Not a TIFF container")
    endian = "<" if head[:2] == b"II" else ">"
    require(struct.unpack_from(endian + "H", head, 2)[0] == 42, "Only classic TIFF is supported")
    queue = [(struct.unpack_from(endian + "I", head, 4)[0], False)]
    seen, pages, identity = set(), [], {}
    wanted = {256, 257, 258, 259, 262, 271, 272, 273, 274, 277, 278, 279, 322, 323, 324, 325, 330, 339}
    while queue:
        offset, nested = queue.pop(0)
        if not offset:
            continue
        require(offset >= 8 and offset not in seen and len(seen) < 64, "Invalid/cyclic TIFF IFD graph")
        seen.add(offset)
        count = struct.unpack(endian + "H", read_at(offset, 2))[0]
        require(count <= 4096, "TIFF tag count exceeds bounds")
        entries = read_at(offset + 2, count * 12 + 4)
        page = {}
        for index in range(count):
            entry = entries[index * 12:index * 12 + 12]
            tag, kind, n = struct.unpack_from(endian + "HHI", entry)
            if tag not in wanted:
                continue
            require(tag not in page and kind in (1, 2, 3, 4, 13), "Unsupported/duplicate TIFF route tag")
            size = n * {1: 1, 2: 1, 3: 2, 4: 4, 13: 4}[kind]
            require(0 < size <= 65536, "TIFF route tag exceeds bounds")
            value = entry[8:8 + size] if size <= 4 else read_at(struct.unpack_from(endian + "I", entry, 8)[0], size)
            if kind == 2:
                page[tag] = value.rstrip(b"\0").decode("ascii", errors="strict")
            else:
                page[tag] = list(struct.unpack(endian + {1: "B", 3: "H", 4: "I", 13: "I"}[kind] * n, value))
        for tag, name in ((271, "make"), (272, "model"), (274, "tiff_orientation")):
            if tag in page and name not in identity:
                identity[name] = page[tag] if tag in (271, 272) else page[tag][0]
        queue.extend((child, True) for child in page.get(330, []))
        queue.append((struct.unpack_from(endian + "I", entries, count * 12)[0], nested))
        pages.append((nested, page))
    raw = [page for nested, page in pages if nested and page.get(262) == [32803]]
    require(len(raw) == 1, "Exactly one nested CFA IFD is required by the existing IPX adapter")
    raw = raw[0]

    def scalar(tag, default=None):
        values = raw.get(tag, [default] if default is not None else [])
        require(len(values) == 1, f"Expected one TIFF value for tag {tag}")
        return values[0]

    width, height = scalar(256), scalar(257)
    offset, count = scalar(273), scalar(279)
    bits = scalar(258)
    require(0 < width <= 32000 and 0 < height <= 32000 and width * height <= 128000000, "Invalid RAW dimensions")
    require(offset >= 8 and 155 <= count <= 256 * 1024 * 1024 and offset + count <= length, "Invalid RAW strip bounds")
    require(bits in (12, 14, 16) and scalar(277, 1) == scalar(339, 1) == 1 and
            scalar(278) >= height and scalar(259) == 34713 and not any(t in raw for t in (322, 323, 324, 325)),
            "Outside the existing IPX single-strip representation")
    prefix = read_at(offset, 155)
    require(b"CONTACT_INTOPIX" in prefix[:32], "RAW strip is not recognized IPX")
    return dict(identity, width=width, height=height, bits=bits, offset=offset, count=count,
                header_155_hex=prefix.hex(), metadata_bytes_read=budget)


def validate_ipx_profile(metadata):
    """Exact unchanged profile from pipeline-graph/crates/raw/src/ipx_header.rs."""
    require(metadata.get("make", "").upper() in ("NIKON", "NIKON CORPORATION") and
            metadata.get("model") in ("NIKON Z 8", "Z 8") and
            (metadata["width"], metadata["height"], metadata["bits"]) == (8280, 5520, 14),
            "Outside the existing verified Z8 full-frame 14-bit helper profile")
    expected = b"\xff\x10"
    for marker, body in ((0xff50, CAP), (0xff12, struct.pack(">I", metadata["count"]) + PIH_TAIL),
                         (0xff13, CDT), (0xff14, WGT), (0xff20, b"\0\0")):
        expected += struct.pack(">HH", marker, len(body) + 2) + body
    require(len(expected) == 155 and bytes.fromhex(metadata["header_155_hex"]) == expected,
            "Unknown IPX extension/profile; no decode submitted and no header rewritten")


def metadata(path):
    with path.open("rb") as stream:
        return inspect_tiff(stream, path.stat().st_size)


def capabilities():
    return {
        "imprint": {"open": "open_file + complete unpack + metadata; combined C API timing",
                    "rgb": ["full", "half_size after full CFA unpack"],
                    "postprocess_params": ["half_size", "use_camera_wb", "no_auto_bright", "output_bps",
                                           "output_color", "gamma[2]", "has_user_wb", "user_wb[4]",
                                           "has_user_flip", "user_flip"],
                    "decode_scale": False, "decode_roi": False, "crop_parameter": False,
                    "entropy_reconstruction_split": "Not exposed by the C ABI"},
        "ipx": {"decode": "Existing DecodeIpxData(payload, original-size destination) helper CLI",
                "decode_scale": False, "decode_roi": False, "slice_parameter": False,
                "rgb": False, "half_size": False,
                "timing": "Helper setup/decode/host-pack/read/write; entropy/reconstruction combined"},
        "host_roi": "Post-full-decode buffer crop only; never partial RAW decoding",
        "thumbnail_or_embedded_jpeg": "Never requested by this runner",
        "sdk_scope": "No official SDK header with scale/slice/ROI is present in the inspected adapter surface"}


def artifact(data, output, label, details, retain):
    start = time.perf_counter_ns()
    row = dict(details, label=label, sha256=hashlib.sha256(data).hexdigest(), bytes=len(data), path=None)
    row["hash_ms"] = (time.perf_counter_ns() - start) / 1e6
    if retain:
        path = output / (label + ".u16")
        start = time.perf_counter_ns()
        with path.open("xb") as stream:
            stream.write(data)
        row.update(path=str(path), write_ms=(time.perf_counter_ns() - start) / 1e6)
    save(output / (label + ".artifact.json"), row)
    return row


def host_roi(data, output, label, details, roi):
    width, height, channels = details["width"], details["height"], details["channels"]
    if roi == "center512":
        w, h = min(512, width), min(512, height)
        x, y = ((width - w) // 2) & ~1, ((height - h) // 2) & ~1
    else:
        x, y, w, h = [int(part) for part in roi.split(",")]
    require(x >= 0 and y >= 0 and w > 0 and h > 0 and x + w <= width and y + h <= height,
            "ROI exceeds this variant's unrotated output bounds")
    start = time.perf_counter_ns()
    stride, pixel = width * channels * 2, channels * 2
    packed = b"".join(data[(y + row) * stride + x * pixel:(y + row) * stride + (x + w) * pixel]
                      for row in range(h))
    crop_ms = (time.perf_counter_ns() - start) / 1e6
    row = artifact(packed, output, label + "-host-roi", dict(details, width=w, height=h,
                   parent_width=width, parent_height=height, roi=[x, y, w, h],
                   work_scope="Host crop after complete decode; not partial RAW decode"), True)
    row["host_crop_ms"] = crop_ms
    save(output / (label + "-host-roi.artifact.json"), row)
    return row


def imprint_probe(args, source, output, source_hash, locks):
    memory = {"before_identity_load": memory_snapshot()}
    identity = imprint_identity(locks)
    old = imprint_module()
    native = source
    copy_ms = 0.0
    if not str(source).isascii():
        native = output / (source.stem + ".nef")
        _, copy_ms = timed(shutil.copy2, source, native)
        require(str(native).isascii(), "Imprint char* API requires an ASCII output path for this corpus")
        require(digest(native) == source_hash, "ASCII input copy hash mismatch")
        locks.add(native)
    os.environ["IMPRINT_TICORAW_BACKEND"] = "cpu"
    dll, load_ms = timed(old.load_api)
    error, selected = C.create_string_buffer(4096), C.c_int32()
    encoded = str(native).encode("ascii")
    code, probe_ms = timed(dll.imprint_raw_probe_nikon_he, encoded, C.byref(selected), error, len(error))
    native_stat = native.stat()
    result = {"identity": identity, "native_path": str(native), "ascii_copy_ms": copy_ms,
              "ascii_copy_used": native != source,
              "ascii_copy_metadata": "shutil.copy2 when needed; independent copy content hash verified",
              "native_input_sha256": source_hash, "native_input_bytes": native_stat.st_size,
              "native_input_mtime_ns": native_stat.st_mtime_ns,
              "dll_load_ms": load_ms, "metadata_probe": {"ms": probe_ms, "returncode": code,
              "is_nikon_he": bool(selected.value), "error": error.value.decode(errors="replace")}, "artifacts": []}
    require(code == 0 and selected.value, f"Imprint does not select this actual NEF: {result['metadata_probe']}")
    info, handle = old.Info(), C.c_void_p()
    error.value = b""
    code, open_ms = timed(dll.imprint_raw_open, encoded, C.byref(handle), C.byref(info), error, len(error))
    memory["after_full_unpack"] = memory_snapshot()
    result["open_including_full_unpack"] = {"ms": open_ms, "returncode": code,
                                           "error": error.value.decode(errors="replace")}
    result["native_calls_to_cfa_ms"] = open_ms
    try:
        require(code == 0 and handle.value, f"Imprint full open/unpack failed: {result['open_including_full_unpack']}")
        result["backend_actual"] = dll.imprint_raw_decoder_backend(handle).decode("ascii")
        require(result["backend_actual"] == "c++/cpu", "Imprint CPU backend was not used")
        sizes = {name: getattr(info.sizes, name) for name, _ in old.Sizes._fields_}
        result["metadata"] = {"make": info.make.decode(errors="replace"), "model": info.model.decode(errors="replace"),
                              "nef_compression": info.compression, "sizes": sizes, "filters": info.filters,
                              "raw_pattern_size": info.raw_pattern_size,
                              "raw_pattern": list(info.raw_pattern)[:info.raw_pattern_size ** 2],
                              "camera_whitebalance": list(info.camera_whitebalance),
                              "black_level_per_channel": list(info.black_level_per_channel), "white_level": info.white_level}
        count = sizes["width"] * sizes["height"]
        require(0 < count <= 128000000, "Imprint visible buffer exceeds bounds")
        samples, alloc_ms = timed(lambda: (C.c_uint16 * count)())
        error.value = b""
        code, copy_ms = timed(dll.imprint_raw_copy_visible, handle, samples, count, error, len(error))
        require(code == 0, f"Imprint CFA copy failed: {error.value!r}")
        result.update(caller_cfa_zero_allocation_ms=alloc_ms, copy_visible_ms=copy_ms)
        memory["caller_cfa_ready"] = memory_snapshot()
        details = {"kind": "visible-cfa", "source_sha256": source_hash, "width": sizes["width"],
                   "height": sizes["height"], "channels": 1, "bits": 16, "dtype": "<u2",
                   "visible_origin": [sizes["left_margin"], sizes["top_margin"]],
                   "orientation_applied": False, "metadata_flip": sizes["flip"],
                   "sample_domain": "unscaled camera DN", "metadata": result["metadata"]}
        view = memoryview(samples).cast("B")
        result["artifacts"].append(artifact(view, output, "visible-cfa", details, args.save_cfa))
        result["artifacts"].append(host_roi(view, output, "visible-cfa", details, args.roi))
        del view, samples
        if args.variant != "cfa":
            params = old.Params()
            params.half_size = int(args.variant == "half")
            params.use_camera_wb = params.no_auto_bright = 1
            params.output_bps, params.output_color = 16, 0
            params.gamma[:] = (1.0, 1.0)
            params.has_user_flip, params.user_flip = 1, 0
            data, size = C.c_void_p(), C.c_size_t()
            width, height, channels, bits = (C.c_uint32() for _ in range(4))
            error.value = b""
            code, render_ms = timed(dll.imprint_raw_postprocess, handle, C.byref(params), C.byref(data), C.byref(size),
                                    C.byref(width), C.byref(height), C.byref(channels), C.byref(bits), error, len(error))
            result["postprocess"] = {"ms": render_ms, "returncode": code, "error": error.value.decode(errors="replace"),
                                     "params": {"half_size": params.half_size, "use_camera_wb": 1, "no_auto_bright": 1,
                                     "output_bps": 16, "output_color": 0, "gamma": [1.0, 1.0],
                                     "has_user_wb": 0, "has_user_flip": 1, "user_flip": 0}}
            result["native_calls_to_rgb_ms"] = open_ms + render_ms
            memory["rgb_ready"] = memory_snapshot()
            require(code == 0, f"Imprint RGB rendering failed: {result['postprocess']}")
            require(channels.value == 3 and bits.value == 16 and data.value and 0 < size.value <= 1024 ** 3 and
                    size.value == width.value * height.value * 6, "Unexpected Imprint RGB representation")
            pixels = memoryview((C.c_ubyte * size.value).from_address(data.value)).cast("B")
            rgb_details = dict(details, kind="camera-linear-rgb", width=width.value, height=height.value,
                               channels=3, sample_domain="LibRaw camera RGB16, camera WB, linear gamma, no auto bright",
                               variant=args.variant, processing_params=result["postprocess"]["params"])
            result["artifacts"].append(artifact(pixels, output, "camera-linear-rgb", rgb_details, args.save_rgb))
            result["artifacts"].append(host_roi(pixels, output, "camera-linear-rgb", rgb_details, args.roi))
            del pixels
    finally:
        _, result["close_ms"] = timed(dll.imprint_raw_close, handle)
    result["timing_scope"] = "open includes construction, I/O, allocations, complete entropy/lifting/reconstruction; no internal split exposed"
    result["memory"] = memory
    return result


def ipx_probe(args, source, output, source_hash, locks):
    identity = ipx_identity(args.pipeline, locks)
    meta, header_ms = timed(metadata, source)
    validate_ipx_profile(meta)
    payload = output / "payload.ipx"
    start = time.perf_counter_ns()
    with source.open("rb") as src, payload.open("xb") as dst:
        src.seek(meta["offset"])
        left = meta["count"]
        while left:
            chunk = src.read(min(left, 1024 * 1024))
            require(bool(chunk), "Short IPX strip read")
            dst.write(chunk)
            left -= len(chunk)
    extract_ms = (time.perf_counter_ns() - start) / 1e6
    locks.add(payload)
    raw, native_report = output / "visible-cfa.u16", output / "helper-report.json"
    command = [identity["helper"], "--framework", identity["framework"], "--payload", str(payload),
               "--width", str(meta["width"]), "--height", str(meta["height"]), "--repeats", "1",
               "--output", str(raw), "--report", str(native_report)]
    start = time.perf_counter_ns()
    child = subprocess.run(command, capture_output=True, timeout=args.timeout, check=False,
                           cwd=str(Path(identity["helper"]).parent))
    external_ms = (time.perf_counter_ns() - start) / 1e6
    (output / "helper-stdout.log").write_bytes(child.stdout)
    (output / "helper-stderr.log").write_bytes(child.stderr)
    require(child.returncode == 0, f"Existing IPX helper failed ({child.returncode}); see {output}")
    report = json.loads(native_report.read_text(encoding="utf-8"))
    require(report.get("success") is True and report.get("framework_sha256") == FRAMEWORK_SHA and
            report.get("rendered_rgb") is False and report.get("normalization_applied") is False,
            "IPX report success/identity/domain mismatch")
    for key, expected in (("width", meta["width"]), ("height", meta["height"]), ("sample_type", 3),
                          ("sample_layout", 7), ("logical_channels", 4), ("interlacing_channels", 1), ("native_image_bytes", 640)):
        require(report.get(key) == expected, f"IPX output representation mismatch: {key}")
    require(len(report["trials"]) == 1 and raw.stat().st_size == meta["width"] * meta["height"] * 2 and
            digest(raw) == report["trials"][0]["pixel_sha256"], "IPX CFA byte count/hash mismatch")
    details = {"kind": "visible-cfa", "source_sha256": source_hash, "width": meta["width"],
               "height": meta["height"], "channels": 1, "bits": 16, "dtype": "<u2", "visible_origin": [0, 0],
               "orientation_applied": False, "sample_domain": "unscaled camera DN", "metadata": meta}
    # This is only the full-size Z8 profile, whose verified adapter has zero margins.
    with raw.open("rb") as stream:
        data = stream.read()
    descriptor = dict(details, path=str(raw), sha256=report["trials"][0]["pixel_sha256"], bytes=len(data), label="visible-cfa")
    save(output / "visible-cfa.artifact.json", descriptor)
    roi = host_roi(data, output, "visible-cfa", details, args.roi)
    return {"identity": identity, "metadata": meta, "header_parse_ms": header_ms,
            "payload_extract_read_and_write_ms": extract_ms, "payload_sha256": digest(payload),
            "helper_command": command, "helper_external_wall_ms": external_ms, "native_report": report,
            "artifacts": [descriptor, roi], "timing_scope": report["timing_scope"],
            "unsupported_variants": ["half", "partial", "slice", "ROI", "RGB"],
            "native_helper_memory_scope": "The existing external helper exposes no process memory counters; Python worker RSS is not its decoder RSS"}


def worker(args):
    source = args.input[0].resolve(strict=True)
    output = new_output(args.output)
    record = {"source": str(source), "backend": args.backend, "variant": args.variant,
              "started_utc": datetime.now(timezone.utc).isoformat(), "success": False}
    with ReadLocks() as locks:
        locks.add(source)
        before = source.stat()
        source_hash = digest(source)
        record.update(source_sha256=source_hash, source_bytes=before.st_size, source_mtime_ns=before.st_mtime_ns)
        try:
            probe = imprint_probe if args.backend == "imprint" else ipx_probe
            record["work"] = probe(args, source, output, source_hash, locks)
            record["success"] = True
        except Exception as exc:
            record["error"] = f"{type(exc).__name__}: {exc}"
        finally:
            after = source.stat()
            record["source_unchanged"] = (before.st_size, before.st_mtime_ns, source_hash) == (
                after.st_size, after.st_mtime_ns, digest(source))
            record["success"] &= record["source_unchanged"]
            record["finished_utc"] = datetime.now(timezone.utc).isoformat()
            save(output / "case.json", record)
    print(json.dumps({"success": record["success"], "case": str(output / "case.json")}))
    return 0 if record["success"] else 2


def compare_descriptors(actual_path, reference_path):
    actual = json.loads(actual_path.read_text(encoding="utf-8"))
    reference = json.loads(reference_path.read_text(encoding="utf-8"))
    keys = ["source_sha256", "kind", "width", "height", "channels", "bits", "dtype", "orientation_applied", "visible_origin", "sample_domain"]
    if actual.get("roi") is not None or reference.get("roi") is not None:
        keys += ["roi", "parent_width", "parent_height"]
    if actual["kind"] != "visible-cfa":
        keys += ["variant", "processing_params"]
    mismatches = {key: [actual.get(key), reference.get(key)] for key in keys
                  if key not in actual or key not in reference or actual[key] != reference[key]}
    result = {"actual": str(actual_path), "reference": str(reference_path), "geometry_or_policy_mismatches": mismatches}
    if mismatches:
        return dict(result, comparable=False, exact=None, note="No pixel comparison across mismatched coordinates/policy")
    if actual["sha256"] == reference["sha256"]:
        return dict(result, comparable=True, exact=True, method="Declared native-order uint16 pixel hashes")
    if not actual.get("path") or not reference.get("path"):
        return dict(result, comparable=True, exact=False, method="Different declared hashes; pixel files not both retained")
    import numpy as np
    paths = [Path(value["path"]) for value in (actual, reference)]
    paths = [p if p.is_absolute() else origin.parent / p for p, origin in zip(paths, (actual_path, reference_path))]
    for path, value in zip(paths, (actual, reference)):
        require(digest(path) == value["sha256"], f"Artifact descriptor hash mismatch: {path}")
        require(path.stat().st_size == actual["width"] * actual["height"] * actual["channels"] * 2,
                f"Artifact descriptor dimensions/length mismatch: {path}")
    a, b = (np.memmap(p, dtype="<u2", mode="r") for p in paths)
    count, maximum, total = 0, 0, 0
    for offset in range(0, len(a), 1024 * 1024):
        delta = np.abs(a[offset:offset + 1024 * 1024].astype(np.int32) - b[offset:offset + 1024 * 1024].astype(np.int32))
        count += int(np.count_nonzero(delta))
        maximum = max(maximum, int(delta.max(initial=0)))
        total += int(delta.sum(dtype=np.int64))
    return dict(result, comparable=True, exact=count == 0, nonzero_samples=count,
                max_absolute_dn=maximum, mean_absolute_dn=total / len(a), method="Chunked uint16 sample comparison")


def compare_libraw_cfa(record, reference_root):
    """Compare visible/ROI pixels to the parent's retained full-sensor CFA."""
    import numpy as np
    manifest = json.loads((reference_root / "manifest.json").read_text(encoding="utf-8"))
    measurements = json.loads((reference_root / "measurements.json").read_text(encoding="utf-8"))
    source = Path(record["source"])
    require(manifest.get("source_hashes_unchanged") is True and
            manifest["sources"][source.name]["sha256"] == record["source_sha256"],
            "Parent reference source identity is not verified for this actual NEF")
    rows = [r for r in measurements if r.get("source_name") == source.name and r.get("case") == "raw-full-reference"]
    require(len(rows) == 1, "Expected one parent full-sensor CFA reference record")
    meta = rows[0]
    rw, rh = int(meta["raw_width"]), int(meta["raw_height"])
    require(meta["channels"] == 1 and int(meta["output_width"]) == rw and int(meta["output_height"]) == rh,
            "Parent reference is not full-sensor CFA")
    path = reference_root / f"{source.stem}-full-cfa.u16"
    require(path.stat().st_size == rw * rh * 2, "Parent full-sensor CFA byte length mismatch")
    reference = np.memmap(path, dtype="<u2", mode="r", shape=(rh, rw))
    comparisons = []
    for descriptor in record["work"]["artifacts"]:
        if descriptor["kind"] != "visible-cfa":
            continue
        x, y = descriptor["visible_origin"]
        w, h = descriptor["width"], descriptor["height"]
        if "roi" in descriptor:
            x += descriptor["roi"][0]
            y += descriptor["roi"][1]
        else:
            require((w, h, x, y) == (int(meta["active_width"]), int(meta["active_height"]),
                    int(meta["left_margin"]), int(meta["top_margin"])),
                    "Alternative visible bounds differ from parent; automatic CFA comparison refused")
        require(descriptor["orientation_applied"] is False and x >= 0 and y >= 0 and x + w <= rw and y + h <= rh,
                "Alternative CFA coordinates are not comparable to parent sensor coordinates")
        hasher = hashlib.sha256()
        for row in range(y, y + h):
            hasher.update(reference[row, x:x + w].tobytes())
        expected_hash = hasher.hexdigest()
        comparison = {"label": descriptor["label"], "reference": str(path), "reference_sensor_roi": [x, y, w, h],
                      "reference_sha256_of_region": expected_hash, "actual_sha256": descriptor["sha256"],
                      "exact": expected_hash == descriptor["sha256"],
                      "domain": "Unrotated, unscaled CFA DN; no crop/rotation/color conversion reconciliation"}
        if not comparison["exact"] and descriptor.get("path"):
            require(digest(descriptor["path"]) == descriptor["sha256"], "Alternative CFA artifact changed")
            actual = np.memmap(descriptor["path"], dtype="<u2", mode="r", shape=(h, w))
            count, maximum, total = 0, 0, 0
            for first in range(0, h, 128):
                delta = np.abs(actual[first:first + 128].astype(np.int32) -
                               reference[y + first:y + min(first + 128, h), x:x + w].astype(np.int32))
                count += int(np.count_nonzero(delta))
                maximum = max(maximum, int(delta.max(initial=0)))
                total += int(delta.sum(dtype=np.int64))
            comparison.update(nonzero_samples=count, max_absolute_dn=maximum, mean_absolute_dn=total / (w * h))
        comparisons.append(comparison)
    comparisons.append({"kind": "RGB", "comparable": False,
                        "reason": "Parent uses sRGB/bilinear; Imprint exposes camera RGB and no user_qual. Cross-backend RGB parity is not assumed."})
    return comparisons


def self_test():
    # In-memory classic TIFF fixture only: no actual NEF decode, DLL load or build.
    for endian, magic in (("<", b"II"), (">", b"MM")):
        data = bytearray(magic + struct.pack(endian + "HI", 42, 8))
        data += struct.pack(endian + "HHHII", 1, 330, 4, 1, 26) + struct.pack(endian + "I", 0)
        fields = [(256, 8280), (257, 5520), (258, 14), (259, 34713), (262, 32803),
                  (273, 256), (277, 1), (278, 5520), (279, 155), (339, 1)]
        data += struct.pack(endian + "H", len(fields))
        for tag, value in fields:
            data += struct.pack(endian + "HHII", tag, 4, 1, value)
        data += struct.pack(endian + "I", 0)
        data += bytes(256 - len(data))
        prefix = b"\xff\x10"
        for marker, body in ((0xff50, CAP), (0xff12, struct.pack(">I", 155) + PIH_TAIL),
                             (0xff13, CDT), (0xff14, WGT), (0xff20, b"\0\0")):
            prefix += struct.pack(">HH", marker, len(body) + 2) + body
        require(len(prefix) == 155, "Profile constant size mismatch")
        data += prefix
        parsed = inspect_tiff(io.BytesIO(data), len(data))
        require((parsed["width"], parsed["height"], parsed["offset"]) == (8280, 5520, 256), "TIFF fixture mismatch")
        parsed.update(make="NIKON CORPORATION", model="NIKON Z 8")
        validate_ipx_profile(parsed)
        bad = dict(parsed, header_155_hex=(prefix[:20] + bytes([prefix[20] ^ 1]) + prefix[21:]).hex())
        try:
            validate_ipx_profile(bad)
        except RuntimeError:
            pass
        else:
            raise RuntimeError("Modified extension was not rejected")
        try:
            inspect_tiff(io.BytesIO(data[:-1]), len(data) - 1)
        except RuntimeError:
            pass
        else:
            raise RuntimeError("Truncated payload was not rejected")
    print(json.dumps({"success": True, "self_test": "in-memory TIFF endian/profile/truncation", "native_decode_invoked": False}))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--action", choices=("inventory", "probe", "worker", "compare", "self-test"), default="inventory")
    parser.add_argument("--backend", choices=("imprint", "ipx", "both"), default="both")
    parser.add_argument("--input", type=Path, action="append", default=[])
    parser.add_argument("--pipeline", type=Path, default=PIPELINE)
    parser.add_argument("--variant", choices=("full", "half", "cfa"), default="full", help="Worker-only; probe uses --variants")
    parser.add_argument("--variants", default="full,half", help="Imprint variants, comma separated: full,half,cfa")
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--roi", default="center512", help="Host-only crop in each output's unrotated coordinates: center512 or x,y,w,h")
    parser.add_argument("--save-rgb", action="store_true", help="Retain full RGB16 artifacts in addition to hashes and small ROI")
    parser.add_argument("--save-cfa", action="store_true", help="Worker-only; probe retains CFA for run 1/first Imprint variant")
    parser.add_argument("--output", type=Path, default=TARGET / "preview-nef-alternatives")
    parser.add_argument("--actual", type=Path, help="Actual .artifact.json for compare")
    parser.add_argument("--reference", type=Path, help="Reference .artifact.json for compare")
    parser.add_argument("--libraw-references", type=Path,
                        help="Parent nef_corpus.py output directory; compares raw CFA in explicit sensor coordinates")
    args = parser.parse_args()
    require(1 <= args.runs <= 20 and 1 <= args.timeout <= 3600, "Invalid repeat/timeout bounds")
    if args.action == "self-test":
        return self_test()
    if args.action == "compare":
        require(args.actual is not None and args.reference is not None, "compare requires --actual and --reference descriptors")
        result = compare_descriptors(args.actual.resolve(strict=True), args.reference.resolve(strict=True))
        print(json.dumps(result, indent=2))
        return 0 if result.get("exact") else 2
    if args.action == "worker":
        require(len(args.input) == 1 and args.backend != "both", "worker needs one input and one backend")
        return worker(args)
    inputs = [p.resolve(strict=True) for p in (args.input or [CORPUS / name for name in NAMES])]
    require(all(p.suffix.lower() == ".nef" for p in inputs), "Actual NEF inputs are required")
    variants = args.variants.split(",")
    require(bool(variants) and len(variants) == len(set(variants)) and all(v in ("full", "half", "cfa") for v in variants),
            "--variants must be unique full,half,cfa values; no unsupported partial knobs")
    out = new_output(args.output)
    report = {"command": [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]], "capabilities": capabilities(),
              "sequential_children": True, "cache_scope": "Warm OS cache; source hashes/ASCII copies precede decode; no cold-drive claim",
              "inputs": [], "cases": []}
    if args.action == "inventory":
        if args.backend in ("both", "imprint"):
            report["imprint_identity"] = imprint_identity()
        if args.backend in ("both", "ipx"):
            report["ipx_identity"] = ipx_identity(args.pipeline)
        for source in inputs:
            before = source.stat()
            row = {"source": str(source), "bytes": before.st_size}
            try:
                row["metadata"] = metadata(source)
                validate_ipx_profile(row["metadata"])
                row["verified_ipx_header_profile"] = True
            except Exception as exc:
                row["header_error"] = f"{type(exc).__name__}: {exc}"
            after = source.stat()
            row["size_mtime_unchanged"] = (before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns)
            report["inputs"].append(row)
        report["inventory_scope"] = "Bounded TIFF/IPX headers only; no source pixel decode, no source SHA-256 claim"
        save(out / "inventory.json", report)
        print(json.dumps({"success": True, "decoded": False, "inventory": str(out / "inventory.json")}))
        return 0
    failures = 0
    first_cfa = {}
    for run in range(1, args.runs + 1):
        for source in inputs:
            jobs = []
            if args.backend in ("both", "imprint"):
                jobs.extend(("imprint", v) for v in variants)
            if args.backend in ("both", "ipx"):
                jobs.append(("ipx", "cfa"))
            for backend, variant in jobs:
                case = out / f"run-{run:02}" / source.stem / f"{backend}-{variant}"
                command = [sys.executable, str(Path(__file__).resolve()), "--action", "worker", "--backend", backend,
                           "--variant", variant, "--input", str(source), "--output", str(case), "--pipeline", str(args.pipeline),
                           "--roi", args.roi, "--timeout", str(args.timeout)]
                if run == 1 and backend == "imprint" and variant == variants[0]:
                    command.append("--save-cfa")
                if args.save_rgb:
                    command.append("--save-rgb")
                start = time.perf_counter_ns()
                try:
                    child = subprocess.run(command, capture_output=True, timeout=args.timeout + 120, check=False)
                    status = child.returncode
                    case.mkdir(parents=True, exist_ok=True)
                    (case / "worker-stdout.log").write_bytes(child.stdout)
                    (case / "worker-stderr.log").write_bytes(child.stderr)
                except subprocess.TimeoutExpired:
                    status = -1
                row = {"run": run, "source": str(source), "backend": backend, "variant": variant,
                       "returncode": status, "external_wall_ms": (time.perf_counter_ns() - start) / 1e6,
                       "case": str(case / "case.json")}
                if (case / "case.json").is_file():
                    row["result"] = json.loads((case / "case.json").read_text(encoding="utf-8"))
                result = row.get("result", {})
                if status == 0 and result.get("success"):
                    cfa = next(a for a in result["work"]["artifacts"] if a["label"] == "visible-cfa")
                    key = (result["source_sha256"], backend)
                    if key not in first_cfa:
                        first_cfa[key] = cfa
                    baseline = first_cfa[key]
                    comparable = all(cfa[k] == baseline[k] for k in
                                     ("width", "height", "visible_origin", "orientation_applied", "sample_domain"))
                    row["same_backend_full_cfa"] = {"comparable": comparable,
                                                    "exact": comparable and cfa["sha256"] == baseline["sha256"],
                                                    "baseline_path": baseline.get("path")}
                    if not row["same_backend_full_cfa"]["exact"]:
                        failures += 1
                    other = first_cfa.get((result["source_sha256"], "imprint" if backend == "ipx" else "ipx"))
                    if other and other.get("path"):
                        try:
                            row["cross_backend_cfa"], row["cross_backend_comparison_ms"] = timed(
                                compare_descriptors, case / "visible-cfa.artifact.json",
                                Path(other["path"]).with_suffix(".artifact.json"))
                            failures += int(not row["cross_backend_cfa"].get("exact"))
                        except Exception as exc:
                            row["cross_backend_error"] = f"{type(exc).__name__}: {exc}"
                            failures += 1
                    if args.libraw_references:
                        try:
                            row["libraw_reference_comparisons"], row["libraw_reference_comparison_ms"] = timed(
                                compare_libraw_cfa, result, args.libraw_references.resolve(strict=True))
                            failures += int(any(c.get("exact") is False for c in row["libraw_reference_comparisons"]))
                        except Exception as exc:
                            row["libraw_reference_error"] = f"{type(exc).__name__}: {exc}"
                            failures += 1
                failures += int(status != 0 or not row.get("result", {}).get("success"))
                report["cases"].append(row)
                save(out / "report.json", report)
                print(json.dumps({key: row[key] for key in ("run", "source", "backend", "variant", "returncode")}), flush=True)
    report.update(success=failures == 0, failures=failures)
    save(out / "report.json", report)
    print(json.dumps({"success": failures == 0, "cases": len(report["cases"]), "report": str(out / "report.json")}))
    return 0 if failures == 0 else 2


if __name__ == "__main__":
    raise SystemExit(main())
