# Actual JPEG corpus reduced/ROI results

## Status and scope

The runner was initially prepared without native workloads. The parent then
executed the complete actual-image matrix sequentially. Evidence is in
`target/preview-jpeg-corpus/actual-jpeg-30-20261101/`: **1,776 fresh workers,
528 exact ROI comparisons, all 30 source hashes unchanged**. The standalone
build succeeded in 16.61 seconds with at most two jobs; the complete run,
including build and identity checks, took 569.31 seconds.

The older `jpeg-notes.md` synthetic/progressive/chroma-boundary fixtures remain
separate support-boundary evidence, not replacements for these actual files.

The fixed source directory is
`E:/Documents/AI/corpus/3 этаж single/halo`: all 30 actual `.jpg` codestreams,
six scenes × source halo variants 0/24/48/96/256. Each JPEG is decoded as an image,
not taken from a DNG preview. **Embedded JPEGs are thumbnails only, not a RAW
decode.** The 30 DNGs and separate six original NEFs belong to the independent
actual-RAW experiment and are not decoded or represented by this JPEG runner.

Only the standalone experiment is changed. Production codecs, dependency pins,
LibRaw, source images, root application binaries and manual application workflows
remain untouched. No commit or push is part of the experiment.

## Exact sequential command

Run from `E:/Documents/AI/Diff` **after other benchmarks/builds have stopped**:

```powershell
python experiments/preview/jpeg_corpus.py --build --run-id actual-jpeg-30-20261101
```

This builds the existing scalar libjpeg-turbo 2.1.5.1 recipe with at most two
build jobs, then executes one worker at a time. The build and all evidence are
isolated under `target/preview-jpeg-corpus/actual-jpeg-30-20261101/`. It does not
overwrite the old `target/preview-jpeg/jpeg_probe.exe`, build logs, measurements,
or synthetic evidence. An existing run directory is an error: omit `--run-id`
for a new UTC-stamped run, rather than deleting or overwriting evidence.

Optional header-only inventory/workload preview (no native execution/build):

```powershell
python experiments/preview/jpeg_corpus.py --plan
```

To reuse a previously built **schema-v2** probe without a rebuild, select its
exact path with `--probe` instead of `--build`, and choose a new run ID. The
original pre-instrumentation executable is intentionally rejected. `--native-only`
is available when Pillow cannot run, but the complete default protocol includes
both backends. Do not silently combine native-only evidence with a complete run.

## Coverage and estimated cost

Every one of the 30 actual JPEGs:

- Three native full-image decodes at 1/1, 1/2, 1/4 and 1/8 dimensions.
- Three independent Pillow native `draft()` + `load()` decodes at each scale.
  The installed backend/version is recorded per worker; the earlier environment
  had Pillow 12.3.0 / libjpeg-turbo 3.1.4.1 (SIMD-enabled build). Particular SIMD
  instructions are not profiled, and the scalar/native and Pillow versions are
  not a controlled SIMD comparison.
- Three genuine native central 512- and 1024-edge ROI decodes at 1/1 and 1/4,
  with a clamped 16-output-pixel horizontal halo; no full RGB buffer is constructed
  inside an ROI worker. This includes quarter-scale plus ROI decoding.
- A separate unpadded native decode for each ROI correctness comparison.

Six deterministic comprehensive representatives, **the halo0 file from every
scene**, additionally cover central ROI at 1/2 and 1/8, top/bottom 512/1024 ROI at
all four scales, and deliberately horizontally iMCU-aligned central boundaries
at all four scales. All padded ROI performance cases have three repeats; the
unpadded variants are single correctness diagnostics, excluded from performance
summaries. Coordinates use scaled output pixels. Each requested edge is clamped
to the scaled image, explicitly recorded. An ROI covering the entire scaled
image (including one made full by horizontal padding) is omitted and listed in
the plan, rather than mislabelled as partial output.

With no omissions this is **1,872 sequential fresh workers** (360 native full,
360 Pillow full, 864 padded ROI, 288 unpadded diagnostics) and **576 same-backend
ROI comparisons**. A planning estimate, **not a measured result**, is roughly
10–30 minutes including process startup, compressed reads, RGB dump I/O and a
small standalone build. Actual source entropy, dimensions, storage/cache and
system contention can change this substantially. Each worker has a 120-second
default timeout, not an overall run deadline. The header-only plan reports exact
case counts for these files.

Only one full scaled RGB reference and one partial ROI dump exist at a time;
each is deleted before the next scale/source. For a 45 MP input that is roughly
130 MiB full RGB plus at most a few MiB partial output, **not 30 full RGB images
retained together**. `plan.json` reports actual maximum scratch size and total
RGB bytes written over the run. Budget at least ~300 MiB plus the isolated native
build and JSON evidence (allow ~0.5–1 GiB free overall); source-dependent exact
requirements are not measured here. The runtime free-space guard covers scratch
and a 64 MiB evidence reserve after the build, not the build's initial space.

## Stage timing and memory boundaries

`jpeg_probe.cpp` gains additive schema-v2 instrumentation, with its existing
CLI, scalar codec, decoder settings and partial-output implementation retained:

- Input open/length, compressed-vector allocation/zero initialization, and file
  read timings are separate. Compressed bytes remain fully preloaded for decode.
- Header, `start_decompress`, crop, vertical skip, output-vector allocation,
  scanline decode/copy, decoder cleanup and optional RGB dump write are separate.
- `decode_stage_sum_ms` sums native decode phases without intervening memory
  queries. `decode_ms` is the decode wall interval and **includes** those queries.
  `output_ms` is allocation plus scanline work, not disk output.
  `read_allocate_decode_stage_sum_ms` adds the measured input stages to the decode
  phases; cleanup, memory-query overhead and optional disk output remain separate.
- Working set, private commit and process-lifetime peaks are sampled before
  input, after input allocation/read, header, start, crop, skip, output allocation,
  decoded output, and decoder destruction. At the last boundary, encoded and RGB
  vectors are deliberately still retained: it is not an all-buffers-freed sample.
- Pillow reports input read+allocation, header/draft, and native load separately,
  with before-input/read/header/output memory boundaries. It cannot expose its
  internal allocation as a separate stage without native instrumentation;
  `load_ms` honestly includes internal allocation and decoding. `BytesIO`/wrapper
  storage can add overhead. Packed RGB byte count is not Pillow heap accounting.
- Parent `worker_wall_ms` includes process startup/capture and, for the first
  native repeat, dump I/O. Do not treat that mixed-scope min/median/max as a pure
  decoder comparison. Native phase timings exclude dump writes. The parent
  correctness process's memory-mapped reference is outside child memory counters.

Memory values are absolute Windows process counters, not a precise codec heap
trace. Peaks at a stage are cumulative since process startup; consecutive peak
differences do not measure allocations. Intermediate allocations or transient
peaks may lie between observations. No cold-cache eviction or warmup is performed;
the SHA-256 passes also touch file data. No UI, presentation, orientation, color
management, GPU upload or preview-then-full interaction is timed.

## Correctness and source integrity

Each first native full decode at a requested scale writes a temporary packed RGB
reference. Every ROI output is compared **only with that same native backend at
that same scale**, never with a differently scaled full image, Pillow pixels or
a generic resize filter. Comparisons process at most 64 rows of delta arrays at
a time over read-only mappings. Reference-crop and trimmed-ROI SHA-256, maximum
and mean channel error, differing channels, and interior errors excluding two
horizontal edge columns are retained.

The measured ROI pads horizontally by 16 scaled output pixels (clamped to the
image), then trims **only that partial output** for comparison. Unpadded variants
expose possible fancy-chroma boundary differences, especially on aligned cases.
No nonzero tolerance is silently accepted: **every padded comparison must be
byte-identical** or the run finishes with failed status and nonzero exit. Sixteen
pixels is an experiment parameter to validate on this corpus, not a universal
proof for all JPEG formats. Native crop may internally round x down to an iMCU
boundary; returned alignment, row scratch bytes and compressed bytes consumed
are retained. A worker aborts after the requested last row and does not decode
the remainder merely to obtain a crop. Full output also uses abort/destroy, so
this is not an EOI validation or malformed-file robustness test.

All 30 JPEGs have byte size, raw SOF marker, baseline/progressive flags, precision,
dimensions, component sampling factors and readable common sampling labels in
the manifest. Their SHA-256 is taken before measurements and again after the run,
including caught benchmark failures/interruptions; altered identity fails the run.
Hard termination/power loss cannot guarantee the final pass. DNG/NEF source
identity is the separate RAW runner's responsibility.

## Evidence files and remaining gaps

- `manifest.json`: identities before/after, JPEG metadata, runtime/probe/source
  identity, execution status, counts and limitations.
- `plan.json`: selected six representatives, counts, omissions and disk estimates.
- `measurements.jsonl`: flushed after every sequential worker, preserving partial
  evidence on failure; includes single unpadded diagnostics with an explicit tag.
- `summary.json`: min/median/max per measured case, sample count and median memory
  boundaries. Only `purpose=measurement` rows are aggregated.
- `correctness.jsonl`, `correctness-summary.json`: every actual ROI comparison and
  explicit padded/unpadded nonidentical cases.
- `failure.json`: recorded exception and traceback on a caught failure.
- `native/`: this run's standalone build, log and build metadata when `--build`
  is used. No EXE/DLL is committed.

Preparation validation was Python syntax only. The subsequent complete run
compiled the modified C++ successfully and decoded the actual corpus; the
manifest has `status=complete`, no source identity failures, and no padded or
unpadded nonidentical ROI comparisons. There were 24 intentionally omitted
ROI cases that would become full-image output after clamping/padding; the plan
records them rather than counting them as partial decoding.
This does not implement a production preview path, a reusable coefficient cache,
restart-marker indexing, arbitrary constant-time JPEG panning, progressive
early-scan display, or actual RAW-region reconstruction.

## Measured corpus results

Each case has three fresh processes. Values below summarize **per-file medians**
across the 30 files; bracketed values are their min–max, not a single-image
confidence interval. Decoder time is `decode_stage_sum_ms`, excluding file reads,
inter-stage memory queries, cleanup and dump I/O. Memory is process peak WS.

| Scale | Scalar C++ turbo 2.1.5.1 median ms | Scalar WS MiB | SIMD-enabled turbo 3.1.4.1 via Pillow median ms | Pillow WS MiB |
| --- | ---: | ---: | ---: | ---: |
| Full | 581.13 [547.42–621.28] | 160.59 | 328.97 [306.82–369.75] | 225.82 |
| 1/2 | 396.31 [368.57–442.53] | 62.94 | 284.25 [265.34–319.53] | 95.62 |
| 1/4 | 310.68 [285.88–349.75] | 38.53 | 260.06 [239.83–296.52] | 63.08 |
| 1/8 | 217.07 [198.10–248.42] | 32.42 | 235.32 [217.80–268.84] | 54.97 |

All actual inputs are baseline 8-bit 4:4:4, 8256x5504. Scalar central full-scale
512x512 ROI was **116.16 ms** [104.70–139.07], **31.31 MiB** median WS; central
1024x1024 was **135.09 ms** [121.34–157.56], **33.61 MiB**. Six-scene halo0
1024x1024 top/bottom medians were **50.67 / 217.09 ms**, corroborating the cost
of preceding-row entropy traversal. These are native partial reconstruction,
not full decode followed by a crop, and not arbitrary constant-time access.

For `AK8_0005_halo0.jpg`, full-scale full decode had median compressed allocation
**3.909 ms**, file read **17.053 ms**, RGB output allocation **14.793 ms**, and
scanlines **599.196 ms** (total decode phases 614.069 ms). The central 512 ROI
had compressed allocation/read **3.998 / 17.058 ms**, vertical entropy skip
**101.247 ms**, output allocation **0.228 ms** and requested scanlines
**29.618 ms** (decode phases 131.164 ms). Source read/allocation is deliberately
not hidden inside the ROI codec timing.

Ordinary quarter-preview then full still requires another decoder cycle.
For the SIMD path, corpus medians sum to about 589 ms versus 329 ms full only;
this is an illustrative sum of independent timings, not a measured presentation
sequence. Most benefit is lower first-output memory; reduced pixel count does
not imply proportional latency reduction.
