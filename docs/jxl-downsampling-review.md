# What djxl --downsampling actually does

## Correction and scope

JPEG XL/libjxl **can avoid later decoding passes** when a suitable progressive
codestream is available. It was too broad to imply that no usable downsampling
mechanism exists. The earlier all-pass/all-flush probe was also not a fair timing
proxy for `djxl -s`: it reconstructed every earlier preview unnecessarily.

The user-linked Debian manual describes libjxl-tools 0.11.2. The selection logic
was inspected in both v0.11.2 and the tested native runtime's v0.12.0 sources.
The follow-up reproduces that logic using public C APIs; **it does not invoke a
djxl executable**. It keeps the isolated probe's single-frame RGB16/original
color-profile semantics, not every CLI output-format/color/animation option.

## Version-pinned implementation evidence

1. [`djxl_main.cc`](https://github.com/libjxl/libjxl/blob/v0.12.0/tools/djxl_main.cc#L392-L417)
   assigns `args.downsampling` to `JXLDecompressParams::max_downsampling`.
2. [`lib/extras/dec/jxl.cc`](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/extras/dec/jxl.cc#L186-L195)
   subscribes to `JXL_DEC_FRAME_PROGRESSION` and selects **kLastPasses**, not
   kPasses, when a ratio greater than one is requested without a pass limit.
3. Its [event handler](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/extras/dec/jxl.cc#L484-L500)
   queries `JxlDecoderGetIntendedDownsamplingRatio`. Once `ratio <= requested`,
   it calls **one** `JxlDecoderFlushImage` and stops for the final still frame.
   Earlier stages are not flushed. If no suitable event occurs, full decoding
   completes. [v0.11.2 uses the same policy](https://github.com/libjxl/libjxl/blob/v0.11.2/lib/extras/dec/jxl.cc#L440-L456).
4. This is real avoided work, not just early display after full reconstruction:
   [`dec_frame.cc`](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/jxl/dec_frame.cc#L682-L689)
   limits how many AC passes are decoded before the pause. `kLastPasses` obtains
   the pause boundaries from `num_downsample`/`last_pass` in the frame header.
5. The tested libjxl [explicitly disables this progression for Modular](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/jxl/dec_frame.h#L144-L179).
   It also excludes single-group/single-pass sections and extra-channel cases.
   This is an implementation boundary, not a claim that the JXL specification
   can never represent Modular progression.
6. It does **not** request smaller output dimensions. The
   [public header](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/include/jxl/decode.h#L305-L318)
   describes upscaled detail in a full-resolution buffer, and extras allocates
   its frame/output at the original dimensions. A smaller display image still
   requires separate reduction. Full-size allocation does not negate avoided
   entropy passes, but prevents assuming JPEG-like scaled-output memory savings.

## Actual DNG encodings: 360 repeat checks

The retained first/spatial-center/last tiles from every actual DNG were reused,
with identities checked against the earlier extraction manifest. All **90 tiles
from 30 inputs** have this independently inspected frame header:

```text
encoding: Modular
groups: 1
passes: 1
downsample_targets: []
last_pass: []
```

Each was tested with requested ratios **1, 2, 4 and 8**: **360 fresh processes**.
There were **zero progressive events**, **zero partial outputs**; all 360 outputs
were identical to independent full native decodes and consumed the complete tile.
Thus using djxl's selection loop did not unlock reduced decoding for these
sampled encodings. A user option alone cannot create absent passes or change
libjxl's Modular progression support. This is sampled-tile coverage, not inspection
of every one of the 21,780 tiles across the 30 DNGs.

## Positive controls and corrected timing

The existing 4096x3072 synthetic VarDCT controls were tested independently, three
fresh processes per ratio/control, with one native decoder thread. Input mapping
and DLL validation/loading are outside the native-ready interval. Native setup,
full-size output allocation, the required decoding and one selected flush are
inside it. Full-reference decoding, hashing and independent frame-header
inspection are after readiness. Hashing warms the OS cache; these are not cold
storage or UI measurements.

| Requested detail | Progressive AC control median ready | Bytes consumed at stop |
| --- | ---: | ---: |
| Full | 201.58 ms | 869,438 |
| 1/2 | 176.62 ms | 590,697 |
| 1/4 | 154.88 ms | 416,429 |
| 1/8 | 146.86 ms | 210,962 |

Every partial case flushed **once** and skipped later passes. All returned
4096x3072 RGB16: **75,497,472 bytes**, not reduced-size buffers. These controls
demonstrate actual JXL pass skipping, not performance on the user's DNGs.

With spectral AC progression disabled, only the 1/8 DC step was usable; 1/2 and
1/4 requests completed the full image. So even VarDCT does not guarantee every
ratio. The previous quarter/half figures of about 258/363 ms included earlier
flushes and must **not** be used to reject an optimized one-flush preview path.

## Reproduce and artifacts

```powershell
python scripts/build_dng_experiments.py --preview
$env:DNG_JXL_LIBRARY = 'D:/Tools/libjxl-0.12.0/jxl.dll'
python scripts/build_dng_experiments.py --preview --tests
target/preview-experiments/preview_capabilities.exe --mode djxl-downsampling --library $env:DNG_JXL_LIBRARY --downsampling 4 INPUT.jxl
python experiments/preview/jxl_downsampling_review.py --library $env:DNG_JXL_LIBRARY --output target/NEW_DOWNSAMPLING_REVIEW
```

The runner defaults to the earlier actual-corpus extraction and synthetic control
directories. It rejects an existing output directory. Evidence for this run is
`target/jxl-downsampling-review-20261101/results/{measurements.jsonl,summary.json}`:
**384 cases**, all retained input hashes unchanged. Source snapshots, URLs and
SHA-256 hashes are in its neighboring `sources/manifest.json`. The native library
reports 12000 and retains SHA-256
`b0a7f7721836359b29841cca0f15969e85bd386344f9ee0775292089bd3bcfda`.
The isolated combined Rust suite passed **30 tests**, including full fallback,
exact one-flush selection and bounds checks. Only the experimental harness was
built; viewer code, photographs, pins and root binaries were not changed.
