# Experimental progressive NEF preview

**0.3.2 status:** progressive NEF preview is now included in the normal
`scrutivue.exe` through default features. The implementation and limitations below
still apply. `--no-default-features` provides the non-progressive comparison build.
See [0.3.2 release notes](release-0.3.2.md) for the normal release package.

## Original isolated experiment (before 0.3.2)

This originally opt-in experiment follows the full-frame NEF optimizations shipped in
[0.3.1](release-0.3.1.md). It is not a replacement for the release executable.
The feature is `experimental-nef-progressive`; build only its separate target:

```powershell
cargo build --release --locked --offline --features experimental-nef-progressive --bin scrutivue-nef-progressive
.\target\release\scrutivue-nef-progressive.exe 'D:/Photos/example.NEF'
```

Keep the matching `onnxruntime.dll`, external `models/` and pinned `libjxl/`
beside this EXE. Existing normal/comparison EXEs, source photographs, dependency
pins and the 0.3.1 draft release are not replaced by this experiment.

## What becomes progressive

Full CFA unpacking, black calibration, global maximum/normalization, white
balance and channel scaling still finish first. Developing independently
normalized regions would risk brightness changes and seams. This is **not**
viewport-only entropy decoding or a reduced-resolution RAW decoder.

After those global stages, bilinear development finishes 256-sensor-row bands
in center-priority order. Each band independently copies camera-to-sRGB,
gamma-mapped, oriented RGB16 samples onto the globally aligned 1:4 grid. These
samples equal the corresponding authoritative final samples; the displayed
preview nevertheless has reduced spatial resolution and uses nearest sampled
values. Missing regions have a checkerboard. Final full-resolution rendering
replaces the preview without changing the ordinary final development path.

Only the existing guarded Nikon HE selector-13 acceleration path emits bands.
Other profiles retain the ordinary complete-frame fallback. Single and Slider
can display previews; Diff and High pass wait for complete frames. Statistics,
pixel inspection and the authoritative frame cache never receive partial data.
A completed A can remain available while B is loading, with independent source
geometry and the existing normalized alignment policy.

## Scheduling and safety

- The existing six-worker maximum decode budget remains shared by selected A/B.
  One extra preview worker uses two render threads, not a pool per band/source.
- Nonblocking queues hold at most eight sampled patches and two rendered
  snapshots. Slow preview consumers may drop bands; final frames fill all holes.
- At most two selected sampled canvases are retained, each capped at 8 Mi sampled
  pixels. No extra full-resolution RGB16 preview canvas is allocated.
- Requests coalesce; preview work checks generation, path, photo revision token
  and flight cancellation. Source identity is rechecked off the UI thread before
  every emitted band and again before publishing/caching the full frame.
- Superseded jobs/frame references are released off the UI thread. A decode
  failure revokes queued preview snapshots and the UI removes its partial view.
- The exact Redlamp fork and its vendor sources remain unchanged. New adapters
  retain their existing CDDL/LGPL attribution; no additional codec DLL is needed.

## Validation and measurement

The read-only six-NEF native parity test checks **2,856,600 sampled pixels per
file**, complete coverage exactly once, and complete final RGB16 equality. All
six files pass. A separate callback-cancellation test stops after the first band
and confirms that no partial `Frame` is returned. Deterministic tests cover
all eight native output flips, bounded/malformed patches, callback ownership,
cancelled flights, changed photo revisions, completed-source protection, masks,
letterboxing, ready-A/partial-B geometry and snapshot revocation.

Current unit suites: 172 passed / 19 optional ignored with the experiment;
165 / 17 with normal defaults; 160 / 15 without default features. The two
actual-corpus progressive tests were also explicitly run and passed.

Native isolated observations gave first sampled bands at 500–540 ms, versus
927–955 ms for complete RGB16. They are not visible-image times.

### Repeated release measurements (2026-11-02)

Ryzen 5 5600, RTX 3070 / DX12, 1280x720 Single Fit, three separate processes per
file, warm OS cache and no competing build/benchmark. The real asynchronous
Services/preview workers report these medians:

| NEF | First preview buffer | Complete RGB16 | Final DX12 buffer |
| --- | ---: | ---: | ---: |
| `AK8_0005.NEF` | 478 ms | 879 ms | 963 ms |
| `AK8_0036.NEF` | 487 ms | 887 ms | 967 ms |
| `AK8_0149.NEF` | 479 ms | 885 ms | 971 ms |
| `AK8_0988.NEF` | 484 ms | 879 ms | 958 ms |
| `AK8_1588.NEF` | 485 ms | 889 ms | 972 ms |
| `AK8_1714.NEF` | 485 ms | 883 ms | 962 ms |

All 18 complete RGB16 hashes and geometries match the installed 0.3.1 baseline;
all six source hashes/sizes/mtimes remain unchanged. GPU startup, separately
reported at 342–348 ms, is excluded from the three columns. This establishes
about 0.4 s earlier preview availability, not a claim of faster final decoding.

Three additional native GUI starts on `AK8_0005.NEF` recorded actual `Desk`
acceptance: median **542 ms first preview**, **930 ms full decoded**, **1023 ms
final render accepted**, relative to the initial selected-source request. Four
previews were accepted before the final frame in every run. Startup can overlap
decoding/thumbnail work here; these timings are not the isolated Services timer.
422 event-loop probes had zero 100 ms timeouts (maximum 18.03 ms).

The available desktop's PrintWindow captures were black, so visible screen
pixels/presentation could not be validated. The GUI trace proves acceptance by
the actual window state machine, not first visible-pixel latency. Check the
preview-to-final appearance interactively before promoting this experiment.

### Reproduction

```powershell
.\target\release\scrutivue-nef-progressive.exe --nef-preview-probe 'D:/Photos/example.NEF'
python scripts/validate_nef_preview.py --exe target/release/scrutivue-nef-progressive.exe --corpus 'D:/Photos/six-NEFs' --baseline target/nef-031-diagnosis/probe-0.json --output target/nef-progressive-review/new-release-probes
```

The baseline is a saved normal-0.3.1 `--probe` JSON. The validation driver uses
three fresh processes per source, compares every final hash/geometry against
that baseline, checks source hash/size/mtime before and after, and refuses to
overwrite its output directory. Timings exclude GPU startup (reported
separately), GPUI texture upload and screen presentation; OS caches are not
flushed. Do not equate these timers with cold-start or first-visible-pixel time.

For optional GUI acceptance timestamps, set `SCRUTIVUE_NEF_UI_TRACE` to a fresh
JSONL filename before starting the experimental EXE. A bounded diagnostic channel
writes off the UI thread, recording requests, accepted previews, full decoded
frames and accepted final render buffers. It does not observe GPU presentation.

Local evidence lives under `target/nef-progressive-review/`. Previous binaries
and runtime/model assets are preserved under its `previous/` directory.

## Portable package and remaining GUI caveats

The release build succeeded (only the two existing unused DNG helper warnings).
The separate package is
`target/nef-progressive-review/Scrutivue-NEF-preview-experimental-windows-x64.zip`.
Extract its entire `Scrutivue-NEF-preview` directory and run
`scrutivue-nef-progressive.exe`. It deliberately contains no normal-named
`scrutivue.exe`, so do not copy it over a release installation.

The extracted spaced/Unicode-path installation passed a real early-preview
probe with the 0.3.1 final hash. Standard package validation also passed native
decoding, exact DNG fallback/completion, statistics, external LPIPS and its
missing/corrupt/working-directory-decoy isolation. The final ZIP preserves those
same payloads while making its folder, executable and README experimental.
The CPU renderer also produced early snapshots and the unchanged final hash.
All previous release/root EXEs and runtime/model/JXL asset hashes are unchanged;
the full external model payload is absent from the new EXE.

The extended 2x-DPI native GUI smoke passed its instrumented rerun: **718 probes,
zero 100 ms timeouts, maximum 16.29 ms**. It exercised early Slider/Diff/Slider,
selection stress, divider/pan/navigator gestures, fullscreen, statistics/picker
input, English Help and settings restart. Its trace includes accepted paired
previews before authoritative completion.

However, two earlier runs failed different input/persisted-state assertions:
one restored a different camera zoom after restart; another did not move the
early Slider divider. A preserved normal-0.3.1 control run passed. The failures
were not reproduced in the instrumented run, but their cause is **unresolved**;
do not describe the broad GUI smoke as consistently passing or claim these were
proven harness-only issues. Together with unavailable screen captures, this is
why the feature remains an explicitly separate experiment rather than being
enabled in the normal application.

The original isolated experiment did not commit, push, dispatch a remote build
or update 0.3.1. Later source publication and promotion are tracked in 0.3.2.
