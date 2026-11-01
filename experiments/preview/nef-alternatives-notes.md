# Actual-NEF alternatives: Imprint C++ and the existing intoPIX helper

## Status and scope

Prepared on 2026-11-01 without native workloads, then executed by the parent on
the actual six NEFs. The complete sequential three-repeat result is
`target/preview-nef-alternatives-20261101/report.json`: **54 successful cases,
zero failures**, all source hashes unchanged, all CFA comparisons exact. Earlier
Z9 CC0 controls do not substitute for these actual six-file observations.

The runner is `experiments/preview/nef_alternatives.py`. Its defaults select
exactly these files under
`E:/Documents/AI/corpus/3 этаж single/3 этаж single/`:

- `AK8_0005.NEF`
- `AK8_0036.NEF`
- `AK8_0149.NEF`
- `AK8_0988.NEF`
- `AK8_1588.NEF`
- `AK8_1714.NEF`

It never calls an embedded-preview or thumbnail extraction function. All
outputs go into a **new** directory under this repository's ignored `target/`.
No production source, dependency pin, application binary, neighboring source,
or existing probe is edited. No tools or libraries are installed or downloaded.

## Verified source/API inventory

### Imprint commit `3916a114bc32b487de611135cf66514447c541a6`

The actual prepared source location is
`target/dng-jxl-imprint/source/native-raw/`, **not**
`target/dng-jxl-imprint/native-raw/`.

Evidence in that source tree:

| Surface | Source evidence | Consequence |
| --- | --- | --- |
| `imprint_raw_probe_nikon_he` | `include/imprint_raw.h:89`; `src/raw_bridge.cpp:293-321` | Metadata-only `LibRaw::open_file`; accepts Nikon selector 13/14 **and** `nikon_he_load_raw()`, not TIFF compression 34713 alone. |
| `imprint_raw_open` | `include/imprint_raw.h:84-100`; `src/raw_bridge.cpp:236-291` | Constructs LibRaw, calls `open_file`, validates decoder, performs **complete `unpack()`**, fills metadata. One combined timing is honest; the ABI does not split I/O/allocation/entropy/IDWT/reconstruction. |
| Visible CFA copy | `src/raw_bridge.cpp:328-360` | Copies active `width × height` uint16 samples using native pitch and top/left margins; no rotation, WB, black subtraction, or RGB conversion. |
| RGB | `src/raw_bridge.cpp:363-447` | `dcraw_process()` + `dcraw_make_mem_image()` in one API call; three-channel 8/16-bit output. |
| Half size | `src/raw_bridge.cpp:402-415` | Sets LibRaw `half_size` **after full unpack has already finished**. This reduces RGB development, not RAW entropy decode. |
| Crop fields | `include/imprint_raw.h:23-39`; `src/raw_bridge.cpp:114-133` | `crop_left_margin`, `crop_top_margin`, `crop_width`, `crop_height` are output metadata describing a source inset crop, **not inputs requesting decode ROI**. |

Complete `imprint_raw_postprocess_params` fields, from
`include/imprint_raw.h:71-82`:

```text
half_size, use_camera_wb, no_auto_bright, output_bps, output_color,
gamma[2], has_user_wb, user_wb[4], has_user_flip, user_flip
```

There is **no** input `cropbox`, ROI, slice range, entropy scale, target size,
demosaic quality (`user_qual`), or maximum-adjustment threshold in this narrow
C ABI. The probe therefore does not invent or write any of those fields.

The prepared runner uses the existing ctypes declarations and native struct
size checks in `experiments/dng-jxl/imprint_probe.py`, without modifying that
file. Expected native sizes are `sizes=64`, `info=824`, `params=72` bytes.
The orchestrator is Python; parsing, entropy, lifting, reconstruction and RGB
development remain the existing compiled C++ implementation.

Recorded Release DLL fingerprint, rechecked before use:

```text
target/dng-jxl-imprint/build/Release/imprint_raw.dll
5523606c669ad499ab1c8ee7571a638d4651d1d42e2fa489ccbc874ba6e8eb06
```

The runner verifies every file against the existing `sources.json` and records
the original `build.json` provenance. Imprint's isolated build uses LibRaw
`4abfcd2ab82f2211b1f7e7c3cd83974c87e0d812`, archive SHA-256
`2ca1d9673531fccabcc7c8bce589444163868bcde625aa1f09eb442df1bcce2d`.
This is **not** Scrutivue's pinned production LibRaw commit
`f4956c404fba0c6035ce9e06353d4b4ffe068fc7`. The probe neither changes nor
substitutes the production pin.

### Is truly reduced decoding present in Imprint source?

Not as an existing callable option:

- `src/ticoraw_decode.hpp:22-31` takes a complete precinct stream, original
  image width/height, transfer LUT and full Bayer output pointer; no scale/ROI.
- `src/ticoraw_decode.cpp:125-160` indexes the image-derived full precinct
  count, validates ordered slice markers, and requires terminal EOC.
- `src/ticoraw_decode.cpp:635-661` allocates full-image tile coefficients and
  decodes every tile, not a requested subset.
- `src/ticoraw_decode.cpp:663-689` reconstructs all four bands to the full
  Bayer plane. The internal `rows=image_height/2` and `cols=image_width/2`
  describe the transform/Bayer organization; they are **not a half-resolution
  user output or a scale control**.
- `src/ticoraw_decode.cpp:443-480` has per-tile prediction state, vertical
  carry, overlapping precinct work, and previous-tile overflow. Existing
  slices are not evidence that arbitrary stream truncation is a valid ROI.

A future source-level partial Imprint fork would need bounded indexing,
dependency/halo handling, selective coefficient storage, ROI reconstruction,
and comparisons to each same-backend full CFA, including slice seams and
edges. It is not equivalent to changing `image_height`, requesting a smaller
destination, dropping high bands, or stopping early. None is implemented or
claimed in this alternative probe. The parent session's separate pinned-LibRaw
source-level stripe experiment owns the actual partial candidate.

### Existing pipeline-graph intoPIX bridge

Inspected only the supplied source/adapter and its existing artifact paths under
`E:/Documents/AI/REA_DXO/pipeline-graph`. No disassembly, decompilation, private
ABI discovery, codec-key handling, extension masking, or security bypass.

`native/dxo_ipx_helper.cpp:159-186` declares the already-verified call:

```cpp
bool DecodeIpxData(const std::vector<unsigned char>& payload, void* destination);
```

The actual native export has the `DXOImage&` C++ type; the supplied helper alone
owns its pinned object/calling contract. The new Python probe invokes only
that **existing helper executable's CLI**, not the export or a recreated
foreign C++ object.

- `native/dxo_ipx_helper.cpp:216-259`: CLI options are `--framework`,
  `--payload`, `--output`, `--report`, `--width`, `--height`, `--repeats`.
- `native/dxo_ipx_helper.cpp:278-315`: width/height allocate an original-size
  single-plane 16-bit CFA destination; geometry is checked before and after
  decode. They are **not documented scale/ROI knobs**. This runner never tries
  half dimensions, modified payloads, or shortened streams.
- `native/dxo_ipx_helper.cpp:325-342`: separate destination setup, combined
  native decode, host packing, payload read, DLL load and output write timings.
  The native decode timing includes context/probe/validate/decode/copy into
  DXOImage; it does **not** expose separate entropy and reconstruction times.
- `crates/raw/src/ipx.rs:267-279`: persistent protocol requests contain only
  request ID, original width/height and three path lengths/paths, with no
  scale/slice/ROI field.
- `crates/raw/src/tiff.rs:317-380`: bounded classic TIFF, exactly one nested
  CFA IFD and one complete Nikon IPX strip.
- `crates/raw/src/ipx_header.rs:29-72`: fail-closed full-frame Z8 14-bit
  CAP/PIH/CDT/WGT profile, including original codestream length.

No TicoRAW/TicoPIX/intoPIX SDK headers were found under `resources/`; the
source references lead to this adapter, not an official SDK decode-parameter
surface. Therefore **no SDK scale/slice/ROI capability is established by
these supplied files**. This is not a claim that every official intoPIX SDK
or the format itself lacks reduced decoding.

The runner reproduces the bounded public TIFF route and exact unchanged
155-byte IPX header profile before invoking the helper. Unknown extensions
fail before submission. It is a CFA-only helper experiment, **not** a copy of
the complete Rust metadata/color adapter: it does not promise that adapter's
full MakerNote, WB, white-policy, inset-crop or camera-matrix validation.
It does not render IPX output or infer its color processing. The existing
verified Z8 profile has zero visible margins. Reference comparisons still
require identical source hashes and matching explicit sensor coordinates.

Required existing artifacts; no override of their certified identity:

```text
resources/native/ipx/dxo_ipx_helper.exe
fce4f2e3bd537e5e66995ad4491f8c1ded7e0307ef30a3de8041bb7ffdce44b0

resources/native/ipx/dxo-6.8.0.7/DxOFramework.dll
f3064b4bf321402e6cce16388e135bb6b15de824ed717a6074a53784c9c662c0

resources/native/ipx/dxo-6.8.0.7/manifest.json
59aa78cce27ee5a0b2ac36d767e75da4f5083e6431f2e422f1e060fa0dc4321b
```

All runtime manifest files are checked by length/SHA-256 and held read-only
while used. The helper independently checks the framework hash. Runtime
redistribution is not implied.

## Exact commands for the parent: run sequentially, not alongside benchmarks

From `E:/Documents/AI/Diff`, with the existing Python environment:

```powershell
# Lightweight: no native decode, no build, no actual image access.
python experiments/preview/nef_alternatives.py --action self-test

# Optional header-only inventory of the actual six NEFs and existing artifacts.
# This hashes native artifacts, but reads only bounded headers from each NEF.
python experiments/preview/nef_alternatives.py --action inventory --output target/preview-nef-alternatives-inventory

# After the parent finishes nef_corpus.py reference generation:
# 12 fresh Imprint decodes: full + half RGB for every actual NEF.
python experiments/preview/nef_alternatives.py --action probe --backend imprint --runs 1 --libraw-references target/preview-nef-corpus --output target/preview-nef-imprint

# Then 6 full-size intoPIX CFA decodes through the existing helper.
python experiments/preview/nef_alternatives.py --action probe --backend ipx --runs 1 --libraw-references target/preview-nef-corpus --output target/preview-nef-ipx

# Alternatively, one strictly sequential 18-case suite, also automatically
# comparing Imprint versus IPX full visible CFA for the same source/geometry:
python experiments/preview/nef_alternatives.py --action probe --backend both --runs 1 --libraw-references target/preview-nef-corpus --output target/preview-nef-alternatives-all
```

Choose the split pair **or** the combined suite; do not run both redundantly.
Every output directory must be new. Adjust `--libraw-references` if the parent
uses a differently named reference directory. It expects the parent's
`nef_corpus.py` schema: `manifest.json`, `measurements.json`, and
`AK8_XXXX-full-cfa.u16` full-sensor references. References must be complete
before starting alternatives; source hashes and reference dimensions/margins
are verified rather than matching only filenames.

`--variants cfa` skips Imprint RGB development for a smaller CFA-only check.
`--input PATH` can select one actual NEF for an initial sequential smoke test.
`--runs 3` repeats all requested cases. `--save-rgb` retains complete RGB16
buffers instead of just hashes and small host-cropped ROI artifacts. No native
build is needed: reuse the already built DLL/helper. If their fingerprints
differ, the runner stops that case; it does not rebuild or disable the checks.

## Correctness and timing interpretation

- Fresh process/handle for every Imprint full/half variant; changing processing
  state cannot contaminate the other variant. CPU reconstruction is explicitly
  required with `IMPRINT_TICORAW_BACKEND=cpu` and checked as `c++/cpu`.
- Each probe locks its actual source against concurrent write/delete, records
  SHA-256 + length + mtime before/after, and never opens it for writing.
  Unicode input paths receive independent `shutil.copy2` ASCII-path copies
  inside the new ignored case directory, with content hash verification.
- The parent added Windows process snapshots around Imprint full unpack, caller
  CFA allocation/copy and RGB readiness. Their cumulative peak WS includes the
  Python host and native library, not only decoder allocations. The external IPX
  helper has no such counter in its existing protocol; Python-worker RSS must
  not be reported as that helper's native decoder RSS.
- A successful Imprint full and half job both expose a **full** visible CFA.
  The suite compares same-backend CFA hashes across variants/repeats. The
  combined suite additionally compares CFA across the two alternatives.
- With `--libraw-references`, the full-sensor parent CFA is cropped by the
  reported visible margins; visible and host-ROI hashes/pixels are checked in
  original sensor coordinates. A mismatch is reported with sample count,
  maximum and mean absolute DN when the candidate artifact is retained.
  There is no normalization, shift fitting, camera substitution or rotation
  to conceal differences. Hash-only comparisons are identified as such.
- Imprint RGB settings are: camera/raw RGB (`output_color=0`), camera WB,
  no auto bright, linear gamma, 16-bit, explicit `user_flip=0`. Full and half
  are **not** asserted to be the same rendering or a resize of each other.
  The parent's current probe uses sRGB and explicit bilinear quality, whereas
  this Imprint ABI cannot select quality. **Do not compare those RGB buffers
  as if color/demosaic policies were identical.** RGB parity is recorded as
  unsupported, not failed pixel correctness.
- `--roi center512` (default) writes an even-origin 512×512 host crop of each
  unrotated output. `--roi x,y,w,h` is in **that variant's output coordinates**,
  not displayed/oriented camera coordinates. A half-RGB ROI is consequently
  not automatically the same sensor region as the same numeric full-RGB ROI.
  These crops are always **after full RAW decode**, and are not a partial
  decoder or an independent ROI/full RGB oracle. No native ROI/full RGB
  comparison is possible through either inspected narrow API.
- Each child writes `case.json`, pixel `.artifact.json` descriptors, native
  diagnostics and retained artifacts. The parent suite writes `report.json`.
  Cases fail on native errors, changed sources, reference errors or pixel
  mismatches. Unsupported camera/profile cases remain explicit errors; there
  is no automatic alternate decoder replay/fallback within a submitted case;
  requested variants/repeats are independent planned cases, not error recovery.
- Main Imprint fields: `open_including_full_unpack.ms`,
  `caller_cfa_zero_allocation_ms`, `copy_visible_ms`, `postprocess.ms`,
  `close_ms`. `native_calls_to_rgb_ms` is the sum of open/unpack and RGB API
  durations, **not** uninterrupted end-to-end ready latency: validation,
  CFA copying/hashing/export and ROI work run between those calls.
- IPX fields retain the helper's original timing scope. Host strip extraction
  is read+write combined; helper read, setup, combined decode, packing and
  output write remain separate. A new CLI helper process loads the runtime
  for each case, unlike the Rust adapter's persistent session; compare inner
  native times only when that distinction matters.
- Source hashes, native hashes, Unicode copies and repeated reads warm the OS
  cache. Nothing here is a physical cold-drive benchmark. SHA/validation,
  retained writes and subprocess startup inside the child are excluded from
  native stage times but included in suite `external_wall_ms`. The parent's
  subsequent reference/cross-backend comparison times are reported separately
  as `libraw_reference_comparison_ms` / `cross_backend_comparison_ms`.

Standalone compatible `.artifact.json` files can also be compared:

```powershell
python experiments/preview/nef_alternatives.py --action compare --actual target/preview-nef-alternatives-all/run-01/AK8_0005/ipx-cfa/visible-cfa.artifact.json --reference target/preview-nef-alternatives-all/run-01/AK8_0005/imprint-full/visible-cfa.artifact.json
```

Geometry, source hash, sample type, orientation and visible origin must match;
RGB additionally requires identical processing parameters/variant. Differing
retained pixel files use chunked NumPy comparison; no package installation is
performed. NumPy is also required for parent-reference comparisons, not for
native decode itself or the lightweight self-test.

## Estimated workload, not measured performance

Assuming the expected 8280×5520 profile, each CFA is 45,705,600 samples /
91,411,200 bytes (87.18 MiB); full RGB16 is 274,233,600 bytes (261.53 MiB), and
half RGB16 is 68,558,400 bytes (65.38 MiB), before any library-specific output
dimension changes. Actual returned geometry is always recorded/validated.

The default combined one-run suite has 18 sequential full-RAW decodes:
12 Imprint (six full RGB + six half RGB), six IPX CFA, and 12 separate Imprint
metadata-only probes. It retains six Imprint CFA + six IPX CFA files, about
1.02 GiB, plus independent ASCII NEF copies, IPX strips, small ROI artifacts
and logs. `--save-rgb` adds approximately 1.92 GiB for those 12 RGB outputs.
Three runs perform 54 full-RAW decodes; IPX keeps one CFA file per run, while
Imprint full CFA retention is restricted to run 1/first variant.

Parent-reference checking rereads each applicable full CFA region and small
ROI outside timed native stages. Peak memory includes the decoder's full
coefficient storage and pixel buffers, so it cannot be inferred just from
output bytes. No wall-time estimate is substituted for actual measurement.

## Completed three-repeat actual-NEF results

All six actual files selected the expected Z8 HE profile and decoded successfully
with both alternatives. The suite completed **54 cases**, including 36 Imprint
full/half variants and 18 full-size IPX CFA decodes. All **108** complete-CFA/host
ROI checks against production LibRaw references were exact. The 54 same-backend
CFA checks and 42 available cross-alternative CFA checks were also exact. Source
SHA-256/size/mtime remained unchanged. RGB is not asserted cross-backend equal.

Times below summarize six per-file three-repeat medians, with their min–max:

| Operation | Median ms [range] | Scope |
| --- | ---: | --- |
| Imprint open + complete CFA unpack, full-RGB cases | 1240.05 [1215.11–1251.89] | Construction/container I/O/unpack/metadata combined. |
| Imprint open + complete CFA unpack, half-RGB cases | 1234.63 [1215.72–1240.34] | Still full raw reconstruction. |
| Imprint full camera-linear RGB postprocess | 4091.54 [4050.56–4120.27] | Library-selected demosaic; unlike the parent's bilinear/sRGB policy. |
| Imprint half camera-linear RGB postprocess | 219.49 [217.67–224.53] | After full raw unpack. |
| Imprint summed native API time to full RGB | 5331.48 [5275.63–5360.97] | Open/unpack + postprocess; intermediate validation gaps excluded. |
| Imprint summed native API time to half RGB | 1457.07 [1433.39–1462.14] | Same combined-call caveat. |
| **IPX complete CFA native decode** | **121.27 [120.56–122.31]** | Native context/probe/validate/decode/copy to DXOImage. |
| IPX destination setup | 0.068 [0.064–0.071] | Separate native destination setup. |
| IPX full-CFA host packing | 10.09 [9.49–12.80] | Removes native row padding; separate from decode. |

IPX additional stages: bounded TIFF parse **0.241 ms**; original-strip extraction
read+write **8.06 ms**; helper payload read **10.41 ms**; runtime load **14.81 ms**;
full CFA file write **20.71 ms**. These do not all belong in one persistent-viewer
request; the experiment deliberately uses a new CLI helper and validation files.
The actual summed helper input-read/setup/decode/pack interval was **142.01 ms**
[140.43–144.40]. It still excludes runtime validation/loading and output write.

The measured **fresh helper process wall** was **415.87 ms** [412.34–417.09].
The wider fresh Python orchestration worker was **841.33 ms** [829.71–846.21],
including repeated artifact/source hashing, ASCII copies and retained artifacts;
parent reference/cross-backend comparisons are later still. Consequently neither
121 ms nor the stage sum is proven end-to-end UI load latency.

Imprint caller CFA zero-allocation/copy medians were about **15.18 / 8.90 ms**
for full cases, **12.16 / 9.03 ms** for half cases. Process peak WS at RGB readiness
was **724.36 / 690.14 MiB**, including Python/native runtime and earlier full-CFA
work. IPX decoder-process RSS was not exposed/measured by this existing helper;
its output is 91,411,200 CFA bytes, not a peak-memory measurement.

**Practical consequence:** the supplied IPX full native decode is competitive
with or faster than the source-level 512x512 partial reconstruction in its inner
codec interval, despite producing the entire CFA. Their measured ready scopes
differ, so do not claim the existing CLI delivers a full image faster than the
partial CLI based only on 121 versus 148 ms. A persistent/in-memory IPX + matched
RGB/ROI development path is a useful next experiment, with unchanged pixels,
runtime identity and distribution rights reviewed. No partial/half/quarter IPX
control was established, and no proprietary runtime is redistributed here.
