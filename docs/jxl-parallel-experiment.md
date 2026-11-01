# Separate experimental libjxl-parallel executable

**Historical experiment:** promoted to the normal executable's default features
in [0.3.1](release-0.3.1.md). For a runner-only comparison today, use
`--no-default-features --features experimental-jxl-parallel --bin scrutivue-jxl-parallel`.
The opt-in/default-disabled descriptions and cropped-DNG measurements below refer
to the preserved 0.3.0 experiment, not current defaults or current DNG geometry.

The normal `scrutivue.exe` and the 0.3.0 draft release are unchanged. The opt-in
`experimental-jxl-parallel` Cargo feature enables a second local executable:

```powershell
cargo build --release --locked --features experimental-jxl-parallel --bin scrutivue-jxl-parallel
.\target\release\scrutivue-jxl-parallel.exe
```

Keep the matching `target/release/onnxruntime.dll`, `target/release/models/` and
complete `target/release/libjxl/` beside newly built executables, just as for the normal application. No additional
DLL, dependency download, LibRaw revision or codec version is introduced. Build
only the named target above: explicitly enabling the feature for the normal target
also enables the experiment in that target. The default build has no runner.

## What changes

The pinned libjxl 0.12.0 decoder receives `JxlDecoderSetParallelRunner` before its
first input-processing call. libjxl can then split **work inside a single JXL
tile** among workers. Existing parallelism across DNG tiles and the paired
Slider/Diff scheduler remain in place, including visible-first ordering, retained
partial tiles, progressive continuation, cancellation checks and final-frame
eligibility/fallback safeguards. Decode settings, output domain and RGB16 buffers
are unchanged.

The Rust adapter implements libjxl's public `parallel_runner.h` ABI. It calls
initialization exactly once on the caller's thread, supplies the actual Rayon
registry size, gives each callback that registry's worker index, and joins every
callback before returning. Initialization failures propagate to libjxl and Rust
panics are caught at the runner's C boundary. Each decoder remains exclusively
owned and movable between calls; no worker receives its handle.

When called from a Rayon worker, inner tasks reuse **that same pool**, not a
new pool per tile. Thus the viewer's existing six-worker tile budget on a
12-logical-CPU machine does not become six times six. Calls outside any pool
(for example, a single progressive session on its coordinator) share one lazy
process-wide pool of half the available logical CPUs, bounded to two through
six workers. That fallback is separate from existing service pools; this is not
a process-wide six-thread cap on rendering, metrics and all decoding together.

The initial runner-only experiment did not change viewport composition. The
subsequent [one-ready-source Diff fix](diff-streaming-performance.md) also
parallelizes the streaming difference preview. Neither change raises the
scheduler's six-worker limit, enables OpenMP in LibRaw, or changes the
single-retained-session policy for progressive sources. Files with few independent codec tasks may gain
nothing or become slower from scheduling and scratch-buffer overhead. More CPU
utilization is not by itself a performance result.

## Validation

Adapter tests cover initialization thread/count, exact once-only task ranges,
error/empty/invalid ranges, genuine callback overlap and nested decoders sharing
one bounded registry. A separate opt-in native test compares sequential and
parallel decoding of real first/middle/edge tiles at every available progressive
stage, including cancellation before work and moving a resumed session.

```powershell
cargo test --locked --features experimental-jxl-parallel --bin scrutivue
$env:SCRUTIVUE_JXL_TEST_LIBRARY = "$PWD/target/release/libjxl/jxl.dll"
$env:SCRUTIVUE_PROGRESSIVE_TEST_DNG = "D:/Photos/progressive-copy.dng"
cargo test --locked --features experimental-jxl-parallel --bin scrutivue native_parallel_runner_matches_sequential_tiles_and_progressive_stages -- --ignored --nocapture
python scripts/ci_smoke.py --exe target/release/scrutivue-jxl-parallel.exe
```

The corpus validators also accept `--exe target/release/scrutivue-jxl-parallel.exe`.
Generated binaries, timing records and read-only source-identity checks belong
under `target/jxl-parallel-review/`, not in a commit or the existing release.

## Initial runner-only results, 2026-11-01

The initial separate release build succeeded. Its binary/runtime bundle is now
preserved under `target/diff-one-ready-review/previous/`; the normal experimental
EXE path below will contain the newer Diff fix after rebuilding:

```text
target/release/scrutivue-jxl-parallel.exe
SHA-256: 2477012f7f78a7092ddce27d77b106421177cbd8bb1f6a0b5f2b9bc86a251387
```

- Experimental unit suite: **151 passed, 7 ignored**. Normal feature-disabled
  suite: **147 passed, 6 ignored**. Three explicit native test executions also
  passed: sequential/parallel progressive-stage comparisons on original and
  progressive tiles, plus the existing cross-thread progressive-continuation test.
- Native fast-completion/fallback CLI fixtures, statistics and embedded LPIPS
  passed with the experimental executable and existing pinned runtimes.
- The source corpus now contains **36** originals (six `halo128` files added
  since the earlier matrix), plus two existing lossy progressive derivatives.
  The old fixed-32 validator correctly stopped at its input-count assertion;
  a separate review harness checked the actual **38** inputs without modifying
  the old validator or any source image.
- All 38 finished through developed tiles, reused panned tiles and matched their
  independent legacy whole-frame development: **5,180,276,736 RGB16 components,
  zero differences**. The original 32 inputs also retained every archived
  sequential-decoder frame hash. Before/after source hashes, sizes and mtimes
  were unchanged. Records: `target/jxl-parallel-review/current-corpus/`.

Paired-worker times below are medians of three fresh-process repetitions on
Ryzen 5 5600 / 12 logical CPUs. Variants ran sequentially in alternating order,
with warm OS storage, a 512x512 viewport and no PNG encoding. These are times
until **both complete RGB16 frames**, not screen presentation times:

| Pair / mode | Shipped 0.3.0 | Experimental runner | Observed change |
| --- | ---: | ---: | ---: |
| Original 256x256-tiled DNGs / Slider | 4068.9 ms | 4250.3 ms | 4.5% slower |
| Progressive derivatives / Slider at Fit | 4516.7 ms | 3798.0 ms | 15.9% faster |
| Progressive derivatives / Diff at 100% | 2013.0 ms | 2011.1 ms | effectively unchanged |

First paired progress was 35.0 vs 34.7 ms for original Slider, 278.9 vs 264.6 ms
for progressive Slider/Fit, and 141.2 vs 135.9 ms for progressive Diff. The baseline
is the exact downloaded hosted 0.3.0 release; the experiment is built locally.
They are not an identical-toolchain causal benchmark of the feature alone. The
script also records whole-process CPU usage, which includes later reference
decoding and must not be presented as tile-stage utilization. All compared Diff
buffers had zero byte error. Records: `ab-measurements.json` and `ab-summary.json`
in the review directory. These results do not justify enabling the feature by
default for every file.

Native UI smoke was not uniformly clean: the first no-capture run stopped at
`Navigator continued after release`. A baseline control passed, and a second
experimental run, with unchanged code and inputs, passed the full suite with
**689 responsiveness probes, zero 100 ms timeouts**, maximum 9.16 ms. This does
not establish the cause of the first assertion or claim the issue was fixed.
The failing settings and passing repeat/control records remain under
`gui-no-capture/`, `gui-repeat-no-capture/` and `gui-baseline-control/`. No desktop
screenshots or end-to-end visible-image timings are claimed.

The normal local executable's SHA-256 remained unchanged. No root executable,
photograph, native dependency pin, remote Actions run, release asset, tag or
published release was changed; the experiment is uncommitted local source work.
