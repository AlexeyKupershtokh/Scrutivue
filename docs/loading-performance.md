# DNG pair loading and decode-cache reliability

Measured on 2026-10-31, Windows, Ryzen 5 5600 (6 cores / 12 logical CPUs),
NVIDIA RTX 3070 / DX12. These are optimized **debug-profile** measurements
(`opt-level=2`), not a claimed release result. No root application executable,
source image, LibRaw pin, or neighboring Redlamp checkout was changed.

## Exact sources and measurement boundaries

Only this requested pair was used for the DNG loading measurements:

```text
D:\Photos\halo\AK8_0005_halo0.dng
D:\Photos\halo\AK8_0005_halo24.dng
```

Paths above and below are publication-safe examples; the local measurement paths are not published. Both are 8280×5520 JPEG XL DNG canvases, 726 tiles of 256×256, developed by
the pinned Redlamp LibRaw, then default-cropped/oriented to **5504×8256 RGB16**
(272,646,144 bytes each). The comparison never uses the embedded JPEG preview.

“Cold” below means a fresh process and empty application decode cache, **not**
an emptied Windows standby/file cache. There was no privileged cache flush,
reboot, artificial disk-cache eviction, or source rewrite. Baseline and later
processes can therefore have different OS-cache and background-load conditions.

The isolated pair harness starts two decode callers sharing one bounded Rayon
pool. Its pair wall clock includes both full decodes and cleanup, but excludes
GPU rendering, presentation, and hashes (computed after timing). It is not the
sequential `--probe` timing.

The ignored service test uses the actual `Services::decode` queue, workers,
revision/cache checks, event channel, and render mailbox. It starts timing after
backend initialization (reported separately), submits the exact two paths as
selected A/B, waits for **both** complete RGB16 frames, then requests the normal
Slider pair at 1920×1080. “Render ready” includes source GPU upload, compute,
readback, CPU image ownership, and the returned `Event::Rendered`; it excludes
GPUI texture upload, UI event-poll delay, compositor/vsync, and actual screen
presentation. Thus it is a useful selection-to-view lower bound, **not** a
measurement of the last displayed pixel.

## Before and after

| Measurement | Pair full decode | First pair render after decode | Selection to render ready |
| --- | ---: | ---: | ---: |
| Original decode/crop path, isolated shared **2-thread** pool | 10,388.373 ms | not measured | not measured |
| Old **2-thread budget** reproduced through real Services, with new direct-crop copy | 11,048.562 ms | 518.808 ms | 11,567.371 ms |
| New shared **6-thread budget**, real Services, direct-crop copy | **5,563.632 ms** | **479.322 ms** | **6,042.955 ms** |

The second row is a controlled reproduction of the previous **CPU budget**, not
a claim that the original entire UI was re-measured unchanged. Service startup
was 371.757 ms, outside the selection interval. Clear/re-add with new IDs took
**0.458 ms** for both cache-hit decode events in that run; that excludes a fresh
GPU render after clearing GPU resources.

The production-budget Services run initialized the backend in 350.738 ms.
halo24's decoded event arrived at 5,552.023 ms, halo0's at 5,563.621 ms. The
render worker itself took 479.274 ms; both clear/re-add cache-hit events arrived
in **0.581 ms**, with pointer identity confirming that neither source was
redecoded. Both service measurements included post-decode source stat validation.

Exploratory six-thread isolated runs before/after the copy change were
4,993.557 ms, 5,522.967 ms, and 5,243.218 ms. A later contended run was
**8,552.353 ms**. Do not interpret these as a fixed latency or a guaranteed
2–3 second pair. The requested target has **not** been reached.

## Where the time goes

Original isolated two-thread run, per image (the stages overlap across A/B):

| Stage | halo0 | halo24 |
| --- | ---: | ---: |
| JXL transport, including virtual TIFF allocation/copy | 8,795.773 ms | 8,787.261 ms |
| LibRaw open | 0.051 ms | 0.560 ms |
| LibRaw unpack | 282.916 ms | 284.118 ms |
| LibRaw process | 639.796 ms | 634.823 ms |
| LibRaw oriented RGB16 bitmap creation | 291.549 ms | 280.091 ms |
| Rust full RGB16 copy | 93.097 ms | 90.600 ms |
| Separate default-crop allocation/copy | 199.596 ms | 203.870 ms |

New real-services six-thread run (all stages are per-image wall intervals):

| Stage | halo0 | halo24 |
| --- | ---: | ---: |
| JXL transport | 3,890.589 ms | 3,890.669 ms |
| LibRaw open | 0.852 ms | 0.775 ms |
| LibRaw unpack | 345.004 ms | 344.758 ms |
| LibRaw process | 738.342 ms | 730.218 ms |
| LibRaw oriented RGB16 bitmap creation | 334.604 ms | 337.497 ms |
| Rust final RGB16 copy + default crop | 128.398 ms | 128.892 ms |
| Native buffers + virtual TIFF + mapping release | 124.524 ms | 117.740 ms |
| Full raw loader total | 5,562.826 ms | 5,551.045 ms |

Original per-file trace totals were emitted before native/virtual-buffer
destruction; the **pair wall clock does include destruction**. The new trace
reports `release` separately and includes it in `total`, avoiding that ambiguity.
The new `copy_crop` stage copies only final oriented crop rows into the final
RGB16 owner, instead of copying the full bitmap and then allocating a crop.

Transport diagnostics now distinguish setup, JXL read, render, and sample stream.
`*_worker_sum` is the sum of worker elapsed intervals, **not wall time** and not
an OS CPU-time measurement. In the real-services two-thread reproduction,
halo0 spent 9,452.701 ms in transport: 152.467 ms setup, 43.346 ms summed read,
8,300.182 ms summed JXL render, and 914.997 ms summed stream. This identifies
JXL reconstruction, rather than TIFF header parsing or the copy alone, as the
dominant remaining bottleneck.

At six threads, halo0 transport setup was 152.270 ms, summed read 48.417 ms,
summed reconstruction 9,441.056 ms, and summed sample streaming 1,089.626 ms;
transport wall time was 3,890.455 ms. Increasing the bounded budget parallelizes
that reconstruction, but does not remove its work. A future faster JXL backend
would require independent sample-equivalence, packaging, and CPU-budget tests;
no unvalidated codec substitution or optimistic 2–3 second promise is made here.

Each virtual TIFF still preserves the compressed original metadata/offset space
and appends 274,233,600 bytes of samples: 383,300,116 bytes for halo0 and
382,894,312 for halo24. These costs are **not** omitted or replaced by previews.
LibRaw's intermediate image and oriented bitmap remain additional allocations.

## Implementation and cache behavior

- Two outer decode workers share **one** bounded pool, sized to half available
  logical CPUs, at most six (six total, not six per image). Render and thumbnail
  pools remain separate. This is not unlimited nested CPU oversubscription.
- Selected A/B jobs precede speculative neighbors. No new prefetch starts while
  either selected decode is outstanding; at most one prefetch runs afterward.
  A matching active prefetch is promoted/reused. Unrelated speculative work is
  cancelled when a new selected decode needs its worker.
- Cancellation is checked between JXL tiles and after tile reconstruction,
  around native stages, through LibRaw's progress callback, before cache
  publication, and before completion dispatch. It is cooperative, not forcible
  interruption of a currently executing tile/native loop. Raster decoders can
  finish their current non-interruptible call before noticing cancellation.
- Cache identity is **canonical source path + file size + modification time +
  creation time**, not a transient `PhotoId`. Canonicalization/stat and header
  reads stay on workers/scanner, never UI callbacks. No full-file hash is used
  on the UI or cache-hit path. `ScannedPhoto`/`PhotoMetadata` fields are unchanged.
- The revision is checked before lookup/decode and again before accepting a hit
  or decoded result. A changed source is rejected; a newly successful revision
  removes older cache revisions of that path. Missing modification timestamps
  disable caching rather than accepting unsafe hits.
- One in-flight decode per canonical revision prevents duplicate pixel work,
  even when path aliases reach separate queue slots. Pending requests coalesce;
  current IDs receive the shared immutable frame. Stale/cancelled completion
  cannot publish as a newer request. Unexpected decoder unwinds are contained
  at the worker boundary rather than permanently losing a worker/active slot.
- Clearing the list cancels obsolete work but **does not clear decoded frames**.
  Re-add/new IDs can reuse them. LRU storage remains bounded to six frames and
  approximately 1 GiB, retaining at least two frames even if the selected pair
  alone exceeds the byte budget. Selected A/B paths are protected from neighbor
  eviction; large evicted buffers are destroyed off the UI and outside locks.
- This is an in-process bounded cache, not a persistent disk decode cache.
  Process restart and genuine eviction still require a full decode. Stat-based
  revision identity cannot detect an adversarial same-size rewrite that also
  restores all relevant filesystem timestamps. Concurrent truncation/replacement
  of mapped source files during decode remains unsupported; do not edit RAWs
  while they are being decoded. Refresh is still required to revise already
  displayed library/GPU IDs after an external edit.

## Pixel/source integrity

Full little-endian RGB16 SHA-256 stayed identical before and after:

| Source | RGB16 SHA-256 | Source-file SHA-256 |
| --- | --- | --- |
| halo0 | `f8599b83cf742d5c9e83d957eae515e2069e99331f6ece3fb89f0d99c008131f` | `5158a92acd72f7e6f7427d6fc51652370785508cedc28666538a4b9d8d14cc07` |
| halo24 | `cc7f1de3650cd2b34c01bf51fca0baeea710f80e03a0cf5976cf580bac5d596a` | `f4f55db44983bd98e2533ce88039c5a44b67a53704ce9bbbbb634f8a8a36711f` |

No LibRaw development settings changed: 16-bit sRGB, camera WB,
`no_auto_bright=1`, bilinear quality, no median passes. DefaultCrop uses the
same exact rational/integer geometry and all eight orientations; only the
location of the final lossless row copy changed. The Redlamp LibRaw commit
remains `f4956c404fba0c6035ce9e06353d4b4ffe068fc7` (including Nikon HE/HE*).

The existing CC0 Nikon fixtures under `target/fixtures/raw` were also decoded
read-only after the native/copy changes. They remain **8280×5520**, with the
previous full RGB16 SHA-256 values:

- HE: `0823f42e07a4616fb0ec75f68baed88b5868c04e0661b42574ba0c1adc15c266`.
- HE*: `e995586232443fe7ea8a46e6895b0315e6b695c19b8dda77c5fec924bd51f2e0`.

That isolated six-thread HE/HE* pair took 2,364.815 ms; this is a different,
non-JXL pair and **must not be presented as the requested DNG pair timing**.
Source SHA-256 remained `be91bac25573adf7ec6320a542105d87b4fb9c956d7b620ec90d57ac2ae6c3f0`
and `e113deb305d84153b26251ce259d1f839be1f36b048cbe83a4548cbda806c0f5`.

## Reproduce

```powershell
$env:SCRUTIVUE_PROFILE_PAIR_A = 'D:\Photos\halo\AK8_0005_halo0.dng'
$env:SCRUTIVUE_PROFILE_PAIR_B = 'D:\Photos\halo\AK8_0005_halo24.dng'
$env:SCRUTIVUE_DECODE_TRACE = '1'
cargo test services::tests::profile_dng_pair_through_services -- --ignored --nocapture
# Test-only old CPU budget; this override does not exist in production builds:
$env:SCRUTIVUE_PROFILE_DECODE_THREADS = '2'
cargo test services::tests::profile_dng_pair_through_services -- --ignored --nocapture
Remove-Item Env:SCRUTIVUE_PROFILE_DECODE_THREADS
```

For isolated decode/hash tests without rebuilding the main application, after
debug dependencies exist:

```powershell
python scripts/profile_loading.py --threads 6 $env:SCRUTIVUE_PROFILE_PAIR_A $env:SCRUTIVUE_PROFILE_PAIR_B
python scripts/profile_loading.py --tests
cargo test services::tests
cargo test dng::tests
cargo test decode::tests
```

`scripts/profile_loading.py` is a Windows/MSVC developer harness, keeps its build
artifacts under `target/loading-profile`, and links the existing pinned LibRaw
objects with the current native wrapper. Synthetic tests use scratch directories;
measurements/hashing do not modify sources.
Release builds are intentionally left to the coordinating parent session;
none was launched by this performance work.

Final full debug test run: **96 passed, 0 failed, 3 ignored** (the two optional
native-GPU suites and the opt-in loading measurement). The real-services loading
measurement was run explicitly and passed separately. Tests cover cache hits
after clear/re-add/new IDs, canonical aliases, single-flight counters, size/mtime/
created revision identity, source mutation during decode, selected-frame eviction
protection, duplicate jobs, prefetch promotion/preemption, cancellation/stale
completion, native LibRaw cancellation/recovery, exact RGB16 crop/orientation,
and JPEG EXIF orientation without source changes.
The isolated `scripts/profile_loading.py --tests` run also passed **11/11**.
