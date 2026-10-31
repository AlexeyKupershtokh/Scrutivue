"""Fetch the exact Redlamp LibRaw source. No changes are made to ../redlamp."""
import hashlib
import json
from pathlib import Path
import shutil
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
pin = json.loads((ROOT / "config/libraw.json").read_text())
destination = ROOT / "vendor" / "libraw"
stamp = destination / ".diff-pin"
if stamp.exists() and stamp.read_text().strip() == pin["version"]:
    print("LibRaw is ready:", pin["version"])
    raise SystemExit(0)
if destination.exists():
    raise SystemExit("vendor/libraw already exists without the expected pin; move it aside first")

cache = ROOT / "vendor"
cache.mkdir(exist_ok=True)
archive = cache / (pin["version"] + ".tar.gz")
sibling = ROOT.parent / "redlamp/vendor/cache" / ("LibRaw-" + pin["version"] + ".tar.gz")
if not archive.exists():
    if sibling.exists():
        shutil.copyfile(sibling, archive)
    else:
        print("Downloading Redlamp LibRaw", pin["version"], flush=True)
        urllib.request.urlretrieve(pin["url"], archive)
if hashlib.sha256(archive.read_bytes()).hexdigest() != pin["sha256"]:
    raise SystemExit("LibRaw archive SHA-256 mismatch; refusing to build")
with tarfile.open(archive) as tar:
    members = tar.getmembers()
    prefix = members[0].name.split("/")[0]
    staging = cache / "extract"
    staging.mkdir(exist_ok=True)
    tar.extractall(staging, filter="data")
    (staging / prefix).rename(destination)
    staging.rmdir()
stamp.write_text(pin["version"] + "\n")
print("Ready:", destination)
