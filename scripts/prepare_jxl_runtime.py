"""Fetch and install the pinned official Windows x64 JPEG XL runtime and notices."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.error
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def load_pin():
    return json.loads((ROOT / "config/libjxl.json").read_text(encoding="utf-8"))


def bundle_files(pin):
    return {**pin["files"], **{f"licenses/{name}": value for name, value in pin["licenses"].items()}}


def verify_file(path, expected):
    if not path.is_file():
        raise RuntimeError(f"Required JPEG XL runtime file is missing: {path}")
    with path.open("rb") as file:
        actual = hashlib.file_digest(file, "sha256").hexdigest()
    if actual != expected:
        raise RuntimeError(f"JPEG XL runtime checksum mismatch: {path}")


def validate_bundle(source, pin):
    for name, expected in bundle_files(pin).items():
        verify_file(source / name, expected)


def download_archive(pin, directory):
    directory.mkdir(parents=True, exist_ok=True)
    archive = directory / f"libjxl-{pin['version']}-windows-x64.zip"
    if archive.exists():
        verify_file(archive, pin["archive"]["sha256"])
        return archive
    for attempt in range(3):
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(prefix="libjxl-", suffix=".partial", dir=directory, delete=False) as output:
                temporary = Path(output.name)
                request = urllib.request.Request(pin["archive"]["url"], headers={"User-Agent": "Scrutivue-build"})
                with urllib.request.urlopen(request, timeout=60) as response:
                    count = 0
                    while chunk := response.read(1024 * 1024):
                        count += len(chunk)
                        if count > pin["archive"]["bytes"]:
                            raise RuntimeError("JPEG XL archive exceeds its pinned size")
                        output.write(chunk)
            verify_file(temporary, pin["archive"]["sha256"])
            if archive.exists():
                verify_file(archive, pin["archive"]["sha256"])
            else:
                # Exclusive creation never overwrites an existing cached download.
                with archive.open("xb") as output, temporary.open("rb") as source:
                    shutil.copyfileobj(source, output)
            return archive
        except (OSError, urllib.error.URLError) as error:
            if attempt == 2:
                raise RuntimeError("Could not download the pinned JPEG XL archive") from error
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
    raise RuntimeError("Could not download the pinned JPEG XL archive")


def extract_archive(archive, output, pin):
    # The official ZIP uses Deflate64, which Python's zipfile cannot decompress.
    # Verify its identity before invoking 7-Zip and extract only named runtime files.
    verify_file(archive, pin["archive"]["sha256"])
    seven_zip = shutil.which("7z") or str(Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "7-Zip/7z.exe")
    if not Path(seven_zip).is_file():
        raise RuntimeError("7-Zip is required to extract the official Deflate64 JPEG XL archive")
    prefix = pin["archive"]["prefix"]
    members = [prefix + "bin/" + name for name in pin["files"]]
    members += [prefix + "licenses/" + name for name in pin["licenses"]]
    with zipfile.ZipFile(archive) as bundle:
        names = bundle.namelist()
        for name in members:
            if names.count(name) != 1:
                raise RuntimeError(f"Missing or duplicate JPEG XL archive entry: {name}")
            entry = bundle.getinfo(name)
            if entry.is_dir() or entry.file_size > 16 * 1024 * 1024 or (entry.external_attr >> 16) & 0o170000 == 0o120000:
                raise RuntimeError(f"Invalid JPEG XL archive entry: {name}")
    subprocess.run([seven_zip, "x", "-bd", "-y", f"-o{output}", str(archive.resolve()), *members],
        check=True, stdout=subprocess.DEVNULL, timeout=60)
    return output / prefix


def prepare_source(pin, root=ROOT, archive=None):
    vendor = root / "vendor"
    source = vendor / "libjxl"
    if source.exists():
        validate_bundle(source, pin)
        return source
    vendor.mkdir(parents=True, exist_ok=True)
    if archive is None:
        archive = download_archive(pin, vendor / "cache")
    with tempfile.TemporaryDirectory(prefix="libjxl-", dir=vendor) as temporary:
        temporary = Path(temporary)
        extracted = extract_archive(archive, temporary / "extract", pin)
        ready = temporary / "ready"
        ready.mkdir()
        (ready / "licenses").mkdir()
        for name in pin["files"]:
            shutil.copyfile(extracted / "bin" / name, ready / name)
        for name in pin["licenses"]:
            shutil.copyfile(extracted / "licenses" / name, ready / "licenses" / name)
        validate_bundle(ready, pin)
        ready.rename(source)
    return source


def install_bundle(source, output, pin):
    validate_bundle(source, pin)
    files = bundle_files(pin)
    # Check every pre-existing destination first; never partially overwrite a
    # different installation or silently replace a notice with different text.
    for name, expected in files.items():
        if (output / name).exists():
            verify_file(output / name, expected)
    for name in files:
        destination = output / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        if not destination.exists():
            with (source / name).open("rb") as input_file, destination.open("xb") as output_file:
                shutil.copyfileobj(input_file, output_file)
    validate_bundle(output, pin)
    (output / "bundle-pin.json").write_text(json.dumps(pin, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    choice = parser.add_mutually_exclusive_group()
    choice.add_argument("--source", type=Path, help="Use an already audited DLL bundle instead of downloading")
    choice.add_argument("--archive", type=Path, help="Use a local copy of the exact pinned official ZIP")
    parser.add_argument("--profile", choices=["debug", "release"], default="release")
    parser.add_argument("--prepare-only", action="store_true", help="Prepare the cached source without installing target/ files")
    args = parser.parse_args()
    pin = load_pin()
    source = args.source or prepare_source(pin, archive=args.archive)
    validate_bundle(source, pin)
    if args.prepare_only:
        print(f"Pinned JPEG XL runtime is ready: {source}")
        return
    output = ROOT / "target" / args.profile / "libjxl"
    install_bundle(source, output, pin)
    print(f"Installed pinned JPEG XL runtime: {output}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        raise SystemExit(str(error)) from error
