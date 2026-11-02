# Scrutivue 0.3.2

## Changes

- Enable progressive preview for supported Nikon HE NEFs in the normal
  `scrutivue.exe`. Single/Slider display centre-prioritized, 1:4 sampled RGB16 bands
  before complete development; final authoritative RGB16 pixels are unchanged.
- Narrow the floating Statistics card from 346 to 288 logical pixels.
- Allow panning at every zoom, including Fit/reduced views. The viewport centre
  stays inside the reference image; empty space at its edges is intentional so
  details can be moved away from floating controls. `F` / `0` recentres and fits.
- Enlarge the color picker swatches into one continuous 56-pixel-high field:
  A left, B right, with no gap or artificial dividing line. Values remain original
  developed RGB16; the 1600% loupe is unchanged.
- Add an Author section at the end of Help: **Alexey Kupershtokh**,
  **alexey.kupershtokh@gmail.com**, and a clickable
  [repository link](https://github.com/AlexeyKupershtokh/Scrutivue).

## NEF scope and limitations

The first preview still waits for complete CFA unpacking, global normalization
and white balance. This is not viewport-only entropy decoding or an embedded
JPEG replacement. Supported 256-sensor-row development bands run in centre-first
order with parallel row processing; the existing RAW stripe decode shares the
bounded background pool. Queues and sampled canvases are bounded; slow consumers
may drop preview bands. Diff, High pass, statistics and exact pixel inspection
wait for complete authoritative frames. Unsupported profiles retain full-frame
fallback. Source photographs are never changed.

Earlier isolated release measurements on the six requested Nikon Z8 NEFs found
first preview buffers at **478–487 ms**, versus **879–889 ms** for complete RGB16
and **958–972 ms** for final DX12 buffers. Those measurements predate this release
build, use warm caches, and exclude separately reported GPU startup, GPUI upload
and screen presentation. They are not cold-start or first-visible-pixel promises.

The original prototype had intermittent broad GUI-smoke assertions involving
early divider input and camera restoration. An instrumented rerun and targeted
view-control tests passed, but those earlier failures were not proven to be
harness-only. Desktop capture was unavailable, so input/state acceptance checks
are not visual presentation validation. Keep these limits explicit in the draft.

## Build and distribution

Pre-build validation passed **175 default-feature tests** (20 optional tests
ignored), **163 feature-disabled tests** (16 ignored), all **three native DX12
suites**, both explicitly invoked actual-corpus progressive NEF tests, and
**16 Python asset/runtime tests**. Ignored tests not explicitly invoked are not
counted as if they ran. Final release/package checks and their exact executable
identity are recorded separately in `target/release-0.3.2/`.

```powershell
cargo test --locked --offline --bin scrutivue
cargo test --locked --offline --no-default-features --bin scrutivue
cargo test --locked --offline --bin scrutivue gpu_matches_cpu -- --ignored --nocapture
cargo build --release --locked --offline --bin scrutivue
```

The normal default feature set now includes `experimental-nef-progressive` and
its cumulative parallel JXL/Single dependencies; the feature name is retained
for source/build compatibility. Build only `--bin scrutivue` to preserve preceding
comparison EXEs. `--no-default-features` remains available for comparisons.

The Windows x64 ZIP contains `scrutivue.exe`, matching `onnxruntime.dll`, the
unchanged external LPIPS model, the pinned JPEG XL bundle/licenses, adapted C++
source/notices and clean-build provenance at `build-info/release.json`. Extract
the entire folder. Python and 7-Zip are not runtime requirements. This unsigned
portable build may trigger SmartScreen and may need the Microsoft Visual C++ x64
runtime on a clean Windows installation.

The exact Redlamp LibRaw fork, runtime/model pins, previous EXEs and published
0.3.0/0.3.1 releases are preserved. This draft uses a validated local build from
its committed/tagged source; the manual-only Windows workflow need not be
dispatched. Version changes in Cargo.lock affect only the root package.

Local validation/provenance and draft-asset verification live under
`target/release-0.3.2/`. Implementation and earlier evidence:
[NEF preview](nef-progressive-preview.md), [view controls](view-controls.md).
