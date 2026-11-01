"""Validate the Windows ZIP and run the extracted binary with its shipped runtime."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--archive", type=Path, default=ROOT / "target/dist/Scrutivue-windows-x64.zip")
archive = parser.parse_args().archive
pin = json.loads((ROOT / "config/libjxl.json").read_text(encoding="utf-8"))
with zipfile.ZipFile(archive) as bundle:
    assert bundle.testzip() is None, "ZIP checksum failure"
    required = {
        "Scrutivue/scrutivue.exe", "Scrutivue/onnxruntime.dll", "Scrutivue/LICENSE",
        "Scrutivue/LICENSES/ONNXRuntime/LICENSE",
        "Scrutivue/LICENSES/ONNXRuntime/ThirdPartyNotices.txt",
        "Scrutivue/LICENSES/CDDL-1.0.txt", "Scrutivue/LICENSES/LPIPS.txt",
        "Scrutivue/LICENSES/torchvision.txt", "Scrutivue/LICENSES/rust-dependencies.json",
        "Scrutivue/LICENSES/Rust-supplemental/index.json",
    }
    required.update("Scrutivue/libjxl/" + name for name in pin["files"])
    required.update("Scrutivue/libjxl/licenses/" + name for name in pin["licenses"])
    required.update({"Scrutivue/libjxl/bundle-pin.json", "Scrutivue/build-info/libjxl.json"})
    assert required.issubset(bundle.namelist()), required - set(bundle.namelist())
    assert json.loads(bundle.read("Scrutivue/libjxl/bundle-pin.json")) == pin
    assert json.loads(bundle.read("Scrutivue/build-info/libjxl.json")) == pin
    for name, expected in pin["files"].items():
        assert hashlib.sha256(bundle.read("Scrutivue/libjxl/" + name)).hexdigest() == expected, name
    for name, expected in pin["licenses"].items():
        assert hashlib.sha256(bundle.read("Scrutivue/libjxl/licenses/" + name)).hexdigest() == expected, name
    assert bundle.read("Scrutivue/onnxruntime.dll") == (ROOT / "target/release/onnxruntime.dll").read_bytes()
    inventory = json.loads(bundle.read("Scrutivue/LICENSES/rust-dependencies.json"))
    assert inventory and all(p["notice_files"] or p.get("supplemental_notice_files") for p in inventory)
    with tempfile.TemporaryDirectory(prefix="package-test-", dir=ROOT / "target") as temporary:
        bundle.extractall(temporary)
        exe = Path(temporary) / "Scrutivue/scrutivue.exe"
        subprocess.run([sys.executable, str(ROOT / "scripts/ci_smoke.py"), "--exe", str(exe)], check=True)
print("Windows ZIP, pinned JPEG XL runtime/notices and extracted native decoding passed.")
