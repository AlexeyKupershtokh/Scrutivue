# Ordinary JPEG loading: parallel, fused pixel preparation

The preserved measurements below predate [0.3.1](release-0.3.1.md). The normal
executable now includes the cumulative optimizations; JPEG geometry/pixels are
unchanged, while DNG display now uses full ActiveArea instead of DefaultCrop.

## Diagnosis on the requested JPEG corpus

`E:/Documents/AI/corpus/3 этаж single/halo` currently contains **36 actual JPEGs**,
six scenes with source halo variants 0/24/48/96/128/256. Their headers identify
8-bit sequential JPEG (`FFC0`), 4:4:4 sampling and no restart markers. These are
the JPEG files themselves, not embedded DNG/RAW previews.

The application uses the existing pinned `image 0.25.10` / `zune-jpeg 0.5.15`
decoder. It has SIMD code but no public per-image parallelism setting. The
variable-length Huffman stream in these restart-less images is not a set of
independently decodable DNG-style tiles. Adding threads to the caller's pool does
not by itself parallelize this decoder's entropy loop. A single busy logical CPU
on a 12-logical-CPU system can look like low total CPU usage; disk/GPU cannot make
progress on work that is still waiting for CPU pixels.

For `AK8_0005_halo0.jpg`, the decoded codestream is 8256x5504 but EXIF requests
`Rotate270`, producing 5504x8256. The prior path first decoded RGB8, then rotated
that complete image serially into another RGB8 buffer, then serially expanded it
to the final RGB16 frame. A warm-file-cache optimized-debug stage profile found:

| Stage | Approximate time |
| --- | ---: |
| File read and decoder headers | 11–12 ms |
| RGB8 allocation + JPEG decoding + decoder cleanup | 454–462 ms |
| Whole-image EXIF rotation | 213–225 ms |
| RGB8 to RGB16 conversion | 85–88 ms |
| Actual async `Services` cache-miss decode | 778 ms |
| DX12 upload/render/readback worker | 90 ms |
| Decode request through rendered buffer ready | 868 ms |

The GPU was genuinely selected (`DX12`, RTX 3070). Renderer initialization was
separate and took another 0.3–0.5 s in local runs; it is not included in the ready
timers and is not optimized by this change. GPUI upload/presentation, folder
scanning and simultaneous thumbnail workloads are not measured by this profile.
Do not report its headless ready timer as exact visible latency for the entire UI.

## Change

- Keep the **same JPEG entropy/IDCT/color backend and settings**. No codec swap,
  new dependency, native DLL, source resave, scaled substitute or GPU JPEG decoder.
- Apply all eight EXIF orientations and exact `u8 * 257` channel expansion in one
  pass directly into the final RGB16 allocation. RGB8 and grayscale8 are covered;
  other future pixel layouts retain the legacy transformation/conversion fallback.
- Give parallel tasks disjoint output row blocks, with 32x32 pixel tiles to keep
  transposed reads cache-local. This removes a complete temporary rotated RGB8
  allocation/copy. It is not a claim that process peak RSS falls by that allocation:
  the final RGB16 buffer remains the dominant owner on both paths.
- Reuse the caller's existing bounded Rayon decode pool. The GUI still has one
  shared budget for both full-frame workers. Headless callers use a single lazy
  fallback pool capped at six, not a new pool per file.
- Check cancellation after codec decoding and during pixel preparation. Output
  capacity is initialized in disjoint slices and exposed as a frame only after
  every channel is written and cancellation checked. Interrupted work never
  returns a partially initialized frame or enters the cache.

RAW/DNG decoding, geometry, filters, analysis data and non-JPEG raster conversion
remain unchanged. JPEG thumbnails retain their existing decoding/resizing path;
this experiment does not claim to accelerate cold thumbnail generation.

The fused preparation took **47–50 ms with six workers**, versus about 300 ms for
the old rotation plus conversion. The same fused kernel took 166–172 ms with one
worker: both removing the intermediate pass and adding parallelism contribute.
The initial actual asynchronous decode fell to **504 ms**, render-ready to
**595 ms**, with an identical final RGB16 hash. These are diagnostic individual
optimized-debug worker observations; see the repeated release comparisons below.

## Alternative multithreaded codec was rejected

An isolated, in-memory experiment compiled the cached `jpeg-decoder 0.3.2` with
its Rayon feature, without changing production dependencies or registry sources.
On the first 45 MP JPEG, three-run medians were approximately:

- Existing image/zune path: **429 ms**, essentially unchanged between one and six
  workers.
- `jpeg-decoder`: **523 ms** with one worker; **492 ms** with six.

The alternative was still slower and changed **7,049,282 RGB8 components**, maximum
error **3/255**. It was not adopted. This one-file codec experiment excludes
orientation, RGB16 expansion, rendering and disk reads, and is not a benchmark of
a shipped alternative application. Its harness is optimized, while its existing
dependency artifacts use the checkout's optimized-debug settings. No claim of
pixel equivalence or a production codec replacement is made.

## Repeated release comparisons, 2026-11-01

The preceding external-model third application is the baseline, not the older
feature-disabled or runner-only EXE. Three separate-process repetitions per scene
alternate baseline/candidate order on a Ryzen 5 5600. Both release applications
run `--probe`; its thumbnail step warms the file before the measured full decode.
`decode_ms` includes full-file read, codec decoding, EXIF orientation and RGB16
preparation, but excludes the thumbnail, hashing, process startup and rendering.
No competing build or benchmark ran during these comparisons.

| JPEG | Previous median | Fused parallel median | Speedup |
| --- | ---: | ---: | ---: |
| `AK8_0005_halo0.jpg` | 689 ms | 480 ms | 1.44x |
| `AK8_0036_halo0.jpg` | 694 ms | 486 ms | 1.43x |
| `AK8_0149_halo0.jpg` | 634 ms | 421 ms | 1.50x |
| `AK8_0988_halo0.jpg` | 645 ms | 443 ms | 1.46x |
| `AK8_1588_halo0.jpg` | 667 ms | 449 ms | 1.49x |
| `AK8_1714_halo0.jpg` | 629 ms | 430 ms | 1.46x |

Thus full-frame preparation takes about **30–34% less time**, not a universal 2x
improvement. All **36 corpus JPEGs** passed dimensions, sample-sum and complete
RGB16 SHA-256 comparisons against the saved application. All 36 source hashes,
sizes and modification times remained unchanged. Six additional generated JPEGs
(RGB, grayscale and CMYK, each sequential/progressive) plus three PNG layouts
(RGB, RGBA, grayscale16) also matched. Tiny generated JPEGs pay approximately
0.1 ms more in these headless probes; they are correctness fixtures, not evidence
that every image size benefits from parallel scheduling.

The actual asynchronous `Services` test was separately repeated three times with
alternating order at 2560x1440 Fit on DX12 / RTX 3070. These are **optimized-debug
test executables using production services**, not the release EXEs above:

| Worker measurement | Previous median | Candidate median |
| --- | ---: | ---: |
| Decode request through RGB16 frame event | 764 ms | 514 ms |
| GPU upload/render/readback worker | 84 ms | 86 ms |
| Decode request through rendered buffer ready | 848 ms | 599 ms |
| Separate renderer startup | 333 ms | 346 ms |

Output frame hashes matched in every run. Startup, folder scanning, concurrent
thumbnail generation, GPUI texture upload and screen presentation are excluded
from the ready timer. These results do **not** guarantee sub-second visible GUI
opening, particularly during a cold start or initial folder population. The
remaining codec work and unchanged thumbnail path still limit total latency.

Evidence: `target/jpeg-parallel-review/release-comparison/` and
`services-comparison/`, driven by `validate_jpeg.py` and `compare_services.py` in
the review directory. Measurements were sequential, read-only and local.

## Application build and validation

The EXE/hash and ZIP below record the JPEG implementation before the later
[parallel NEF adapters](nef-loading-performance.md). That EXE is now preserved
under `target/nef-parallel-review/previous/`; the JPEG ZIP below remains unchanged.
The shared release target has subsequently been rebuilt with the cumulative NEF
change. These JPEG timings are not relabeled as measurements of the later build.

The release build succeeded with the cumulative `experimental-single-parallel`
feature enabled. Feature-disabled unit tests passed **156**, with 11 optional
tests ignored; third-feature tests passed **161**, with 13 ignored. New tests
compare every output sample for all eight EXIF orientations, RGB/grayscale,
one/six workers, odd/thin/empty dimensions, cancellation and fallback layouts.
Formatting and whitespace checks passed. Codec/model pins and `Cargo.lock` are
unchanged by this work.

Updated EXE: `target/release/scrutivue-single-parallel.exe`, **32,119,808 bytes**,
SHA-256 `881d97059263f29351aca1e7e8e4afecd1c8803a8c43c181ebafc2547979c252`.
The previous model-only third EXE remains in `previous/`, SHA-256
`d5a45a0c9440f913c95578fe3b859aebba83d6bf70ee0aa5a29b9e3c38964da1`.
The original normal/runner-only EXEs, ONNX Runtime, pinned JPEG XL bundle and
external LPIPS model remain byte-for-byte unchanged. The full model payload is
not embedded in the updated EXE. Integrity evidence: `binary-integrity.json` in
the review directory.

Portable archive: `target/jpeg-parallel-review/Scrutivue-windows-x64.zip`,
**24,452,592 bytes**, SHA-256
`a616480d9505811db411081256987adba0be5c91aade4bfb6ddc79209a23988b`.
Its EXE matches the measured candidate, and the ZIP includes the unchanged
external LPIPS model, matching ONNX Runtime, pinned JPEG XL bundle and notices.
Extracted CLI smoke passed from a spaced/Unicode installation path and another
working directory: native DNG direct completion/fallback with legacy RGB16
equality, statistics and external LPIPS. Missing/corrupt-model isolation also
passed; native metrics remain available and a working-directory decoy is ignored.

Native no-capture GUI smoke on two copied corpus JPEGs passed **791 event-loop
probes with zero 100 ms timeouts**, maximum **14.25 ms**, at 2x DPI. It covered
Single/Slider/Diff selection, divider movement, panning/clamping, navigator,
fullscreen, statistics/picker input, Help and settings restart. It did not
capture desktop pixels or measure end-to-end visible-image latency; the 297 ms
window-opening observation is not an image-ready measurement. Report:
`gui-release-no-capture/result.json` in the review directory. The final integrity
check again confirmed all 36 original source hashes and modification times, the
saved EXEs, and the original normal/runner-only builds. Combined evidence:
`final-report.json`. No commit, remote build or release update was performed.

## Reproduction

```powershell
$env:SCRUTIVUE_JPEG_PROFILE_PATH = 'D:/Photos/example.jpg'
$env:SCRUTIVUE_JPEG_PROFILE_OUTPUT = 'target/jpeg-parallel-review/new-profile.json'
$env:SCRUTIVUE_JPEG_PROFILE_THREADS = '6'
cargo test --locked --features experimental-single-parallel --bin scrutivue jpeg_loading_stage_profile -- --ignored --nocapture
```

Always use a new output path. The profiler records three legacy serial reference
passes, three isolated fused-preparation timings, then a fresh actual async
Single decode and 2560x1440 Fit GPU render. The thread override applies to the
isolated stage pool; `Services` uses its production thread budget. Hash/reference
checks are outside timers. The profile needs a working native GPU backend.

The JPEG change is shared by normal and experimental builds. The local updated
test application keeps the preceding DNG experiments enabled:

```powershell
cargo build --release --locked --features experimental-single-parallel --bin scrutivue-single-parallel
.\target\release\scrutivue-single-parallel.exe --probe 'D:/Photos/example.jpg'
```

The release `--probe` reports thumbnail/decode times separately and hashes the
complete RGB16 result after the decode timer. It does not time visible GUI opening.

Keep `onnxruntime.dll`, `models/` and `libjxl/` beside the executable. Previous
application binaries and matching assets are preserved under
`target/jpeg-parallel-review/previous/`. No commit, push or remote release update
is part of this experiment.
