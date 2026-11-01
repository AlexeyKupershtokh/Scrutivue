# Isolated DNG/JPEG XL experiments

These are developer command-line programs, **not a production decoder change**.
The application, its pinned Redlamp LibRaw, root binaries and source photographs
remain unchanged. Build products, downloaded experiment sources and measurements
belong under `target/`. No proprietary runtime is redistributed.

## Programs

| Program | Purpose |
| --- | --- |
| `dng_jxl_bench.exe --backend production` | The current `src/dng.rs` transport, followed by the exact pinned LibRaw development/crop. |
| `dng_jxl_bench.exe --backend oxide` | Instrumented jxl-oxide transport: separately time full-tile sample streaming and canvas assembly. |
| `dng_jxl_bench.exe --backend libjxl` | Public native libjxl C ABI, one serial decoder per tile, bounded outer parallelism. |
| `dng_jxl_bench.exe --backend libjxl --reuse` | Reuse a decoder and initialized tile output per worker. |
| `dng_io_bench.exe buffered\|mmap\|direct` | Separate buffered reads, read-only mapping/prefault and Windows unbuffered reads. |
| `tico_probe.exe` | Ask the existing hash-pinned intoPIX/IPX recognizer about an actual JXL tile; **not a JXL decode**. |
| `imprint_probe.py` | Build the requested pinned Imprint C++ library and call its native CPU API; Python only orchestrates. |

Full findings, timing tables and limitations: [DNG/JXL experiment report](../../docs/dng-jxl-experiments.md).

## Build

Windows x64, existing MSVC Build Tools and optimized Rust dependencies are needed.
After the application's ordinary preparation and `cargo test --locked`:

```powershell
python scripts/build_dng_experiments.py
$env:DNG_JXL_LIBRARY = 'D:\Tools\pipeline-graph\resources\native\libjxl-0.12.0\jxl.dll'
python scripts/build_dng_experiments.py --tests
```

The builder compiles only standalone experiments, not the application. It reuses
the existing exact pinned LibRaw archive and Rust `.rlib` files. The harness is
compiled with `rustc -C opt-level=3`; the reused Cargo debug dependencies were
compiled at `opt-level=2`, so this is **not an all-release-build measurement**.
The generated ignored module copies the current production DNG source, normalizes
only inner comments/test-asset paths, and adds `transport.rs` inside that module.
Thus the TIFF parser, crop math and production baseline come from the real source,
without adding an experimental API or native dependency to the application.

The native library path is explicit and must be absolute/trusted. DLL loading
executes native code; this is not a sandbox. Neither helper nor decoder is
approved for untrusted/hostile images by these tests. The libjxl binding accepts
0.11/0.12 ABI versions and rejects unexpected dimensions, extra channels,
animation, incomplete input and unrepresentable original color profiles.

## Measure

Set the two inputs to existing photographs; these examples are illustrative:

```powershell
$a = 'D:\Photos\halo\AK8_0005_halo0.dng'
$b = 'D:\Photos\halo\AK8_0005_halo24.dng'
$jxl = 'D:\Tools\pipeline-graph\resources\native\libjxl-0.12.0\jxl.dll'
$ipx = 'D:\Tools\pipeline-graph\resources\native\ipx\dxo-6.8.0.7\DxOFramework.dll'
python scripts/benchmark_dng_jxl.py --runs 3 --threads 1,6 --pair `
  --library $jxl --tico-framework $ipx $a $b
```

This creates a new `target/dng-jxl-results-*` directory containing complete
`results.json`, median `summary.csv` and a few extracted representative tiles.
All inputs are read-only. Existing output directories are refused. Source hashes
are checked before/after; every successful codec result is checked against the
actual production canvas and final RGB16 hashes. Timing cases are sequential,
except the explicitly labeled paired experiment (two processes, three workers
each). Rotated backend ordering reduces systematic first/last-run bias.

For individual invocations:

```powershell
target/dng-jxl-experiments/dng_jxl_bench.exe --backend libjxl --library $jxl --threads 6 --reuse $a
target/dng-jxl-experiments/dng_jxl_bench.exe --backend oxide --threads 6 --transport-only $a
target/dng-jxl-experiments/dng_io_bench.exe direct $a
```

Optional `--raw-output NEW_FILE` / `--rgb-output NEW_FILE` write validation buffers
outside the timed pipeline. They use create-new semantics and never overwrite.
`--extract-tile NEW_FILE --tile-index N` extracts the original compressed tile for
an independent compatibility probe; extraction is not decoding.

## Timing boundaries

- **I/O:** open/stat, capacity reservation, zeroing/first-touch, buffered read;
  or mapping plus explicit 4 KiB page touches. `direct` uses an aligned buffer and
  `FILE_FLAG_NO_BUFFERING`; it bypasses the Windows file cache, **not drive/controller
  caches**. No privileged cache flush or claim of mechanically cold disk is made.
- **Transport setup:** metadata parsing, virtual-TIFF capacity reservation,
  compressed-source copy and separate sample-canvas zeroing/first-touch.
- **Codec:** header/setup, reconstruction, sample conversion, row assembly and
  cleanup. `parallel_tiles_ms` is wall time; `workers.*_ns` are sums of worker
  elapsed intervals, **not OS CPU time**. Do not add worker sums to wall time.
- **Development:** LibRaw open, unpack, processing, oriented bitmap creation,
  crop geometry, final RGB reservation, row copy/first-touch and native cleanup.
  Internal LibRaw allocations remain inside those calls; they are not invented
  as separately measured allocator times.
- **Validation:** source/canvas/final-RGB hashes and optional exports are outside
  `pipeline_ms_excluding_setup_validation`. Native library loading and thread-pool
  creation are also separate. `process_wall_ms` includes them all.
- **Memory:** OS peak working set plus per-stage RSS/private-byte checkpoints;
  parent sampling tracks private bytes approximately every 2 ms. A sampled peak
  can miss a shorter spike. Paired wall time includes child startup, hashing and
  cleanup; its two 3-worker pools are not the application's one shared pool.

`--trace-alloc` installs the public libjxl memory-manager callbacks and reports
malloc/free elapsed time, requested bytes and call counts. This is a **separate
instrumented run**, with clock/counter overhead. Allocation time is nested inside
codec timings; requested cumulative bytes are not retained memory or peak RSS.
In reuse mode final destruction occurs at worker drop, outside per-tile counters.

The instrumented oxide case streams a full tile into bounded scratch before row
assembly; production streams rows directly. It is a useful stage decomposition,
not a claim that the two are byte-for-byte identical implementations. Synthetic
tests and actual-image hashes check that their **outputs** are identical.

## Other candidates

- [intoPIX/IPX probe, pins and safety boundaries](tico-notes.md)
- [Imprint C++ build, provenance and support-boundary results](imprint-notes.md)

The supplied intoPIX bridge and Imprint API target Nikon HE/HE* **NEFs**, not
JPEG XL DNG tiles. Rejection is an incompatibility finding, not a fast decode.
Do not compare a different NEF's CFA or half-size render time with full DNG RGB16.
