# Nikon HE / HE* partial strip experiment

## Status and scope

`nef_stripes.h` / `nef_stripes.cpp` implement an isolated **source-level**
experiment using internal functions from the exact production Redlamp LibRaw
fork, commit `f4956c404fba0c6035ce9e06353d4b4ffe068fc7`. This is **not** a public
LibRaw ROI feature or a reduced-resolution entropy decoder. Production source,
decoder settings, the fork pin, neighboring checkouts and application binaries
are unchanged.

The implementation worker prepared source without native workloads. The parent
subsequently compiled it against the exact existing archive and checked an
initial 512x512 region on `AK8_0005.NEF`: exact CFA equality, 14 of 87 stripes,
about 147 ms ready time and 69.3 MiB process peak WS. This is a single smoke
observation, not a six-file result or a stable benchmark median. The parent
corpus run and separate boundary checks establish the final coverage below.
No native REA analysis session was needed for this source-based experiment.

## Calling contract

```cpp
NefStripeResult decode_nef_stripes(
    const uint8_t* strip, size_t length, int width, int height,
    const std::array<unsigned, 4>& roi);
```

- Pass the **complete existing compressed TIFF strip buffer** beginning with
  the picture header, not the precinct stream alone. The parent obtains its
  offset/length from the already-open LibRaw internal unpacker metadata. This
  function performs no file I/O and does not copy the full compressed strip.
- `width` and `height` are uncropped full raw sensor dimensions, matched against
  the parsed picture header. `roi` is `{x, y, width, height}` in the same raw
  coordinates, not LibRaw's visible/margin-cropped coordinates.
- Result pixels are tightly packed, row-major `uint16_t` raw samples. Odd ROI
  positions/sizes are allowed; no demosaicing, margin adjustment, resampling or
  source-image modification occurs.
- Requires the production supported picture-header profile, Nikon's vendor
  transfer-curve tail, width divisible by 8, and dimensions at least 64. This
  experiment additionally rejects odd sensor heights rather than silently
  retaining an unproduced last raw row. Dimensions must fit the header's 16-bit
  fields. TIFF `NEFCompression` 13/14 should also be verified by the parent: the
  strip-only contract cannot read TIFF/MakerNote tags or distinguish their IDs.
- Header, bounds, length, slice-index, workspace and decode failures throw
  English `std::runtime_error` messages. The existing bit reader zero-pads
  exhausted entropy substreams; this wrapper does **not** add complete entropy
  corruption detection or make the decoder a hardened untrusted-file parser.
- Links against the existing matching static `diff_raw.lib`. Do not compile a
  second decoder copy or substitute upstream LibRaw. Includes resolve directly
  to `../../vendor/libraw/src/decoders/nikon_he/` from the experiment source.

## Why random-access decoding should work (source reasoning, then tested below)

1. A decoder tile covers **64 original sensor rows**, with 32 internal
   coefficient rows. It uses file precincts `t*16 .. t*16+17`; neighboring tiles
   share two compressed precincts. A length-prefix/slice-header walk locates
   precincts without entropy decoding preceding tiles.
2. The production image orchestrator initializes predecessor/GCLI state fresh
   **per tile**, despite the older tile-header parameter comment suggesting
   cross-tile predecessor sharing. This wrapper follows the implementation.
3. The only cross-tile input to `decode_tile` is the copied two-row overflow.
   The tile's lift state/carry vectors are initialized locally. Incoming
   overflow is copied into the first two coefficient rows; it is never read
   by the subsequent lifting arithmetic. The output overflow is generated
   locally at coefficient rows 32/33. Therefore one preceding **discarded
   priming tile** supplies an exact overflow even if that priming tile starts
   with zero incoming carry. Its own first two coefficient rows can be wrong,
   but no reconstruction consumes them, and its output slot is overwritten.
4. Step 1 reads one neighboring coefficient row on each side. Step 2 reads
   neighboring step-1 rows. For simplicity and conservative isolation this
   experiment computes step 1 on requested tiles plus **one whole tile per
   side**, and decodes coefficients on that range plus **another whole tile
   per side**. It adds one preceding priming tile when the coefficient range
   does not begin at sensor row zero.
5. `decode_tile` uses `tile_index` only to select the coefficient output slot.
   Local rebasing is therefore possible. All original sensor-width subband
   layout/horizontal transforms remain unchanged. `is_first` / `is_last` always
   refer to actual sensor edges, never local halo edges.
6. Bayer step 2 writes one reusable full-width, 64-row staging buffer. Only its
   output row origin is rebased to zero; its full-width transform and real
   edge flags are preserved. Requested columns/rows are copied afterward.
   This is not passing a smaller image size to the decoder and not allocating
   a full raw canvas before cropping it.
7. A scoped active picture header supplies the exact header-derived GTLI
   weights through the production thread-local setter. The transfer LUT uses
   the production **integer** B/A/C curve, including negative arithmetic
   shifts, clamping and the two-bit output convention. It does not use the
   historical approximate breakpoint LUT.

Random-start equality required the explicit tests reported below; source reasoning
alone was not accepted as proof. There is no silent prefix/full-image fallback.
Run
ordinary calls on a worker thread; do not nest this function inside another
active HE decoder on the same thread (the existing setter has no getter for
restoring an enclosing active header).

## Work/memory counters and timings

- `total_stripes`: `ceil(sensor_height / 64)`.
- `decoded_stripes`: all 64-row decoder tiles actually entropy/IDWT decoded,
  including conservative halos and the discarded priming tile.
- `compressed_bytes_addressed`: the union of complete precinct spans selected
  by those tiles, including each precinct's 12-byte prefix and counting shared
  overlap precincts **once**. It excludes picture/slice headers, terminators,
  and header-only index probes. It is neither disk bytes read nor an exact
  hardware-memory traffic counter; overlap precincts are entropy decoded
  again in neighboring tile calls, just as in production.
- `header_index_ms`: validation, full header-only precinct indexing, layout,
  range/budget and unique-byte calculations (including the small index vector).
- `allocation_ms`: large buffer allocation/zeroing and exact transfer-LUT
  construction. Allocations internal to the existing decoder are instead
  included in `entropy_idwt_ms`.
- `entropy_idwt_ms`: prediction-table access/first-use setup plus tile decoding
  and per-tile predecessor initialization/destruction.
- `bayer_ms`: step 1 on its halo range plus step 2 on requested tiles.
- `copy_ms`: requested raw ROI copies out of full-width tile staging.
- `total_ms`: complete call including temporary-buffer release; excludes
  caller strip acquisition and full-reference LibRaw decode. Timed phases do
  not promise an exact sum of total due to orchestration/timer overhead.

Coefficient and scratch allocations depend on the local selected range, not
`total_stripes`. No image-wide raw staging canvas is allocated. At original
width 8256, a 512-row request intersects at most nine output tiles, thirteen
coefficient tiles and eleven scratch tiles, plus one priming decode whose
output reuses the first coefficient slot. Main coefficient/scratch storage is
54,263,808 bytes; full-width tile staging is 1,056,768 bytes. Even a full-width
512-row returned ROI is 8,454,144 bytes. A conservative 128 MiB admission check
also includes carry, transfer LUT, index, decoder transient buffers and a
1 MiB allowance. This is an allocation estimate, **not measured process RSS**;
the caller-owned compressed input and separately allocated full LibRaw
reference are excluded. Larger requests are rejected when the estimate reaches
128 MiB. A small sensor / large ROI may naturally select every tile; this code
does not promise partial work when the requested support covers the full image.

The optional full-sensor same-function benchmark is intentionally not enabled
for large sensors: it would exceed this workspace limit. Use the parent's
production `raw-full` mode as the full reference instead. The pinned
predecessor type exposes `destroy()` but no destructor; this wrapper calls
`destroy()` with a per-tile scope so ordinary repeated successful requests do
not inherit the full orchestrator's predecessor-allocation leak. Internal
decoder allocation-failure cleanup remains a limitation of the pinned code.

The index accepts the same `sz + 12` precinct spans and zero terminator as the
production walker; it additionally validates initial/subsequent slice headers
and a terminal EOC/sentinel. It does **not** infer a minimum precinct count from
height plus overlap: production passes the actual available count to each tile,
and its zero-initialized tile buffer/tail-state flush can accept shorter final
tails. Missing tiles receive count zero without unsigned subtraction underflow;
the existing tile decoder rejects invalid tail states. Padded last tiles are
accepted up to the production tile-count bound. For exactness,
the argument passed to `decode_tile` for each precinct remains **`sz`**, exactly
as in production, not `sz + 12`. Partial final-tile profile compatibility also
requires tests; the corpus dimensions were not inspected by this worker.

Parent-reported runtime correction: the initial `ceil(height/4) + 2` minimum
incorrectly rejected actual `AK8_0005.NEF` (Nikon Z8, NEFCompression 13,
8280 x 5520) before any selected-tile decoding, while production unpack had
succeeded. That unsupported validation assumption was removed; span bounds,
slice markers and terminal checks remain. No full-decode fallback was added.
The corrected helper still needs the parent's rebuild and existing-reference
ROI comparison; this worker ran no decode or benchmark.

## Required parent validation

Compile the driver with these two files and the **same existing production
static library**, then run sequentially on all six actual NEFs. Open metadata
once, pass the matching raw width/height and in-memory strip, and compare each
returned ROI row with the corresponding LibRaw full `raw_image` row using its
actual raw pitch. Exact `memcmp`, not a perceptual tolerance, is required.

Cover center/top/bottom 512-row regions, x at left/interior/right edges, ROIs
crossing several 64-row seams, and small windows at y values 0, 1, 62, 63, 64,
65 and the last sensor rows. Include odd ROI coordinates and sizes. Center
random starts must leave a real skipped prefix, otherwise they do not test
priming. Compare both HE / HE* when actually represented; do not infer both
profiles from one passing file. Assert selected tile count is below the full
count on normal 512-row requests and report unique addressed bytes separately
from the already-resident full strip length. Add invalid/overflowing ROI,
dimension mismatch, truncated strip and damaged slice-marker negative cases.

If equality fails, report the first differing sensor coordinate and whether
it is adjacent to a seam or physical edge. Random access is then unproven;
do not label this successful or quietly time a full decode instead. A
prefix-only early-stop variant would be a separate, explicitly labeled
fallback experiment, not an established property of this implementation.

## Completed actual-corpus validation

The parent built the helper and driver against the unchanged production static
archive, then ran all six actual NEFs. All are **Nikon Z8 HE, selector 13**,
8280x5520, with 87 full-width decoder stripes. No actual HE* file is present in
this directory; HE* compatibility remains outside this run's measured coverage.

Evidence:

- `target/preview-nef-corpus-20261101-v2/`: 252 total observations, including
  **72 exact stripe-region decodes** (four region types x six files x n=3),
  complete LibRaw raw/RGB references and **42 negative checks**.
- `target/preview-nef-boundaries-20261101/`: **72 additional exact comparisons**
  covering one-pixel regions, left/right/top/bottom physical edges, odd positions
  and dimensions, y=62/63/64/65 seams, central random starts, last sensor pixels,
  and a full-width two-row seam region.
- All source hashes were checked and unchanged. No tolerance, normalization or
  coordinate shifting was used. **144 actual raw ROI decodes were sample-exact**
  against complete production LibRaw CFA references.

Main results, medians of per-file three-repeat medians; ranges span the six
per-file medians. Ready time includes read-only mapping, metadata and the helper.
Peak WS is absolute Windows process MiB before reference validation.

| Raw region | Median ready ms [range] | Peak WS MiB |
| --- | ---: | ---: |
| Center 512x512 | **147.88 [147.16–150.97]** | **69.32** |
| Top 512x512 | 108.69 [108.36–109.27] | 57.92 |
| Bottom 512x512 | 121.64 [120.47–122.64] | 62.37 |
| 127x130 seam-crossing region at y=63 | 65.08 [64.35–66.44] | 39.68 |
| Full LibRaw CFA plus caller copy | 985.49 [977.06–1007.15] | 505.06 |

The center request decodes **14 of 87 stripes**, including halos/priming, and
returns **524,288 bytes**, rather than allocating a full CFA image. For
`AK8_0005.NEF`, unique selected precinct spans were **2,806,744 bytes** of the
17,139,200-byte compressed strip. Index/header probes are additional; these
counters do not claim physically cold disk bytes.

That file's center-stage medians were header/index **1.15 ms**, major allocation
and transfer LUT **6.33 ms**, entropy/IDWT **123.65 ms**, Bayer reconstruction
**13.00 ms**, complete helper **146.22 ms**, and samples ready **147.16 ms**.
Temporary allocations/destruction are accounted for as documented above.

The original height-derived minimum precinct-count assumption rejected valid
short final tails on the first file. It was removed to match production's actual
available-count/tail-flush behavior; span, slice and terminal validation remain.
There is no full/prefix fallback in the successful partial measurements.

This is approximately **6.7x faster** and **7.3x lower process peak WS** than the
measured full-CFA reference interval on this corpus. It is **not** a comparison
to equivalent fully developed display RGB: WB/color/demosaic/orientation/crop
still require a separately validated partial-development design. Horizontal
entropy/IDWT still spans the original sensor width, so arbitrary narrow x windows
do not receive a proportional entropy-work reduction.

## Source provenance

All links below are fixed to the required fork revision. Observations above
are from source inspection, not runtime observations:

- [Production full-image orchestration, indexing, fresh prediction, three passes](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he/nikon_he_decode.cpp#L63-L197)
- [Tile-local state, overflow input and output, output-slot-only tile index](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he/nikon_he_tile.cpp#L90-L221)
- [Vertical lift uses local carry, not prior output rows](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he/nikon_he_idwt_vertical.cpp#L48-L143)
- [Bayer neighbor dependencies and physical boundaries](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he/nikon_he_bayer.cpp#L28-L232)
- [Production picture validation, exact transfer curve and active-header scope](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he_decoder.cpp#L33-L137)
- [Supported picture-header profile](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he/nikon_he_picture_header.cpp#L44-L140)
- [Thread-local header-derived GTLI](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he/nikon_he_gtli_table.cpp#L119-L148)
- [Precinct mini-header bounds and size convention](https://github.com/pdcgomes/redlamp-libraw/blob/f4956c404fba0c6035ce9e06353d4b4ffe068fc7/src/decoders/nikon_he/nikon_he_precinct_header.cpp#L41-L108)

The copied/adapted orchestration and transfer logic retain Dmitri Sotnikov's
2026 attribution and LibRaw's LGPL-2.1 / CDDL-1.0 alternative license notice
in `nef_stripes.cpp`; see the pinned vendor's license files for full terms.
