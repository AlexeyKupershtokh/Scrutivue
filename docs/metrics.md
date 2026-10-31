# Statistics and color picker

Open one or two images and choose **Statistics** / `S`. With only A selected, its noise estimates are available and column B shows `—`; paired metrics require B. The card floats above the image, with computation separate from the UI and render worker. Zoom, pan, divider position, gain and High pass filter do not change the statistics inputs. **Each completed metric appears immediately**, without waiting for SSIMULACRA2/LPIPS; A and B noise estimates update independently. `…` means pending; `—` means unavailable, for example when an image is too small. **A is the reference; B is the comparison image.** Some perceptual metrics are asymmetric. A selected B that is still decoding is shown as waiting, not as a request to select B.

Changing the pair, clearing the library or closing the card cancels the current request but **does not erase completed stages from the cache**. On return, cached partial values are the first event and only unfinished stages resume; a completed pair returns in one event without running any stages. A cancelled LPIPS run is not cached as a permanent error.

## What is measured

| Metric | Scope and definition | Better |
|---|---|---|
| PSNR | Full RGB16, `−10 log10(mean((A−B)²))`, samples normalized to 0…1; identical images give ∞ | ↑ |
| MAE / RMSE | Full RGB16, mean absolute / root mean squared error; displayed as % of full range | ↓ |
| Max. error | Maximum absolute channel difference, as % of full range | ↓ |
| Bias B−A | Mean signed RGB error, as % of full range | 0 |
| Changed pixels | Fraction of pixels with at least one different 16-bit channel | Task-dependent |
| SSIM Y | Wang SSIM: Gaussian 11×11, σ=1.5, population covariance, valid window, K1=.01/K2=.03 | ↑ |
| MS-SSIM Y | Five scales, weights .0448/.2856/.3001/.2363/.1333; 2×2 average pooling with odd-edge extension; minimum 176×176 | ↑ |
| GMSD Y | Average-pool 2×2, Prewitt 3×3, C=170/255², population standard deviation of the gradient-similarity map, zero-padded edges (PIQ) | ↓ |
| SSIMULACRA2 | `ssimulacra2 0.5.1`, nominal sRGB/BT.709 → linear RGB → XYB; 100 for identical images, negative scores are possible | ↑ |
| LPIPS | ONNX export of official `lpips 0.1.4`, **SqueezeNet**, learned v0.1; sRGB32F input is converted from 0…1 to −1…1 inside the graph; not the AlexNet/VGG variant | ↓ |

**Sizes:** PSNR and pixel errors always use full resolution. SSIM/MS-SSIM/GMSD/SSIMULACRA2 share a copy with a longest edge of at most **512 or 1536 px**, using Triangle without upscaling. Y = .2126 R′ + .7152 G′ + .0722 B′ in gamma-coded sRGB. LPIPS uses a separate copy ≤768 px, with at least 32 px on each axis. Under **Structure and perception**, the buttons show actual width×height, for example `341×512` and `1024×1536`, followed by `Triangle · RGB`. Settings still store the 512/1536 edge limits. Decoded dimensions take precedence over headers; unknown, absent or unequal source dimensions show disabled `—×—` choices rather than an invented common size. Exact analysis dimensions are also in JSON. Small images yield `—` / `null` or an explicit unsupported-metric error, not a fabricated score.

These are actual computations at the stated resolution, **not full-resolution perceptual scores**. Downsampling can hide fine noise. No scene alignment, exposure, white-balance or color-profile matching is performed. Unequal images are **not stretched for statistics**: the UI and JSON report that paired metrics are unavailable, while independent A/B noise estimates remain available. The viewport may scale B for viewing. Moiré filtering affects only the Diff view.

## A / B noise estimates

These are three **blind estimates of luminance-noise σ**, not measured ground truth, A−B difference noise or Bayer/RAW sensor read noise. Each source is measured independently; the UI has A/B columns as percentages of full range. JSON stores σ on a 0…1 scale. Luminance `Y′ = .2126R′ + .7152G′ + .0722B′` comes directly from decoded RGB16 in gamma-coded nominal sRGB.

Noise is **never estimated after resizing**. To bound memory and execution time, up to 16 non-overlapping spatially distributed patches of up to 128×128 original pixels are sampled. The count per axis is `clamp(floor(size/128), 1, 4)`; outer patches touch the edges, intermediate patches are evenly spaced and a single patch is centered. Small images limit the patch size to the image dimensions. Patch sizes and counts are recorded in `noise_sampling`; this is not a full analysis of every pixel. These method details are documented here rather than repeated in the compact statistics card.

| Estimate | Definition |
|---|---|
| Haar HH · MAD | Non-overlapping 2×2 blocks, `HH=(Y00−Y10−Y01+Y11)/2` (orthonormal Haar). `median(abs(HH−median(HH)))/0.6744897501960817`. |
| Immerkær | Mean absolute response of the 3×3 kernel `[[1,−2,1],[−2,4,−2],[1,−2,1]]`, multiplied by `sqrt(π/2)/6`. Valid positions within each patch only. |
| High-pass · MAD | Residual `Ycenter−(Yleft+Yright+Yup+Ydown)/4`; centered MAD divided by `0.6744897501960817 * sqrt(1.25)` (filter L2 norm). Valid positions only. |

For independent additive white Gaussian noise, these normalizations estimate σ in Y′. Textures, edges, sharpening, demosaicing, JPEG, denoising, clipping, correlated/signal-dependent noise and nonlinear sRGB can substantially bias the estimate; a smaller number alone does not mean a better photo. An insufficient window yields `—`/`null`, not an artificial zero.

## Caches and cancellation

* An LFU of **100 ordered pairs** `(A revision, B revision, analysis edge)` stores reports and stage readiness only. A/B and B/A are separate entries, as are 512/1536. Equal-frequency ties evict the least recently requested entry; stage publications do not increase frequency.
* A separate LFU of **20 actually reduced RGB32F copies** stores `(source revision, exact output size)`. Structural metrics and LPIPS use the same pixels; equal output dimensions do not trigger another resize. Native RGB16 and full-resolution RGB32F frames never enter this cache. Neither statistics cache retains source `Arc<Frame>` values.
* A revision is the SHA-256 of decoded dimensions and RGB16 pixels, **not `PhotoId`, file path or allocation address**. Thus deselection/reselection and clear/re-add with new library IDs or new decoded allocations reuse completed and partial reports, provided the entry has not been evicted. Changed pixels under the same ID cannot obtain stale statistics. The previous `(PhotoId, pixel hash)` key unnecessarily missed after re-add, even when the hash matched.
* `Worker::request(aid, a, bid, b, edge)` and `request_single(aid, a, edge)` retain their caller API; IDs are not used as content identity. A bounded 220-entry weak-frame table memoizes verified revisions by allocation, independently of IDs. Weak identity checks prevent address reuse from impersonating an old decode and do not retain original pixel buffers. A known completed report is published synchronously, even while the worker finishes an indivisible previous call. The UI consumes that ready snapshot immediately. Mutexes cover only small metadata lookups/copies, never hashing, decoding or analysis. An unknown/new allocation is hashed with cancellation on the worker, then hits the same content-keyed cache without rerunning finished stages. This verification is required for content safety; the separate decode cache controls whether a new allocation/decode is necessary.
* One sequential metrics worker uses a private two-thread Rayon pool; ONNX uses two intra-op threads and one inter-op thread. The mailbox retains only the newest request. Pixel/SSIM/noise/resize CPU loops regularly check cancellation; active ONNX runs are interrupted with `RunOptions::terminate`. SSIMULACRA2 library calls are indivisible: cancellation is checked at their boundaries, completed results can be retained for a revisit, and stale reports are not published. The bounded event queue does not block cancellation when the UI is paused.

Triangle resizing now streams the same vertical-then-horizontal RGB16 arithmetic with one RGB32F row rather than a full-width RGBA32F intermediate. Tests compare its output exactly with `imageops`; no preview substitution or metric-input change is made. Resizing checks cancellation between rows. ONNX retains the model/session but disables its CPU activation arena to release high-water buffers after inference, trading some first-inference allocation time for lower retained memory. Completed report hits perform neither operation. Real DNG stage timings, hash/cache counters, memory peaks and limitations: [statistics-performance.md](statistics-performance.md).

## ONNX Runtime

The application **does not use Python, PyTorch, subprocesses or a separate environment**. The executable embeds `assets/models/lpips-squeeze-v0.1.onnx` (~2.8 MB); its provenance, export details and SHA-256 are recorded in the adjacent JSON. Official SqueezeNet ImageNet weights and learned LPIPS v0.1 weights are not replaced with random weights. Inference uses Rust `ort 2.0.0-rc.12` / official CPU ONNX Runtime **1.24.4**, with two intra-op threads. The session is initialized lazily and reused by the background metrics worker. The model has dynamic H/W, NCHW `[1,3,H,W]`, opset 17.

`onnxruntime.dll` must be beside the executable; build preparation downloads it with SHA-256 verification. If the DLL is missing, native metrics still work and LPIPS reports an error. The application does not access the network or upload photographs. The UI makes no promise that expensive metrics finish within 100 ms.

`scripts/export_lpips.py` is **developer-only**, for reproducing the shipped ONNX. It requires CPU `torch 2.8.0`, `torchvision 0.23.0`, `lpips 0.1.4`, `onnx 1.19.1` and official weights. Neither the build nor the application runs it. LPIPS/torchvision licenses are in `LICENSES/`.

## CLI and validation

```powershell
.\target\debug\scrutivue.exe --statistics A.NEF B.NEF --metric-edge 1536
python scripts/test_metrics.py
```

JSON contains dimensions, values, LPIPS status and timing. Final keys and numerical definitions for `pixels`, SSIM/MS-SSIM/GMSD/SSIMULACRA2/LPIPS remain unchanged for compatible pairs. Added fields are `noise_a`, `noise_b`, `noise_sampling`/`noise_sampling_b`, `original_size_b`, `comparison_error` and `pending` (empty in a final CLI report). In intermediate worker reports, `pixels: null` means an unfinished reduction or an unavailable comparison explained by `comparison_error`; in completed pixel statistics, `pixels.psnr_db: null` at zero error means +∞. For other nullable values, `pending` distinguishes unfinished work from known unavailability. The existing `neural_pending` field remains. The CLI still accepts two paths; unequal dimensions produce JSON with an explicit comparison error and both noise estimates.

`test_metrics.py` is an independent developer check; run it in your own Python environment with NumPy/Pillow/PIQ/Torch/official LPIPS. The application does not use that environment. A seeded RGB8 pair compares Rust PSNR/SSIM/MS-SSIM/GMSD with NumPy/PIQ, and LPIPS ONNX with direct PyTorch inference on the same inputs, including minimal/odd dimensions. Native unit tests cover single-step RGB16 differences, identity, dimensions, LFU capacity, partial progress and cancellation, completed cache hits, revision checks after decode, shared structural/LPIPS resizes, source release, constant signals and synthetic Gaussian noise with known σ. Real worker request sequences additionally cover reselection, changed IDs, new decoded allocations, cache snapshots and completed reports returning without any stages.

## Color picker

`C` or the top button enables a source-pixel loupe at fixed **1600% in physical pixels**: each original pixel is a solid 16×16 px square without interpolation. Viewport zoom/DPI therefore does not change the loupe grid scale. The odd-sized grid keeps the inspected pixel centered and is clipped to its window. It follows the current Single / Slider / Diff, gain and High pass settings; this is a local calculation from original frames, **not an enlarged viewport bitmap**. A small High pass/Diff patch is computed asynchronously with stale-request cancellation; mouse movement does not trigger RAW decode or full-image recomputation.

Main-view wheel zoom keeps the current same-source loupe visible until its replacement arrives, instead of hiding it during camera refinement. Source or Diff/filter changes still invalidate stale content. The navigator wheel also zooms the synchronized camera, but its cursor is not treated as a main-image inspection point.

A/B swatches and RGB8/16, HEX, HSL, HSV, OKLCH, Lab D65, XYZ D65 and linear RGB values come from the **original decoded pixel**, not amplified Diff or High pass output. RGB8/HEX are rounded from RGB16; B coordinates are remapped for unequal dimensions. OKLCH uses Oklab/D65 and the standard sRGB EOTF. These are nominal sRGB values after RAW development, not Bayer samples or ICC-managed measurements of a physical display. The loupe follows the mouse and switches sides near a window edge.
