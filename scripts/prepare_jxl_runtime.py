"""Install the optional hash-pinned libjxl bundle beside a target/ executable."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Audited libjxl 0.12.0 DLL bundle directory")
    parser.add_argument("--profile", choices=["debug", "release"], default="release")
    args = parser.parse_args()
    pin = json.loads((ROOT / "config/libjxl.json").read_text())
    for name, expected in pin["files"].items():
        assert hashlib.sha256((args.source / name).read_bytes()).hexdigest() == expected, f"Runtime hash mismatch: {name}"
    output = ROOT / "target" / args.profile / "libjxl"
    output.mkdir(parents=True, exist_ok=True)
    for name in pin["files"]:
        destination = output / name
        if destination.exists():
            assert hashlib.sha256(destination.read_bytes()).hexdigest() == pin["files"][name], f"Refusing to replace a different runtime: {destination}"
        else:
            shutil.copyfile(args.source / name, destination)
    licenses = args.source / "licenses"
    assert licenses.is_dir(), "The matching upstream notices must accompany the runtime"
    shutil.copytree(licenses, output / "licenses", dirs_exist_ok=True)
    (output / "bundle-pin.json").write_text(json.dumps(pin, indent=2) + "\n")
    print(f"Installed optional libjxl runtime: {output}")


if __name__ == "__main__":
    main()
