# Pinned LibRaw NEF partial-output experiment

This probe links the application's exact existing optimized Redlamp LibRaw
archive, commit `f4956c404fba0c6035ce9e06353d4b4ffe068fc7`. It does not alter that
archive, vendor sources, the production wrapper, dependency pins or root binaries.
All inputs are the six actual NEFs from the user-selected source directory.
No thumbnail API is called.

## Source-established distinctions

- `half_size` changes `raw2image_start` and later Bayer/RGB development, **not**
  `nikon_he_load_raw`'s full raw decode. The latter reads the complete compressed
  strip, allocates a full Bayer image, calls `decode_nikon_he_image` and copies
  the complete output into LibRaw's raw owner. A smaller RGB image alone does
  not establish reduced entropy decoding.
- LibRaw exposes no quarter-resolution processing setting in this path. The
  explicit quarter experiment uses **half-size development followed by 2x2 box
  reduction**. It must not be described as native quarter-resolution decoding.
- `params.cropbox` is applied in `raw2image_ex`, after `unpack`, before allocating
  the developed image. It can save demosaic/color/output work, but does not skip
  Nikon entropy reconstruction. Its coordinates are in the unrotated active
  image, not the final oriented viewport and not necessarily sensor coordinates.
- `dcraw_process` calls `raw2image_start`, which restores the retained raw state.
  A half-then-full experiment therefore reuses one `LibRaw` and one complete raw
  unpack while rerunning development. This is different from continuing a partly
  decoded entropy stream.
- Bilinear crop boundaries need context. The probe compares both an unpadded
  region and a 16-sensor-pixel development halo, trimmed only after partial
  development, against same-settings full output.
- Default `adjust_maximum` can depend on the maximum found over the developed
  region. A separate **fixed-maximum** experiment sets `adjust_maximum_thr=0` for
  both full and cropped references. It is not silently substituted for the
  application's default rendering behavior.

The independent source-level stripe experiment uses the same pinned internal
Nikon routines but bypasses the full-image orchestration; its capability and
limitations are reported separately in `nef-stripes-notes.md`. It is not a
public LibRaw reduced-resolution/ROI option.

## Commands

```powershell
python experiments/preview/nef_corpus.py --build
python experiments/preview/nef_corpus.py --directory 'D:/Photos/NEFs' --repeats 3 --output target/preview-nef-corpus-new

# Direct modes; output files use CREATE_NEW and are never replaced:
target/preview-nef-build/nef_libraw.exe full 'D:/Photos/AK8_0005.NEF'
target/preview-nef-build/nef_libraw.exe half 'D:/Photos/AK8_0005.NEF'
target/preview-nef-build/nef_libraw.exe roi 'D:/Photos/AK8_0005.NEF' --roi 3800 2400 512 512 --halo 16
target/preview-nef-build/nef_libraw.exe stripe-roi 'D:/Photos/AK8_0005.NEF' --roi 3800 2400 512 512
```

## Measurement boundaries

One fresh process per observation. Read-only mmap creation, LibRaw construction,
metadata parsing, full `unpack`, processing, bitmap creation, caller allocation,
ROI copying and decoder release have distinct timers where their APIs permit.
`unpack` still combines internal allocation, compressed-strip reading, entropy,
IDWT/Bayer reconstruction and internal copying. There is no invented split of
that interval. The source-level stripe helper provides its own narrower stages.
Demand page faults are inside the stages that use the mapped data, not counted
as completed disk reading by the small mapping-creation time.

Source SHA-256/size/mtime are checked before and after; these checks warm the OS
cache. This is not physically cold-drive I/O. Output comparisons and reference
exports are outside ready times. Memory is absolute Windows process working set
and private commit, including LibRaw internals and mapped pages, not just returned
pixels. Every developed reference is unrotated RGB16/sRGB, camera white balance,
bilinear quality, no automatic brightness. Output orientation is forced to zero
only to make crop coordinates and exact comparisons unambiguous.

## Source locations

- `vendor/libraw/src/decoders/nikon_he_decoder.cpp`: complete-strip/full-Bayer
  integration and transfer curve.
- `vendor/libraw/src/decoders/nikon_he/nikon_he_decode.cpp`: full-image stripe
  orchestration, precinct indexing and cross-stripe dependencies.
- `vendor/libraw/src/preprocessing/raw2image.cpp`: retained-raw restoration,
  half-size geometry and development crop allocation.
- `vendor/libraw/src/postprocessing/dcraw_process.cpp`: development ordering.
- `vendor/libraw/src/utils/utils_libraw.cpp`: local `adjust_maximum` rule.

The runner saves the observed corpus, correctness, timing and memory evidence
under its caller-selected ignored `target/` directory. Runtime findings must be
read from those results; source-established capabilities alone are not measured
performance or pixel equivalence.

## Completed actual-six-NEF observations

Evidence: `target/preview-nef-corpus-20261101-v2/`, **252 records**. All six sources
are Nikon Z8, `NEFCompression=13` (HE), raw/active 8280x5520 with zero margins.
All before/after source hashes/size/mtime are unchanged. The six files do **not**
establish HE* corpus coverage. Each main timing case has three fresh processes;
the table gives the median of per-file medians and their min–max.

| Output path | Ready median ms [per-file range] | Median peak WS MiB |
| --- | ---: | ---: |
| Full developed RGB16 | 2096.16 [2086.12–2148.70] | 720.37 |
| Half-size developed RGB16 | 1250.96 [1244.30–1258.11] | 504.84 |
| Quarter, half-size plus explicit box reduction | 1255.54 [1246.27–1266.40] | 504.84 |
| 512x512 developed `cropbox`, 16-pixel halo | 977.47 [968.27–987.44] | 505.08 |
| Half-size region, 16-pixel halo | 966.25 [959.02–977.32] | 504.78 |
| Half first, then full using retained raw | 2510.82 [2493.03–2516.57] | 720.59 |
| Full raw CFA reference, including caller copy | 985.49 [977.06–1007.15] | 505.06 |

Full/half/quarter output sizes are 274,233,600 / 68,558,400 / 17,139,600 bytes.
Quarter is **not** a native reduced raw entropy decode. Half/ROI retain nearly
the same unpack-time peak because Nikon raw reconstruction is still complete.

Representative `AK8_0005.NEF` stage medians:

- Full: **965.85 ms unpack**, **1022.00 ms processing**, **105.82 ms bitmap**.
- Half: **956.76 ms unpack**, **259.97 ms processing**, **26.65 ms bitmap**.
- Cropped region with halo: **958.64 ms unpack**, **7.48 ms processing**,
  **1.94 ms bitmap**. Main output is only a 512x512 region, not a cropped full
  developed bitmap, but full raw unpack remains.
- Half-then-full: first half pixels at **1243.87 ms**, additional full development
  **1256.01 ms**, with **one** full unpack. All 18 continued final outputs exactly
  match independent full RGB references. The stage gap includes small temporary
  release work; ready times are not asserted to be exact sums of median stages.

Unpadded regions differed from full output at boundaries: across the six cases,
up to **6113 differing channel values**, maximum **8058/65535**. With a 16-pixel
development halo, all 18 default-policy full-scale regions, 18 fixed-maximum
regions and 18 half-size regions were exact. This validates these files, bilinear
settings and coordinates, not a universal halo for other demosaicers or cameras.

The initial attempt stopped on an overstrict precinct-count check in the new
source-level stripe helper, not on a production LibRaw decode failure. That
assumption was corrected to match production's actual short-tail handling; the
new complete run above is separate from the preserved partial attempt.
