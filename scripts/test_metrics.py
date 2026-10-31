"""Cross-check the Rust statistics CLI against PIQ and official LPIPS on a seeded pair.

Development-only: run after cargo build in a Python environment with
NumPy, Pillow, Torch, PIQ and LPIPS installed. The application does not use it.
"""
import argparse
import contextlib
import json
from pathlib import Path
import subprocess
import sys
import numpy as np
from PIL import Image
import torch
import piq
import lpips

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--exe", type=Path, default=ROOT / "target/debug/scrutivue.exe")
args = parser.parse_args()

torch.set_num_threads(2)
root = ROOT / "target/metric-tests"
root.mkdir(parents=True, exist_ok=True)
rng = np.random.default_rng(8721)
a = rng.integers(0, 256, (192, 256, 3), dtype=np.uint8)
b = np.clip(a.astype(np.int16) + rng.integers(-12, 13, a.shape), 0, 255).astype(np.uint8)
Image.fromarray(a).save(root / "a.png")
Image.fromarray(b).save(root / "b.png")
report = json.loads(subprocess.check_output([str(args.exe), "--statistics", str(root / "a.png"),
    str(root / "b.png")], text=True, encoding="utf-8"))

def tensor(image):
    # Match Rust's conversion of RGB16 samples to f32, then f64 for SSIM.
    array = image.astype(np.float32) * np.float32(257) / np.float32(65535)
    return torch.from_numpy(array.transpose(2, 0, 1)).unsqueeze(0)
x, y = tensor(a), tensor(b)
weights = torch.tensor([0.2126, 0.7152, 0.0722], dtype=torch.float64).view(1, 3, 1, 1)
xx, yy = (x.double() * weights).sum(1, keepdim=True), (y.double() * weights).sum(1, keepdim=True)
reference = {
    "psnr": float(-10 * np.log10(np.mean(((a.astype(np.float64) - b) / 255) ** 2))),
    "ssim": float(piq.ssim(xx, yy, data_range=1., downsample=False)),
    "ms_ssim": float(piq.multi_scale_ssim(xx, yy, data_range=1.)),
    "gmsd": float(piq.gmsd(xx, yy, data_range=1.)),
}
with contextlib.redirect_stdout(sys.stderr), torch.inference_mode():
    model = lpips.LPIPS(net="squeeze", version="0.1", verbose=False).eval()
    reference["lpips"] = float(model(x * 2 - 1, y * 2 - 1))
actual = {"psnr": report["pixels"]["psnr_db"], "ssim": report["ssim_y"],
    "ms_ssim": report["ms_ssim_y"], "gmsd": report["gmsd_y"], **report["neural"]["Ok"]}
for name in reference:
    tolerance = 2e-6
    assert abs(reference[name] - actual[name]) <= tolerance, (name, reference[name], actual[name])
assert np.isfinite(report["ssimulacra2"]["Ok"]) and report["ssimulacra2"]["Ok"] < 100
(root / "reference.json").write_text(json.dumps({"reference": reference, "actual": actual,
    "ssimulacra2": report["ssimulacra2"]}, indent=2), encoding="utf-8")
print(json.dumps({"reference": reference, "actual": actual}, indent=2))

# Exercise dynamic ONNX axes / ceil-mode pooling, including minimum and odd sizes.
for h, w in [(32, 32), (37, 65), (191, 255), (256, 192)]:
    aa = rng.integers(0, 256, (h, w, 3), dtype=np.uint8)
    bb = np.clip(aa.astype(np.int16) + 17, 0, 255).astype(np.uint8)
    Image.fromarray(aa).save(root / "odd-a.png")
    Image.fromarray(bb).save(root / "odd-b.png")
    r = json.loads(subprocess.check_output([str(args.exe), "--statistics", str(root / "odd-a.png"),
        str(root / "odd-b.png")], text=True, encoding="utf-8"))
    with torch.inference_mode():
        expected = float(model(tensor(aa) * 2 - 1, tensor(bb) * 2 - 1))
    actual = r["neural"]["Ok"]["lpips"]
    assert abs(actual - expected) < 2e-6, (w, h, expected, actual)
    print(f"LPIPS {w}x{h}: PyTorch {expected:.10f}, Rust/ort {actual:.10f}")
