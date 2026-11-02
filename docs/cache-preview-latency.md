# Cached-preview latency: Nikon D4 follow-up

Local Windows x64 investigation on **2026-11-02**, following the [Settings/cache implementation](cache-settings-validation.md). The reported corpus is `E:\Documents\AI\corpus\309NC_D4`, containing 499 NEFs in its nested `309NC_D4` directory. The existing application cache contained all 499 JPEG XL previews (about 1.45 GB of payloads). Most source frames are portrait 3292×4940; their previews are 1706×2560.

This is a historical distance-0.03 development-build measurement. Version 0.3.3 changes the encoder policy to distance 0.05 and invalidates older preview identities; the timings below are not a new measurement of that policy.

## Cause

Cache hits worked, but the old display path decoded the JXL and then interpolated every physical viewport pixel on a two-thread CPU pool. At the tested 3276×2019 viewport, that second step largely erased the benefit of the preview. With full-frame Prefetch enabled, authoritative frames often finished before the preview's viewport render.

An isolated stage profile on `ND4_4516.NEF`, using the existing pinned native runtime and the normal two-thread preview budget, measured:

| Stage | Approximate time |
| --- | --- |
| Read cached JXL | 1 ms |
| Payload SHA-256 | 1.1 ms |
| Decode JXL | 68 ms |
| Old CPU viewport interpolation | 640 ms |
| New bounded source-texture preparation | 8.8 ms |

The stage timings are from optimized test builds, not end-to-end presentation measurements. Increasing thread count helped the old loop but was not the fix: the production preview pool remains bounded at two threads.

## Fix

`src/cache.rs` now converts the decoded sRGB16 preview to a bounded BGRA8 source texture on its worker. GPUI scales, positions and clips that texture directly on its rendering device. There is no intermediate viewport-sized CPU raster for a cached preview. A complete partner frame in a mixed ready/cached pair is reduced to the same 2560 px texture bound; small ready frames are not enlarged.

`src/ui.rs` distinguishes source textures from prepared viewport textures. `Camera::source_transform` in `src/model.rs` positions previews using original image geometry, so rounded preview dimensions do not move image features. Normal full-frame viewport rendering is unchanged. Full RAW decoding still runs, and Diff, High pass, Statistics and Color picker still require authoritative RGB16 frames.

For this display-path-only fix, existing disk previews remained valid; no cache reset, re-encoding, pin/dependency change or source-image modification was needed. The later 0.3.3 encoder-policy change separately requires lazy regeneration.

## Native before/after checks

Separate native application runs used isolated profiles copied from the user's settings, without changing that profile. Each run opened the corpus, waited for startup, jumped to the final file, then selected five preceding files. Each navigation sample selected a distinct source not previously viewed in that process. The OS file cache was warm. The table excludes startup and reports six navigation samples per row.

| Configuration | Before: first-view median | After: first-view median | After range |
| --- | ---: | ---: | ---: |
| Disk cache, Fit, Prefetch/build-all off | 758 ms | 99 ms | 91–112 ms |
| Disk cache, Fit, Prefetch/build-all on | 638 ms | 134 ms | 99–154 ms |
| Disk cache, 400%, Prefetch/build-all off | Not measured | 95 ms | 83–100 ms |
| Disk cache disabled, Fit | 868 ms | 887 ms | 854–910 ms |

The cached view arrived before the complete frame in all six post-fix disk-cache navigation samples in each tested configuration. Before the fix, the Prefetch/build-all run accepted a cached preview in only one of six samples; the others waited for the full frame. The complete-view median remains about 0.9 s without Prefetch, or 0.6 s with Prefetch: this change accelerates **first display**, not RAW entropy decoding/development.

These are GUI acceptance timestamps, not verified physical first-pixel presentation times or a universal 100 ms latency guarantee. Native runs used `--no-capture` because privacy-safe desktop capture was unavailable. All WM_NULL responsiveness probes in the measured runs completed within the 100 ms timeout.

## Correctness and build

- Default tests: **190 passed / 22 ignored**; no-default tests: **178 passed / 18 ignored**.
- Explicit native JPEG XL roundtrip, three GPU/CPU suites and the new cache stage profile passed.
- New tests exhaustively check RGB16→BGRA8 quantization, bound source-texture allocations independently of viewport size, and check original-geometry positioning across Fit/zoom/pan/resize. Existing exact-tool and preview-corruption safeguards remain covered.
- Seven NEFs (the first and final six corpus files) retain identical final RGB16 SHA-256 hashes, dimensions, sizes, sums and thumbnail dimensions compared with the prior EXE. Those seven source-file hashes remain unchanged; size/mtime checks remain unchanged for all 499 source files. The user's preferences remain unchanged.
- The full native Settings integration was repeated against the new release EXE: Apply/restart persistence, lazy preview building, threshold, all-file thumbnails, budget enforcement and safe reset passed. Its cached preview was accepted before full decode, with 63 responsiveness probes and no 100 ms timeouts.
- Formatting, locked/offline release build and matching runtime/model assets were verified. Only the existing unused `crop`/`unpack` warnings remain.

Normal release executable: `target/release/scrutivue.exe` (**37,585,408 bytes**), SHA-256:

```text
02cd276a90286e21df5638e3352164904e01e004d8aa75f840e4e8d3ac8dcb72
```

The prior EXE and matching assets are preserved in `target/cache-preview-latency/before-build/`. No repository-root binary was overwritten, and no commit, push, tag or GitHub release update was performed.

Local scripts, traces, stage profiles, native run results, frame probes and `comparison.json` are under `target/cache-preview-latency/`. They are generated evidence, not committed application assets. The ignored stage-profile test can be repeated with `SCRUTIVUE_CACHE_PROFILE_SOURCE`, `SCRUTIVUE_CACHE_PROFILE_PARENT` and `SCRUTIVUE_CACHE_PROFILE_OUTPUT`, then `cargo test --locked --offline --bin scrutivue native_cached_preview_stage_profile -- --ignored --nocapture`.
