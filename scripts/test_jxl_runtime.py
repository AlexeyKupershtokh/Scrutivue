"""Isolated tests for pinned runtime preparation and fail-closed packaging."""
import hashlib
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package_windows as package
import prepare_jxl_runtime as runtime


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        (runtime.ROOT / "target").mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="jxl-test-", dir=runtime.ROOT / "target")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.payloads = {"jxl.dll": b"test decoder", "jxl_cms.dll": b"test CMS",
            "licenses/LICENSE.libjxl": b"test license", "licenses/LICENSE.brotli": b"test dependency notice"}
        self.pin = {"version": "test", "files": {}, "licenses": {},
            "archive": {"url": "https://example.invalid/runtime.zip", "prefix": "x64-windows/"}}
        for name, data in self.payloads.items():
            group = "licenses" if name.startswith("licenses/") else "files"
            self.pin[group][Path(name).name] = hashlib.sha256(data).hexdigest()
        self.source = self.root / "source"
        self.write_bundle(self.source)

    def write_bundle(self, directory):
        for name, data in self.payloads.items():
            path = directory / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)

    def archive(self, omit=None, symlink=None):
        path = self.root / "runtime.zip"
        with zipfile.ZipFile(path, "w") as bundle:
            for name, data in self.payloads.items():
                member = self.pin["archive"]["prefix"] + (name if name.startswith("licenses/") else "bin/" + name)
                if name == omit:
                    continue
                entry = zipfile.ZipInfo(member)
                if name == symlink:
                    entry.create_system = 3
                    entry.external_attr = 0o120777 << 16
                bundle.writestr(entry, data)
        self.pin["archive"].update(sha256=hashlib.sha256(path.read_bytes()).hexdigest(), bytes=path.stat().st_size)
        return path

    def test_installation_is_idempotent_and_preserves_existing_files(self):
        output = self.root / "installed"
        runtime.install_bundle(self.source, output, self.pin)
        before = {name: (output / name).stat().st_mtime_ns for name in self.payloads}
        runtime.install_bundle(self.source, output, self.pin)
        runtime.validate_bundle(output, self.pin)
        self.assertEqual(before, {name: (output / name).stat().st_mtime_ns for name in self.payloads})

    def test_corrupt_source_never_creates_an_installation(self):
        (self.source / "jxl.dll").write_bytes(b"wrong")
        output = self.root / "installed"
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
            runtime.install_bundle(self.source, output, self.pin)
        self.assertFalse(output.exists())

    def test_missing_notice_is_rejected_before_copying(self):
        (self.source / "licenses/LICENSE.libjxl").unlink()
        output = self.root / "installed"
        with self.assertRaisesRegex(RuntimeError, "missing"):
            runtime.install_bundle(self.source, output, self.pin)
        self.assertFalse(output.exists())

    def test_different_destination_is_not_overwritten_or_partially_filled(self):
        for name in ["jxl_cms.dll", "licenses/LICENSE.brotli"]:
            output = self.root / Path(name).name
            path = output / name
            path.parent.mkdir(parents=True)
            path.write_bytes(b"existing user file")
            with self.assertRaisesRegex(RuntimeError, "checksum mismatch"):
                runtime.install_bundle(self.source, output, self.pin)
            self.assertEqual(path.read_bytes(), b"existing user file")
            self.assertFalse((output / "jxl.dll").exists())

    def test_cached_source_is_verified_without_network(self):
        source = self.root / "vendor/libjxl"
        self.write_bundle(source)
        with patch.object(runtime, "download_archive") as download:
            self.assertEqual(runtime.prepare_source(self.pin, root=self.root), source)
            download.assert_not_called()
        (source / "licenses/LICENSE.brotli").write_bytes(b"wrong")
        with self.assertRaises(RuntimeError), patch.object(runtime, "download_archive") as download:
            runtime.prepare_source(self.pin, root=self.root)
        download.assert_not_called()

    def test_archive_identity_is_checked_before_external_extraction(self):
        archive = self.archive()
        archive.write_bytes(b"wrong")
        with self.assertRaisesRegex(RuntimeError, "checksum mismatch"), patch.object(runtime.subprocess, "run") as extract:
            runtime.extract_archive(archive, self.root / "extract", self.pin)
        extract.assert_not_called()

    def test_missing_or_symlinked_archive_member_is_rejected(self):
        for kwargs in [{"omit": "jxl.dll"}, {"symlink": "jxl.dll"}]:
            archive = self.archive(**kwargs)
            with patch.object(runtime.shutil, "which", return_value=sys.executable), patch.object(runtime.subprocess, "run") as extract:
                with self.assertRaisesRegex(RuntimeError, "archive entry"):
                    runtime.extract_archive(archive, self.root / "extract", self.pin)
                extract.assert_not_called()

    def test_prepared_source_is_published_only_after_complete_validation(self):
        extracted = self.root / "extracted"
        for name, data in self.payloads.items():
            path = extracted / (name if name.startswith("licenses/") else "bin/" + name)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        with patch.object(runtime, "extract_archive", return_value=extracted):
            source = runtime.prepare_source(self.pin, root=self.root, archive=self.root / "input.zip")
        runtime.validate_bundle(source, self.pin)
        self.assertEqual(list((self.root / "vendor").iterdir()), [source])

    def test_failed_extraction_does_not_publish_partial_source(self):
        with patch.object(runtime, "extract_archive", return_value=self.root / "missing"):
            with self.assertRaises(OSError):
                runtime.prepare_source(self.pin, root=self.root, archive=self.root / "input.zip")
        self.assertFalse((self.root / "vendor/libjxl").exists())

    def test_downloaded_archive_is_verified_and_temporary_files_are_removed(self):
        payload = b"test download"
        self.pin["archive"].update(sha256=hashlib.sha256(payload).hexdigest(), bytes=len(payload))
        cache = self.root / "cache"
        with patch.object(runtime.urllib.request, "urlopen", return_value=io.BytesIO(payload)):
            archive = runtime.download_archive(self.pin, cache)
        self.assertEqual(archive.read_bytes(), payload)
        self.assertEqual(list(cache.iterdir()), [archive])
        with patch.object(runtime.urllib.request, "urlopen") as download:
            self.assertEqual(runtime.download_archive(self.pin, cache), archive)
            download.assert_not_called()
        archive.write_bytes(b"existing wrong archive")
        with patch.object(runtime.urllib.request, "urlopen") as download:
            with self.assertRaises(RuntimeError):
                runtime.download_archive(self.pin, cache)
            download.assert_not_called()

    def test_truncated_download_is_never_accepted(self):
        self.pin["archive"].update(sha256=hashlib.sha256(b"complete").hexdigest(), bytes=8)
        cache = self.root / "cache"
        with patch.object(runtime.urllib.request, "urlopen", return_value=io.BytesIO(b"partial")):
            with self.assertRaises(RuntimeError):
                runtime.download_archive(self.pin, cache)
        self.assertEqual(list(cache.iterdir()), [])

    def test_package_requires_runtime_and_notices(self):
        with patch.object(package, "load_pin", return_value=self.pin):
            with self.assertRaisesRegex(SystemExit, "Cannot package"):
                package.required_jxl_runtime(self.root / "missing/scrutivue.exe", self.root / "package")
        self.assertFalse((self.root / "package").exists())

    def test_package_copies_only_verified_runtime_files(self):
        source = self.root / "application/libjxl"
        self.write_bundle(source)
        (source / "unrelated.dll").write_bytes(b"do not distribute")
        (source / "licenses/unrelated.txt").write_bytes(b"do not distribute")
        output = self.root / "package"
        with patch.object(package, "load_pin", return_value=self.pin):
            package.required_jxl_runtime(source.parent / "scrutivue.exe", output)
        runtime.validate_bundle(output / "libjxl", self.pin)
        self.assertFalse((output / "libjxl/unrelated.dll").exists())
        self.assertFalse((output / "libjxl/licenses/unrelated.txt").exists())


if __name__ == "__main__":
    unittest.main()
