# Visible-first and progressive DNG loading

## Viewer behavior

Single, Slider and normal Diff use a background tile loader when the selected files are
supported **tiled JPEG XL LinearRaw RGB16 DNGs**. Existing Modular DNGs benefit
from spatial tile selection even though they have no usable JXL resolution
passes. No embedded JPEG is used for the main view.

- At 100% or above, decode the tiles intersecting the viewport, with sampling
  context, before touching unrelated tiles. Keep decoded raw/developed tiles for
  the selected pair. Panning reprioritizes missing tiles; revisiting a cached
  area does not decode them again.
- **Slider:** A and B load simultaneously from their respective sides of the
  divider: A toward the left, B toward the right. Tile ordering uses oriented,
  cropped display coordinates, not raw TIFF tile indices. Moving the divider
  reprioritizes missing tiles while loading; completed views still change only
  clipping, without decoding or creating new image handles.
- **Diff:** both sources load simultaneously from the viewport center outward.
  Display a difference only where both corresponding regions have fully decoded
  tiles; missing or coarse regions remain gray. Compute absolute per-pixel
  differences **before** reducing them for zoom-out, using the normal gain and
  pixel-center alignment when dimensions differ. Incremental viewport mip caches
  avoid repeatedly recomputing unchanged regions. This remains a display-only
  preview, not authoritative picker or statistics data.
- Below 50%, a progressive source may first flush **1/8 detail for a tile**, then
  continue the **same decoder** to its final pixels. There is at most one retained
  progressive decoder per selected source. Unsupported/absent stages complete
  normally; there is no separate embedded-preview substitution. Diff skips these
  coarse stages so that unequal preview resolutions cannot create false differences.
- The automatic policy deliberately does **not** flush 1/4 and 1/2 as well:
  flush/development/upload are not free. The decoder supports and tests those
  continuation points, but an unconditional four-step ladder adds unnecessary
  work. These are incremental **tile** updates, not a guarantee that a complete
  whole-image 1/8 raster appears before any tile is refined.
- After **250 ms without a new view request**, fill remaining tiles in the
  background. Either source's visible work always outranks the other's offscreen
  work. Both sides share one bounded pool of two to six threads, including paired
  final development. Low-zoom batches expand after the first update without adding
  threads. Provisional viewport snapshots are throttled to approximately 40 ms;
  offscreen completion updates progress without recompositing an unchanged view.
- Once raw tiles are complete, assemble an uncompressed DNG **in memory using
  original metadata** and run the pinned LibRaw's normal full development.
  There is no second JXL entropy decode. The final frame uses the existing GPU,
  high-pass, navigator, picker, statistics and frame-identity paths.
- Display-tile development is **provisional**: regional LibRaw development can
  differ from whole-frame development. Tiles now keep the metadata white level;
  per-tile maximum normalization is explicitly disabled (see the exposure fix
  below), rather than accepting brightness seams as a preview limitation.
  The preview status explicitly says exact tools are waiting. Partial pixels
  are never inserted into analysis caches. High-pass still requests ordinary
  full decoding because it requires correct neighboring-pixel context.

Unsupported sources, spatial opcodes/linearization/patterned black levels,
oversized files, mixed non-DNG pairs and decode failures use the existing full
path. This is not a CFA/NEF partial-development implementation. Regional decode
does not run on the UI thread. Source mappings are read-only; source size/mtime
are checked before publishing full completion. Stale view generations are not
displayed and large allocations are released by workers.

Supported image geometry is limited by a **768 MiB per-source nominal raw plus
developed estimate**, with at most the selected pair retained. This is not a hard
allocation or process peak-RSS cap: padded edge tiles, native working memory,
full completion, renderer buffers, mappings and the final frames can add memory.
Tile cache lifetime currently covers the live
selection/loading session; it is not a persistent on-disk raw tile cache.
Completed streamed frames also seed the existing revision-aware full-frame
cache. Reselecting a retained source can retrieve that frame asynchronously,
without redeveloping it or performing filesystem checks on the UI thread.
If one selected frame is already complete, the tile worker reuses it while
loading the other source instead of decoding the completed side again.

## Native runtime in Windows distributions

Windows distributions include the pinned native runtime and its notices. Custom
development builds without it can still load tiles through the existing
jxl-oxide decoder, but no native early-resolution flush is attempted. Native
progression requires the audited Windows x64 libjxl 0.12.0 bundle whose individual
DLL hashes are pinned in `config/libjxl.json`. Only `libjxl/jxl.dll` beside the
running executable is considered, not the working directory, PATH or photos.
All pinned dependencies are verified before loading; normal MSVC runtime lookup
uses restricted loader search flags. Keep the matching upstream license notices.

With that bundle installed, **full-resolution tile decoding also prefers
libjxl**, not just progressive previews. The application reads the TIFF tile
table and passes each independent JXL tile stream to the native decoder; libjxl
does not parse the enclosing DNG. This is different from arbitrary tile access
inside a single monolithic JXL image. jxl-oxide remains the fallback and supplies
a first-tile header inspection used only as a progressive scheduling hint. Decoder
names are deliberately absent from normal viewer loading status.

```powershell
python scripts/prepare_jxl_runtime.py --profile release
cargo build --release --locked
target/release/scrutivue.exe 'D:/Photos/halo'
```

The default preparation command downloads the official `v0.12.0`
`jxl-x64-windows.zip` from the URL recorded in `config/libjxl.json`, verifies
archive SHA-256 `4ee3f72134a2a49774c357f2bffecf30d752d702147896c493578789de15a97c`,
then verifies all five DLLs and ten notices. Those DLL hashes are unchanged from
the previously audited local bundle. The source ZIP uses Deflate64: 7-Zip is
required during preparation, not by users of the resulting application ZIP.

The normalized source is cached under `vendor/libjxl`; installation goes only
into `target/<profile>/libjxl/` and refuses to replace different DLLs or notices.
`--source 'D:/Tools/libjxl-0.12.0'` retains an audited local-bundle option, and
`--archive 'D:/Downloads/jxl-x64-windows.zip'` supports the exact pinned ZIP offline.
CI prepares/caches the source, then installs after restoring Cargo outputs so an
old cache cannot replace the selected runtime. Workflow dispatch remains manual.
Packaging now fails without the matching runtime/notices, and the extracted ZIP
must pass a native tiled-DNG probe, not merely contain DLL filenames. Include
the whole `libjxl/` directory; matching `onnxruntime.dll` must also remain beside
the EXE. Restart after changing runtime installation.

Ordinary full decoding of supported sources uses the **same native backend** as
streamed final reconstruction when this bundle is installed. This matters for
VarDCT: independent codec implementations can round reconstructed samples
differently. When the optional bundle is absent both paths use jxl-oxide. The
Redlamp LibRaw revision/settings are not substituted. An unsupported native
full-decode failure falls back to the existing oxide path; cancellation does not
restart decoding through another backend.

## Two progressive test copies

Created under `target/progressive-dng-samples/`:

| File | Raw dimensions / tiles | File bytes | Raw MAE / maximum error |
| --- | --- | ---: | ---: |
| `AK8_0005_halo0-lossy-progressive.dng` | 8280x5520 / 54 | 136,499,842 | 37.42 / 4071 |
| `AK8_0988_halo48-lossy-progressive.dng` | 8280x5520 / 54 | 122,111,234 | 35.13 / 1683 |

These are explicitly **LOSSY VarDCT test derivatives**, not pixel-preserving
replacements for the original DNGs. The checked libjxl progression path cannot
be obtained by just setting a decoder option on the existing Modular tiles.
The original files were only read and their SHA-256/mtime stayed unchanged.
Full reconstruction is exact **relative to each converted file**, not relative
to its original. Do not use these copies as lossless RAW comparison references.

The converter uses 1024x1024 progressive JXL tiles, original-profile 16-bit
camera-linear values, effort 4 and distance 0.1. It validates advertised
8/4/2 stages and final reconstruction for each tile. The JSON beside each copy
records source identity, errors, dimensions, tile hashes and stages. Small
physical-edge tiles need not advertise every ratio.

TIFF orientation, crop, calibration and the original thumbnail are retained;
the software/description tags explicitly identify the lossy derivative, and
stale raw digests are removed. A new raw IFD points to the progressive payloads.
For safe metadata preservation the original TIFF bytes remain in the test copy,
including unreferenced old raw payloads: **the copies are not compactly packed**.
This converter currently requires a classic TIFF with the LinearRaw JXL image
in IFD0, as in the supplied corpus; it is not a universal DNG writer.

### Why these copies are larger

The conservative test writer appends new payloads to the original TIFF instead
of relocating every retained metadata/preview offset. The new raw IFD references
only the new JXL tiles, but the old compressed raw bytes are still physically in
the file, unreferenced. This is a writer limitation, not evidence that progressive
coding intrinsically increases the file size.

| Derivative | Original file | Appended active JXL tiles | Resulting test file |
| --- | ---: | ---: | ---: |
| `AK8_0005_halo0` | 109,066,514 B | 27,432,308 B | 136,499,842 B |
| `AK8_0988_halo48` | 95,731,632 B | 26,378,582 B | 122,111,234 B |

The remaining appended bytes are approximately 1 KiB of new tables/IFD/text.
A proper TIFF repack could remove obsolete raw payloads while retaining metadata
and previews. No originals or existing derivatives were repacked or replaced.

### Lossless progressive versus this viewer's native pause API

JPEG XL **does support lossless progressive coding**, using Modular and its
reversible Squeeze/responsive representation. The public encoder exposes
`JXL_ENC_FRAME_SETTING_RESPONSIVE = 16` for Modular progression, independently of
`JxlEncoderSetFrameLossless`. It would be incorrect to say progression inherently
requires lossy VarDCT.

The narrower limitation is the tested libjxl 0.12.0 **LastPasses / intended-ratio
pause API** used by this viewer: `FrameDecoder::SetPauseAtProgressive` explicitly
excludes Modular. A lossless responsive control therefore encodes and reconstructs
exactly, but does not yield the 8/4/2 pause events through this particular path.
This does **not** mean the native library cannot expose intermediate lossless
images: supplying partial input and flushing at `JXL_DEC_NEED_MORE_INPUT` works
on the control below. Supporting that continuation needs a separate input-feeding
and display policy, not necessarily a replacement decoder. It must not be
advertised as implemented by this viewer. Visible-first **lossless tile loading**
already works independently of that limitation.

Reproduction:

```powershell
python experiments/preview/lossless_responsive_probe.py `
  --library-directory target/release/libjxl `
  --output target/NEW-lossless-responsive-review
```

The control is RGB16, 1024x768, strict lossless, forced Modular, responsive=1;
all final samples matched. Independent frame inspection found Modular, 12 groups,
one frame-header pass and no downsample targets; requesting 1/4 through the native
LastPasses loop completed the full frame without an intermediate event.

The same runtime also decoded this control incrementally on **one decoder**:
feeding 50% and 75% of its 31,061-byte stream produced successful nonempty partial
flushes, with sample MAE 45.37 and 16.14; completing the input gave **zero error**.
That test is retained under `target/lossless-responsive-partial-input-review/`.
These partial images do not advertise 1/8, 1/4 or 1/2 intended resolution, and
successful flushes do not establish free repeated reconstruction, small output
allocations, or performance on actual DNG tiles. This is a synthetic codec
control, not a newly created lossless DNG derivative or production integration.

## Make more copies

Python 3.11+ and NumPy are needed **only for this offline conversion script**.
The viewer does not use Python for decoding. Choose a new output directory;
existing files are never overwritten. `--allow-lossy` is mandatory.
New conversions write a `.dng.partial` staging file, verify source integrity,
then rename it and exclusively create the JSON report. Failed conversions leave
their staging artifact for inspection rather than replacing an existing copy.

```powershell
python scripts/make_progressive_dng.py `
  'E:/Documents/AI/corpus/3 этаж single/halo/AK8_0005_halo0.dng' `
  'E:/Documents/AI/corpus/3 этаж single/halo/AK8_0988_halo48.dng' `
  --library-directory target/release/libjxl `
  --output-directory target/NEW-progressive-dng-samples `
  --tile-size 1024 --distance 0.1 --allow-lossy
```

`--tile-size` accepts 256/512/1024/2048: smaller tiles improve region granularity
but add decoder/development overhead; larger tiles offer coarser spatial access.
`--distance` is a lossy encoder quality parameter, **not a bound on raw-sample
error**. Reducing it must not be represented as losslessness. Inspect the saved
error report and compare appearance before adopting generated files elsewhere.

## Validation

The production `--stream-probe` command loads a 512x512 100% view, pans, returns,
optionally obtains a coarse stage, fills cached raw tiles, develops the complete
frame, and compares it sample-for-sample with independent ordinary full decoding.
It also reports regional-development differences separately, rather than
claiming every provisional tile is authoritative.

```powershell
cargo test --locked
target/release/scrutivue.exe --stream-probe 'D:/Photos/input.dng'
python scripts/validate_dng_streaming.py --output target/dng-streaming-validation/NEW-run
```

Source-level tests cover orientation inverses (all eight EXIF cases), visible
tile selection, pan overlap/reuse, B-visible versus A-idle priority, incomplete
cache rejection, cancellation, modified-source rejection and cached final
equivalence. Paired tests cover divider-aware ordering, center-first Diff, actual
cross-thread overlap, completed-source reuse, missing/coarse masks, unequal image
dimensions and incremental/full CPU Diff agreement across zoom and gain settings.
A native integration test exercises **8→4→2→full on one decoder**, transferring
its exclusive ownership between threads before resuming:

```powershell
$env:SCRUTIVUE_JXL_TEST_LIBRARY = 'D:/Tools/libjxl-0.12.0/jxl.dll'
$env:SCRUTIVUE_PROGRESSIVE_TEST_DNG = 'D:/Photos/input-lossy-progressive.dng'
cargo test --locked native_progressive_resume -- --ignored --nocapture
```

The paired probe exercises the actual asynchronous production worker and compares
completed frames with independent full decoding. Diff additionally compares its
fully populated visible tile preview with the full CPU renderer (at most one
8-bit rounding unit). Optional PNGs are direct compositor buffers, not desktop
screenshots. Choose a new output directory:

```powershell
target/release/scrutivue.exe --stream-pair-probe diff-fit `
  --stream-probe-output target/NEW-paired-preview 'D:/Photos/A.dng' 'D:/Photos/B.dng'
python scripts/validate_paired_streaming.py --output target/NEW-paired-validation
```

Probe modes are `slider`, `diff`, `slider-fit` and `diff-fit`. The sequential
10-case runner covers original and progressive pairs at 100% and Fit, plus
mixed 256/1024-tile Diff pairs. It verifies early A+B progress, full-frame equality,
partial/full Diff agreement and unchanged source SHA-256/size/mtime. PNG encoding
can affect observation timers; these are not GPU presentation measurements.

Native input smoke can also move the divider and switch paired modes immediately
after startup, before its ordinary warm-view tests:

```powershell
python scripts/ui_smoke.py --exe target/release/scrutivue.exe `
  --path target/progressive-dng-samples --streaming --picker --statistics `
  --no-capture --output target/NEW-paired-native-smoke
```

`--no-capture` is for unavailable/obscured desktops. This smoke verifies native
input, persisted state, responsiveness and restart, not decoded-tile readiness or
visible pixels. Use the worker probes for exact tile/render-buffer assertions.

The 32-input runner requires the supplied 30-file corpus and both derivatives.
It checks SHA-256/size/mtime before and after each file, final equality, exactly
one completed entropy decode per tile and zero new tile decodes on returning to
the original viewport. Measurements use fresh sequential processes and six inner
tile workers. Input hashing warms the Windows file cache. `ready_ms` measures
tile decode plus regional development, not GPU upload, presentation, total
startup time or a cold-storage benchmark.

### Baseline release validation, 2026-11-01 (before the exposure fix)

`cargo build --release --locked --offline` succeeded. The serial 32-input run is
recorded in `target/dng-streaming-validation/release-32-20261101/` (manifest
includes the tested EXE hash and every source hash/revision).

| Inputs | First visible region, median | Missing region after pan, median | All tiles, median | Final developed frame, median |
| --- | ---: | ---: | ---: | ---: |
| 30 original DNGs | 32.7 ms | 13.7 ms | 1.89 s | 3.70 s |
| 2 progressive derivatives | 78.3 ms | 73.5 ms | 0.91 s | 2.74 s |

- **32/32** final cached-tile frames matched fresh full decoding sample-for-sample;
  every source retained its SHA-256, size and modification time.
- Original views decoded **9 → 3 → 0** new tiles for initial view, pan and return;
  derivatives decoded **2 → 2 → 0**. Exactly one completed entropy decode was
  counted per source tile over the full probe.
- Both derivatives yielded a genuine **1/8** stage and continued the same native
  decoder to full; none of the 30 Modular originals advertised an early stage.
- Across the originals, regional spot checks reached a maximum provisional
  difference of **11024/65535** from full LibRaw development. This is not an error
  in final cached reconstruction, but it **was a display-path bug**, subsequently
  traced and corrected below; the preview designation did not make brightness
  seams acceptable. Both derivative regional
  spot checks happened to match, which is not a general exactness guarantee.
- Unit tests: **116 passed, 5 ignored**. The explicitly enabled native
  8→4→2→full continuation test also passed. Isolated experimental preview and DNG
  harness tests passed after the production-module bridge update. Python syntax,
  Rust formatting, transactional conversion/refusal to overwrite, and optional
  runtime packaging/notices checks passed.
- A separate portable EXE with **no libjxl directory** passed original and
  progressive-file probes through jxl-oxide, including exact final equality and
  zero new decodes on viewport return. Records are under
  `target/dng-streaming-validation/oxide-fallback/`.
- The native GUI smoke run on both progressive files passed selection stress,
  Single/Slider/Diff transitions, left/middle pan, edge clamping, wheel/navigator
  zoom, divider movement, fullscreen, picker/statistics interaction and saved
  settings restart. It made **592** event-loop probes with **zero** 100 ms
  timeouts (maximum 7.95 ms). This measures responsiveness, not image latency.
  Result: `target/dng-streaming-validation/gui-native-no-capture-final/result.json`.
- **Visual validation remains uncompleted:** PrintWindow returned black and a
  system `Backstop Window` obscured the test window even when raised. The capture
  helper refused to screenshot another application's surface. The GUI smoke was
  therefore rerun with `--no-capture`; no screenshot/pixel-level confirmation of
  the visible preview-to-exact transition or warm-reselection presentation is
  claimed. Full-frame cache reuse is covered by the revision-cache tests.

The GUI harness now waits for a stable edge instead of assuming 32 pan gestures
can reach it: these 45 MP images at 800% require more movement than small test
fixtures. The initial fixed-gesture assertion failed for that reason; no camera
implementation change was needed.

### Fix: one bright tile in AK8_0036_halo*.dng

All five requested variants reproduced the problem at **zero-based tile 349**,
stored raw origin **(4864, 2560)**, extent **256x256**. The DNG white level and the
whole-image maximum are both 65535, but that tile's maximum is 51021–51605.

LibRaw's `dcraw_process` calls `adjust_maximum` separately from auto brightness.
The default `adjust_maximum_thr=0.75` replaces the white level when
`0.75 * white < local_maximum < white`. Consequently this one compact tile
received a scale multiplier of **1.270–1.284**, while other tiles did not.
`no_auto_bright=1` only disabled histogram brightening and did not prevent this.

The caller was wrong to treat each tile as an independently normalized image.
The native regional entry point now sets **`adjust_maximum_thr=0`**, retaining
the metadata white level consistently across tiles and coarse stages. Normal
whole-frame processing and its settings are unchanged; the pinned LibRaw source
and original DNGs are untouched.

- A synthetic test reproduces the old content-dependent brightness, checks
  identical shared pixels for differently peaked tiles after the fix, and covers
  1/8 development plus cancellation.
- The explicit requested-corpus test compared **all 196,608 samples of tile 349
  in each of the five files** with an independently developed full-frame crop.
  Before: every sample differed, maximum error 7123–7497/65535. After: **zero
  differing samples and zero maximum error** in all five. Source hashes/revisions
  remained unchanged.
- `target/dng-tile-exposure-review/corrected/` contains JSON and side-by-side
  pixel evidence: **legacy tile / corrected tile / full-frame reference**, in
  stored tile orientation. These are direct decoder outputs, not GUI captures.

```powershell
$env:SCRUTIVUE_TILE_TEST_CORPUS = 'E:/Documents/AI/corpus/3 этаж single/halo'
$env:SCRUTIVUE_TILE_TEST_OUTPUT = 'E:/Documents/AI/Diff/target/NEW-tile-exposure-check'
$env:SCRUTIVUE_JXL_TEST_LIBRARY = 'E:/Documents/AI/Diff/target/release/libjxl/jxl.dll'
cargo test --locked requested_halo_tile_exposure -- --ignored --nocapture
```

The corrected release was rebuilt successfully and the complete **32-input**
runner was repeated under `target/dng-tile-exposure-review/release-32-corrected/`.
All final frame hashes remain identical to the baseline release; original file
hashes/revisions stayed unchanged. Regional spot comparisons now report **zero
differences for all 32 files**, versus the earlier local-normalization errors.
Pan reuse still performs zero new tile decodes on viewport return. The complete
unit suite now reports **117 passed, 6 ignored**; the requested five-file exposure
test was additionally run explicitly and passed. The isolated preview harness
also passed 35 tests with the native runtime enabled (2 optional tests ignored).

### Paired Slider/Diff release validation, 2026-11-01

`cargo build --release --locked --offline` succeeded with the paired scheduler
and incremental Diff compositor. The tested EXE is
`target/release/scrutivue.exe`, SHA-256
`eab42c32d51d08e70f464fc392292ef7a8bbcd1b9a80aec9d30fe3019162bc75`.
The matching ONNX Runtime and audited `libjxl/` bundle remain beside it.

The sequential **10/10** production-worker matrix is recorded under
`target/paired-streaming-review/release-cases/`:

| Pair / mode | First observed A+B tile progress | Both full developed frames |
| --- | ---: | ---: |
| Original / Slider 100% | 31.9 ms | 6.05 s |
| Original / Diff 100% | 70.3 ms | 6.04 s |
| Original / Slider Fit | 31.3 ms | 5.77 s |
| Original / Diff Fit | 54.8 ms | 10.86 s |
| Progressive / Slider 100% | 106.4 ms | 3.96 s |
| Progressive / Diff 100% | 157.0 ms | 3.87 s |
| Progressive / Slider Fit | 115.6 ms | 6.26 s |
| Progressive / Diff Fit | 380.8 ms | 7.33 s |
| Mixed 256/1024 tiles / Diff 100% | 153.1 ms | 5.65 s |
| Mixed 256/1024 tiles / Diff Fit | 133.1 ms | 9.24 s |

- Both sources progressed before full completion in every case; all completed
  frames matched independent ordinary full decoding sample-for-sample.
- All six Diff cases matched the full CPU reference with **zero byte error**.
  Saved initial/completed buffers show the center-out readiness mask and
  divider-adjacent Slider loading. These are compositor PNGs, not GUI screenshots.
- All source SHA-256/size/mtime identities remained unchanged. Progressive test
  copies are still the existing lossy derivatives; none were regenerated.
- Four representative single-source release regressions (two originals and both
  derivatives) also passed: returning to a cached viewport decoded zero new tiles,
  each raw tile completed one entropy decode, regional spot error remained zero,
  and final frame hashes matched the preceding exposure-corrected release.
  Sources retained their recorded identities. Results and the reproduction script
  are under `target/paired-streaming-review/single-source-regressions/` and
  `target/paired-streaming-review/check_single_regressions.py` respectively.
  The complete 32-file matrix above was not repeated for this paired-only change.
- The full unit suite passed **125 tests, 6 ignored**. The explicitly enabled
  native 8→4→2→full test also passed after transferring the retained decoder
  between threads. Formatting and Python syntax checks passed.
- The native progressive-pair smoke passed early divider movement and
  Slider→Diff→Slider input, subsequent selection stress, left/middle pan and edge
  clamping, wheel/navigator interaction, fullscreen, picker/statistics and restart.
  **845** event-loop probes had **zero** 100 ms timeouts (maximum 26.73 ms).
  Result: `target/paired-streaming-review/gui-release-no-capture-synchronized/result.json`.
  It used `--no-capture`: no GUI screenshot or visible-pixel verification is claimed.

Two preliminary extended-smoke runs failed selection-reset/edge assertions. The
harness now isolates the early scenario with a clean test-process restart and
uses a persisted camera-neutral picker toggle as a barrier after edge-pan batches,
rather than interpreting an older settings save as a settled camera. The final
run reached the legal edge and confirmed an additional outward drag did not move
it. No application camera behavior was changed to satisfy these checks.

These are warm-storage worker observations, not GPU presentation latencies.
PNG encoding can affect observation timers, and Fit includes display-only
full-resolution pixel-difference reduction work before final development.

Generated measurements, captures and native binaries belong under `target/`.
The legacy/root EXEs are not overwritten. No original photo, LibRaw pin, workflow
trigger or unrelated uncommitted work was changed.
