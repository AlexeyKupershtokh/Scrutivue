# Nikon HE NEF: parallel stripes and exact full-frame development

The measurements and saved binaries below predate 0.3.1. The optimization is now
in the normal executable; [0.3.1](release-0.3.1.md) also promotes the cumulative
DNG features and changes DNG display to full ActiveArea. NEF geometry is unchanged.

## Scope and diagnosis

The requested directory, `E:/Documents/AI/corpus/3 этаж single/3 этаж single`,
contains six Nikon Z8 NEFs: `AK8_0005`, `0036`, `0149`, `0988`, `1588`, `1714`.
All use Nikon HE selector 13, with a full 8280x5520 Bayer sensor and 87 full-width
64-row decoder stripes. Their displayed orientation is 5520x8280. The existing
[partial-stripe experiment](../experiments/preview/nef-stripes-notes.md) established
exact random-access **CFA** output, not a complete developed RGB viewer.

The preceding JPEG-optimized application was saved before this work under
`target/nef-parallel-review/previous/`. A fresh release `--probe` on all six
actual NEFs found 2.75–2.96 s per full-frame decode, approximately:

- 1.16–1.24 s unpacking Nikon HE into Bayer samples.
- 1.09–1.15 s LibRaw development: black correction, WB/scaling, bilinear demosaic
  and camera-to-sRGB conversion.
- 0.35–0.44 s gamma mapping and oriented bitmap creation.
- 0.08–0.11 s copying RGB16 into the Rust owner, plus 0.05–0.06 s native release.
- About 0.3 ms header parsing and 1–2 ms embedded thumbnail extraction.

These are diagnostic, warm-cache observations, not repeated-comparison medians.
Unlike JPEG, NEF thumbnails already use a small embedded preview; that preview is
not substituted for authoritative image/analysis data. The main serial CPU work
precedes GPU rendering, so low total CPU/GPU usage does not establish an I/O wait.

## Implementation

`native/parallel_raw.cpp` adds a `LibRaw` subclass and source-level adapters.
The exact Redlamp LibRaw commit and SHA-256 in `config/libraw.json` remain pinned;
all vendor sources and the neighboring checkout remain unchanged. The original
library is still compiled, linked and used for headers, memory ownership, masked
pixels/black calibration, global development policy and unsupported inputs.

### Full Nikon HE unpack

- Admit only the identified Nikon HE selector-13 Bayer decoder, supported picture
  header and full-sensor geometry, with even height, width divisible by eight,
  zero margins, square pixels and bounded workspace. HE*, other RAW decoders,
  half-size/cropped processing and unknown profiles retain the original path.
- Index the existing compressed strip without copying/resaving the source.
  Validate precinct spans, slice numbers and terminal markers before dispatch.
  Like the pinned entropy reader, this is not a promise of exhaustive detection
  of every corrupted entropy substream.
- Decode independent 64-row stripes with fresh predecessor state and a
  thread-local active picture header. Keep the original transfer LUT, exact
  integer arithmetic, short final-tail behavior and physical image-edge flags.
- Incoming overflow is copied into the first two coefficient rows but is not
  consumed by that tile's lifting arithmetic. Save every tile's outgoing carry;
  after all entropy jobs join, repair the next tile's first two coefficient rows
  from the preceding tile's carry. Only then run the two Bayer reconstruction
  passes, with a join between each pass. This avoids decoding a priming tile for
  every full-frame stripe and preserves seam dependencies.
- Reconstruct directly into LibRaw's allocated raw owner rather than an additional
  full Bayer canvas and final full-CFA copy. RAII frees predecessor allocations
  that the pinned orchestration does not otherwise release per tile.

All 87 stripes are decoded for a full frame. This change is **parallel full-frame
processing**, not viewport-first/progressive NEF loading or a reduced entropy
decode. No lossy recoding or embedded-preview replacement is involved.

### Development and output

The pinned library explicitly exposes virtual processing hotspots. The adapter
overrides Bayer copy/black subtraction, channel scaling, the bilinear interior
loop, and RGB conversion/histogram accumulation. Independent rows/blocks run in
parallel; border interpolation and global calibration remain in the base library.
The bilinear code reads only native CFA channels from neighboring pixels, not
their concurrently written interpolated channels. Private block histograms are
merged with exact integer addition instead of sharing writable counters.

Gamma mapping and all eight LibRaw orientation flags are applied in a cache-tiled
parallel bitmap pass with the same generated gamma curve and samples. The final
RGB16 copy into Rust is still present. White balance, global maximum adjustment,
sRGB matrix, bilinear quality, brightness and 16-bit analysis data are unchanged.

Native jobs synchronously reuse the caller's existing bounded Rayon decode pool,
including concurrent selected-frame jobs. Headless callers use one lazy fallback
capped at six workers. There is no per-file/per-stripe thread pool, OpenMP runtime
or additional codec DLL. Native callback exceptions/cancellation are caught before
returning through Rust; the worker joins before owners can be released. Failed or
cancelled work never publishes a partial frame to the cache.

The adapted C++ retains the original LGPL-2.1/CDDL-1.0 attribution and is
distributed under the CDDL option. Its source and notice accompany local ZIPs
under `build-info/native/` and `LICENSES/LibRaw-adapters.txt`.

## Native correctness and diagnostics

The read-only actual-corpus test compares complete CFA and developed RGB16
buffers to an independently executed, unaccelerated pinned LibRaw path with
identical settings and orientation. All six files matched every sample with
both **one and six workers**: 45,705,600 CFA samples and 137,116,800 RGB16 channel
values per input. No tolerance, normalization, geometry fitting or visual-only
comparison is used. Sources remain unchanged.

In the final guarded six-worker diagnostic run, native calls took 715–742 ms,
versus 2469–2578 ms for the reference; unpack was 320–347 ms, processing
327–347 ms and bitmap creation 53–55 ms. These timers exclude Rust's final owner copy, native
release, GPU startup/rendering and presentation, and use optimized-debug Rust
with optimized native C++. The one-worker candidate was sample-exact but slower
than the serial reference (approximately 2.9 s vs 2.5 s); do not describe these
adapters as intrinsically faster independent of available parallelism.

Additional deterministic native tests compare all processing kernels against
their original base methods on thin/odd dimensions, three black-correction
layouts, clipping, raw/color matrices, exact histograms, gamma/brightness and all
eight output flips. Precinct-index corruption cases are rejected. Actual-file
cancellation was checked across entropy, reconstruction, development and output,
followed by simultaneous decodes in the same six-worker pool with matching CFA
and RGB hashes. A truncated actual input was rejected.

Evidence: `target/nef-parallel-review/final-native-parity.json`; the native
tests are in `src/decode/nef_tests.rs` and `native/parallel_raw.cpp`.

Final unit suites passed **159** feature-disabled tests (14 optional tests
ignored) and **164** third-feature tests (16 ignored). The actual-corpus parity
and cancellation/concurrent-different-file tests above were additionally invoked
explicitly, not counted as if all ignored tests had run. The release build
succeeded with only the two existing unused-function warnings. `Cargo.lock` and
codec/model pins are unchanged.

## Repeated release comparison, 2026-11-02

Three separate-process repetitions per input, alternating baseline/candidate
order, Ryzen 5 5600, warm OS cache. Baseline is the preserved JPEG-optimized
third application; candidate retains its DNG/JPEG/model work and adds NEF
parallelism. No competing build or benchmark ran during these measurements.
`--probe` times complete read/unpack, authoritative full-resolution development,
gamma/orientation/bitmap creation, Rust RGB16 ownership copy and native release.
Thumbnail extraction, hashing, process startup, GPU and presentation are excluded.

| NEF | Previous median | Candidate median | Speedup |
| --- | ---: | ---: | ---: |
| `AK8_0005.NEF` | 2696 ms | 868 ms | 3.11x |
| `AK8_0036.NEF` | 2719 ms | 863 ms | 3.15x |
| `AK8_0149.NEF` | 2691 ms | 875 ms | 3.07x |
| `AK8_0988.NEF` | 2674 ms | 866 ms | 3.09x |
| `AK8_1588.NEF` | 2687 ms | 874 ms | 3.07x |
| `AK8_1714.NEF` | 2701 ms | 876 ms | 3.08x |

All 18 baseline/candidate pairs matched dimensions, complete RGB16 SHA-256 and
sample sums, with unchanged source hashes, sizes and modification times. The
candidate also matched the preceding application on two actual JPEGs, two actual
JXL DNGs, a progressive CMYK JPEG and a grayscale16 PNG. Those regression cases
are pixel checks, not repeated speedup claims for unchanged paths.

Representative `AK8_0005.NEF` three-run stage medians:

| Stage | Previous | Candidate |
| --- | ---: | ---: |
| Header open | 0.24 ms | 0.26 ms |
| RAW unpack | 1137 ms | 335 ms |
| LibRaw development | 1095 ms | 345 ms |
| Gamma/oriented bitmap | 331 ms | 54 ms |
| RGB16 copy into Rust | 89 ms | 88 ms |
| Native owner release | 49 ms | 50 ms |

The native trace's `rayon_threads` describes the outer caller's registry; a
headless caller can print its global registry size even though native jobs use
the bounded six-worker fallback. It is not evidence of twelve NEF workers.
Medians of stages need not sum to the median complete decode.

The candidate's actual asynchronous `Services`/DX12 path was separately repeated
three times at 2560x1440 Fit on RTX 3070, with medians **849 ms decode**, **87 ms
GPU upload/render/readback**, **935 ms request-to-rendered-buffer ready**. Separate
GPU initialization took **322 ms**. This uses optimized-debug Rust and optimized
C++ in a production-services test executable, not the release applications above.
The output hash matched the saved baseline. No equivalent baseline asynchronous
NEF profile was run, so no baseline-to-candidate Services speedup is invented.

These are not exact visible-image or cold-start times: GPUI texture upload,
screen presentation, concurrent thumbnail/scanner work and GPU startup are not
in the ready timer. The GPU renderer itself was not changed, and full-frame NEF
preparation is still CPU work. Source caches were not flushed. No claim of
universal 3x acceleration for other cameras, HE* or other RAW formats is made.

Drivers/evidence: `target/nef-parallel-review/validate_nef.py`,
`release-comparison/`, `profile_services.py`, and `services-profile/`.

Updated cumulative EXE: `target/release/scrutivue-single-parallel.exe`,
**32,152,064 bytes**, SHA-256
`4ba7b67f6d9e3c55fccac135f173036772f8df8c755a7bb67a26cacff301eaf8`.
Baseline EXE SHA-256:
`881d97059263f29351aca1e7e8e4afecd1c8803a8c43c181ebafc2547979c252`.

## Package and native GUI validation

Local portable archive: `target/nef-parallel-review/Scrutivue-windows-x64.zip`,
**24,471,977 bytes**, SHA-256
`8037a947808e7abc10e3e67bf5ec64873d4d1bbf7c75ce8f44c2f3641bc62b41`.
The packed EXE matches the measured candidate. The archive includes the matching
ONNX Runtime, unchanged external LPIPS model and pinned JPEG XL bundle/notices,
plus the adapted C++ sources and license notice. No new runtime DLL is needed.

Extracted CLI smoke passed from a spaced/Unicode installation and another working
directory: native DNG direct completion and full-development fallback with legacy
RGB16 equality, statistics and external LPIPS. Missing/corrupt-model isolation
also passed while native statistics remained available and a valid decoy model
in the working directory was correctly ignored.

No-capture native GUI smoke on the actual six-NEF directory passed **830 event-loop
probes, zero 100 ms timeouts**, maximum **15.35 ms**, at 2x DPI. It exercised early
Slider/Diff/Slider changes, divider dragging, Single/paired selection, selection
stress, panning/clamping, navigator gestures, fullscreen, statistics/picker input,
Help and settings restart. These are input/persisted-state assertions, not visual
pixel observation or image-ready measurements; the 313 ms window-opening and
3.06 s multi-gesture early-input intervals must not be called image-loading times.

The final integrity check again verified all six original source hashes, sizes
and modification times. The previous three EXEs are preserved under `previous/`;
the normal and runner-only release EXEs, ONNX Runtime, external model and JPEG XL
assets remain byte-for-byte unchanged. The full model payload is absent from the
new EXE. Evidence: `gui-release-no-capture/result.json`, `preservation-check.json`
and `final-report.json` under the NEF review directory. Formatting/whitespace
checks passed. No commit, push, remote build or release update was performed.

## Reproduction

```powershell
$env:SCRUTIVUE_NEF_PROFILE_DIRECTORY = 'D:/Photos/six-NEFs'
$env:SCRUTIVUE_NEF_PROFILE_OUTPUT = 'target/nef-parallel-review/new-native-parity.json'
cargo test --locked --features experimental-single-parallel --bin scrutivue nef_parallel_matches_pinned_cfa_and_rgb_references -- --ignored --nocapture
cargo test --locked --features experimental-single-parallel --bin scrutivue nef_parallel_cancellation_concurrency_and_truncation_are_safe -- --ignored --nocapture
```

The corpus comparison expects the requested six-file inventory; use a new output
filename. The separate actual asynchronous Services/GPU profiler takes an exact
`SCRUTIVUE_NEF_PROFILE_PATH` and fresh `SCRUTIVUE_NEF_PROFILE_OUTPUT` and is named
`nef_async_decode_render_profile`. It does not measure GPUI presentation or folder
thumbnail contention.

```powershell
cargo build --release --locked --features experimental-single-parallel --bin scrutivue-single-parallel
.\target\release\scrutivue-single-parallel.exe --probe 'D:/Photos/example.NEF'
```

The NEF adapter is shared by all newly built targets. The local cumulative third
target retains the previous DNG and JPEG improvements. Keep its matching
`onnxruntime.dll`, external `models/` and `libjxl/` beside it. Original normal and
runner-only EXEs and the preceding third build are preserved; no commit, push,
remote workflow or published-release change is part of this work.
