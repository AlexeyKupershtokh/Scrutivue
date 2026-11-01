# DNG/JPEG XL decoder experiments

Measured on 2026-11-01, Windows x64, AMD Ryzen 5 5600 (6 cores / 12 logical CPUs).
Standalone optimized developer harnesses only: no UI/GPU presentation, production
decoder substitution, application binary replacement or source-image changes.

## Reproduce and interpret

Build, commands, measured scopes and safety boundaries are in
[experiments/dng-jxl/README.md](../experiments/dng-jxl/README.md).
Complete local observations are saved under `target/dng-jxl-results-20261101/`.
Imprint source/build/API results are under `target/dng-jxl-imprint/`.
These ignored results can contain private paths; this document uses source
basenames and publication-safe example paths only.

The inputs are the requested `AK8_0005_halo0.dng` and `AK8_0005_halo24.dng`:
8280×5520 RGB16 canvases, 726 JPEG XL tiles of 256×256 including clipped edges,
274,233,600 unpacked sample bytes per canvas. The preserved LibRaw development,
orientation and DefaultCrop produce 5504×8256 RGB16, 272,646,144 bytes per frame.
Neither candidate is allowed to substitute the embedded JPEG preview.

Harness code uses `rustc -C opt-level=3` and existing optimized-debug Rust
dependencies (`opt-level=2`), including jxl-oxide 0.12.5. The same pinned Redlamp
LibRaw `f4956c404fba0c6035ce9e06353d4b4ffe068fc7` develops each successful JXL
transport result. Native libjxl is 0.12.0, DLL SHA-256
`b0a7f7721836359b29841cca0f15969e85bd386344f9ee0775292089bd3bcfda`.
Its existing local bundle's files are checked against its manifest. No native
library or decoder source is copied into the shipped application.

Each timing case is a fresh process. Source integrity hashing warms the Windows
file cache before the matrix; the matrix is **not a cold-disk benchmark**.
Normal reads, mmap/prefault and Windows cache-bypassing aligned reads are measured
separately. Even unbuffered I/O does not flush a drive/controller cache.
Three repetitions and rotated backend order reduce, but do not remove, normal
scheduling, thermal and background-load variation.

## Candidate support boundaries

| Candidate | Actual JXL-DNG result |
| --- | --- |
| Current production jxl-oxide transport | Baseline full decode/development. |
| Instrumented jxl-oxide | Full decode with separately timed sample conversion and assembly. |
| Native libjxl, per-tile decoder | Full decode/development; sample equivalence checked. |
| Native libjxl, per-worker decoder/output reuse | Full decode/development; sample equivalence checked. |
| Existing intoPIX/IPX bridge | Recognition-only experiment, not a JPEG XL decoder API. |
| Imprint C++ native-raw, requested commit | Both actual DNGs rejected by its explicit NEFCompression 13/14 gate before unpack. |

Imprint's unmodified Windows x64 Release build succeeded at the requested commit
`3916a114bc32b487de611135cf66514447c541a6`, with CPU/C++ forced (no Python codec
or Metal fallback). Its own pinned LibRaw dependency was built separately under
`target/`; the application's pin was not changed. Both existing CC0 Nikon Z9
HE/HE* NEF controls decoded, but these are different files/formats and their
half-size camera-space renders are **not DNG speed results**.
Details: [Imprint findings](../experiments/dng-jxl/imprint-notes.md).

## Full single-file timings

All values below are medians of **three fresh processes**. Times are seconds;
the full pipeline includes buffered source I/O, transport, the same full LibRaw
development/crop and buffer release. Library/thread-pool startup, validation
hashes and exports are separate. Each row's stage medians are independent and
need not sum exactly to its median end-to-end time.

| Backend | Workers | halo0 transport | halo0 full | halo24 transport | halo24 full |
| --- | ---: | ---: | ---: | ---: | ---: |
| Production jxl-oxide | 1 | 8.469 | 9.826 | 8.461 | 9.693 |
| Instrumented jxl-oxide | 1 | 8.493 | 9.710 | 8.432 | 9.713 |
| libjxl, decoder per tile | 1 | 5.710 | 6.928 | 5.745 | 6.991 |
| libjxl, decoder/output reuse | 1 | 5.640 | 6.862 | 5.627 | 6.885 |
| Production jxl-oxide | 6 | 1.856 | 3.082 | 1.852 | 3.078 |
| Instrumented jxl-oxide | 6 | 1.891 | 3.118 | 1.864 | 3.097 |
| libjxl, decoder per tile | 6 | **1.304** | **2.554** | **1.306** | **2.526** |
| libjxl, decoder/output reuse | 6 | **1.255** | **2.491** | **1.292** | **2.512** |

For this pair, libjxl reduces six-worker transport by about **30%**, but the whole
single-file pipeline by only **17–18%** because LibRaw development remains.
Per-worker reuse adds a modest and variable improvement (about 0.5–2.5% full
single-file time over per-tile native creation); it is not a second large win.
The production and instrumented Rust timings are close, not identical.

## Paired load: six-worker total budget

Two children run concurrently, **three workers per process**. This is a controlled
six-worker budget, **not the application's single shared Rayon pool**. Pair wall
time includes process creation, library/pool setup, all three validation hashes
per image, and cleanup; it is not directly interchangeable with the single-file
pipeline column above, older Services timings, or GUI presentation latency.

| Backend | Median pair wall | Observed range, n=3 | Sampled combined peak RSS |
| --- | ---: | ---: | ---: |
| Production jxl-oxide | 5.374 s | 5.146–6.515 s | 3.258 GB |
| libjxl, decoder per tile | **4.176 s** | 4.122–4.180 s | 3.342 GB |
| libjxl, decoder/output reuse | **4.155 s** | 4.085–4.176 s | 3.349 GB |

Native libjxl reduced this measured paired wall interval by about **22%**
(roughly **1.29×** throughput). A 2–3 second full-pair/UI target is **not proven**.
The first production pair was an outlier; the complete range is retained rather
than reporting only the favorable median or minimum. Decoder reuse's roughly
0.5% paired improvement is small relative to normal variation.

## Reading and explicit memory allocation

These isolated I/O observations use the same approximately 109 MB source files.
All times are milliseconds, median n=3. Hashing occurs after the I/O interval.

| Operation | halo0 | halo24 |
| --- | ---: | ---: |
| Buffered heap capacity reservation | 0.008 | 0.008 |
| Buffered input zeroing / first touch | 11.429 | 11.857 |
| Buffered `ReadFile`, Windows cache warm | **16.352** | **19.962** |
| Entire buffered open/allocation/read interval | 27.730 | 31.882 |
| Read-only mapping creation | 0.024 | 0.024 |
| Touch every mapped 4 KiB page | **22.054** | **22.298** |
| Direct buffer virtual reservation | 0.004 | 0.004 |
| Direct buffer virtual commit | 0.150 | 0.108 |
| Direct buffer zeroing / first touch | 27.871 | 27.896 |
| Aligned `FILE_FLAG_NO_BUFFERING` read | **531.413** | **515.169** |
| Entire unbuffered open/allocation/read interval | 559.860 | 543.288 |

Mapping's tiny syscall time alone is **not file reading**: later page faults
must be included. The explicit prefault pass makes that distinction visible.
Likewise, reserving virtual capacity is cheap; committing/touching pages and
copying hundreds of MB are the meaningful memory costs. The unbuffered test
bypasses the Windows file cache, not device caches; it is not a claim of a
physically cold drive. It also does not replace the buffered/mapped I/O path in
the decoder matrix. Actual first-open disk/cache conditions can therefore add
hundreds of milliseconds not visible in warm-cache source reads.

## Detailed six-worker libjxl stage breakdown

Milliseconds, median n=3, decoder created per tile:

| Sequential/wall stage | halo0 | halo24 |
| --- | ---: | ---: |
| TIFF metadata parse / validation | 0.020 | 0.018 |
| Virtual TIFF capacity reservation | 0.010 | 0.011 |
| Copy original compressed file into virtual TIFF | 13.434 | 13.369 |
| Zero / first-touch the 274 MB sample canvas | 29.349 | 29.292 |
| Parallel JXL tiles, including output conversion and assembly | **1,261.403** | **1,263.619** |
| Patch the original raw IFD | 0.012 | 0.014 |
| LibRaw open on memory buffer | 0.625 | 0.595 |
| LibRaw unpack | **264.327** | **259.536** |
| LibRaw processing | **599.059** | **605.078** |
| LibRaw oriented RGB16 bitmap creation | **247.198** | **250.941** |
| DefaultCrop geometry | 0.013 | 0.012 |
| Final RGB capacity reservation | 0.013 | 0.012 |
| Final crop row copy / first touch | 40.499 | 39.924 |
| Native LibRaw buffers release | 34.087 | 32.215 |
| Virtual TIFF and source release | 16.015 | 16.588 |
| Validation hashes, outside timed pipeline | 305.779 | 304.986 |

The JPEG XL work still dominates a single stage, but the remaining roughly
**1.19 s LibRaw/crop/cleanup interval** is now a substantial floor. More threads
or a faster JXL backend cannot eliminate that interval.

### Worker sums are not wall time

For halo0 at six workers, native reconstruction summed **6,520.6 ms** across
workers while tile wall time was **1,261.4 ms**. Header handling summed 60.9 ms,
decoder setup 20.6 ms, initialized tile output allocation 41.7 ms, assembly
33.8 ms and native destruction 63.3 ms. These overlap across workers; **do not
add them to the wall-clock table**. Native reconstruction includes its uint16
output conversion; a zero separate sample-conversion counter does not mean it
performs none. The Rust backend exposes that streaming conversion separately.

### Allocator instrumentation is a separate experiment

The libjxl-reuse allocation-traced runs counted **17,585 / 17,582** allocation
requests and about **1.406 GB cumulative requested bytes** for halo0/halo24.
Measured malloc/free callback durations summed **126.9 / 134.4 ms** across
workers, nested within setup/decode/cleanup. This is neither 1.406 GB of live
decoder memory nor 127 ms of extra sequential wall time.

The custom allocator and per-call clock/counter tracing change execution; traced
full pipelines were **3.015 / 3.082 s**, not the untraced medians above. Those
observations must not be used as the production allocator's exact cost. Reuse
mode's final destruction occurs outside per-tile allocation counters. Internal
LibRaw allocator timings are not exposed by the wrapper and remain inside its
measured unpack/process/bitmap calls.

## Memory findings

Changing JXL backends did **not** solve the full-decode peak: single-file OS peak
RSS stayed around **1.78 GB** for both Rust and native cases. Typical checkpoints
are about 114 MB with the source resident, 497 MB with virtual TIFF/canvas,
approximately 510 MB after tile decode, 1.51 GB with LibRaw's bitmap, **1.78 GB**
while copying the final RGB owner, and 280–283 MB after temporary release.
These are decimal GB/MB, not GiB/MiB.

The paired native peak was slightly higher, not lower, in this measurement.
Faster stages can overlap differently; a codec speedup is not a memory-saving
claim. Avoiding full virtual-TIFF copies and reducing LibRaw intermediates would
be separate future experiments requiring the same pixel/metadata checks.

## intoPIX/IPX and Imprint conclusions

The supplied intoPIX entry point rejected all **six** real JXL payload probes:
tile 0, 363 and 725 from each source. Native recognition took 3.1–3.4 microseconds;
runtime validation/loading stayed separate. No decode was attempted and no pixel
result was fabricated. This is a finding about the supplied Nikon IPX API, not
every intoPIX product. [Probe details](../experiments/dng-jxl/tico-notes.md).

Imprint's actual C++ DLL built and ran on Windows, but both DNGs failed its
explicit NEF-only gate (`NEF Nikon NEFCompression is not 13 or 14`) before unpack.
Its two successful NEF controls show the native C++ path really executed, not
a Python fallback. They do not establish JXL-DNG support or a DNG speedup.

## Correctness boundaries

The isolated test suite checks malformed extents, synthetic lossless samples,
big/little TIFF endianness, partial right/bottom tiles, all stored JXL orientations,
original linear profiles, allocator modes/reuse/error recovery, and the existing
LibRaw crop/orientation/cancellation tests. A correct synthetic round trip alone
does not establish real lossy-DNG equivalence: each measured real result also
hashes its full unpacked canvas and final RGB16 against production.

The native binding preserves stored orientation and the original structured
RGB/grey profile rather than requesting display sRGB. Unsupported ICC-only XYB
profiles fail explicitly. The real pair does not establish arbitrary-camera,
arbitrary-profile DNG support, complete cancellation behavior, GUI latency or
safe decoding of hostile files. No production backend change is made here.

**Observed validation:** all 48 main-matrix decodes, two traced decodes and 18
paired children matched production canvas and final RGB16 SHA-256. All 18 I/O
reads matched source SHA-256. Both source files' before/after hashes were
unchanged. The isolated 21-test suite passed with the actual native DLL enabled.

| Source | Canvas SHA-256 | Final RGB16 SHA-256 |
| --- | --- | --- |
| halo0 | `0b0b4a021ae33750a918e11317f79e382ee51ef599d978480325c395def05e92` | `f8599b83cf742d5c9e83d957eae515e2069e99331f6ece3fb89f0d99c008131f` |
| halo24 | `bbb5da9eee53216b39926d84fa2df27c325c855da54c04c0d3df62fc45e39dcd` | `cc7f1de3650cd2b34c01bf51fca0baeea710f80e03a0cf5976cf580bac5d596a` |

The unchanged application's complete correctness run also passed **110 tests**,
including both explicitly enabled native DX12 suites. Only the two opt-in real
DNG profiling tests were excluded. Standalone optimized experiment executables
and the Imprint C++ Release DLL were built; the application was not rebuilt or
replaced for this developer-only change.

## Recommended next step

**libjxl is the useful candidate for a separate production integration**, with
the tested original-profile/uint16 semantics, a bounded shared CPU budget,
cancellation and distribution/notices reviewed before adoption. Reuse is an
optional small optimization. The supplied TicoRAW/IPX and Imprint C++ paths are
not alternatives for these JXL-DNG files. No automatic application backend
switch, pin change, commit or push is part of these experiments.

## Follow-up: previews and visible regions

[JPEG/JXL preview and ROI experiments](preview-decoding-experiments.md) separately
measure real reduced JPEG output, encoding-dependent native JXL progression,
jxl-oxide region reconstruction, embedded DNG JPEGs and tile-selective raw regions.
These approximate/partial results do not replace the full developed-RGB16 timings
or correctness requirements above.

The expanded, actual-input-only follow-up is
[the 30 DNG / 30 JPEG / six NEF corpus investigation](corpus-partial-decoding-experiments.md).
Embedded JPEGs are thumbnail-only in that scope.
