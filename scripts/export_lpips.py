"""Development-only ONNX export; the application never launches Python.

Pinned exporter: CPU torch 2.8.0, torchvision 0.23.0, lpips 0.1.4, onnx 1.19.1.
TORCH_HOME must contain official squeezenet1_1-b8a52dc0.pth (no random weights).
"""
import hashlib
import json
import os
from pathlib import Path
import torch
import lpips
import onnx

ROOT = Path(__file__).resolve().parents[1]


class Metric(torch.nn.Module):
    def __init__(self):
        super().__init__()
        self.metric = lpips.LPIPS(net="squeeze", version="0.1", verbose=False).eval()

    def forward(self, a, b):
        return self.metric(a * 2 - 1, b * 2 - 1).reshape(1)


if __name__ == "__main__":
    torch.set_num_threads(2)
    model = Metric().eval()
    out = ROOT / "assets/models/lpips-squeeze-v0.1.onnx"
    out.parent.mkdir(parents=True, exist_ok=True)
    with torch.inference_mode():
        torch.onnx.export(model, (torch.zeros(1, 3, 192, 256), torch.ones(1, 3, 192, 256)),
            str(out), input_names=["a", "b"], output_names=["lpips"], opset_version=17,
            dynamic_axes={"a": {2: "height", 3: "width"}, "b": {2: "height", 3: "width"}},
            dynamo=False)
    onnx.checker.check_model(onnx.load(str(out)))
    manifest = {"model": out.name, "sha256": hashlib.sha256(out.read_bytes()).hexdigest(),
        "source": "richzhang/PerceptualSimilarity lpips 0.1.4, learned v0.1 + torchvision SqueezeNet1_1_Weights.IMAGENET1K_V1",
        "export": "torch 2.8.0+cpu, torchvision 0.23.0+cpu, onnx 1.19.1, opset 17",
        "inputs": "a,b: float32 NCHW [1,3,H,W], nominal sRGB 0..1, H/W >=32",
        "output": "lpips: float32 [1], lower is more similar"}
    out.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))
