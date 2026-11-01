# Native JPEG preview and ROI probe

## Findings

**Observed:** libjpeg-turbo really decodes JPEG at 1/2, 1/4 and 1/8 dimensions using reduced IDCT, without allocating a full-resolution RGB image. Its `jpeg_crop_scanline()` plus `jpeg_skip_scanlines()` APIs also produce genuine partial-image output, including combined reduced decoding and ROI. This probe does not call Pillow `crop()` and describe that as ROI decoding.

**Current application capability, from source rather than runtime:** `Cargo.lock` selects `image` 0.25.10 → `zune-jpeg` 0.5.15 / `zune-core` 0.5.3. `image::codecs::jpeg::JpegDecoder` has no scale/ROI setter or `ImageDecoderRect` implementation. Its constructor reads the compressed input into a `Vec`; `read_image()` creates a zune decoder and calls `decode_into()` with the full advertised output size. The inspected zune decoder/options public API likewise has no reduced-IDCT or ROI option. These are bounded findings for these exact versions, not a statement about all Rust JPEG libraries.

**Implication:** a native libjpeg-turbo integration could supply real JPEG previews and viewport output, but the existing `image` JPEG path does not already provide them. This experiment did not alter the application or benchmark its zune decoder.

## Reproduction and evidence

Run from the repository root in PowerShell, setting `$photo` to the read-only source JPEG:

```powershell
python experiments/preview/jpeg_probe.py --build
python experiments/preview/jpeg_probe.py --photo "$photo" --repeats 3
python experiments/preview/jpeg_probe.py --edge-check

# Direct public-API examples; coordinates are in scaled-output pixels:
target/preview-jpeg/jpeg_probe.exe "$photo" 4 full
target/preview-jpeg/jpeg_probe.exe "$photo" 1 roi 3619 2371 1024 768
target/preview-jpeg/jpeg_probe.exe "$photo" 4 roi 691 462 688 458
```

The build locates existing Visual Studio/CMake with `vswhere`, reads the already-present vendored libjpeg-turbo 2.1.5.1 source from the `jpegli-sys` 0.1.0+0.10.2 Cargo package, and writes only under `target/preview-jpeg`. It uses Release `/O2`, static linkage and at most two build jobs. No installer, application rebuild, dependency-pin change, registry-source edit, neighboring checkout edit, or root binary replacement was performed. NASM/Yasm was not found in the checked executable locations, so this standalone C++ build explicitly disables SIMD. The successful incremental configure/library-check/compile/link took **3.71 seconds**; the initial library compilation preceded a failed Windows environment-setup step and was not separately timed. Do not call 3.71 seconds a clean-build measurement.

The independent Python path is Pillow 12.3.0 with **libjpeg-turbo 3.1.4.1, SIMD-enabled build** (`features.check_feature('libjpeg_turbo') == True`). `JpegImageFile.draft()` configures the C JPEG decoder's scale before `load()`; it is not a Python JPEG codec. No particular SIMD instruction execution was profiled.

Generated evidence, intentionally untracked under `target/preview-jpeg`:

- `manifest.json`: source identities, dimensions, sampling, backend versions and photograph hashes.
- `measurements.json`: all **180 fresh-process decodes**, three repeats of every main case.
- `summary.json`: timing min/median/max and median memory measurements.
- `correctness.json`: **36** same-backend full-decode-versus-ROI comparisons at four scales.
- `edge-correctness.json`: **12** additional aligned-edge/padded-ROI comparisons.
- `build.log` and `build-metadata.json`: standalone build details and selected source hashes.

Main inputs:

| Label | Dimensions | JPEG process / chroma | Encoded size |
|---|---|---|---:|
| `photo-45mp` | 8256 × 5504, 45.44 MP | Sequential, 4:4:4 | 32,714,367 bytes |
| `synthetic-baseline` | 4096 × 3072, 12.58 MP | Baseline, 4:2:0 | 9,593,421 bytes |
| `synthetic-progressive` | 4096 × 3072, 12.58 MP | Progressive, 4:2:0 | 8,532,720 bytes |

The two bounded synthetic JPEGs use identical deterministic source pixels, quality 92 and 4:2:0. They are generated only under `target/preview-jpeg`. Photograph SHA-256 before **and** after: `0079375231ca781e7dc9a9f2de0d5da37493308e4f0c2cea4c51da2c48544b89`.

Timing starts after the entire compressed file has been read into memory; it includes decoder initialization, header parsing, native start/decode, output allocation and requested-row copying. It excludes file reads, process startup, RGB dump writes, color management, orientation, texture upload, rendering and UI presentation. Native cleanup is separately recorded. The ROI worker aborts after its requested last row instead of decoding the remaining image. Full native output also uses abort/destroy after all rows; this is not a malformed-file/EOI validation test. All main workers run sequentially, not concurrently. These are local three-repeat measurements, not latency guarantees.

Memory is Windows process **peak working set**, plus separately recorded peak private commit, not codec-only allocation accounting. Compressed input and runtime overhead are included. The scalar native and Pillow processes have different baselines (~34.4 versus ~53.3 MiB for this photograph); Pillow RGB storage can also use four bytes internally. Do not attribute all cross-backend memory differences to the codec or SIMD.

## 45 MP reduced decoding

Median decode milliseconds / median absolute peak working-set MiB:

| Scale | Output dimensions | Packed RGB MiB | Pillow / turbo 3.1.4.1, SIMD-enabled | C++ / turbo 2.1.5.1, scalar |
|---|---|---:|---:|---:|
| Full | 8256 × 5504 | 130.01 | 376.70 ms / 228.62 MiB | 618.41 ms / 164.69 MiB |
| 1/2 | 4128 × 2752 | 32.50 | 334.17 ms / 98.54 MiB | 452.67 ms / 67.04 MiB |
| 1/4 | 2064 × 1376 | 8.13 | 307.89 ms / 65.92 MiB | 350.71 ms / 42.62 MiB |
| 1/8 | 1032 × 688 | 2.03 | 268.09 ms / 57.72 MiB | 244.96 ms / 36.52 MiB |

**Observed:** Pillow quarter output saved ~71% of peak working set but only ~18% of median decode time compared with its own full decode; eighth output saved ~75% of peak working set and ~29% of time. The scalar backend's quarter/eighth decode saved ~43%/~60% of its own time. Backend version, buffers and wrapper paths differ, so this is not a controlled SIMD-versus-scalar comparison.

**Explanation from source:** reduced IDCT reduces transform/output work, but sequential JPEG still needs entropy decoding across the compressed image. This 31.2 MiB compressed 4:4:4 photograph is a clear example of why pixel-count reduction does not imply a 4× or 16× latency improvement.

## Genuine ROI decoding, including reduced + ROI

All following timings use the standalone scalar native backend and the same backend's full output as the correctness reference. The full-scale photograph ROI is 1024 × 768 at x=3619; top/middle/bottom y values are 0/2371/4733.

| Photograph case | Median ms | Peak WS MiB | `skip_scanlines` ms | Compressed bytes consumed |
|---|---:|---:|---:|---:|
| Full image, full scale | 618.41 | 164.69 | 0 | ~100% |
| Full-scale ROI, top | 41.60 | 36.86 | 0 | 12.55% |
| Full-scale ROI, middle | 143.39 | 36.86 | 96.94 | 58.34% |
| Full-scale ROI, bottom | 242.98 | 36.86 | 202.47 | ~100% |
| Quarter image, no ROI | 350.71 | 42.62 | 0 | ~100% |
| Quarter ROI, top | 96.54 | 35.40 | ~0 | 30.43% |
| Quarter ROI, middle | 179.09 | 35.40 | 73.40 | 68.56% |
| Quarter ROI, bottom | 246.91 | 35.41 | 154.54 | 99.88% |

The quarter ROI is 688 × 458 at x=691, with y=0/462/915 in 2064 × 1376 scaled coordinates. It is not the same sensor rectangle or output pixel count as the full-scale ROI, so these rows demonstrate combined support, not a controlled scale comparison for one viewport.

`jpeg_crop_scanline()` is called **after** `jpeg_start_decompress()` and **before** skip/read. It rounds the left x down to an iMCU boundary and expands width to preserve the requested right edge; the probe allocates using the returned width and trims only the extra decoded pixels. For example, the photograph full-scale x=3619 becomes 3616 with decoded width 1027. Vertical skip accepts arbitrary output-row offsets but operates on MCU rows internally and may decode boundary/context rows.

**Not arbitrary compressed-stream random access:** for single-scan JPEG, `jdapistd.c` skips preceding complete iMCU rows by calling the entropy decoder for **every MCU** and discarding coefficients. Cropping avoids much unnecessary IDCT/upsampling/color-conversion/output, but cannot seek straight to an arbitrary entropy-coded tile. The top-to-bottom timing and consumed-byte gradient above directly corroborate this. Restart-marker-aware indexing or a tile cache would be a different, untested design; the public calls here do not provide it. Moving an already-started cropped scanline window arbitrarily is also not what this API supports.

### Progressive limitation

For the 12.58 MP 4:2:0 progressive fixture, full-scale full output was **279.22 ms / 83.59 MiB**, versus top/middle/bottom ROI **216.18 / 211.76 / 215.28 ms**, each ~49.84 MiB. Native `jpeg_start_decompress()` alone took ~207–211 ms in these cases, and **every progressive ROI consumed 100% of the input**. Quarter full output was 213.78 ms / 49.71 MiB; quarter ROI remained ~200–212 ms / 47.70 MiB. Eighth full output was 204.48 ms / 48.01 MiB; eighth ROI stayed ~202 ms / 47.5 MiB.

By contrast, the same-sized baseline fixture at full scale was 142.97 ms / 48.58 MiB, and top/middle/bottom ROI was 21.09 / 40.18 / 60.08 ms, ~14.84 MiB.

**Source-backed explanation:** this ordinary non-buffered, non-suspending progressive decode reads all scans and retains a full-image coefficient buffer during `jpeg_start_decompress()`, before cropping is available. Reduced output and ROI do not remove that cost. The documented typical 4:2:0 coefficient buffer is ~3 bytes/source pixel (~36 MiB here), independent of reduced RGB dimensions. Buffered progressive scan-by-scan display is a separate mode, not tested by this probe; do not generalize these results to claim progressive JPEG can never show early scans.

## Correctness and edge handling

- All **36 main ROI cases** were byte-identical to cropping a **full decode at the same requested scale with the same native backend**: all three inputs, four scales, three vertical positions. Reduced output is not claimed to equal an arbitrary full-resolution resize filter.
- The additional 4:2:0 edge tests deliberately put the requested boundary on an iMCU edge. Without halo, only horizontal edge columns differed: observed maximum channel error **31/255**, with exact interior pixels after excluding two columns at each side. This is the documented fancy-chroma-upsampling boundary behavior, not arbitrary image corruption or a blanket tolerance for interior errors.
- Decoding a **16-output-pixel horizontal halo** on both sides, then trimming that *partial* decoded output, produced **byte-identical requested pixels in all six padded checks**, baseline and progressive. This never constructs the full RGB image for the ROI operation. In an integration, derive suitable padding/alignment from sampling/scaling and clamp to source edges; the fixed 16-pixel halo here is validated only for these tested 4:2:0 cases.
- No cross-codec pixel-exactness, ICC, EXIF orientation, grayscale/CMYK, arithmetic-coded JPEG, malformed-input, restart-marker-indexing, or multithreaded decoder behavior was tested.

## Preview then full: restart, not magnification

**Public-API/source conclusion:** these normal scanline decodes select `scale_num/scale_denom` before `jpeg_start_decompress()`. After half/quarter/eighth output, obtaining full-resolution pixels requires another decoder cycle from the compressed JPEG: finish/abort/destroy as appropriate, rewind/rebind the source (for memory, call `jpeg_mem_src()` again), read the header and start at 1/1. The already decoded low-resolution pixels cannot recover discarded detail. Even buffered-image mode's documented permitted between-pass parameter changes do **not** include scaling.

The main measurements use independent decoder cycles. **Inference, not measured end-to-end UI latency:** serial Pillow quarter-then-full would spend roughly 307.89 + 376.70 = **684.59 ms** of decode work; eighth-then-full roughly **644.79 ms**, before presentation/I/O/dispatch overhead. This can improve first usable display and memory pressure, but it is additional work and delays final pixels if run serially. A retained-coefficient/transcoding-based design might avoid some entropy work at much higher memory/complexity; that alternative was not implemented or measured.

**Integration recommendation, not an implementation:** use a native decoder behind the existing worker boundary; cancel stale preview/full/ROI requests; cache full pixels or suitable tiles once decoded; consider ROI mainly for first high-zoom display and memory pressure, not constant-time panning. Choose quarter/eighth previews based on measured source entropy cost, viewport size and final-output deadline rather than assuming downscale always means near-instant decode. Source photographs must remain unchanged.

## Pinned references

- [libjpeg-turbo 2.1.5.1 public API manual](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/2.1.5.1/libjpeg.txt): partial image decompression lines 735–819; scale parameters 1212–1219; permitted buffered-pass parameter changes 2180–2206; coefficient memory 3010–3033. Local manual SHA-256: `dcc94dda12e38935a31de2ee8bba6545b36b8732b0cda6206208229f2ec61a98`.
- [libjpeg-turbo 2.1.5.1 `jdapistd.c`](https://github.com/libjpeg-turbo/libjpeg-turbo/blob/2.1.5.1/jdapistd.c): `jpeg_crop_scanline()` lines 145–260 and skip/progressive/entropy behavior lines 417–591. Local SHA-256: `c2ba7030eab69145f91bf1460d6979b415c450c41a59de38f23c26d73bec9a8d`.
- [Pillow 12.3.0 native `JpegDecode.c`](https://github.com/python-pillow/Pillow/blob/12.3.0/src/libImaging/JpegDecode.c): applies `context->scale` to libjpeg `scale_denom` before `jpeg_start_decompress()`, then calls native `jpeg_read_scanlines()`.
- [Pillow 12.3.0 `JpegImagePlugin.py`](https://github.com/python-pillow/Pillow/blob/12.3.0/src/PIL/JpegImagePlugin.py): `draft()` chooses 8/4/2/1 and passes the decoder configuration.
- [image 0.25.10 JPEG decoder source](https://docs.rs/crate/image/0.25.10/source/src/codecs/jpeg/decoder.rs): inspected local file SHA-256 `135116b08028d03ba7481f7e704a461e239b01f960f576c1e70f48d7e9616569`; full compressed input read at lines 28–40, complete wrapper API through 179, full `decode_into()` at 148–165.
- [zune-jpeg 0.5.15 decoder source](https://docs.rs/crate/zune-jpeg/0.5.15/source/src/decoder.rs): inspected local SHA-256 `7fdf4e5f585a1f7bfdba51b3ab3a851fb519a326324fdb9f44a236864c26a554`. [zune-core 0.5.3 decoder options](https://docs.rs/crate/zune-core/0.5.3/source/src/options/decoder.rs): local SHA-256 `bf482212d229d535df6d01152853e1433d8704c1ededa6e218e9e0d138ae19bf`.

Evidence type is explicitly source inspection or execution of the two experiment backends; no shipped-application reverse-engineering/runtime claim is made.
