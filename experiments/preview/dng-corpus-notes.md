# Actual DNG corpus partial-decoding results

`dng_corpus.py` is an isolated, Python 3.11+ standard-library-only orchestrator for the
**30 actual DNGs** in `E:/Documents/AI/corpus/3 этаж single/halo`: six scenes,
each with halo values `0, 24, 48, 96, 256`. It also inventories and hashes the
30 paired JPGs, but does **not** decode them. JPG and parent-directory NEF
experiments are separate work. No embedded JPEG is decoded, even for a
thumbnail, by this runner. The application policy allowing embedded JPEG only
for thumbnails is not changed.

The parent completed the default run in
`target/preview-dng-corpus/halo30-20261101/`: **1,590 sequential processes**, all
30 actual DNGs, **1,350 exact ROI decodes** and **90 original-tile progression
probes**. The manifest is complete without gaps, and all 60 DNG/JPG source
hashes/metadata are unchanged. The original preparation-only status has been
superseded by this measured run.

## Commands

Run from the repository root in PowerShell. A command **without `--run`** only
checks filenames/file sizes/tool availability and prints the plan. It starts
no decoder, builds nothing, hashes no source, and writes no output:

```powershell
python experiments/preview/dng_corpus.py --output target/preview-dng-corpus/halo30-run-01
```

The parent/operator can run this workload sequentially after other benchmarks
have stopped. The selected output directory **must not already exist**:

```powershell
python experiments/preview/dng_corpus.py --run --output target/preview-dng-corpus/halo30-run-01
```

The default trusted DLL is the explicitly supplied
`E:/Documents/AI/REA_DXO/pipeline-graph/resources/native/libjxl-0.12.0/jxl.dll`.
The runner records its SHA-256 and those of the two prebuilt executables. It
never searches for a replacement DLL or invokes a compiler/build system.

## Default bounded workload

- One child process at a time; six outer tile workers per child. No concurrent
  benchmark processes are launched by the runner. Existing probes use a
  single native decoder thread per tile; native progression probes one tile.
- Per-input layout discovery uses `dng_jxl_bench.exe --extract-tile` on tile 0.
  Its existing JSON supplies width, height, tile width/height, channels, tile
  count, sample byte count and TIFF byte order. These are checked on every
  input, not assumed to be 8280 pixels wide or to have a particular tile count.
- Each input gets a native full transport-only decode and a new raw reference.
  The export's size and SHA-256 must match the reported layout/canvas hash.
  This does not run LibRaw display development or export developed RGB16.
- Five raw-coordinate rectangles per input: centered 512×512, top-left
  128×128, 257×257 across an internal tile boundary near center, centered
  1024×1024, and bottom-right 80×60. Dimensions are bounded by the discovered
  canvas. For a canvas without an internal tile boundary, the boundary case
  falls back to a centered rectangle. Actual coordinates and selected tile
  indices are retained per input.
- Each rectangle runs **three repeats** in `dng-roi-native`, `dng-roi-oxide`,
  and `dng-roi-oxide-crop`: **1,350 ROI processes** in total. Backend order
  rotates by repeat. Every result must be exact against the native reference,
  report zero differing samples/zero maximum error, and match an independently
  row-hashed reference crop. Source SHA-256 and ROI geometry are checked too.
- One halo-48 input per scene is representative: all six scenes receive
  **three native full** and **three oxide full** transport decodes, compared by
  complete-canvas SHA-256. The other 24 inputs get one native full decode.
  Totals: **42 native full + 18 oxide full = 60 full processes**. Representative
  selection is recorded and can be changed with `--representative-halo`.
- Actual compressed tiles **first, spatial center, last** from **all 30 DNGs**
  are extracted and tested by `native-progressive`. Center is the tile
  containing the raw canvas center, not an assumed midpoint index. Tile 0 is
  reused from metadata discovery. With distinct indices this is **90 extraction
  + 90 progressive processes**. All events, flush statuses, intended ratios,
  final hashes and fresh-baseline comparisons are retained. A non-three-channel
  input is explicitly marked as a progression coverage gap because the existing
  progression CLI hardcodes three channels; ROI/full layout checks still run.
- Default total: **1,590 sequential child processes**. Native progression itself
  also performs one ordinary full **tile** decode for exact comparison; this is
  not an additional child or a full-canvas decode.

`--oxide-full-scope all` additionally checks full oxide/native canvas equivalence
on the remaining 24 inputs (one oxide full decode each), for **1,614 total child
processes**. This closes the default whole-canvas oxide coverage gap at extra
CPU cost. `--progressive-scope representatives` reduces progression sampling to
all six scenes at halo 48 (18 tiles), for **1,470 total children** with other
defaults. `--repeats`, `--full-repeats` and `--threads` may reduce cost and are
capped at 3, 3 and 6 respectively. The per-child timeout defaults to 180 seconds;
failure/timeout stops the workload instead of silently omitting a sample.

## Correctness and interpretation boundaries

Native full → native ROI is a same-backend sample check on all 30 inputs. Oxide
and oxide-crop ROIs are checked directly against native full crops on all 30.
On the six representatives, full oxide/native canvas hash equality additionally
establishes the same-backend full-canvas reference equivalence for the oxide ROI
checks. By default, whole-canvas oxide equivalence is **not** established for the
other 24 inputs; their tested ROIs are checked exactly. Request the `all` option
above to extend that full-canvas coverage. Different scene/halo files are never
assumed to have identical content.

These are **unoriented raw samples**, not developed display RGB: no LibRaw white
balance, color transforms, DNG opcodes, DefaultCrop, orientation/display conversion
or GPU upload is measured here. Correctness/timing must not be described as a
finished display preview or production ROI renderer. The existing tile selection
and oxide sub-tile crop implementations are measured as-is, not optimized.

Progression observations concern these actual sampled tile codestreams. If no
progression events appear, that is a bounded negative result across the recorded
inputs/tiles, not proof that all JPEG XL encodings lack progression. Successful
native flushes still allocate/output **full tile dimensions**; intended detail
ratios 2/4/8 are not public half/quarter-sized output buffers. Same-decoder final
samples must equal a fresh complete native tile decode. No private scaling API
or embedded preview is used.

## Timing separation

`measurements.jsonl` durably records each finished child, its exact command,
stdout/stderr, parsed JSON and external wall time. At completion/failure,
`measurements.json` and per-group median/min/max `summary.json` are also written.
Use the original disjoint stage fields rather than treating external wall time
as decode time:

- ROI: file-open/mapping, DLL setup, pool setup, metadata/tile planning, ROI
  output allocation, selected tile decode/crop work, and assembly are separate.
  The selected-tile stage also includes its decoder/buffer/piece allocation and
  destruction; the current EXE cannot separate those sub-stages further.
- Full transport: open/stat, input reserve/first-touch/buffered read, virtual
  TIFF reserve/compressed copy/zero-first-touch, parallel tile work and IFD patch
  are retained independently. Native per-worker setup/header/output allocation/
  reconstruction/assembly/destruction counters are retained. Worker nanoseconds
  are **summed across workers**, not additional end-to-end wall time.
- Derived `input_to_samples_ready_ms` is `open_map_ms + work_ms` for ROI and
  `io.total_ms + transport.total_ms` for full transport. The companion
  `setup_plus_input_to_samples_ready_ms` adds DLL/pool setup in both cases.
  Neither includes source hashing, reference validation/export or process launch.
  Full transport's virtual-TIFF allocation/copy remains included as implemented.
- ROI source hashing and ROI validation have separate existing fields. Full
  hashing plus optional export is combined by the EXE in
  `validation_and_export_ms`; this runner does not pretend to separate them.
  Full results without `--raw-output` avoid reference file writes in repeated
  full measurements. Python's reference verification/row hashes are outside
  child stage timings.
- Ready-state working set/private memory snapshots are retained, separately
  from post-validation memory. A process peak working set is not peak live
  decoder allocation and is not a precise stage-local peak.

All source files are SHA-256 hashed with size/mtime/file identity before and
after execution, including inventoried JPGs. This and prior probes warm the OS
cache. ROI mmap is not prefaulted; addressed page faults remain inside work.
Full transport uses explicit buffered input; allocation/first-touch/read costs
remain visible. These are **not cold-disk measurements**, and the full/ROI I/O
strategies are not identical. Parent-directory NEFs are not opened.

The existing ROI EXE hashes the **entire DNG after every timed ROI**, not just
the addressed tiles. For this corpus's metadata-only discovered total of
3,114,284,072 DNG bytes, 45 ROI calls per input hash **140,142,783,240 logical
source bytes** in total. This is validation CPU/cache traffic, not generated
disk output and not a claim of physical disk reads. Initial/final identities,
full-decode validation and extraction add further work. Avoid interpreting a
long overall runner duration as ROI reconstruction cost.

## Output/disk safety and validation status

All generated files must be in a **new child** of `target/preview-dng-corpus`.
Each full reference is deleted by its **exact newly created filename only after
all checks for that input pass**; no directory/glob cleanup occurs. On failure,
the current reference can remain for inspection. At most **one full reference**
is retained at a time (historical controls were about 274 MB, but actual sizes
are discovered per input; the existing EXE caps a canvas at 1 GiB). Ninety small
compressed tile files are retained as reproducible evidence with the default
coverage. Their combined upper bound is the sum of the DNG source sizes, not
30 full raw canvases. JSON plus retained stdout adds overhead. The metadata-only
plan prints this conservative disk bound and source sizes; actual tile/reference
sizes are recorded as the run progresses.

The metadata-only plan found **3,114,284,072 DNG bytes** plus **880,502,025 paired
JPG bytes**. The conservative retained-output bound is therefore about **3.90
GiB plus JSON/log overhead** (one 1 GiB-bounded reference plus at most the DNG
source total in distinct selected tiles). This is deliberately loose: a run
retains only three selected compressed tiles per input, not each whole DNG.

The runner was prepared with syntax/metadata-only validation, then the parent
completed the recorded native workload. `manifest.json` reports
`complete_without_gaps=true`; every expected ROI and progression probe completed.
No application binary or production codec was changed.

## Measured actual-corpus results

All 30 layouts were **8280x5520, RGB16, 726 tiles of 256x256**. Each ROI/backend
has n=3 fresh processes. Values summarize per-input medians across all 30;
brackets span their min–max. Ready time is mapping + work, before native DLL/pool
setup, hashing and reference validation. Mmap demand faults are included.

| Raw region | Native libjxl ready ms [range] | Full-tile oxide ready ms | Sub-tile oxide ready ms |
| --- | ---: | ---: | ---: |
| Center 512x512 | **21.22 [20.11–24.26]** | 28.92 | 27.58 |
| Top-left 128x128 | 9.11 [8.66–9.95] | 12.33 | 11.70 |
| Tile-boundary 257x257 | 11.23 [10.57–12.52] | 14.95 | 14.40 |
| Center 1024x1024 | **50.20 [46.65–57.01]** | 70.14 | 67.79 |
| Bottom-right 80x60 | 7.57 [7.28–8.15] | 10.58 | 9.55 |

Center 512 selects **9 of 726 tiles**, returning **1.5 MiB** of RGB16 raw samples.
Median peak WS before validation is **19.35 MiB native**, **15.10 MiB oxide**,
**14.89 MiB oxide-crop**. Center 1024 native returns 6 MiB with **25.20 MiB**
median peak WS. A later whole-source integrity hash touches many more mapped
pages; post-validation peak memory is not the ROI-only figure.

The six representative halo48 inputs have n=3 full observations per backend:
full transport-ready medians across representatives are **1331.67 ms native**
and **1921.49 ms oxide**. Native full transport ran on all 30 files (n=1 on
nonrepresentatives), with median **1327.49 ms** across their observations. Those
full intervals include buffered whole-input allocation/read and virtual-TIFF
construction, but no display development. ROI and full output domains agree;
their different I/O strategies are explicitly included, not normalized away.

One recorded `AK8_0005_halo48.dng` center-native run addressed **1,608,768 bytes**
of **108,515,623 compressed tile bytes**. Stages were mapping **0.052 ms**,
planning **0.025 ms**, ROI output allocation call **0.005 ms**, selected tiles
**20.495 ms**, final assembly **0.364 ms**. Fresh-process DLL/pool setup was
**4.629 / 0.128 ms**, separate from ROI work. Allocation first-touch and tile-local
allocations remain inside later work where applicable; these are not isolated
allocator CPU totals.

All **1,350 ROI results** were sample-exact against complete native canvases,
including edges and tile crossings. Full native/oxide canvas hashes also matched
on all six whole-canvas representatives; the default does not claim full-oxide
coverage for the other 24, whose oxide ROIs were nevertheless checked exactly.

All **90 actual first/spatial-center/last JXL tiles** had **zero progression
events and zero successful progressive flushes**, with final output matching a
fresh native full decode. No half/quarter preview stage was found for these
sampled encodings. This is not a universal JXL-format claim. The large real-file
gain here is selecting tiles, not uniformly downscaling the entire image.
