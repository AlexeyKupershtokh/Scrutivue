"""Check exact Modular+responsive output separately from libjxl ratio events.

This writes a synthetic standalone JXL control, not a converted RAW/DNG.
"""
import argparse
import ctypes as C
import importlib.util
import json
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]


def incremental_flushes(m, dll, payload, expected):
    """Exercise partial-input flushes, not the VarDCT-only ratio pause API."""
    dll.JxlDecoderReleaseInput.argtypes = [C.c_void_p]
    dll.JxlDecoderReleaseInput.restype = C.c_size_t
    decoder = dll.JxlDecoderCreate(None)
    assert decoder
    offset = 0
    pixels = None
    observations = []
    complete = False
    try:
        m.checked(dll.JxlDecoderSubscribeEvents(decoder, 0x40 | 0x400 | 0x1000))
        m.checked(dll.JxlDecoderSetKeepOrientation(decoder, 1))
        boundaries = sorted({1024, 4096, len(payload)//8, len(payload)//4,
                             len(payload)//2, len(payload)*3//4, len(payload)})
        for boundary in boundaries:
            if boundary <= offset or boundary > len(payload):
                continue
            data = C.create_string_buffer(payload[offset:boundary])
            m.checked(dll.JxlDecoderSetInput(decoder, data, boundary-offset))
            for _ in range(64):
                status = dll.JxlDecoderProcessInput(decoder)
                if status == 0x40:
                    info = m.Basic()
                    m.checked(dll.JxlDecoderGetBasicInfo(decoder, C.byref(info)))
                    assert info.original and info.bits == 16 and info.channels == 3
                    assert [info.width, info.height] == [expected.shape[1], expected.shape[0]]
                elif status == 0x400:
                    pass
                elif status == 5:
                    pixels = np.zeros_like(expected)
                    fmt = m.Format(3, 3, 0, 0)
                    m.checked(dll.JxlDecoderSetImageOutBuffer(decoder, C.byref(fmt), pixels.ctypes.data, pixels.nbytes))
                elif status == 2:
                    if pixels is not None:
                        flush = dll.JxlDecoderFlushImage(decoder)
                        assert flush in (0, 1)
                        observations.append({"supplied_bytes": boundary,
                            "input_fraction": boundary/len(payload), "flush_success": flush == 0,
                            "mean_absolute_error": float(np.abs(pixels.astype(np.int32)-expected.astype(np.int32)).mean()) if flush == 0 else None,
                            "nonzero_samples": int(np.count_nonzero(pixels)) if flush == 0 else None})
                    break
                elif status == 0x1000:
                    assert np.array_equal(pixels, expected)
                    complete = True
                    break
                else:
                    raise RuntimeError(f"Unexpected incremental decoder status: {status}")
            else:
                raise RuntimeError("Incremental event limit exceeded")
            remaining = dll.JxlDecoderReleaseInput(decoder)
            assert remaining <= boundary-offset
            offset = boundary-remaining
            if complete:
                break
        assert complete
        assert any(row["flush_success"] and row["nonzero_samples"] > 0 for row in observations)
        return {"observations": observations, "final_exact": True,
                "note": "Successful partial-input flushes do not advertise an intended 8/4/2 ratio or imply cheap repeated reconstruction."}
    finally:
        dll.JxlDecoderDestroy(decoder)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library-directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location("converter", ROOT / "scripts/make_progressive_dng.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    dll = m.library(args.library_directory)
    dll.JxlEncoderSetFrameLossless.argtypes = [C.c_void_p, C.c_int]
    dll.JxlEncoderSetFrameLossless.restype = C.c_int
    y, x = np.indices((768, 1024), dtype=np.uint32)
    pixels = np.stack([(x * 41 + y * 17) % 65536,
                       (x * 13 + y * 53) % 65536,
                       (x * 67 + y * 7) % 65536], axis=2).astype(np.uint16)
    encoder = dll.JxlEncoderCreate(None)
    assert encoder
    try:
        info = m.Basic()
        dll.JxlEncoderInitBasicInfo(C.byref(info))
        info.width, info.height, info.bits, info.channels, info.original = 1024, 768, 16, 3, 1
        m.checked(dll.JxlEncoderSetBasicInfo(encoder, C.byref(info)))
        color = m.Color()
        color.space, color.white, color.primaries, color.transfer, color.intent = 0, 1, 1, 8, 1
        m.checked(dll.JxlEncoderSetColorEncoding(encoder, C.byref(color)))
        settings = dll.JxlEncoderFrameSettingsCreate(encoder, None)
        # Effort 4; force Modular; enable responsive/Squeeze; preserve RGB;
        # normal progressive output ordering (not streaming seek output).
        for option, value in [(0, 4), (11, 1), (16, 1), (24, 1), (40, 0)]:
            m.checked(dll.JxlEncoderFrameSettingsSetOption(settings, option, value))
        m.checked(dll.JxlEncoderSetFrameLossless(settings, 1))
        fmt = m.Format(3, 3, 0, 0)
        m.checked(dll.JxlEncoderAddImageFrame(settings, C.byref(fmt), pixels.ctypes.data, pixels.nbytes))
        dll.JxlEncoderCloseInput(encoder)
        pieces = []
        while True:
            buffer = C.create_string_buffer(65536)
            pointer, available = C.c_void_p(C.addressof(buffer)), C.c_size_t(len(buffer))
            status = dll.JxlEncoderProcessOutput(encoder, C.byref(pointer), C.byref(available))
            pieces.append(buffer.raw[:len(buffer) - available.value])
            if status == 0:
                break
            assert status == 2 and available.value < len(buffer)
        payload = b"".join(pieces)
    finally:
        dll.JxlEncoderDestroy(encoder)
    decoded, ratios = m.decode(dll, payload, progression=True)
    assert np.array_equal(pixels, decoded)
    incremental = incremental_flushes(m, dll, payload, pixels)
    with (args.output / "lossless-responsive.jxl").open("xb") as file:
        file.write(payload)
    report = {"libjxl_version": "0.12.0", "size": [1024, 768], "bits": 16,
              "mode": "Modular", "responsive_option": 1, "strict_lossless": True,
              "encoded_bytes": len(payload), "final_exact": True,
              "last_passes_progression_ratios": ratios,
              "partial_input_flush": incremental,
              "note": "Lossless responsive encoding exists; this decoder's LastPasses pause API excludes Modular. Empty ratios do not mean the format lacks progressive lossless coding."}
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
