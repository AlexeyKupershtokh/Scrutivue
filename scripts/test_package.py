"""Validate the Windows ZIP and run the extracted binary with its shipped runtime."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import zipfile
from ci_smoke import png

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--archive", type=Path, default=ROOT / "target/dist/Scrutivue-windows-x64.zip")
archive = parser.parse_args().archive
pin = json.loads((ROOT / "config/libjxl.json").read_text(encoding="utf-8"))
model_pin = json.loads((ROOT / "assets/models/lpips-squeeze-v0.1.json").read_text(encoding="utf-8"))
with zipfile.ZipFile(archive) as bundle:
    assert bundle.testzip() is None, "ZIP checksum failure"
    required = {
        "Scrutivue/scrutivue.exe", "Scrutivue/onnxruntime.dll", "Scrutivue/LICENSE",
        "Scrutivue/LICENSES/ONNXRuntime/LICENSE",
        "Scrutivue/LICENSES/ONNXRuntime/ThirdPartyNotices.txt",
        "Scrutivue/LICENSES/CDDL-1.0.txt", "Scrutivue/LICENSES/LPIPS.txt",
        "Scrutivue/LICENSES/torchvision.txt", "Scrutivue/LICENSES/rust-dependencies.json",
        "Scrutivue/LICENSES/Rust-supplemental/index.json",
        "Scrutivue/models/" + model_pin["model"],
        "Scrutivue/build-info/lpips-squeeze-v0.1.json",
        "Scrutivue/build-info/native/parallel_raw.cpp",
        "Scrutivue/build-info/native/parallel_raw.h",
        "Scrutivue/LICENSES/LibRaw-adapters.txt",
    }
    required.update("Scrutivue/libjxl/" + name for name in pin["files"])
    required.update("Scrutivue/libjxl/licenses/" + name for name in pin["licenses"])
    required.update({"Scrutivue/libjxl/bundle-pin.json", "Scrutivue/build-info/libjxl.json"})
    assert required.issubset(bundle.namelist()), required - set(bundle.namelist())
    assert json.loads(bundle.read("Scrutivue/libjxl/bundle-pin.json")) == pin
    assert json.loads(bundle.read("Scrutivue/build-info/libjxl.json")) == pin
    assert json.loads(bundle.read("Scrutivue/build-info/lpips-squeeze-v0.1.json")) == model_pin
    for name in ("parallel_raw.cpp", "parallel_raw.h"):
        assert bundle.read("Scrutivue/build-info/native/" + name) == (ROOT / "native" / name).read_bytes()
    assert hashlib.sha256(bundle.read("Scrutivue/models/" + model_pin["model"])).hexdigest() == model_pin["sha256"]
    for name, expected in pin["files"].items():
        assert hashlib.sha256(bundle.read("Scrutivue/libjxl/" + name)).hexdigest() == expected, name
    for name, expected in pin["licenses"].items():
        assert hashlib.sha256(bundle.read("Scrutivue/libjxl/licenses/" + name)).hexdigest() == expected, name
    assert bundle.read("Scrutivue/onnxruntime.dll") == (ROOT / "target/release/onnxruntime.dll").read_bytes()
    inventory = json.loads(bundle.read("Scrutivue/LICENSES/rust-dependencies.json"))
    assert inventory and all(p["notice_files"] or p.get("supplemental_notice_files") for p in inventory)
    with tempfile.TemporaryDirectory(prefix="package-test-", dir=ROOT / "target") as temporary:
        unpacked = Path(temporary) / "portable app — тест"
        bundle.extractall(unpacked)
        exe = unpacked / "Scrutivue/scrutivue.exe"
        subprocess.run([sys.executable, str(ROOT / "scripts/ci_smoke.py"), "--exe", str(exe)], cwd=temporary, check=True)
        image = Path(temporary) / "identity.png"
        png(image)
        model = exe.parent / "models" / model_pin["model"]
        saved = model.with_suffix(".saved")
        model.rename(saved)
        # A valid decoy in the working directory must never substitute for the
        # missing/corrupt asset beside the extracted executable.
        decoy = Path(temporary) / "unrelated working directory"
        (decoy / "models").mkdir(parents=True)
        shutil.copyfile(saved, decoy / "models" / model_pin["model"])
        try:
            for corrupt, expected in [(False, "missing or unreadable"), (True, "checksum mismatch")]:
                if corrupt:
                    model.write_bytes(b"not an ONNX model")
                result = subprocess.run([str(exe), "--statistics", str(image), str(image)], cwd=decoy,
                    check=True, capture_output=True, text=True, encoding="utf-8", timeout=60)
                report = json.loads(result.stdout)
                assert not report["pending"] and report["pixels"]["mae"] == 0, report
                assert abs(report["ssim_y"] - 1) < 1e-6, report
                assert expected in report["neural"]["Err"], report
        finally:
            if model.exists():
                model.unlink()
            saved.rename(model)
print("Windows ZIP, external LPIPS model/hash/failure isolation, pinned JPEG XL notices and extracted native decoding passed.")
