"""External LPIPS packaging guards; no model inference or source-image access."""
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from package_windows import ROOT, required_lpips_model


class ModelAssets(unittest.TestCase):
    def setUp(self):
        (ROOT / "target").mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="model-assets-", dir=ROOT / "target")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.exe = self.root / "build with spaces/scrutivue.exe"
        self.bundle = self.root / "package/Scrutivue"
        self.pin = json.loads((ROOT / "assets/models/lpips-squeeze-v0.1.json").read_text(encoding="utf-8"))
        self.model = self.exe.parent / "models" / self.pin["model"]
        self.model.parent.mkdir(parents=True)

    def test_model_is_copied_from_beside_the_selected_executable(self):
        shutil.copyfile(ROOT / "assets/models" / self.pin["model"], self.model)
        required_lpips_model(self.exe, self.bundle)
        copied = self.bundle / "models" / self.pin["model"]
        self.assertEqual(hashlib.sha256(copied.read_bytes()).hexdigest(), self.pin["sha256"])

    def test_missing_model_never_falls_back_to_repository_assets(self):
        with self.assertRaisesRegex(SystemExit, "Cannot package without the LPIPS model"):
            required_lpips_model(self.exe, self.bundle)
        self.assertFalse(self.bundle.exists())

    def test_corrupt_model_is_not_packaged(self):
        self.model.write_bytes(b"corrupt model")
        with self.assertRaisesRegex(SystemExit, "checksum mismatch"):
            required_lpips_model(self.exe, self.bundle)
        self.assertFalse(self.bundle.exists())


if __name__ == "__main__":
    unittest.main()
