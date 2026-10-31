"""Validate the Windows ZIP and run the extracted binary with its shipped runtime."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
archive = ROOT / "target/dist/Scrutivue-windows-x64.zip"
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
    assert required.issubset(bundle.namelist()), required - set(bundle.namelist())
    assert bundle.read("Scrutivue/onnxruntime.dll") == (ROOT / "target/release/onnxruntime.dll").read_bytes()
    inventory = json.loads(bundle.read("Scrutivue/LICENSES/rust-dependencies.json"))
    assert inventory and all(p["notice_files"] or p.get("supplemental_notice_files") for p in inventory)
    with tempfile.TemporaryDirectory(prefix="package-test-", dir=ROOT / "target") as temporary:
        bundle.extractall(temporary)
        exe = Path(temporary) / "Scrutivue/scrutivue.exe"
        subprocess.run([sys.executable, str(ROOT / "scripts/ci_smoke.py"), "--exe", str(exe)], check=True)
print("Windows ZIP, dependency-notice coverage and extracted runtime passed.")
