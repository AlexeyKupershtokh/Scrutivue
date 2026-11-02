"""Provenance guards do not accept dirty, mismatched or non-CI release builds."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import package_ci_release as release


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "Cargo.toml").write_text('[package]\nname="scrutivue"\nversion="0.3.3"\n[features]\ndefault=["experimental-nef-progressive"]\n')
        (self.root / "config").mkdir()
        for name in ("libraw.json", "onnxruntime.json", "libjxl.json"):
            (self.root / "config" / name).write_text(json.dumps({"version": "pinned"}))
        (self.root / "native").mkdir()
        for name in ("raw.cpp", "parallel_raw.cpp", "parallel_raw.h"):
            (self.root / "native" / name).write_text("pinned source")
        self.exe = self.root / "scrutivue.exe"
        self.exe.write_bytes(b"verified executable fixture")
        self.environment = {"GITHUB_ACTIONS": "true", "GITHUB_SHA": "a" * 40,
                            "GITHUB_REPOSITORY": "owner/repository", "GITHUB_RUN_ID": "123",
                            "GITHUB_RUN_ATTEMPT": "1", "GITHUB_REF": "refs/tags/v0.3.3"}

    def run_provenance(self, revision=None, dirty="", version="scrutivue 0.3.3"):
        with patch.object(release, "output", side_effect=[revision or "a" * 40, dirty, version, "rustc fixture"]):
            return release.provenance(self.root, self.exe, self.environment)

    def test_clean_ci_revision_and_binary_are_recorded(self):
        report = self.run_provenance()
        self.assertEqual(report["source_commit"], self.environment["GITHUB_SHA"])
        self.assertEqual(report["version"], "0.3.3")
        self.assertTrue(report["source_clean"])
        self.assertEqual(report["ci"]["run_url"], "https://github.com/owner/repository/actions/runs/123")
        self.assertEqual(len(report["executable"]["sha256"]), 64)
        self.assertEqual(len(report["native_source_sha256"]), 3)

    def test_non_ci_packaging_is_refused(self):
        self.environment.clear()
        with self.assertRaisesRegex(RuntimeError, "GitHub Actions context"):
            self.run_provenance()

    def test_wrong_checkout_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "does not match"):
            self.run_provenance(revision="b" * 40)

    def test_dirty_checkout_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "clean committed checkout"):
            self.run_provenance(dirty=" M src/main.rs")

    def test_wrong_executable_version_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "executable version"):
            self.run_provenance(version="scrutivue 0.3.2")

    def test_missing_run_identity_is_refused(self):
        self.environment.pop("GITHUB_RUN_ID")
        with self.assertRaisesRegex(RuntimeError, "Incomplete"):
            self.run_provenance()


if __name__ == "__main__":
    unittest.main()
