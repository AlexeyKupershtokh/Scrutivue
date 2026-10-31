# Large-DNG statistics: latency and memory

Measured on 2026-11-01, Windows, Ryzen 5 5600 (6 cores / 12 logical CPUs).
These are optimized **debug-dependency / `rustc -C opt-level=2`** developer-harness
measurements, not release-build or screen-presentation timings. No source image,
root executable, LibRaw pin, or neighboring Redlamp checkout was changed.

## Inputs and boundaries

The requested `AK8_0005_halo0.dng` / `AK8_0005_halo24.dng` pair was decoded
read-only, using the pinned Redlamp LibRaw. Each final frame is **5504×8256 RGB16**,
272,646,144 bytes (260.016 MiB). These are full developments, not JPEG previews.
Public examples below use illustrative `D:\Photos\halo\...` paths.

“Cold statistics” means empty statistics/identity/resize caches with the complete
decoded frame(s) already in memory. A fresh process decodes the sources first;
decode time and its much larger memory peak are reported separately. The Windows
file/standby cache was **not** flushed. Separate runs can differ in OS caching,
DLL startup, scheduling, and background load. An exploratory contended run took
2,702 ms at edge 512; the numbers below are observations, not latency guarantees.

The isolated harness uses the actual `metrics::Worker`, two-thread metrics pool,
two ONNX intra-op threads, and one inter-op thread. Decode uses two outer callers
sharing one six-thread pool, never six threads per source. The harness excludes
the application UI, renderer/GPU resources, thumbnails, neighbor prefetch, display
polling/presentation, and the service decode-cache lookup. Its mailbox is extracted
unchanged from `src/services.rs`. RGB16 integrity checks run **outside** statistics
timing. Before-change stage timings used `Analyzer` directly on the same two-thread
pool; after-change request timings also include worker queue/event delivery.

Windows `GetProcessMemoryInfo` measures process Working Set (RSS) and Private Bytes,
not GPU memory, total system RAM, or exact live Rust allocation size. A separate
Python thread requests a sample every 2 ms. Final runs averaged **2.37–2.45 ms**,
with maximum gaps 3.35–4.32 ms; short-lived peaks can be missed. Initial before-change
runs used the default Windows timer resolution, approximately 16 ms sampling, so
their reported peaks are coarser lower bounds. Stage-boundary readings supplement
sampling. These are sampled peaks, not a claim of byte-perfect allocator accounting.

## Cold stages, before and after

All values are milliseconds. Per-source columns are A / B. Edge 512 means an
actual structural size of **341×512**; edge 1536 means **1024×1536**. LPIPS always
uses **512×768** for this pair. “Neural total” includes its two resizes, runtime/model
initialization, tensor preparation, and inference; do not add those resize rows again.

| Stage | Before, 512 | After, 512 | Before, 1536 | After, 1536 |
| --- | ---: | ---: | ---: | ---: |
| Source SHA-256, A / B | 129.408 / 127.115 | 129.945 / 130.850 | 134.760 / 127.157 | 128.440 / 129.018 |
| Full-resolution pixel reduction | 232.242 | 227.331 | 225.740 | 226.495 |
| Haar noise, A / B | 0.989 / 0.908 | 1.212 / 0.902 | 1.032 / 0.993 | 1.172 / 1.053 |
| Immerkær noise, A / B | 3.822 / 3.934 | 1.327 / 1.392 | 3.821 / 3.852 | 1.273 / 1.247 |
| High-pass noise, A / B | 3.279 / 3.250 | 1.921 / 1.831 | 3.435 / 3.500 | 1.891 / 1.844 |
| Structural Triangle resize, A / B | 195.092 / 183.719 | 111.939 / 112.286 | 300.425 / 299.707 | 191.342 / 192.036 |
| SSIM Y | 5.856 | 6.181 | 49.803 | 50.752 |
| GMSD Y | 1.507 | 1.685 | 16.930 | 15.009 |
| MS-SSIM Y | 7.506 | 7.585 | 70.774 | 71.393 |
| SSIMULACRA2 | 168.098 | 167.350 | 1,452.484 | 1,489.591 |
| LPIPS Triangle resize, A / B | 211.179 / 233.150 | 123.959 / 127.211 | 189.961 / 188.185 | 123.480 / 125.015 |
| Neural total | 1,468.561 | 731.081 | 725.614 | 690.534 |
| All analysis, excluding revision lookup/hash | 2,280.335 | **1,375.163** | 3,163.152 | **2,940.847** |
| First result through final report, including lookup/hash | 2,537.187¹ | **1,636.340** | 3,425.359¹ | **3,198.687** |

¹ Before total is the sum of separately measured cache-key lookup and analysis wall
intervals, not the complete UI path. ONNX/DLL startup varied materially: the 512
before run spent about 1,024 ms in Neural beyond resizing, versus 480 ms after;
the entire difference must **not** be credited to the resize optimization. At 1536,
SSIMULACRA2 remains the largest statistics stage, about 1.49 s. This work does not
claim to have achieved a 2–3 second DNG **loading** target.

The four real resizes together changed **823.140 → 475.395 ms** at edge 512 and
**978.278 → 631.873 ms** at edge 1536. These are full RGB16 downsamplings, not
nearest-neighbor or preview substitutions.

## Which allocations cause the spike?

All memory values below are decimal MB (1 MB = 1,000,000 bytes), and include the
decoded frames. Before/after values are from separate processes.

| Interval | RSS before → after | Private Bytes before → after |
| --- | ---: | ---: |
| Pair statistics peak, edge 512 | 754.840 → **726.729 MB** | 754.627 → **711.078 MB** |
| Pair statistics retained after completion, edge 512 | 745.460 → **605.409 MB** | 745.165 → **589.435 MB** |
| Pair statistics peak, edge 1536 | 829.383 → **830.988 MB** | 828.068 → **828.813 MB** |
| Pair statistics retained after completion, edge 1536 | 774.955 → **637.923 MB** | 774.910 → **622.080 MB** |
| First 1536 structural resize peak | 702.648 → **577.581 MB** | 701.112 → **574.870 MB** |

1. **Triangle scratch was real but was not a 1.5 GB statistics allocation.**
   `image 0.25.10` first constructs an RGBA32F intermediate of
   `source_width × destination_height × 16` bytes: 45,088,768 bytes at 512,
   67,633,152 at 768, and **135,266,304 bytes** at 1536 for these portrait inputs.
   The replacement streams the **same vertical-then-horizontal filter**, retaining
   one 66,048-byte RGB32F row plus small normalized tap tables and the required
   output. It preserves clipped support, pixel centers, f32 accumulation order,
   RGB16 rounding, and 0…1 conversion. No dependency was added. Cancellation is
   checked between source/output rows instead of waiting for an indivisible resize.
2. **ONNX's CPU arena retained activation high-water allocations.** The session and
   model are still reused, but its arena is disabled so activations can be released.
   In the same-process `--compare-arena` experiment with identical prepared samples,
   arena-on took 103–105 ms per inference, retained about 140 MB after the first,
   and grew to roughly 270 MB retained after a second run. Arena-off took
   174–205 ms on the first allocation-heavy inference, then 105–128 ms, and returned
   close to its pre-inference footprint each time. All LPIPS results were identical.
   Thus this is an explicit memory/first-inference allocation tradeoff, not a claim
   that disabling the arena makes inference faster. Complete report hits do not infer.
3. **SSIMULACRA2 still dominates the 1536 statistics peak.** Its internal clones,
   XYB/planar buffers, products and blurred planes produce the ~831 MB process peak.
   Removing resize scratch therefore does **not** lower this overall peak. LPIPS
   now peaks around 756 MB at this size and releases its large temporary buffers.
4. **Native noise is small.** Luminance is converted once per ≤128×128 native patch
   into reusable ≤128 KiB scratch, instead of repeatedly for overlapping stencils.
   Immerkær no longer reserves an unused 2 MiB vector; Haar reserves only its actual
   coefficient count. Sampling geometry and estimator arithmetic are unchanged.
5. **Full decode/prefetch is much larger than single-A statistics.** Separate final
   pair decode intervals peaked at **3.27–3.54 GB RSS / 3.07–3.34 GB Private Bytes**;
   one requested DNG alone peaked at **1.77 GB RSS / 1.67 GB Private Bytes**, before
   statistics started. Decoded pair residency then fell to ~555–558 MB. These peaks
   include JXL transport, virtual TIFF and native decode buffers, not statistics.

The reported UI observation “~1 GB → 2.5 GB → drop” was **not reproduced as an
isolated statistics allocation**. Previously, the UI's neighbor job construction
and the services' one-at-a-time post-selection full-decode prefetch could overlap
with statistics after the selected image became ready. Each unseen 45 MP decoded
frame adds another ~273 MB, plus a much larger temporary decode footprint. This
is a concrete plausible cause, **not** a sampled measurement of the complete UI
event or proof that every observed UI peak came from prefetch.

The integrated UI policy now permits full-resolution neighbor prefetch **only
when both A and B are selected and Statistics is closed**. Single-A selection
never speculates. `schedule_decodes` applies that gate on selection changes and
from `toggle_metrics`: opening Statistics removes queued speculative jobs and
cancels already-running speculative decode, while explicit selected A/B loads
remain wanted and continue. Cancellation remains cooperative at decoder work
boundaries, not an immediate forced deallocation. The coordinating UI change
implements this policy; the service API remains `decode(jobs, selected, protected)`
and no metrics, decoder or DNG API break was needed. The isolated timings above
exclude prefetch in both versions, so they do **not** quantify the additional UI
memory improvement from this gate.

The isolated harness also excludes retained frames elsewhere in the app. Existing
decode storage can approach its ~1 GiB budget, and the 20-entry reduced-copy LFU
can retain up to 540 MiB for twenty distinct 1536×1536 RGB32F images. A full UI
memory total must account for those caches, GPU/host views, and concurrent prefetch;
it cannot be inferred by subtracting one metrics-stage RSS reading.

## Single A and completed memory-cache hits

Only A is decoded/hashed for a single-image request. There is no B revision scan,
paired pixel reduction, resize, SSIMULACRA2 or neural inference.

| Single-A measurement | Edge 512 | Edge 1536 |
| --- | ---: | ---: |
| First worker request through complete noise report | 136.676 ms | 132.554 ms |
| Included one-frame revision SHA-256 | 131.882 ms | 127.820 ms |
| All three actual noise estimates | 4.462 ms | 4.448 ms |
| Statistics peak RSS / Private Bytes | 283.075 / 279.564 MB | 286.192 / 283.140 MB |
| 100 completed warm worker requests, total | 0.0626 ms | 0.0569 ms |

For the final pair runs, 100 complete warm `Worker::request` calls plus immediate
event retrieval took **0.0718 ms (512)** and **0.0503 ms (1536)** total. This is an
in-process API microbenchmark, **not** a claim that the UI paints within a microsecond.
Warm hits have no measurable large memory transient.

The existing content-safe fast path was preserved, not replaced with filename-only
identity. A known immutable `Arc<Frame>` finds its weak identity and completed
report synchronously, without opening a file, scanning pixels, hashing, resizing,
noise/pixel reductions, or ONNX work. Cache locks only cover metadata. Tests now
count hash invocations/bytes, actual resizes and full pixel reductions alongside
stage counts, and assert that **none change** on completed live-frame hits.

An actually new decoded allocation still requires one SHA-256 verification on the
worker: **545,292,288 bytes for the pair**, about 257–261 ms in the final runs.
This is a scan of **decoded RAM**, not a full-file reread. Matching content then
reuses the report in one event without executing stages. Reusing library IDs never
proves content equality. The separate decode cache determines whether clear/re-add
returns the same allocations; its previously measured ~0.6 ms paired clear/re-add
hit is a different interval, documented in `loading-performance.md`.

## Integrity and validation

Every reported metric/noise value for this actual pair was exactly identical
before/after at both edges (only elapsed time differs). Full RGB16 SHA-256 was
verified before and after statistics, outside the measured interval:

- halo0: `f8599b83cf742d5c9e83d957eae515e2069e99331f6ece3fb89f0d99c008131f`
- halo24: `cc7f1de3650cd2b34c01bf51fca0baeea710f80e03a0cf5976cf580bac5d596a`

The Redlamp LibRaw pin remains `f4956c404fba0c6035ce9e06353d4b4ffe068fc7`.
No source-image writes, release builds, commits, pushes or root executable changes
were performed by this statistics work.

The isolated native/metrics suite passed **32 tests, 0 failed, 1 ignored** before
the optional profiling test was explicitly run. Tests include exact RGB16 Triangle
comparison against `imageops` for random/nonseparable patterns, impulses, edges,
odd portrait/landscape and narrow/single-axis images; cancellation; partial-stage
resume; changed pixels/dimensions; weak-source release; LFU capacity; completed
warm-hit counters; single-A-only work; exact direct-stencil noise equivalence;
Gaussian noise; real ONNX dynamic shapes; and repeated allocator-mode equivalence.

## Reproduce

```powershell
python scripts/profile_statistics.py --tests
python scripts/profile_statistics.py --edge 512 --output target/statistics-profile/pair-512.json `
  D:\Photos\halo\AK8_0005_halo0.dng D:\Photos\halo\AK8_0005_halo24.dng
python scripts/profile_statistics.py --edge 1536 --output target/statistics-profile/pair-1536.json `
  D:\Photos\halo\AK8_0005_halo0.dng D:\Photos\halo\AK8_0005_halo24.dng
python scripts/profile_statistics.py --single --edge 512 `
  D:\Photos\halo\AK8_0005_halo0.dng D:\Photos\halo\AK8_0005_halo24.dng
python scripts/profile_statistics.py --edge 512 --compare-arena `
  D:\Photos\halo\AK8_0005_halo0.dng D:\Photos\halo\AK8_0005_halo24.dng
```

This Windows/MSVC harness reuses existing `target/debug/deps` and pinned native
objects, builds only `target/statistics-profile/tests.exe`, and places the matching
ONNX Runtime DLL beside it. It works across the application package rename.
Raw per-stage results remain local under `target/statistics-profile/*.json`; no
photographs or absolute private corpus paths are published in this document.
The equivalent ignored Cargo test is `metrics::tests::profile_dng_statistics`,
configured by `SCRUTIVUE_PROFILE_PAIR_A/B`, `SCRUTIVUE_PROFILE_METRIC_EDGE`, optional
`SCRUTIVUE_PROFILE_SINGLE` and `SCRUTIVUE_PROFILE_COMPARE_ARENA`, and marker flag
`SCRUTIVUE_METRICS_PROFILE`. Profiling markers/counters are test-only and do not
add work to production UI callbacks.
