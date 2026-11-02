# Cache settings: local validation

Validated on Windows x64 on **2026-11-02**, against the uncommitted cache/settings implementation based on `8129c39e4d4340ea406b26df9aa511650d8b487b`. These are local development results, not an update to the existing 0.3.2 release or its GitHub draft.

## Automated checks

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| Default-feature tests, normal `scrutivue` target | 187 passed, 21 ignored |
| No-default-feature tests, normal `scrutivue` target | 175 passed, 17 ignored |
| Explicit native JPEG XL preview roundtrip | 1 passed |
| Explicit native GPU/CPU comparisons | 3 passed |
| Normal locked/offline release build | Passed |

The native JPEG XL test uses the pinned encoder at distance 0.03 / effort 3, checks developed sRGB16 roundtrip, rejects corrupt payloads and invalidates changed source revisions. The three GPU checks cover modes/filters/dimensions, mip/soft-pixel edges and panned Fit/reduced views. The ignored corpus/profiling tests were not all run; the 48-file comparison below provides separate final-frame evidence.

The release build has only the two existing unused DNG-helper warnings (`crop` and `unpack`). LibRaw, JPEG XL, ONNX Runtime and model pins were not changed.

## Native Settings integration

The final release executable was exercised through its real GPUI windows with an isolated settings file and test-owned cache locations. Inputs were one read-only 5520×8280 NEF and generated PNGs of 4000×3000, 3200×3125 and 800×600.

Verified:

- Separate Settings window, Apply, reopen and main-window shutdown.
- RAM/disk limits and location persistence, Prefetch and build-all toggles, and preference restoration after process restart.
- Closing Settings discards unapplied edits.
- Lazy generation of exactly three eligible disk previews, including the exact 10,000,000-pixel boundary; no display preview for the 800×600 image.
- Every generated preview has a 2560 px long edge and a sidecar matching its payload's SHA-256.
- Four independent disk thumbnails, including the small image.
- Background enforcement after reducing the disk limit to 1 MiB.
- Reset removes generated preview files while preserving unrelated files, the ownership marker and all thumbnails.
- Reset targets the applied location, not an edited but unapplied location.
- The source NEF's SHA-256, size and modification time remain unchanged.

In one warm-cache run, the cached preview was accepted by the UI at **954.1 ms**, before full decode at **1249.5 ms**. These are acceptance timestamps, not presentation/first-visible-pixel measurements or a statistical performance guarantee. The run recorded **91 WM_NULL responsiveness probes**, with **zero 100 ms timeouts**; this does not establish end-to-end input latency.

The final release integration used `--no-capture`: desktop obstruction prevented privacy-safe screenshots. Earlier debug screenshots were inspected and show the controls and pinned Apply/Close footer correctly; no new release screenshots are claimed.

## Authoritative-frame preservation

The final release `--probe` results were compared with `target/release-0.3.2/release-probe.stdout.json` for **48 distinct sources**: 6 NEFs, 36 DNGs and 6 JPEGs. All RGB16 SHA-256 hashes, dimensions, buffer sizes, sample sums and thumbnail dimensions matched. Source-file SHA-256, size and modification time were checked before and after; all sources remained unchanged.

## Build identity and isolation

Final executable: `target/release/scrutivue.exe`, **37,581,824 bytes**.

```text
SHA-256: 5d06d159b9c9625807d09413da1ef68f91e28dbe921d37ec4fe843428b620710
```

An initial incremental release build reused instrumented native objects from the separate sampling experiment. Its `AB_METRIC` output exposed the reuse. The final build explicitly reran the native build from the main checkout:

```powershell
$env:DIFF_LIBRAW_SOURCE = (Resolve-Path vendor/libraw).Path
cargo build --release --locked --offline --bin scrutivue
```

This points to the same pinned Redlamp LibRaw source; no fork, pin, native source or neighboring checkout was changed. The final EXE is checked for absence of the experiment's instrumentation marker, and the complete GUI/final-frame checks were repeated against that exact EXE. Experimental worktrees should use separate target directories to avoid this incremental-artifact reuse.

The previous normal 0.3.2 executable and matching assets were backed up under `target/cache-settings-implementation/previous-release-0.3.2-1793605951185656500/`. The new normal EXE retains matching `onnxruntime.dll`, `models/` and `libjxl/` assets beside it. The original 0.3.2 ZIP still has SHA-256 `de041265488f148e75fc79d9ff9b5e39950d5d00c7849f77f601a8560c2f70fc`.

No repository-root application binary was overwritten. No commit, push, tag, package replacement or GitHub release update was performed.

## Local evidence

All generated evidence is under `target/cache-settings-implementation/`:

- `validation.json`: final binary, input/runtime identities and aggregated results.
- `tests-default.log`, `tests-no-default.log`, `tests-native-jxl.log`, `tests-native-gpu.log`.
- `build-release-clean-native.log` and `native-build-inputs.json`.
- `gui-full-release-final/result.json`, `preview-headers.json` and `restart-trace.jsonl`.
- `trace-1793606980133444900.jsonl`: final release cache/decode/render acceptance trace.
- `final-frame-comparison.json`, `release-probe.stdout.json` and `source-identities-before.json`.
- `gui-full-debug-complete/`: earlier inspected Settings screenshots.

These local artifacts are not committed or packaged with the application. Cache behavior and standard validation commands are documented in [Settings and caches](cache-settings.md).
