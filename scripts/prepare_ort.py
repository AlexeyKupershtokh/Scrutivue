"""Build-time only: fetch the pinned official CPU ONNX Runtime for Windows x64."""
import hashlib
import json
from pathlib import Path
import sys
import urllib.request
import zipfile
import time

ROOT = Path(__file__).resolve().parents[1]
if sys.platform != "win32":
    raise SystemExit("Supply a native ONNX Runtime >=1.24 for your platform beside the executable.")
pin = json.loads((ROOT / "config/onnxruntime.json").read_text())
destination = ROOT / "vendor/onnxruntime"
stamp = destination / ".diff-pin"
if stamp.exists() and stamp.read_text().strip() == pin["version"]:
    print("ONNX Runtime is ready:", pin["version"])
else:
    if destination.exists():
        raise SystemExit("vendor/onnxruntime already exists without the expected pin; move it aside first")
    archive = ROOT / "vendor" / ("onnxruntime-" + pin["version"] + ".zip")
    archive.parent.mkdir(exist_ok=True)
    if not archive.exists() or hashlib.sha256(archive.read_bytes()).hexdigest() != pin["sha256"]:
        print("Downloading official CPU ONNX Runtime", pin["version"], flush=True)
        # Resume short HTTP transfers; never accept an unverified partial ZIP.
        for attempt in range(32):
            offset = archive.stat().st_size if archive.exists() else 0
            request = urllib.request.Request(pin["url"], headers={"Range": f"bytes={offset}-"} if offset else {})
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    mode = "ab" if offset and response.status == 206 else "wb"
                    with archive.open(mode) as target:
                        while chunk := response.read(1024 * 1024):
                            target.write(chunk)
            except (OSError, EOFError) as error:
                print("Retrying download:", error, flush=True)
            if archive.exists() and hashlib.sha256(archive.read_bytes()).hexdigest() == pin["sha256"]:
                break
            time.sleep(1)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != pin["sha256"]:
        raise SystemExit("ONNX Runtime archive SHA-256 mismatch")
    with zipfile.ZipFile(archive) as z:
        prefix = "onnxruntime-win-x64-" + pin["version"] + "/"
        for member in z.infolist():
            relative = Path(member.filename.removeprefix(prefix))
            if member.is_dir():
                continue
            if not member.filename.startswith(prefix) or relative.is_absolute() or ".." in relative.parts:
                raise SystemExit("Unsafe archive entry")
            out = destination / relative
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_bytes(z.read(member))
    stamp.write_text(pin["version"] + "\n")
    print("Ready:", destination)
