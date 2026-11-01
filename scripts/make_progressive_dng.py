"""Create explicitly LOSSY progressive-JXL LinearRaw DNG test copies.

Never replaces an input/output. The original TIFF metadata/preview payloads are
retained; a new raw IFD points to newly encoded progressive tiles. Unreferenced
original compressed raw bytes remain in the copy (larger, safer test fixture).
Requires Python 3.11+, NumPy and the audited libjxl bundle pinned by the project.
"""
import argparse
import ctypes as C
import hashlib
import json
from pathlib import Path
import struct
import time

import numpy as np

ROOT = Path(__file__).resolve().parents[1]


class Basic(C.Structure):
    _fields_ = [("container", C.c_int), ("width", C.c_uint32), ("height", C.c_uint32),
        ("bits", C.c_uint32), ("exponent", C.c_uint32), ("intensity", C.c_float), ("min_nits", C.c_float),
        ("relative", C.c_int), ("linear_below", C.c_float), ("original", C.c_int), ("preview", C.c_int),
        ("animation", C.c_int), ("orientation", C.c_int), ("channels", C.c_uint32), ("extras", C.c_uint32),
        ("alpha_bits", C.c_uint32), ("alpha_exponent", C.c_uint32), ("alpha_premultiplied", C.c_int),
        ("preview_size", C.c_uint32 * 2), ("animation_info", C.c_uint32 * 4), ("intrinsic", C.c_uint32 * 2),
        ("padding", C.c_uint8 * 100)]


class Format(C.Structure):
    _fields_ = [("channels", C.c_uint32), ("kind", C.c_int), ("endian", C.c_int), ("align", C.c_size_t)]


class Color(C.Structure):
    _fields_ = [("space", C.c_int), ("white", C.c_int), ("white_xy", C.c_double * 2), ("primaries", C.c_int),
        ("red", C.c_double * 2), ("green", C.c_double * 2), ("blue", C.c_double * 2),
        ("transfer", C.c_int), ("gamma", C.c_double), ("intent", C.c_int)]


def checked(status):
    if status != 0:
        raise RuntimeError(f"libjxl operation failed: {status}")


def library(directory):
    pin = json.loads((ROOT / "config/libjxl.json").read_text())
    for name, expected in pin["files"].items():
        assert hashlib.sha256((directory / name).read_bytes()).hexdigest() == expected, f"Runtime mismatch: {name}"
    dll = C.CDLL(str((directory / "jxl.dll").resolve()), winmode=0x1100)
    signatures = {
        "JxlDecoderCreate": ([C.c_void_p], C.c_void_p), "JxlDecoderDestroy": ([C.c_void_p], None),
        "JxlDecoderSubscribeEvents": ([C.c_void_p, C.c_int], C.c_int),
        "JxlDecoderSetKeepOrientation": ([C.c_void_p, C.c_int], C.c_int),
        "JxlDecoderSetProgressiveDetail": ([C.c_void_p, C.c_int], C.c_int),
        "JxlDecoderSetInput": ([C.c_void_p, C.c_void_p, C.c_size_t], C.c_int),
        "JxlDecoderProcessInput": ([C.c_void_p], C.c_int),
        "JxlDecoderGetBasicInfo": ([C.c_void_p, C.POINTER(Basic)], C.c_int),
        "JxlDecoderSetImageOutBuffer": ([C.c_void_p, C.POINTER(Format), C.c_void_p, C.c_size_t], C.c_int),
        "JxlDecoderGetIntendedDownsamplingRatio": ([C.c_void_p], C.c_size_t),
        "JxlDecoderFlushImage": ([C.c_void_p], C.c_int),
        "JxlEncoderCreate": ([C.c_void_p], C.c_void_p), "JxlEncoderDestroy": ([C.c_void_p], None),
        "JxlEncoderInitBasicInfo": ([C.POINTER(Basic)], None),
        "JxlEncoderSetBasicInfo": ([C.c_void_p, C.POINTER(Basic)], C.c_int),
        "JxlEncoderSetColorEncoding": ([C.c_void_p, C.POINTER(Color)], C.c_int),
        "JxlEncoderFrameSettingsCreate": ([C.c_void_p, C.c_void_p], C.c_void_p),
        "JxlEncoderFrameSettingsSetOption": ([C.c_void_p, C.c_int, C.c_int64], C.c_int),
        "JxlEncoderSetFrameDistance": ([C.c_void_p, C.c_float], C.c_int),
        "JxlEncoderAddImageFrame": ([C.c_void_p, C.POINTER(Format), C.c_void_p, C.c_size_t], C.c_int),
        "JxlEncoderCloseInput": ([C.c_void_p], None),
        "JxlEncoderProcessOutput": ([C.c_void_p, C.POINTER(C.c_void_p), C.POINTER(C.c_size_t)], C.c_int),
    }
    for name, (args, restype) in signatures.items():
        fn = getattr(dll, name)
        fn.argtypes, fn.restype = args, restype
    assert C.sizeof(Basic) == 204 and C.sizeof(Color) == 104
    return dll


def decode(dll, payload, progression=False):
    decoder = dll.JxlDecoderCreate(None)
    assert decoder
    data = C.create_string_buffer(payload)
    pixels = None
    ratios = []
    try:
        checked(dll.JxlDecoderSubscribeEvents(decoder, 0x40 | 0x1000 | (0x8000 if progression else 0)))
        checked(dll.JxlDecoderSetKeepOrientation(decoder, 1))
        if progression:
            checked(dll.JxlDecoderSetProgressiveDetail(decoder, 2))
        checked(dll.JxlDecoderSetInput(decoder, data, len(payload)))
        info = Basic()
        for _ in range(4096):
            status = dll.JxlDecoderProcessInput(decoder)
            if status == 0x40:
                checked(dll.JxlDecoderGetBasicInfo(decoder, C.byref(info)))
                assert info.original and info.channels == 3 and info.bits == 16 and not info.exponent and not info.animation
                assert 0 < info.width <= 16384 and 0 < info.height <= 16384 and info.orientation == 1
            elif status == 5:
                pixels = np.empty((info.height, info.width, 3), dtype=np.uint16)
                fmt = Format(3, 3, 0, 0)
                checked(dll.JxlDecoderSetImageOutBuffer(decoder, C.byref(fmt), pixels.ctypes.data, pixels.nbytes))
            elif status == 0x8000:
                ratios.append(dll.JxlDecoderGetIntendedDownsamplingRatio(decoder))
                # Validate every advertised stage, then continue the SAME decoder.
                checked(dll.JxlDecoderFlushImage(decoder))
            elif status == 0x1000:
                assert pixels is not None
                return pixels, ratios
            else:
                raise RuntimeError(f"JXL decode failed: {status}")
        raise RuntimeError("JXL event limit exceeded")
    finally:
        dll.JxlDecoderDestroy(decoder)


def encode(dll, pixels, distance):
    encoder = dll.JxlEncoderCreate(None)
    assert encoder
    pixels = np.ascontiguousarray(pixels, dtype=np.uint16)
    try:
        info = Basic()
        dll.JxlEncoderInitBasicInfo(C.byref(info))
        info.width, info.height = pixels.shape[1], pixels.shape[0]
        info.bits, info.channels, info.original = 16, 3, 1
        checked(dll.JxlEncoderSetBasicInfo(encoder, C.byref(info)))
        # Camera-linear sample values are passed through, not converted to sRGB.
        # DNG calibration remains responsible for their actual camera color space.
        color = Color()
        color.space, color.white, color.primaries, color.transfer, color.intent = 0, 1, 1, 8, 1
        checked(dll.JxlEncoderSetColorEncoding(encoder, C.byref(color)))
        settings = dll.JxlEncoderFrameSettingsCreate(encoder, None)
        # Disable display-oriented smoothing for camera-linear RAW samples.
        for option, value in [(0, 4), (9, 0), (10, 0), (11, 0), (17, 1), (18, 0), (19, 0), (24, 1), (40, 0)]:
            checked(dll.JxlEncoderFrameSettingsSetOption(settings, option, value))
        checked(dll.JxlEncoderSetFrameDistance(settings, distance))
        fmt = Format(3, 3, 0, 0)
        checked(dll.JxlEncoderAddImageFrame(settings, C.byref(fmt), pixels.ctypes.data, pixels.nbytes))
        dll.JxlEncoderCloseInput(encoder)
        pieces = []
        while True:
            buffer = C.create_string_buffer(65536)
            pointer, available = C.c_void_p(C.addressof(buffer)), C.c_size_t(len(buffer))
            status = dll.JxlEncoderProcessOutput(encoder, C.byref(pointer), C.byref(available))
            pieces.append(buffer.raw[:len(buffer) - available.value])
            if status == 0:
                return b"".join(pieces)
            if status != 2 or available.value == len(buffer):
                raise RuntimeError(f"JXL encode failed: {status}")
    finally:
        dll.JxlEncoderDestroy(encoder)


def convert(source, output, dll, tile_size, distance):
    assert source.resolve() != output.resolve() and not output.exists()
    staging = output.with_suffix(".dng.partial")
    report_path = output.with_suffix(".dng.json")
    assert not staging.exists() and not report_path.exists(), "Use a new output name; existing artifacts are never replaced"
    started = time.perf_counter()
    stat = source.stat()
    data = source.read_bytes()
    source_hash = hashlib.sha256(data).hexdigest()
    endian = "<" if data[:4] == b"II*\0" else ">"
    assert data[:4] in (b"II*\0", b"MM\0*")
    root = struct.unpack_from(endian + "I", data, 4)[0]
    count = struct.unpack_from(endian + "H", data, root)[0]
    entries, values = {}, {}
    for i in range(count):
        at = root + 2 + i * 12
        tag, kind, n, offset = struct.unpack_from(endian + "HHII", data, at)
        unit = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 7: 1, 10: 8, 11: 4, 12: 8}[kind]
        start = at + 8 if n * unit <= 4 else offset
        payload = data[start:start + n * unit]
        assert len(payload) == n * unit
        entries[tag] = data[at:at + 12]
        if kind in (1, 3, 4):
            values[tag] = list(struct.unpack(endian + str(n) + {1: "B", 3: "H", 4: "I"}[kind], payload))
    assert values[259] == [52546] and values[262] == [34892] and values[277] == [3] and values[258] == [16, 16, 16]
    width, height = values[256][0], values[257][0]
    tw, th = values[322][0], values[323][0]
    raw = np.empty((height, width, 3), dtype=np.uint16)
    for i, (offset, length) in enumerate(zip(values[324], values[325])):
        tile, _ = decode(dll, data[offset:offset + length])
        x, y = i % ((width + tw - 1) // tw) * tw, i // ((width + tw - 1) // tw) * th
        w, h = min(tw, width - x), min(th, height - y)
        raw[y:y + h, x:x + w] = tile[:h, :w]
    output.parent.mkdir(parents=True, exist_ok=True)
    offsets, lengths, checks = [], [], []
    with staging.open("xb") as file:
        file.write(data)
        for y in range(0, height, tile_size):
            for x in range(0, width, tile_size):
                original = raw[y:y + tile_size, x:x + tile_size]
                payload = encode(dll, original, distance)
                decoded, ratios = decode(dll, payload, True)
                # Tiny physical edge tiles may not advertise every ratio.
                if original.shape[0] >= 256 and original.shape[1] >= 256:
                    assert all(ratio in ratios for ratio in (8, 4, 2)), ratios
                error = decoded.astype(np.int32) - original.astype(np.int32)
                checks.append({"x": x, "y": y, "ratios": ratios, "maximum_absolute_error": int(np.abs(error).max()),
                    "absolute_error_sum": int(np.abs(error).sum()), "changed_samples": int(np.count_nonzero(error)),
                    "samples": int(error.size), "decoded_sha256": hashlib.sha256(decoded.tobytes()).hexdigest()})
                offsets.append(file.tell())
                lengths.append(len(payload))
                file.write(payload)
        entries.pop(50781, None)
        entries.pop(50972, None)
        def field(tag, kind, n, payload):
            if len(payload) <= 4:
                value = payload.ljust(4, b"\0")
            else:
                file.write(b"\0" * (-file.tell() % 4))
                value = struct.pack(endian + "I", file.tell())
                file.write(payload)
            entries[tag] = struct.pack(endian + "HHI", tag, kind, n) + value
        for tag, numbers in [(322, [tile_size]), (323, [tile_size]), (324, offsets), (325, lengths)]:
            field(tag, 4, len(numbers), struct.pack(endian + str(len(numbers)) + "I", *numbers))
        for tag, text in [(270, f"LOSSY progressive JPEG XL test copy; distance={distance}; source={source.name}"),
                          (305, "Scrutivue progressive DNG test converter")]:
            payload = text.encode("ascii", "backslashreplace") + b"\0"
            field(tag, 2, len(payload), payload)
        file.write(b"\0" * (-file.tell() % 4))
        new_root = file.tell()
        assert new_root < 2**32 and all(o + n < 2**32 for o, n in zip(offsets, lengths))
        file.write(struct.pack(endian + "H", len(entries)))
        for tag in sorted(entries):
            file.write(entries[tag])
        file.write(data[root + 2 + count * 12:root + 6 + count * 12])
        file.seek(4)
        file.write(struct.pack(endian + "I", new_root))
    assert hashlib.sha256(source.read_bytes()).hexdigest() == source_hash
    assert source.stat().st_mtime_ns == stat.st_mtime_ns
    staging.rename(output)
    samples = sum(c["samples"] for c in checks)
    report = {"source": str(source), "output": str(output), "source_sha256": source_hash,
        "source_unchanged": True, "lossless": False, "distance": distance, "tile_size": tile_size,
        "raw_size": [width, height], "tiles": len(checks), "changed_samples": sum(c["changed_samples"] for c in checks),
        "maximum_absolute_error": max(c["maximum_absolute_error"] for c in checks),
        "mean_absolute_error": sum(c["absolute_error_sum"] for c in checks) / samples,
        "bytes": output.stat().st_size, "wall_seconds": time.perf_counter() - started,
        "note": "Retained original TIFF payloads make the test copy larger than a compact DNG. Reduced main views come from progressive raw JXL, never the embedded JPEG.", "checks": checks}
    with report_path.open("x", encoding="utf-8") as file:
        file.write(json.dumps(report, indent=2) + "\n")
    print(json.dumps({k: v for k, v in report.items() if k != "checks"}), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", type=Path, nargs="+")
    parser.add_argument("--library-directory", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument("--tile-size", type=int, choices=[256, 512, 1024, 2048], default=1024)
    parser.add_argument("--distance", type=float, default=0.1)
    parser.add_argument("--allow-lossy", action="store_true", help="Required acknowledgement; never claim RAW sample preservation")
    args = parser.parse_args()
    if not args.allow_lossy or not 0 < args.distance <= 2:
        parser.error("Pass --allow-lossy and use distance in (0, 2]")
    dll = library(args.library_directory)
    for source in args.inputs:
        convert(source, args.output_directory / f"{source.stem}-lossy-progressive.dng", dll, args.tile_size, args.distance)


if __name__ == "__main__":
    main()
