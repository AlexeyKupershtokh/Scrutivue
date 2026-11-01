# JPEG/JPEG XL preview and region decoding

Measured on 2026-11-01, Windows x64, Ryzen 5 5600. These are isolated developer
experiments, **not a viewer/backend change**. Source photographs, the production
LibRaw pin, application binaries and manual-only GitHub Actions remain unchanged.

## Practical answer

- **JPEG:** native libjpeg-turbo supports genuine 1/2, 1/4 and 1/8 output and
  partial-image decoding. The current `image` 0.25.10 / `zune-jpeg` 0.5.15 API
  exposes neither. Scaling saves substantial RGB memory but still requires
  entropy work. Preview followed by full resolution normally restarts decoding.
- **JPEG XL progression:** libjxl 0.12.0 can produce approximate stages and
  continue the same decoder, **if the encoding provides them**. It writes
  full-size upscaled output, not a reduced-size RGB allocation. The six actual
  DNG tiles tested provide no native progressive events or embedded JXL preview.
- **JPEG XL region:** jxl-oxide 0.12.5's `set_image_region(CropInfo)` genuinely
  avoids much reconstruction on a wide image. The same image object can later
  render the full canvas. No equivalent arbitrary ROI/reduced-dimension setter
  was established in the tested libjxl public API.
- **The actual DNG pair:** a separate embedded JPEG gives an oriented
  **854x1280 preview in 7.3–7.4 ms**. For high zoom, selecting DNG tiles first
  produces a 512x512 raw-sample region in **about 21 ms native / 28 ms Rust**.
  These samples are not yet the viewer's developed/oriented/cropped display RGB.

**Scope correction:** the user subsequently excluded embedded JPEGs from main-view
preview use. Their measurement is retained only as thumbnail evidence. The active
follow-up uses the actual 30 DNGs and 30 JPEGs in the supplied `halo` directory,
plus all six NEFs in the supplied source directory; results below originally
cover only the explicitly named pair/control inputs, not that expanded corpus.

Here 1/4 means one quarter of **each dimension**, or 1/16 of the output pixels.
None of the approximate previews may supply authoritative statistics, differences
or color-picker samples. An exact decoded raw region also needs the matching
development/color/coordinate pipeline before it can supply display-domain values.

## JPEG: measured reduced output and ROI

The real sequential 4:4:4 JPEG is 8256x5504, 45.44 MP, 32.7 MB compressed.
All entries are medians of three fresh processes. Memory is absolute Windows
process peak working set, **MiB**, not codec-only allocation.

| Output | SIMD-enabled turbo 3.1.4.1 through Pillow | Standalone C++ turbo 2.1.5.1, scalar |
| --- | ---: | ---: |
| Full | 376.70 ms / 228.62 MiB | 618.41 ms / 164.69 MiB |
| 1/2 | 334.17 ms / 98.54 MiB | 452.67 ms / 67.04 MiB |
| 1/4 | 307.89 ms / 65.92 MiB | 350.71 ms / 42.62 MiB |
| 1/8 | 268.09 ms / 57.72 MiB | 244.96 ms / 36.52 MiB |

Both paths invoke native JPEG decoding; Python does not implement the entropy
decoder. The standalone C++ build lacks NASM/Yasm and explicitly disables SIMD.
Different versions/wrappers/storage mean this is **not** a controlled SIMD
comparison. These numbers also do not benchmark the production zune decoder.

The C++ public APIs `jpeg_crop_scanline` and `jpeg_skip_scanlines` decoded a
1024x768 region at top/middle/bottom in **41.60 / 143.39 / 242.98 ms**, each
**36.86 MiB**, against that backend's 618.41 ms full output. Cropping and reduced
IDCT can be combined. This is not constant-time random access: preceding JPEG
MCUs still undergo entropy decoding. Ordinary progressive-JPEG ROI read every
scan and retained full-image coefficients; its savings were much smaller.

JPEG timings exclude compressed-file reads and include decoder/header/start,
output allocation, decoding and requested-row copying. Cleanup is separate.
No orientation, ICC/display conversion, upload or UI presentation is included.
The detailed evidence records file reads, native stages and memory separately.

All 36 main same-backend/same-scale ROI checks were exact. Additional 4:2:0
chroma-edge tests exposed differences up to 31/255 without padding; all six
16-output-pixel horizontal-halo cases were exact after trimming. Integration
must derive appropriate alignment/padding rather than assume any crop is exact.

Normal quarter-then-full decoding adds work: the SIMD path's measured individual
times sum to roughly **685 ms**, versus 377 ms for full only. This sum is an
inference, not an end-to-end UI measurement. Scaling is selected before start;
the low-resolution output cannot simply be upgraded without another decoder
cycle. Buffered progressive scan display is a different, unmeasured JPEG mode.

Details, API references and commands:
[JPEG findings](../experiments/preview/jpeg-notes.md).

## Native JXL: progression is encoding-dependent

`JxlDecoderSetProgressiveDetail(kPasses)`, `JXL_DEC_FRAME_PROGRESSION`,
`JxlDecoderGetIntendedDownsamplingRatio` and `JxlDecoderFlushImage` were exercised
through the public libjxl 0.12.0 ABI. The existing trusted DLL's SHA-256 remains
`b0a7f7721836359b29841cca0f15969e85bd386344f9ee0775292089bd3bcfda`.

Tiles **0, 363 and 725 from each real DNG** all reported `have_preview=false`
and no progressive events. Requests for half/quarter/eighth fell back to full
tiles, with exact final samples. This does not prove every possible JXL encoding
or every other tile behaves identically.

A generated 4096x3072 VarDCT progressive control did deliver **8 -> 4 -> 2**
detail stages and continued the **same decoder** to an exact independent final
decode. Its first 1/8 preview was about **149 ms**, quarter **258 ms**, half
**363 ms**. Ordinary full decoding was about **205 ms**; requesting/flushing
every stage delayed full completion to about **478 ms**. These are synthetic
native-library results, not timings for a full DNG or the current viewer.

**Timing correction:** those quarter/half calls flushed every earlier stage.
`djxl --downsampling` instead uses `kLastPasses` and only the selected flush;
the same control then measured **155 ms quarter / 177 ms half / 202 ms full**.
This does avoid later passes on suitable codestreams, while retaining full-sized
output. A [source trace and 360 actual-tile repeat checks](jxl-downsampling-review.md)
confirmed that the sampled DNG tiles are Modular/single-pass with no downsampling
targets, and still complete fully with that policy. The old timings must not be
read as a general argument against JXL downsampling.

Every intermediate output still required **75,497,472 bytes of full-size RGB16**.
The intended detail ratio is not the allocated dimensions. A callback that
discards output pixels would not establish partial reconstruction. Early preview
therefore does not automatically save memory or improve final completion time.

Details, controls, repeat measurements and pinned API references:
[native JXL findings](../experiments/preview/native-progressive-notes.md).

## Current Rust JXL: genuine region reconstruction

Two generated 4096x3072 JXL controls, AC progression disabled/enabled, were tested
with `JxlThreadPool::none()` (one decode thread). The region is stored-orientation
`[1788,1240,512,512]`. These are medians of three fresh processes:

| Case | AC progression disabled | AC progression enabled |
| --- | ---: | ---: |
| Full samples ready | 719.46 ms / 367.19 MiB | 737.87 ms / 367.27 MiB |
| 512x512 region ready | **51.11 ms / 22.41 MiB** | **52.18 ms / 22.36 MiB** |
| Reconstruction only, full | 537.32 ms | 554.01 ms |
| Reconstruction only, region | **46.62 ms** | **47.52 ms** |
| Sample streaming/allocation, full | 182.89 ms | 181.57 ms |
| Sample streaming/allocation, region | 3.74 ms | 3.84 ms |
| Later expansion to full, same image object | 732.08 ms | 736.53 ms |

Compressed-data parsing was about **0.7–0.8 ms** in either case. Reconstruction
and memory, not only the final crop/copy, shrink substantially: this is not a
full RGB decode followed by cropping. The current probe still feeds/parses all
compressed bytes; it does **not** establish selective disk reads for monolithic
JXL. Render dependencies and group boundaries mean cost is not simply pixel area.

Both ROI and expanded-full outputs matched same-backend full references exactly;
expansion also preserved the earlier ROI. Expansion is possible, but the measured
additional work is close to another full render, not a promise that all ROI work
is reused. Reference hashing/comparison occurs between stages outside their
timers, so first-ROI plus expansion is not continuous end-to-end load latency.
No cross-backend color/pixel equivalence is inferred from these synthetic tests.

### Incremental Rust rendering

`feed_bytes`, `try_init`, `render_loading_frame` and finalization were tested on
the two controls and all six extracted real tiles. Every incremental final
output equaled a fresh same-backend full decode.

Before EOF, a successful render can be incomplete, have missing channels/regions,
and have **non-monotonic error** as more bytes arrive. It is not a guaranteed
uniform half/quarter-resolution preview. For example, one real edge tile's
32x32 diagnostic-grid mean absolute error increased from about 5,268 to 12,119
u16 levels between 25% and 50% input, before reaching exact final output.

Repeated full-size snapshot rendering was expensive: the wide-control diagnostic
reached its final samples after **4.26–4.80 s**, versus **0.72–0.74 s** for ordinary
full rendering. That repeated-probe schedule is not an optimized streaming
implementation or a fair single-preview benchmark. It does establish that
calling `render_loading_frame` frequently is not a free latency optimization.

## Actual DNGs: embedded preview and tile-selective ROI

The sources are `AK8_0005_halo0.dng` and `AK8_0005_halo24.dng`, each an
8280x5520 RGB16 raw canvas with 726 independently compressed 256x256 tiles.
Read-only mmap avoids allocating/reading the entire source before planning ROI.
The experiment never creates the full virtual TIFF/LibRaw development for ROI.

### Embedded JPEG

The existing production embedded-preview function, reused without changes,
returned **854x1280 oriented RGB8**, 3,279,360 bytes, from a **46,112 / 46,126-byte**
JPEG. Decode plus orientation took **7.30 / 7.44 ms**, with **10.68 / 10.66 MiB**
peak working set at readiness. This is a producer-rendered approximation, not a
downsample of this application's final RGB16. The current thumbnail path already
extracts it, then reduces it to the small list thumbnail; progressive main-view
presentation would still require separate application work.

### Region results

Six outer workers, one serial decoder per selected tile, medians n=3:

| Region in raw coordinates | Tiles | libjxl halo0 / halo24 | jxl-oxide halo0 / halo24 | oxide sub-tile crop halo0 / halo24 |
| --- | ---: | ---: | ---: | ---: |
| `[3884,2504,512,512]` | **9 / 726** | **21.42 / 20.93 ms** | 28.36 / 28.78 ms | 27.47 / 27.90 ms |
| `[3904,2400,128,128]` | 1 / 726 | 9.00 / 9.00 ms | 13.36 / 12.71 ms | 12.30 / 12.34 ms |
| `[8200,5460,80,60]`, clipped edge tile | 1 / 726 | 7.65 / 7.58 ms | 11.20 / 10.80 ms | 9.96 / 9.89 ms |

The center region addresses **1,606,907 / 1,606,029 compressed bytes**, versus
109,011,199 / 108,605,398 bytes for every compressed tile, and allocates **1.5 MiB**
of returned RGB16 samples. Peak working set at region readiness was **18.80 /
18.35 MiB native**, **15.18 / 15.48 MiB oxide**. These counters are captured
**before** the correctness reference and whole-source integrity hash touch
additional mapped pages. Peak WS later in the process is not ROI-only memory.

Representative native center-stage medians, halo0 / halo24:

- TIFF parse/selection: **0.029 / 0.027 ms**.
- ROI output zero-allocation call: **0.005 / 0.006 ms**.
- Selected-tile wall time, including decoder setup/internal allocation,
  reconstruction, sample conversion, piece copying and decoder release:
  **20.991 / 20.390 ms**.
- Final region assembly: **0.374 / 0.419 ms**.
- File open/mapping creation: **0.055 / 0.052 ms**, separately measured.
- Worker-pool setup: **0.138 / 0.133 ms**, separately measured.
- Fresh-process trusted native DLL validation/loading: **4.776 / 4.937 ms**,
  separately measured; a long-lived viewer would normally load it once.

Stage medians need not sum exactly. A zero-filled vector can receive physical
pages on later writes: the tiny allocation-call time is **not** a measurement
of all memory first-touch costs. Tile-local allocations remain inside the
selected-tile interval, not a separately isolated allocator measurement.

The large gain is **skipping 717 unrelated compressed tiles**, not cropping
within a 256x256 tile. Applying oxide's sub-tile region adds only a modest local
benefit here. The earlier full-transport measurements were about **1.30 s native
/ 1.85–1.89 s Rust**, before roughly **1.19 s** of LibRaw development/crop/release.
Do not compare 21 ms raw ROI with full developed viewer RGB as equivalent outputs.

All **54 real ROI decodes** were sample-exact against complete same-input raw
canvas references, including right/bottom edges. The two newly generated full
references also match the previously established production canvas hashes.
These checks do not yet establish partial LibRaw processing: white balance,
color/opcodes, DefaultCrop, orientation, processing halos and any global-image
dependencies still need an explicit, equivalence-tested ROI development design.

## Reproduction and validation

New programs live in `experiments/preview/`; builds and raw references stay under
`target/`. No generated EXE/DLL/sample output is intended for version control.
Replace example paths with the two actual inputs and trusted DLL bundle:

```powershell
python scripts/build_dng_experiments.py
python scripts/build_dng_experiments.py --preview
$env:DNG_JXL_LIBRARY = 'D:/Tools/libjxl-0.12.0/jxl.dll'
python scripts/build_dng_experiments.py --preview --tests

New-Item -ItemType Directory -Force target/preview-native-probe | Out-Null
target/preview-experiments/preview_capabilities.exe --mode generate --library $env:DNG_JXL_LIBRARY --size 4096,3072 --output target/preview-native-probe/synthetic-4096x3072-false.jxl
target/preview-experiments/preview_capabilities.exe --mode generate --library $env:DNG_JXL_LIBRARY --size 4096,3072 --progressive --output target/preview-native-probe/synthetic-4096x3072-true.jxl

# Extract indices 0/363/725 from each DNG into the existing tile-results directory
# first, using dng_jxl_bench.exe --extract-tile NEW_JXL --tile-index INDEX INPUT.
# Use a new output directory: raw reference files are never overwritten.
python experiments/preview/run_capabilities.py --dng 'D:/Photos/AK8_0005_halo0.dng' --dng 'D:/Photos/AK8_0005_halo24.dng' --library $env:DNG_JXL_LIBRARY --repeats 3 --output target/preview-capability-results-new
```

Local evidence: `target/preview-capability-results-20261101/{manifest,
measurements,summary,reference-decodes}.json`, plus JPEG/native-progression evidence
linked above. The ROI/stream matrix completed **90 fresh-process probes**, plus
two full raw-reference decodes. The final combined experimental suite passed
**29 tests** with the real native DLL enabled. Photographic source hashes were
checked before/after and unchanged. All measured workloads were sequential, with
no overlapping builds; the OS cache was warmed by integrity checks. Mmap page
faults for addressed data are inside work time, not hidden in mapping creation.
This is **not** a cold-drive benchmark or UI latency measurement.

Only isolated optimized harnesses were rebuilt; the unchanged application was
not rebuilt/replaced for this capability-only work. Existing full-decode results
and intoPIX/Imprint support limits remain in
[DNG/JXL experiments](dng-jxl-experiments.md); neither is an established preview
backend for these actual JXL-DNG inputs.

## Candidate integration work, not implemented

1. Keep embedded DNG JPEGs **thumbnail-only**. Main-view partial output must come
   from the image's actual compressed raw/JPEG/JXL stream; approximate output
   cannot supply exact-value tools.
2. Add libjpeg-turbo scaled output for large ordinary JPEGs when its first-display
   benefit outweighs a second full decode; consider padded ROI for first high-zoom
   display, not constant-time panning. Cache/cancel stale work under a shared budget.
3. Use jxl-oxide's region rendering for monolithic JXL. For these tiled DNGs,
   prioritize visible tiles, then separately prove an equivalent developed-ROI
   pipeline. Retain exact tile/full caches for later pan/zoom and statistics.
4. Treat native progressive JXL as an optional encoding-dependent optimization,
   not a universal quarter-resolution path and not the primary fix for this pair.
