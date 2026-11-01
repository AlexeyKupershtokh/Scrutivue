# Actual-corpus partial decoding: DNG, JPEG and NEF

## Scope and interpretation

Follow-up to [the initial capability experiment](preview-decoding-experiments.md).
The corrected requirement excludes embedded JPEG previews from the main viewport:
**embedded previews are thumbnail-only**. Every partial result here must come
from the original image's compressed pixel/raw stream.

The supplied corpus contains 66 files:

- `halo`: **30 DNGs and 30 JPEGs**, six scenes `AK8_0005`, `AK8_0036`,
  `AK8_0149`, `AK8_0988`, `AK8_1588`, `AK8_1714`, each with halo suffixes
  `0`, `24`, `48`, `96`, `256`.
- The source-NEF directory: the corresponding **six original NEFs**.
- All 30 actual JPEGs have 8256x5504 pixels, baseline SOF0, 8-bit 4:4:4 sampling;
  compressed sizes range from 25,649,815 to 34,346,535 bytes. Earlier synthetic
  progressive/chroma fixtures are separate support-boundary evidence, not actual
  images substituted into this corpus.

All programs are isolated optimized developer harnesses. Production code,
LibRaw's exact Redlamp pin, source photographs, root binaries and manual-only
Windows workflow remain unchanged. No commit/push or UI integration is implied.
The application is not rebuilt for decoder-capability experiments.

## What counts as partial decoding

| Operation | What it does and does not establish |
| --- | --- |
| libjpeg-turbo scaled IDCT | Actual smaller reconstruction/output, with image-wide entropy work still required. |
| libjpeg-turbo scanline crop/skip | Partial IDCT/output; preceding rows still need entropy traversal. Not constant-time random access. |
| DNG tile selection | Avoids entropy decoding unrelated independently compressed JXL tiles. Returned samples are raw-domain, not LibRaw-developed RGB. |
| JXL progressive flush | Approximate encoding-dependent detail in a full-size output buffer, not a requested smaller allocation. |
| LibRaw `half_size` | Smaller Bayer-to-RGB development, but full Nikon raw entropy unpack remains. |
| LibRaw `cropbox` | Partial development after full raw unpack. It does not supply compressed-stream ROI decoding. |
| Experimental Nikon stripe selection | Uses the pinned source's internal decoder routines on selected full-width 64-row stripes plus dependency context. This is genuine partial raw reconstruction, but not a public LibRaw option or developed display RGB. |
| Half-size NEF plus box reduction | Explicit additional resize after half-size processing. Not native quarter-resolution raw decoding. |

Exact region comparisons use full references at matching settings, scale and
coordinate domain. Same-backend and explicitly labelled cross-backend checks
are distinguished in the results. Approximate previews cannot provide authoritative
statistics/differences/color-picker values. A raw-region match is not by itself
proof of equivalence to final developed/oriented/cropped RGB.

## Measurement boundaries

Each measurement uses a fresh worker process, with experiments/builds scheduled
sequentially. Source integrity hashes warm the OS file cache; there is no claim
of physically cold storage. File access, allocations, decoding, processing and
output copies have separate timers where the API exposes those stages. Library
calls that combine several operations remain explicitly combined.

Memory is absolute Windows process working set/private commit, not codec-only
heap. Mmap creation does not complete reading: demand faults are included in the
stages that actually address the bytes. Reference validation, full source hashing
and output exports are outside decode-ready timers. JPEG workers preload the
compressed input; DNG/NEF probes can address read-only mapped source regions.
No display ICC conversion, GPU upload or UI presentation is timed.

## JPEG: all 30 actual files completed

Three repeats per case, sequential fresh processes. The table summarizes
per-file medians across 30 files; time excludes compressed reads, cleanup,
memory-query instrumentation and disk dumps. Peak WS is process-wide MiB.

| Output | Scalar C++ libjpeg-turbo 2.1.5.1 time / WS | SIMD-enabled libjpeg-turbo 3.1.4.1 via Pillow time / WS |
| --- | ---: | ---: |
| Full | 581.13 ms / 160.59 MiB | 328.97 ms / 225.82 MiB |
| 1/2 | 396.31 ms / 62.94 MiB | 284.25 ms / 95.62 MiB |
| 1/4 | 310.68 ms / 38.53 MiB | 260.06 ms / 63.08 MiB |
| 1/8 | 217.07 ms / 32.42 MiB | 235.32 ms / 54.97 MiB |

The two versions/wrappers are not a controlled SIMD comparison. Both run native
JPEG decoding; Python is orchestration, not an image codec. These genuine scaled
IDCT paths are not full RGB decode followed by resizing. The current Rust API
does not expose equivalent reduced/ROI controls.

The current Rust `image` 0.25.10 / zune-jpeg 0.5.15 path was separately run on
all 30 actual JPEGs, n=3: full samples ready **406.30 ms** median [374.60–452.34],
**188.84 MiB** median peak WS. Header plus owned compressed-input copy was about
10.75 ms and full decoding 395.39 ms; the output-allocation call was only 0.007 ms
because physical first-touch work occurs later inside decoding. This path has
no measured scaled/ROI mode. These are isolated decoder scopes, not viewer-frame
construction or an asserted cross-codec pixel match. All 90 runs were repeatable
and source hashes unchanged (`target/preview-jpeg-current-20261101/`).

Scalar C++ full-scale **central 512x512 ROI** was **116.16 ms / 31.31 MiB**;
1024x1024 was **135.09 ms / 33.61 MiB**. Across six halo0 scene representatives,
top/bottom 1024 ROI medians were **50.67 / 217.09 ms**: JPEG's preceding-row
entropy cost remains, even though most IDCT/output work is avoided. Reduced
decoding and ROI also worked together at all tested scales.

All **528** same-backend/same-scale ROI comparisons were byte-exact, padded and
unpadded. These actual files are 4:4:4; earlier 4:2:0 boundary differences remain
a separate relevant format limitation. The matrix completed **1,776 workers**,
with all 30 before/after hashes unchanged. Requests becoming the complete scaled
image after clamping/padding were explicitly omitted, not labelled partial.

There is no decoder continuation from quarter-sized JPEG pixels to exact full
pixels: normal scaled-then-full restarts. Adding the measured SIMD quarter and
full medians gives roughly **589 ms**, versus **329 ms** for full only. This sum
is an inference, not measured viewport latency, and shows why lower memory/early
display can come at extra total decode cost.

## DNG/JXL: all 30 actual files completed

All canvases are 8280x5520 RGB16, **726 independently compressed 256x256 tiles**.
Six outer workers, one serial native decoder per tile. Three repeats per ROI;
the table summarizes per-file medians across all 30:

| Raw region | libjxl | Current jxl-oxide | oxide with sub-tile crop |
| --- | ---: | ---: | ---: |
| Center 512x512 | **21.22 ms** | 28.92 ms | 27.58 ms |
| Center 1024x1024 | **50.20 ms** | 70.14 ms | 67.79 ms |
| 257x257 crossing tile boundaries | 11.23 ms | 14.95 ms | 14.40 ms |
| Top-left 128x128 | 9.11 ms | 12.33 ms | 11.70 ms |
| Bottom-right 80x60 | 7.57 ms | 10.58 ms | 9.55 ms |

Center 512 needs **9 of 726 tiles**, **19.35 MiB** median native peak WS at
readiness, versus a whole raw transport around **1.33 s native / 1.92 s oxide**
on the six n=3 representatives. Full transport here is **not** final viewer RGB;
later LibRaw development is excluded from both sides. ROI timer includes mapping
and demand faults but excludes fresh DLL/pool setup (roughly another 5 ms native).

The complete matrix had **1,590 sequential processes**, including **1,350 exact
ROI decodes** on all 30 DNGs and full native references for every input. Full
oxide/native hash equality was additionally checked on all six scene
representatives. DNG/JPG before/after hashes and metadata were unchanged.

The **90 first/center/last actual JXL tiles** produced **no progressive events**;
every final tile matched an independent full decode. Thus no useful native
half/quarter stage was observed in this corpus sampling. Existing synthetic
progressive JXL controls do not change that result. For these files the verified
fast path is **spatial tile selection**, not an embedded JPEG and not a universal
reduced-size JXL decoding switch.

**Downsampling review:** tracing `djxl -s` confirmed genuine pass skipping for
suitable encodings. It uses `kLastPasses`, one selected flush and early stop;
there is no separate hidden scale setting. All 90 sampled real tiles were then
independently identified as **Modular, one group, one pass, no downsample targets**
and retested at ratios 1/2/4/8: **360 full exact outputs, zero partial stages**.
The tested native libjxl disables this progressive path for Modular. Synthetic
progressive VarDCT did benefit (quarter **155 ms**, full **202 ms**), correcting
the earlier diagnostic's multiple-flush timing. See the
[version-pinned trace and repeat results](jxl-downsampling-review.md). This is
encoding/implementation-specific, not an absence of downsampling in JPEG XL.

All returned regions remain stored-orientation raw samples: WB, color/opcodes,
DefaultCrop, orientation and equivalent partial development are separate work.
It would be incorrect to advertise 21 ms as end-to-end display-ready latency.

## NEF: all six originals completed

All six are Nikon Z8 HE (`NEFCompression=13`), raw/active 8280x5520, zero visible
margins. There is no HE* file in this corpus. Full/half processing uses the exact
production LibRaw pin, RGB16, camera WB, bilinear development and no automatic
brightness; orientation is disabled only to make comparisons unambiguous.

Three repeats per main case; medians across six per-file medians:

| Path | Ready time | Peak WS | What was actually decoded |
| --- | ---: | ---: | --- |
| Full RGB | 2096 ms | 720 MiB | Full raw and full development. |
| Half-size RGB | 1251 ms | 505 MiB | Full raw, reduced development. |
| Quarter RGB | 1256 ms | 505 MiB | Full raw, half development, then explicit box resize. |
| 512x512 RGB ROI with halo | 977 ms | 505 MiB | Full raw, partial development. |
| Full CFA reference | 985 ms | 505 MiB | Full raw, plus caller copy. |
| **512x512 experimental stripe ROI** | **148 ms** | **69 MiB** | **14 of 87 raw stripes**, including context/priming; not developed RGB. |

For the representative `AK8_0005.NEF`, full/half/ROI unpack remains approximately
**0.96 s**. Subsequent processing is about **1022 / 260 / 7.5 ms**, respectively.
The stripe experiment instead spends about **1.15 ms indexing**, **6.33 ms major
allocation/LUT**, **123.65 ms entropy/IDWT**, **13.00 ms Bayer reconstruction**.
It really avoids unrelated raw reconstruction, using the pinned internal source
routines without changing the production decoder. It is **not a public LibRaw
ROI API** and still reconstructs the original full horizontal stripe width.

The source-level candidate passed **144 exact raw-region comparisons** across
all six files, including one-pixel/odd-size physical edges and 64-row seams;
42 malformed/bounds checks also passed. Source hashes remained unchanged. This
is about **6.7x faster** than the full-CFA reference interval, not a claim that
148 ms already supplies the final color-managed viewer region.

For LibRaw-developed RGB, an unpadded crop differs at demosaic boundaries (up to
8058/65535 in these tests). All **54 padded RGB comparisons** were exact across
default/fixed-maximum full-scale and half-scale settings. Context handling must
be validated again for different demosaicers or processing options.

Half-then-full also works with retained raw samples: half pixels arrive around
**1.25 s**, full around **2.51 s**, with no second unpack. All 18 final outputs
matched fresh full references. It improves first whole-image availability but
adds development work versus the **2.10 s** full-only path, and retains a complete
raw buffer. It is not continued partial entropy decoding.

## NEF alternatives: Imprint C++ and supplied IPX/intoPIX

Both decoded all six actual files. **54 fresh cases** (n=3) passed; complete raw
CFA and cropped CFA were exact against the production references in all **108**
checks, with repeat/cross-backend CFA parity and unchanged source hashes.

- **Imprint C++/CPU:** complete raw open/unpack about **1.24 s**. Half size is
  only later development. Its full/half camera-linear RGB postprocess was about
  **4.09 s / 219 ms**; summed native call intervals were **5.33 s / 1.46 s**.
  Do not compare full RGB time as identical work to the parent's sRGB/bilinear
  path: the narrow Imprint API cannot select its demosaic quality. No raw-scale,
  slice or ROI input is exposed.
- **Supplied IPX:** the complete, exact **8280x5520 CFA native decode was
  121.27 ms**, destination setup 0.068 ms, host packing 10.09 ms. Helper input
  read plus setup/decode/pack summed to **142.01 ms**, excluding runtime
  validation/loading and output export. **Fresh helper wall was 415.87 ms**;
  the validation-heavy Python worker was 841.33 ms. These are explicitly
  different intervals, not a promise of 121 ms viewer readiness.

IPX's optimized **full** native CFA decode is an important competitor to the
148 ms partial-source prototype, but the current CLI/setup scopes differ.
A persistent/in-memory IPX pipeline plus matched RGB/ROI development needs a
separate end-to-end test. The supplied helper exposes only original-size CFA;
its width/height fields are not established reduced/ROI controls. No official
SDK scale/slice/ROI interface was found in the supplied adapter/header surface.
That bounded finding does not establish the capabilities of every intoPIX SDK.
Runtime distribution rights remain separate; no proprietary DLL is shipped.

## Programs and detailed evidence

- Actual JPEG reduced/ROI matrix:
  [runner and scope](../experiments/preview/jpeg-corpus-notes.md).
- Actual DNG tile/ROI/progression matrix:
  [runner and scope](../experiments/preview/dng-corpus-notes.md).
- Exact pinned LibRaw full/half/crop/reuse matrix:
  [LibRaw source and timing boundaries](../experiments/preview/nef-libraw-notes.md).
- Genuine partial Nikon raw reconstruction:
  [stripe experiment](../experiments/preview/nef-stripes-notes.md).
- Imprint C++ and supplied IPX alternatives:
  [alternative NEF decoders](../experiments/preview/nef-alternatives-notes.md).

The individual result manifests distinguish completed measurements from
preparation, failures and unsupported APIs. The initial two-DNG/synthetic results
are retained in their earlier report and are not described as complete-corpus
measurements.

## Reproduce without changing the application

Use existing prepared dependencies and trusted native bundles. Commands are
**sequential**; choose new output names rather than overwrite evidence:

```powershell
python scripts/build_dng_experiments.py
python scripts/build_dng_experiments.py --preview
$env:DNG_JXL_LIBRARY = 'D:/Tools/libjxl-0.12.0/jxl.dll'
python scripts/build_dng_experiments.py --preview --tests

python experiments/preview/jpeg_corpus.py --build --run-id NEW_JPEG_RUN
python experiments/preview/jpeg_current_corpus.py --directory 'D:/Photos/halo' --repeats 3 --output target/NEW_RUST_JPEG_RUN
python experiments/preview/dng_corpus.py --run --corpus 'D:/Photos/halo' --library $env:DNG_JXL_LIBRARY --output target/preview-dng-corpus/NEW_DNG_RUN

python experiments/preview/nef_corpus.py --build
python experiments/preview/nef_corpus.py --directory 'D:/Photos/OriginalNEFs' --repeats 3 --output target/NEW_NEF_RUN
python experiments/preview/nef_boundary_checks.py --directory 'D:/Photos/OriginalNEFs' --references target/NEW_NEF_RUN --output target/NEW_NEF_BOUNDARY_RUN
python experiments/preview/nef_alternatives.py --action probe --backend both --runs 3 --libraw-references target/NEW_NEF_RUN --output target/NEW_NEF_ALTERNATIVES
```

The native JPEG and alternative-NEF runners default to the exact user-selected
local directories; their notes identify those targets and the supplied adapters.
The example `D:/Photos` paths are placeholders, not additional measured files.
The corpus manifest is the authority for actual source identities and coverage.

## Conclusions and validation

1. **JPEG:** reduced IDCT and padded scanline ROI are verified on every actual
   file, but neither removes all entropy work. Normal reduced-then-full restarts;
   use it for first-display/memory needs, not an assumed proportional speedup.
2. **These JXL DNGs:** prioritize visible independently compressed tiles. No
   half/quarter native progression was found in the 90 actual sampled tiles.
   Partial raw-to-display development remains separate implementation work.
3. **NEF:** the existing half/crop controls save development, not unpack. A
   genuinely partial source-level stripe route is verified, but the supplied
   optimized **full-CFA IPX decoder** is an important alternative. Measure a
   persistent/in-memory IPX + equivalent RGB/ROI development pipeline before
   choosing between it and a selective-source implementation.
4. **No embedded main-view preview:** embedded JPEGs stay thumbnail-only. Exact
   full-frame statistics still require the corresponding complete exact pixels;
   approximate or raw-domain display substitutes are not accepted.

The combined isolated Rust suite passed **30 tests** with the actual native DLL
enabled; all new Python runners passed syntax checks, and both standalone native
probes built in optimized configurations. Actual-image checks passed **528 JPEG
ROI comparisons**, **1,350 DNG ROI decodes**, **144 NEF stripe-region comparisons**,
**54 padded NEF RGB comparisons**, **18 retained-raw final-RGB comparisons**,
**42 NEF malformed/bounds checks**, and the alternative-CFA checks above.
All source integrity checks passed. Initial failed preparation attempts are kept
separately and are not silently included as successful corpus measurements.

Only isolated harnesses were rebuilt. The unchanged viewer was not release-built
or replaced, no root application binary or production dependency pin was touched,
and there was no commit/push or workflow change. All decoder workloads finished;
generated native binaries, raw references and private-path evidence remain under
ignored `target/` directories.
