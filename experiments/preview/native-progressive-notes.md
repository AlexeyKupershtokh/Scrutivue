# Native JPEG XL progressive capability probe

**Follow-up correction:** `djxl --downsampling` uses this same public progressive
mechanism with `kLastPasses` and only one selected flush. The earlier all-flush
timings below are diagnostic, not a fair preview-policy comparison. The
[implementation trace and actual-tile repeat checks](../../docs/jxl-downsampling-review.md)
verify useful pass skipping on progressive controls, but no partial output on
the 90 actual Modular/single-pass DNG tiles. Full-sized output buffers and avoided
decoding passes are separate questions; the former does not imply the latter is
impossible.

## Scope and API evidence

This is an isolated experiment, not an application decoder or a new LibRaw
configuration. It uses the caller-selected trusted libjxl DLL and only public C
exports. The observed DLL reports decoder version `12000` (0.12.0).

The version-pinned public headers establish the distinction:

- [`decode.h`, `JXL_DEC_FRAME_PROGRESSION`](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/include/jxl/decode.h#L305-L325)
  explicitly says the default DC event contains 1:8 detail **upscaled to full
  image resolution**, and that some images never produce progressive events.
- [`JxlDecoderSetProgressiveDetail`](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/include/jxl/decode.h#L1412-L1434)
  with `kPasses = 3` requests all implemented pass steps. The intended detail
  ratio can be 8, 4, 2 or 1; it does not select the output dimensions.
- `JxlDecoderFlushImage` writes the current approximation into the registered
  full-resolution output buffer. Flush failure means no preview was produced;
  it is not necessarily a fatal decoder error.
- A bounded search of this pinned public decoding header found no
  `JxlDecoderSetDesiredDownsamplingRatio` or caller-requested ROI/crop API.
  Frame crop metadata and disabling coalescing are not arbitrary ROI decoding.
  An output callback can discard pixels outside a rectangle, but that alone
  does not establish reduced entropy decoding or reconstruction work.

The probe subscribes to basic info, color, frame, progression and full-image
events. It uses initialized, dimension-bounded RGB16/gray16 buffers, stored
orientation and the same original-profile semantics as `JxlWorker`. XYB
requires its original structured RGB/gray profile; ICC-only XYB is rejected
instead of silently using display sRGB. Alpha, extra channels and animations
remain unsupported. An embedded JXL preview, if present, is reported in basic
info but is not decoded by this progressive probe.

`ReleaseInput` is **not** a read-only position query. At every progression the
probe releases input to measure its unconsumed suffix and immediately restores
that suffix with `SetInput`. It deliberately never calls `CloseInput`, which
would forbid that restoration. With no box-event subscription this is permitted
by the public API. Final success still requires a full-image event; truncated
input fails rather than becoming an accepted preview/full result.

## Real DNG tile results (2026-11-01)

The inputs below are the already-extracted source-compressed tiles, not
re-encodings. Both source DNGs are 8280x5520 with 726 256x256 tiles. Every tested
tile reports 16-bit RGB, original-profile samples, no alpha/extra channels,
no animation and **`have_preview = false`**.

| Extracted tile | Tile index | Progression ratios | First progressive preview | Full decode median ms |
| --- | ---: | --- | --- | ---: |
| `0-first.jxl` | 0 | none | unavailable | 8.450 |
| `1-tile-363.jxl` | 363 | none | unavailable | 7.893 |
| `2-tile-725.jxl` | 725 | none | unavailable | 7.012 |
| `3-first.jxl` | 0 | none | unavailable | 8.242 |
| `4-tile-363.jxl` | 363 | none | unavailable | 7.943 |
| `5-tile-725.jxl` | 725 | none | unavailable | 7.057 |
| `assets/tests/dng-jxl-tile.jxl` | synthetic lossless 16x16 | none | unavailable | 0.179 |

Medians are nine warm repetitions after one recorded first invocation, one
native thread, resident compressed input, fresh decoder/output per call, no
UI/GPU or application release build. Timings include export lookup, decoder
creation, header processing, initialized output allocation and decoding up to
the full-image event. They exclude file reads, report construction, destruction,
hashing and the independent baseline decode. This is not a whole-DNG benchmark.
Absence of progression is a valid capability result, not a probe failure; these
samples do not prove that every tile/encoding has the same property.

All seven inputs matched the independent `JxlWorker` final output exactly, both
sample-by-sample and by canonical RGB16 little-endian SHA-256, on the recorded
invocation and all nine repetitions. `preview_only` requests for maximum detail
ratios 8, 4, 2 and 1 all fell back to the complete exact image for these inputs.
Each 256x256 tile still requires 393216 output bytes.

**Implication for the actual sampled DNG encodings:** this API offers no
observed half/quarter preview stage. A full decode followed by display
downsampling must not be described as native scaled decoding. DNG embedded
previews and decoding only the required DNG tiles are separate capabilities.

## Deterministic wide synthetic control

`encode_synthetic(library, width, height, progressive)` uses only public encoder
functions and deterministic RGB16 gradients plus bounded spatial texture. It
does not read photographic input or write files. It enforces VarDCT, distance
1.0, effort 3, linear BT.2100/D65 and no parallel runner. Bounds are 16384 per
dimension and 64 megapixels total. Buffer growth and output length are checked.

The option IDs come from pinned public
[`encode.h`](https://github.com/libjxl/libjxl/blob/v0.12.0/lib/include/jxl/encode.h):
`EFFORT=0`, `MODULAR=11`, `PROGRESSIVE_AC=17`, `QPROGRESSIVE_AC=18`,
`PROGRESSIVE_DC=19`, `OUTPUT_MODE=40`. Spectral AC is explicitly enabled or
disabled according to the flag; quantized AC and additional DC frames are
disabled. Output mode 0 preserves normal progressive-compatible ordering.
The 0.12-only output-order option is not set for a 0.11 library.

Generated ignored controls are 4096x3072 (192 256x256 spatial groups):

| Control | Compressed bytes | Observed progressive ratios | First preview median ms | Full with all flushes median ms | Ordinary baseline median ms |
| --- | ---: | --- | ---: | ---: | ---: |
| Spectral AC disabled | 834196 | 8 | 150.061 | 274.316 | 168.846 |
| Spectral AC enabled | 869438 | 8, 4, 2 | 149.155 | 477.599 | 204.867 |

Disabling AC progression does not remove the VarDCT DC preview. In contrast,
the small checked-in lossless control above has no progression events at all.

Every flush on both wide controls succeeded. The first recorded progressive
control invocation reported these unconsumed suffix lengths: ratio 8: 658476,
ratio 4: 453009, ratio 2: 278741 bytes; final success: 0 bytes. It continued the
same decoder, without reset/redecode, to the exact baseline hash. All nine
repetitions also matched. The common final RGB16 hash of both encodings was
`5a216e7dcf1e3883c59a530132889ece9771dcb3d216420d0845de4d58602adf`.

The progressive control's independent `preview_only` calls stopped at ratios
8/4/2 in 147.802/257.899/363.410 ms respectively. A ratio-1 request fell back
to full completion (477.260 ms). These calls still flushed earlier steps, so
the figures include their cost; they do not predict an implementation that
only flushes a single chosen step. All returned **4096x3072** buffers containing
37748736 samples / 75497472 bytes. They are not native 512x384, 1024x768 or
2048x1536 outputs. Full-size flush/color/output work is material: requesting
all previews increased time-to-full substantially, and quarter/half preview
delivery here was slower than an ordinary full decode. No speedup is assumed.

That timing conclusion applies only to the **all-earlier-flush** policy above.
With djxl's one-flush policy, the same progressive control measured **154.88 ms
quarter / 176.62 ms half / 201.58 ms full** (three fresh runs per case). The
`downsample_like_djxl` API and `djxl-downsampling` CLI mode implement that separate
policy. The linked follow-up records its precise timing scope and original-tile
verification; the old diagnostic evidence is retained rather than overwritten.

Encoded SHA-256 identities, specific to the tested DLL and settings:

- AC disabled: `8ec109145a17cb9f4cb17286a9a287cd77a3862e40ef837939b5ea57787803b4`
- AC enabled: `d36644072a69370626bbd8391d4f374eeeb0ccd5bfcaa1ebd76bf597baa21923`

These monolithic controls allow a separate Rust `jxl-oxide::set_image_region`
benchmark to exercise many spatial groups. No native ROI performance result is
claimed here, and native callback cropping should not be equated with that
Rust decoder's ability to skip spatial reconstruction.

## Integration and reproduction

`experiments/dng-jxl/native_jxl.rs` has only an additive child-module declaration
and re-exports. Existing baseline ABI declarations, implementation and tests
are unchanged. The public additions are:

```rust,ignore
native_jxl::probe_progressive(&library, bytes, channels, max_width, max_height)?;
native_jxl::preview_only(&library, bytes, channels, max_width, max_height, 4)?;
native_jxl::downsample_like_djxl(&library, bytes, channels, max_width, max_height, 4)?;
native_jxl::encode_synthetic(&library, 4096, 3072, true)?;
```

`probe_progressive` returns basic info, required full output bytes, event times,
ratios, flush statuses/durations, unconsumed bytes, first-preview/full times and
the independently checked final hash. The baseline is outside progressive
timings. `preview_only` returns owned full-size samples plus dimensions, intended
ratio, partial flag, ready time and its report. A partial result is approximate
even at intended ratio 1; only completion makes it authoritative. Neither API
performs file I/O; callers must run it off the UI thread.

The isolated local probe/test runner and generated JSON/controls are under
`target/preview-native-probe/`, not tracked fixtures. It was compiled with
`rustc --edition=2021 -C opt-level=3` against existing `target/debug/deps`
`anyhow`, `libloading`, `serde_json` and `sha2` rlibs, using the MSVC environment.
The test runner ran serially with a caller-selected `DNG_JXL_LIBRARY`:
**9 passed**, including all unchanged baseline native tests, canonical hashing,
lossless fallback/bounds/error cases, and synthetic progressive continuation.

Illustrative invocations (replace the trusted library path):

```powershell
$env:DNG_JXL_LIBRARY = 'D:/Tools/libjxl-0.12.0/jxl.dll'
target/preview-native-probe/tests.exe --nocapture --test-threads=1
target/preview-native-probe/native_probe.exe $env:DNG_JXL_LIBRARY encode 4096 3072 true target/preview-native-probe/synthetic-4096x3072-true.jxl
target/preview-native-probe/native_probe.exe $env:DNG_JXL_LIBRARY target/preview-native-probe/synthetic-4096x3072-true.jxl
```

Measured workload costs (wall/process CPU; no concurrent experimental tasks):
six real tiles plus lossless control, each including ten probe+baseline pairs
and four preview-only calls: **1.244 / 1.203 s**. Two wide-control workloads in
the same pattern: **15.475 / 15.406 s**. One progressive synthetic generation:
**0.705 / 0.688 s**; nonprogressive generation: **0.518 / 0.500 s**. Native
generation-only wall times, before file output, were 0.668 and 0.495 s.
CPU is Windows process kernel+user time, not machine-wide utilization.

The final clean isolated builds took **1.264 / 2.875 s** for the test binary and
**0.956 / 3.125 s** for the probe binary (wall / `rustc` process CPU). Compiler
CPU can exceed wall time because Rust compilation uses multiple threads; it
excludes child-linker CPU. The final serial native test run took **0.135 / 0.063
s**, including process startup. All these processes have finished; no native
runner or build is left active.

There is no application/pin/source-image modification, no generated tracked
binary, and no full application release build. Timing numbers are illustrative
local observations, not guarantees across machines or library builds.
