# Diff preview composition with one source already complete

## Diagnosis

A completed A does **not** leave half the decode budget idle: the scheduler gives
the only unfinished source all six workers on a 12-logical-CPU machine. The
regression was in the work between decode rounds. Streaming Diff used a serial
viewport loop and lazily formed its downsampled difference cells on that same
thread. Large physical viewports therefore spent much more wall time composing
previews than decoding B, while the decoder pool was idle. A ready A also caused
an immediate blank paired snapshot to qualify both source headers before B's
first visible tile, although A required no further development.

## Change

- Prepare independent missing difference mip cells in parallel, reusing complete
  cells. Retry unavailable regions on the next snapshot; never compare placeholders
  or a coarse tile with final pixels.
- Compose viewport rows in parallel from the now read-only mip cache. Both phases
  run in the **existing bounded DNG pool**, not a fresh or nested pool per row.
- A small read-only `Samples` view excludes the non-Sync live JXL decoder. The
  decoder remains exclusively owned; no `Sync` implementation was added to it.
- Read the unfinished B before a full A where possible, and avoid computing tile
  coordinates twice for every exact pixel. Empty paired previews bypass mip work.
- Do not qualify a completed source, or qualify an empty source before its first
  tile is published. The guarded completion policy itself is unchanged.

Absolute RGB differences are still computed **before** mip reduction. Summation
order inside each mip cell, interpolation, gain, readiness masks, source geometry
and analysis frames are unchanged. No source image, native dependency pin,
statistics formula or UI-thread filesystem policy changes.

## Validation and measurement scope

An opt-in stage profile uses the production planner, tile decode, composition and
completion functions with A already decoded and B empty, six workers and a
2560x1440 viewport. A's decode and final reference rendering are outside timers.
It asserts that A's frame allocation is retained, A has no tile decode work, only
B completes, and the final Diff matches the independent full CPU render.

The initial diagnostic intentionally composed every snapshot, including offscreen
rounds. Its 100% result (`before-100.json`: 38.0 s total, 35.9 s composition) is a
**forced-repaint stress case, not normal production scheduling** and must not be
reported as the application's measured loading time. The corrected profile only
composes dirty visible previews, like the actual worker. Fit sees the whole photo,
so its diagnostic did measure the relevant visible composition bottleneck; its
initial run included one redundant final cached snapshot.

Local optimized-debug observations on Ryzen 5 5600, warm OS storage:

| Stage, A ready / B loading | Initial serial Fit diagnostic | Fixed Fit profile | Fixed 100% profile |
| --- | ---: | ---: | ---: |
| Total through B completion | 12999 ms | 4270 ms | 2635 ms |
| B decode rounds | 1832 ms | 1772 ms | 1943 ms |
| Diff composition / policy warming | 10990 ms | 2322 ms | 540 ms |
| Initial snapshot / policy warming | 226 ms | 19 ms | 4 ms |
| View snapshots | 32 | 31 | 5 |

These are diagnostic single runs, not identical-work-count or release-profile
latency guarantees. In particular, the 100% column cannot be compared against
the forced-repaint baseline as an application speedup. Reports are under
`target/diff-one-ready-review/`; no GPU upload, compositor or screen presentation
is timed. Expensive preview work still shares the pool with, rather than overlaps,
tile decoding, and Fit composition remains a measurable cost.

Regression coverage compares one-worker and six-worker preview bytes exactly
with full A and empty/partial/coarse/final B, matching and unequal dimensions,
odd viewport sizes, multiple zooms and warmed caches. Existing independent CPU
reference tests retain their one-byte rounding allowance, and difference-before-
downsampling, missing-region retry and full-frame invalidation checks remain.

Reproduce a fresh stage profile:

```powershell
$env:SCRUTIVUE_PROFILE_PAIR_A = 'D:/Photos/A.dng'
$env:SCRUTIVUE_PROFILE_PAIR_B = 'D:/Photos/B.dng'
$env:SCRUTIVUE_JXL_TEST_LIBRARY = "$PWD/target/release/libjxl/jxl.dll"
$env:SCRUTIVUE_STREAM_PROFILE_OUTPUT = 'target/diff-one-ready-review/new-fit.json'
cargo test --locked --features experimental-jxl-parallel --bin scrutivue ready_a_streaming_b_stage_profile -- --ignored --nocapture
# Set SCRUTIVUE_STREAM_PROFILE_100=1 for 100%; the default is Fit.
```

`ready_a_streaming_b_actual_worker` separately tests the actual asynchronous worker
and event stream with ready A and a 2560x1440 Fit Diff view. It needs the pinned
`libjxl/` bundle beside the test executable, not just the explicit library path.
Use a new `SCRUTIVUE_STREAM_PROFILE_OUTPUT` path for each invocation.

## Rebuilt experimental executable

The release-profile build of `target/release/scrutivue-jxl-parallel.exe` succeeded,
SHA-256 `ef1c0bf493bb15ce6b67681fa1079a221e8f63be3a4ff7f0885b09a1160f2a9c`.
The preceding runner-only EXE and matching runtimes are retained under
`target/diff-one-ready-review/previous/`. The normal `target/release/scrutivue.exe`
was not rebuilt or replaced.

Validation after the fix:

- Normal feature-disabled tests: **148 passed, 8 ignored**; experimental tests:
  **152 passed, 9 ignored**. The ignored stage profiles and actual-worker case
  were additionally invoked explicitly as described above.
- The actual asynchronous **A-ready/B-empty** worker case at 2560x1440 Fit passed:
  first B progress 163.6 ms, B completion 4281.8 ms, zero Diff byte error and no
  duplicate A completion. These are optimized-debug worker measurements, not
  release executable or displayed-image timing.
- The rebuilt release passed the native fast/fallback decoding and embedded
  LPIPS CLI smoke. Its paired-worker matrix passed **10/10** original,
  progressive and mixed-tiling Slider/Diff cases, zero Diff byte error and
  unchanged source hashes. Reports: `paired-release/` in the review directory.
- Native no-capture UI smoke passed with **673 responsiveness probes, zero
  100 ms timeouts**, maximum 15.16 ms, including paired input, navigation,
  selection stress, statistics/picker, fullscreen and settings restart.
  Report: `gui-release-no-capture/result.json`; no desktop screenshots claimed.

The earlier 38-file full RGB16/JXL validation was not repeated for this
display-only change; the release paired matrix and independent preview/pixel
tests above cover the modified path. No remote build or release was updated.
