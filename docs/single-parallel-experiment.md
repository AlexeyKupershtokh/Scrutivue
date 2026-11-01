# Third experimental executable: single-source parallel loading

Build only the separate opt-in target:

```powershell
cargo build --release --locked --features experimental-single-parallel --bin scrutivue-single-parallel
.\target\release\scrutivue-single-parallel.exe
```

Keep the matching `onnxruntime.dll`, `models/` folder and complete pinned `libjxl/`
directory beside newly built executables. The feature includes `experimental-jxl-parallel`. It does not
change a codec version, the pinned Redlamp LibRaw fork or any source photograph.
The normal executable, second experimental executable and draft release are not
replaced. Enabling the feature for another target would also enable the experiment
in that build; always use the named third target above.

## Bottlenecks found

The opt-in `single_file_stage_profile` exercises the production planner, decoding,
snapshot and completion functions with one source, without GPU presentation. On a
Ryzen 5 5600, a warm-cache 45 MP original at 2560x1440 Fit took 3441 ms:

- Decode plus regional development: 1864 ms.
- Serial preview composition, allocation and deferred header qualification: 1403 ms.
- Full-frame completion: 139 ms.
- Planning: 2 ms.

Suppressing previews reduced that total to 2016 ms. A twelve-worker decode pool
alone reduced it to 2275 ms, but still spent 776 ms in serial snapshots. Independent
tile decoding already scaled: the no-preview one-worker case took 8716 ms versus
2016 ms with six workers. There is no reason to expect another 2x from merely
doubling the existing six workers on a six-core/12-thread CPU.

The progressive derivative exposed a larger scheduling issue. Fit took 4786 ms
with **108 single-session steps / 54 coarse starts**, whereas 100% used batches
and completed in 1090 ms. Even with previews suppressed, Fit took 2913 ms. The
coarse-preview policy serialized regional development and repeatedly stopped to
compose snapshots rather than exploiting independent tiles. These inputs are
existing **lossy progressive derivatives**, not lossless recodings of originals.

These initial numbers are individual optimized-debug diagnostic runs, not release
executable speedup claims. Stage `decode_ms` combines JXL decoding and regional
LibRaw development; it does not purport to separate those two costs. Hashing and
reference rendering happen after the ready timer. No storage cache was flushed.

## Third experiment

Changes are opt-in and single-source-specific:

1. Compose independent Single preview rows in the bounded tile pool. Each row
   retains identical coordinates, rounding, placeholders and RGB16-to-BGRA8 math.
   Only immutable `Samples` views are shared, never a live decoder handle.
2. Preserve the first quick progressive coarse update and resume that decoder to
   exact pixels. Then batch independent remaining tiles instead of starting a new
   serial coarse session for every tile. Retained partial sources use the same
   policy; cancellation and per-tile successful-result retention remain in place.
3. Execute a retained single-session step inside its selected pool so the native
   runner uses that registry instead of the six-worker CLI fallback registry.
4. Allow `logical CPUs - 2` workers, clamped to 2–12 and never below the normal
   paired budget, **only while one source is selected**. This uses ten workers on
   the test CPU. Twelve workers saved only about 2–4% in the initial candidate
   diagnostics; ten leaves scheduling headroom for the UI and other services.
   This is a worker-count limit, not CPU affinity or guaranteed core reservation.

A lazily created single-source pool and the original paired pool never run tile
work simultaneously. Selecting Slider/Diff with two IDs switches back to the
original six-worker budget, progressive scheduling and preview behavior. The
previous parallel Diff renderer is retained. There is no pool per tile or row.

Six-worker candidate diagnostics isolated the changes: original Fit 2474 ms,
progressive Fit 1121 ms. At ten workers they took 1847 ms and 1005 ms respectively.
Progressive Fit now used **two steps, one coarse start and two batches**. Their
final RGB16 hashes matched the preceding implementation exactly.

## Reproducing measurements

```powershell
$env:SCRUTIVUE_PROFILE_PAIR_A = 'D:/Photos/example.dng'
$env:SCRUTIVUE_JXL_TEST_LIBRARY = "$PWD/target/release/libjxl/jxl.dll"
$env:SCRUTIVUE_SINGLE_PROFILE_OUTPUT = 'target/single-parallel-review/new-stage.json'
$env:SCRUTIVUE_SINGLE_PROFILE_THREADS = '10'
cargo test --locked --features experimental-single-parallel --bin scrutivue single_file_stage_profile -- --ignored --nocapture
```

Optional test-only controls: `SCRUTIVUE_SINGLE_PROFILE_100=1`,
`SCRUTIVUE_SINGLE_PROFILE_VIEWPORT=2560x1440`, and
`SCRUTIVUE_SINGLE_PROFILE_NO_PREVIEWS=1`. Always use a new output filename.
In the preceding implementation, progressive single steps ran outside the pool;
changing its test pool size did **not** change the inner fallback runner's budget.

`single_file_actual_worker_profile` additionally exercises the actual asynchronous
worker and event stream. It uses the production thread budget, not the stage
profile override, and needs the pinned `libjxl/` bundle beside the test executable.
It times source opening through full-frame completion and checks the last preview
against the complete frame; no GPU upload or screen latency is included.

Release comparison artifacts live under `target/single-parallel-review/`. The
baseline release-profile test executable adds only the test profiler to the
second experiment's production code. Both test executables are retained with
their native runtimes for alternating, repeated comparisons.

## Release-profile worker comparison, 2026-11-01

Three repetitions per input/zoom/variant, alternating baseline/candidate order,
warm OS cache, 2560x1440 viewport, Ryzen 5 5600. Baseline is the second experiment
with its Diff fix, not the original feature-disabled application. Each run creates
a fresh asynchronous worker. Timers include opening, progressive updates, preview
composition and authoritative frame completion, but exclude reference checks,
RGB16 hashing, GPU upload and screen presentation. These are release-profile
**test executables using production loading code**, not GUI wall-clock timings.

| Input | Zoom | Second experiment, median | Third experiment, median | Speedup |
| --- | --- | ---: | ---: | ---: |
| `AK8_0036_halo0.dng` original | Fit | 3229 ms | 1789 ms | 1.81x |
| Same original | 100% | 2417 ms | 1787 ms | 1.35x |
| `AK8_0005_halo0-lossy-progressive.dng` | Fit | 4357 ms | 864 ms | 5.04x |
| Same progressive derivative | 100% | 1075 ms | 928 ms | 1.16x |
| `AK8_0988_halo48-lossy-progressive.dng` | Fit | 4340 ms | 878 ms | 4.94x |
| Same progressive derivative | 100% | 1090 ms | 940 ms | 1.16x |

Fit's median first snapshot improved from 45.5 to 35.9 ms for the original, and
from approximately 36–37 to 23–24 ms for the progressive derivatives. The first
coarse update was not sacrificed to delay all display until the large batch ends.

The hoped-for 2x is therefore **not universal**: the ordinary original was about
1.8x faster in Fit and 1.35x at 100%. The much larger progressive Fit gain removes
a repeated serialization policy; it is not a claim of fivefold codec throughput.
At 100%, batching already existed, so additional SMT workers give a smaller gain.
Fit snapshots also become less frequent because each larger batch completes more
tiles; this is part of the measured experiment, not an equal-work-count renderer
microbenchmark. No cold-disk throughput or other image-format acceleration is
claimed.

Raw reports and the comparison driver are under
`target/single-parallel-review/release-comparison/` and `compare.py`. All **36**
worker runs passed their completed-frame/last-preview checks; final RGB16 hashes
were identical between implementations and all source hashes remained unchanged.

## Third application build and pixel validation

The build/hash and validation below record the measured implementation **before**
the later [external LPIPS model change](metrics.md#external-model-validation-2026-11-01).
That EXE is now preserved as
`target/external-model-review/previous/scrutivue-single-parallel.exe`; the current
release target has been rebuilt with external weights and then
[parallel JPEG preparation](jpeg-loading-performance.md), followed by
[parallel NEF adapters](nef-loading-performance.md). These DNG timings were
not remeasured or relabeled as measurements of the subsequent changes.

Release build: `target/release/scrutivue-single-parallel.exe`, SHA-256
`32afda3550ceb263971cf0ba698f4251fc6a83f4ee1d4bd6e4dd7fdbac02e853`.

- Native CLI smoke passed both guarded direct completion and full-development
  fallback with legacy RGB16 equality, plus embedded ONNX/LPIPS statistics.
- Four large original/progressive inputs matched fresh decoding, historical
  frame hashes and **545,292,288 legacy RGB16 components with zero differences**.
  Pan-cache reuse remained intact. The earlier complete 38-input corpus run was
  not repeated for this scheduling/display experiment.
- Actual asynchronous partial-selection return, pause and explicit-clear probes
  passed for both original and progressive pairs. The clear validator now uses
  the selected worker budget instead of assuming a hardcoded six-tile first batch.
- The third release executable passed **10/10** original, progressive and
  mixed-tiling paired Slider/Diff cases with zero Diff byte error and unchanged
  source hashes. Its paired scheduler still uses the original thread budget.

Reports: `pixels-release-final/` and `paired-release-final/` in the review directory. These
headless reports contain reference pixels/buffers, not desktop screenshots.
No commit, push, remote build or draft-release change is part of this experiment.

Feature-disabled tests passed **150/150** (10 optional tests ignored); runner-only
tests passed **154/154** (11 ignored); third-feature tests passed **155/155**
(12 ignored). The native sequential/parallel pixel test and the new native
six/ten-worker **pool-switch continuation/cancellation** regression were also
invoked explicitly and passed. The latter moves the same progressive session
between registries in both directions through all four resolution stages.

The final release executable passed native no-capture UI smoke: **651 event-loop
responsiveness probes, zero 100 ms timeouts**, maximum 12.87 ms. This covers early
Slider/Diff changes, startup Single, selection stress, panning, navigator gestures,
statistics/picker, fullscreen and settings restart at 2x DPI. Report:
`gui-release-no-capture/result.json`. No desktop screenshots or end-to-end visible
image timing are claimed. The original two application executables and matching
ONNX Runtime library remain byte-for-byte unchanged.
