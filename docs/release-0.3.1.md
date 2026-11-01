# Scrutivue 0.3.1

## Changes

- Show the full DNG **ActiveArea**, ignoring recommended DefaultCrop without
  resizing or source changes. The requested Nikon Z8 NEF and DNG inputs both
  display **5520x8280**. Physically cropped JPEG exports remain **5504x8256**.
  The previous DNG crop remains sample-identical inside the full frame.
- Enable the validated parallel JXL runner, Single-image worker scheduling and
  preview preparation in the normal `scrutivue.exe`, not just experimental EXEs.
- Include the cumulative Diff/streaming improvements, fused JPEG orientation and
  RGB16 preparation, and parallel Nikon HE stripes/development/output.
- Keep LPIPS weights external in `models/`, with checksum validation and isolated
  errors if the model is missing/corrupt. Keep the exact pinned LibRaw, JPEG XL
  and ONNX Runtime; ship adapted native source and notices with the application.

Earlier warm-cache performance comparisons found **1.43–1.50x JPEG** and
**3.07–3.15x supported Nikon HE NEF** acceleration with unchanged output samples.
Those measurements predate this release; they are not new benchmarks or universal
camera/cold-start/screen-presentation guarantees.

## Build and validation

Pre-release validation passed **165 default-feature tests** (17 optional tests
ignored) and **160 feature-disabled tests** (15 ignored). Both native DX12 suites
and the 36-file DNG overlap-parity test were additionally run explicitly and
passed. Python asset/runtime tests passed **16/16**. Optional benchmark/corpus
tests not explicitly invoked are not counted as if they ran.

Build the normal application with:

```powershell
cargo test --locked --bin scrutivue
cargo test --locked --no-default-features --bin scrutivue
cargo build --release --locked --bin scrutivue
python scripts/package_windows.py --provenance target/release-0.3.1/release.json --output target/release-0.3.1/Scrutivue-0.3.1-windows-x64.zip
python scripts/test_package.py --archive target/release-0.3.1/Scrutivue-0.3.1-windows-x64.zip
```

Only the root package version changes in Cargo.lock. Existing comparison/root
binaries and source photographs are preserved. Release evidence, source revision,
toolchain, asset checksums and validation results are stored under
`target/release-0.3.1/`; the ZIP includes `build-info/release.json`.
Generate that provenance from the clean committed build and measured EXE before
packaging; the packager checks its source revision, version and executable hash.

The Windows workflow remains manual-only. This draft uses the tested local
Windows x64 release build from its exact committed revision; no additional hosted
Actions compilation is needed. The published 0.3.0 release is not changed.

## Distribution

Extract the **entire** `Scrutivue-0.3.1-windows-x64.zip`; do not move just the EXE.
Keep its matching `onnxruntime.dll`, external `models/` and complete `libjxl/`
beside `scrutivue.exe`. Python, 7-Zip and downloads are not required at runtime.
The Microsoft Visual C++ x64 runtime may be needed on a clean installation.
This portable build is unsigned; Windows SmartScreen may warn. SHA-256 checksums
detect transfer errors but are not a code-signing or automatic-update mechanism.
