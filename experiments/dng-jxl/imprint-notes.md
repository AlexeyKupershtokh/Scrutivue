# Imprint native C++ support-boundary experiment

## Scope and provenance

- Imprint source: [`3916a114bc32b487de611135cf66514447c541a6`](https://github.com/luqie0106/Imprint/tree/3916a114bc32b487de611135cf66514447c541a6/native-raw).
- The upstream CMake file independently pins Redlamp LibRaw commit
  `4abfcd2ab82f2211b1f7e7c3cd83974c87e0d812`, archive SHA-256
  `2ca1d9673531fccabcc7c8bce589444163868bcde625aa1f09eb442df1bcce2d`.
  CMake downloads and verifies that dependency inside this experiment's build.
- This is **not** the application's LibRaw commit. The application pin
  `f4956c404fba0c6035ce9e06353d4b4ffe068fc7`, its configuration, vendor sources,
  neighboring checkouts and application binaries are not changed or substituted.
- Only the pinned `native-raw/` subtree and relevant upstream notices/licenses
  are downloaded from Imprint, not its large research/project data. Git blob IDs
  are checked against the immutable commit's tree. `sources.json` records URLs,
  Git blob IDs and SHA-256 hashes. Original copyright/license files remain under
  the ignored output directory; the native library is LGPL-2.1.
  This download contains 16 files totaling 147,948 bytes.
- All downloads, native build products, temporary input copies and generated
  results stay under `target/dng-jxl-imprint/`.

## What the source actually exposes

This is a **C++ TicoRAW decoder for Nikon HE/HE\* NEFs**, not a JPEG XL codec.
Imprint's own C++ entropy decoding, inverse lifting and CPU Bayer reconstruction
replace selected LibRaw TicoRAW computation source files. LibRaw still supplies
NEF parsing, metadata and RGB rendering. Metal is Apple-only; this experiment
sets `IMPRINT_TICORAW_BACKEND=cpu` explicitly. Python performs orchestration and
`ctypes` C API calls only; there is no Python image codec or rawpy fallback.

At this commit:

- `imprint_raw_probe_nikon_he` constructs a LibRaw decoder and calls `open_file`
  only. Selection requires **Nikon NEFCompression 13/14** and decoder name
  `nikon_he_load_raw()`. TIFF Compression 34713 alone is not this selection rule.
- `imprint_raw_open` combines construction, header/container reads, `unpack`,
  metadata extraction and internal allocation. It is not a metadata-only open.
- `imprint_raw_copy_visible` copies decoded CFA samples into caller-owned storage.
- `imprint_raw_postprocess` combines LibRaw processing with creation of its owned
  RGB memory image. Here the requested render is half-size, camera-space,
  linear RGB16 with camera white balance and no auto brightness.
- The public API does **not** split constructor, header reads, compressed reads,
  entropy decode, inverse transform or internal allocation. No such separate
  timings are inferred. Only actual C API wall time and explicitly measured
  caller allocation/copy time can be reported.

The README validates a local Z6 III HE\* profile and explicitly says other
bodies/profiles need fixtures. Successful metadata selection of a Z9 does not
guarantee successful full-image decoding.

## Reproduction

Use existing Windows Visual Studio 2022 Build Tools and its installed CMake;
the script locates them using `vswhere` and `vcvars64.bat`. It installs nothing
and does not run an application release build.

```powershell
# Download the minimal pinned sources, build the native C++ Release DLL,
# and test the two existing CC0 Nikon Z9 NEF boundary controls.
python experiments/dng-jxl/imprint_probe.py --action all

# Probe exactly the supplied DNG pair and the controls. Supply private paths
# through environment variables; do not add them to public documentation.
python experiments/dng-jxl/imprint_probe.py --action probe --runs 1 `
  --input $env:SCRUTIVUE_PROFILE_PAIR_A --input $env:SCRUTIVUE_PROFILE_PAIR_B `
  --input target/fixtures/raw/Z9_3x2_HE_be91bac2.NEF `
  --input target/fixtures/raw/Z9_3x2_HEstar_e113deb3.NEF
```

The actual configure and build command arrays, tool version, exit status and
logs are recorded in `target/dng-jxl-imprint/build.json`. The upstream source
and CMake file are not patched. CMake's only requested library target is
`imprint_raw`, configuration `Release`, architecture `x64`, parallelism `4`.
A tiny generated C++ executable checks the native public-struct sizes against
the `ctypes` declarations before any image API call; it is not an image codec.

The C API accepts `char*`; an input with a non-ASCII Windows path is copied to
an ASCII path under this experiment's output directory and SHA-256 checked.
This avoids confusing an ANSI filename failure with a codec rejection. Images
are opened read-only; source size, modification time and SHA-256 are checked
before/after. Full private local paths occur only in ignored generated JSON.

Timings are warm-cache observations: source hashing and any necessary copy
happen before native calls, and no filesystem cache is flushed. DNG rejection
or a failed NEF decode is **not a successful image-decode throughput result**.
Any successfully produced CFA/RGB hash is an observation, not a correctness
claim against an independent reference decoder.

## Windows build and observed results

**The unmodified native Windows x64 Release build succeeded. The actual JPEG XL
DNG pair was rejected by the NEF-only API gate. Both CC0 Z9 NEF controls decoded
and rendered successfully using `c++/cpu`.** This is not a DNG decoder speedup.

Build environment: Visual Studio Build Tools 2022 `17.5.33516.290`, MSVC
`19.35.32216.1`, CMake `3.25.1-msvc1`. Configure took 8.80 s and the native
Release library build took 52.04 s. Both exited zero; no source patch, tool
installation, application build or application binary replacement was needed.
The upstream `ZERO` macro redefinition produced one nonfatal C4005 warning.
Optional ZLIB was not found; no replacement dependency was installed.

For contention avoidance, the native build's approximate UTC window was:

| Step | Start UTC | End UTC |
| --- | --- | --- |
| Configure/dependency fetch | 2026-10-31 19:15:17.765 | 2026-10-31 19:15:26.567 |
| Native Release compilation/link, parallelism 4 | 2026-10-31 19:15:26.568 | 2026-10-31 19:16:18.609 |
| Tiny ABI executable compilation/link | 2026-10-31 19:16:18.627 | 2026-10-31 19:16:18.727 |

These historical windows are reconstructed from each Windows log file's
creation time plus its recorded wall duration, not contemporaneously captured
start/end clock readings. They are also saved with that qualification in
`build.json`. Future script runs record actual UTC start/end timestamps.
No further native build was performed after this window.

The CMake-downloaded `archive.tar` was independently rehashed after the build
and matched the pinned LibRaw archive SHA-256 above. Native ABI sizes were
64 bytes (`sizes`), 824 bytes (`info`) and 72 bytes (`params`), matching ctypes.
Built `build/Release/imprint_raw.dll` SHA-256:

```text
5523606c669ad499ab1c8ee7571a638d4651d1d42e2fa489ccbc874ba6e8eb06
```

### Actual JPEG XL DNG inputs: unsupported by Imprint's NEF entry point

| Input | Native metadata probe | Native open |
| --- | --- | --- |
| `AK8_0005_halo0.dng` | return 0, `is_nikon_he=false` | return 1, rejected before unpack |
| `AK8_0005_halo24.dng` | return 0, `is_nikon_he=false` | return 1, rejected before unpack |

Both open errors were exactly `NEF Nikon NEFCompression is not 13 or 14`.
LibRaw's metadata open succeeded; the Imprint wrapper's explicit NEF gate then
rejected the files. This is **not** a failed JPEG XL entropy decode, and the gate
was not removed or bypassed. No CFA, RGB image or DNG pixel-decode timing exists
for either DNG. The sub-millisecond probe/rejection times in `probe.json` are
only metadata/rejection overhead, not image loading or decoding performance.

### Different NEF inputs: support-boundary controls only

The final recorded run used the existing CC0 fixtures below. These timings are
one warm-cache observation per fixture, not medians or physical-disk timings.

| Control | Nikon NEFCompression | Probe ms | Open + unpack ms | Half-size render ms |
| --- | ---: | ---: | ---: | ---: |
| `Z9_3x2_HE_be91bac2.NEF` | 13 | 0.407 | 1180.512 | 216.953 |
| `Z9_3x2_HEstar_e113deb3.NEF` | 14 | 0.360 | 1327.507 | 214.889 |

All calls returned zero, and each handle reported `c++/cpu`. Each visible CFA
was 8280 × 5520 samples; each RGB16 render was 4140 × 2760 × 3 channels,
68,558,400 bytes. The 8256 × 5504 inset crop was reported as metadata, not
manually applied by this harness. These half-size camera-space renders are
not the application's full-size, oriented/cropped DNG rendering workload.

Explicit caller-side zeroed CFA allocations took 11.106/11.424 ms, native CFA
copies 8.654/8.646 ms, and caller RGB-buffer copies 8.364/8.440 ms (HE/HE\*).
These are separate measured operations, **not** the native decoder's hidden
allocation costs. Source SHA-256, size and modification time remained unchanged
for all four images.

Observed CFA SHA-256 hashes:

```text
HE:  d6540f56eed3c5ea46425be9823591289e6461aeb0f8824a2c9342182c768206
HE*: 4fe6c59e56b3b9b148d6a06e3542b19be78b14e82e6df9ce7eef62ca4a97abfa
```

The two successful NEF controls establish runtime coverage for these particular
Z9 files, not all Z9/Z8 profiles. No private Z8 NEF was searched for or tested.
Full diagnostics, render hashes, input hashes, actual probe command and private
local paths are in ignored `target/dng-jxl-imprint/probe.json`; build commands
and tool/dependency provenance are in `build.json` and `sources.json`.
