# Scrutivue

A Rust desktop application for comparing photographs and RAW images. The interface uses **GPUI + GPUI Kit**; computation uses **wgpu / DirectX 12** on Windows (Metal on macOS, Vulkan on Linux). Its dark palette and compact controls are inspired by Redlamp.

## Build and run

Building requires current stable Rust, Python 3.12+ (preparation only) and a C++17 compiler. On Windows, install **Visual Studio Build Tools → Desktop development with C++** and the Windows SDK. GPU processing requires a DX12 driver; neither the OpenCL SDK nor CMake is needed. The built application does not require Python.

```powershell
python scripts/prepare.py
cargo run --release
cargo run --release -- "D:\Photos\A.NEF" "D:\Photos\B.NEF"
cargo run --release -- "D:\Photos\session"
cargo run --release -- --recursive "D:\Photos"
```

If Cargo is not in PATH: `& "$env:USERPROFILE\.cargo\bin\cargo.exe" run --release`.

`scripts/prepare.py` downloads **the same LibRaw fork and commit pinned in `../redlamp/config/vendored-libs.json`** and verifies SHA-256. The neighboring Redlamp checkout is not modified and is not needed for subsequent builds. Do not replace this fork with upstream LibRaw: it supplies Nikon HE/HE* decoding. Pin: `f4956c404fba0c6035ce9e06353d4b4ffe068fc7`. The build reads the authoritative C++ source list from the fork's `Makefile.dist`.

Preparation also downloads Microsoft's official CPU ONNX Runtime **1.24.4**, checking SHA-256 (`config/onnxruntime.json`). Cargo copies `onnxruntime.dll` beside the debug/release EXE. Distribute them together with the notices in `LICENSES/` and ONNX Runtime LICENSE/ThirdPartyNotices. The embedded LPIPS ONNX model needs no export or download during build/startup.

Linux also needs the GPUI/Wayland/X11 and RFD system dependencies, plus native ONNX Runtime ≥1.24 beside the executable. This workspace validates Windows x64; other platforms require separate builds and measurements.

## Usage

- **Add images**, **Add folder** and **Clear list** stay above the library. **Include subfolders** enables recursive folder addition, including drag-and-drop. The CLI equivalent is `--recursive`.
- Collapse the library with ‹ or `L`; it becomes a separate left **☰** button. There is no right sidebar. Floating buttons have consistent heights, with larger gaps between logical groups. `F11` toggles fullscreen.
- Startup selects only the first file in **Single** mode, even when an earlier session used a comparison mode. A normal click selects only A and clears B; Ctrl+click selects/toggles B while preserving A. Diff and Slider require B and are temporarily disabled without it; the previously chosen comparison mode returns when B is added again unless Single was explicitly chosen. The row context menu can open a file, choose an external application, open its containing folder, copy its file name or full path, and explicitly assign A/B. Folder opening runs in the background; Windows uses native Explorer APIs without a command shell.
- Rows show dimensions and readable decimal file sizes, without row numbers: for example **7.1 MB**, **500 KB**, **92 MB**. KB means 1,000 bytes, MB means 1,000,000 bytes; values round to one decimal with trailing zeroes omitted. Metadata is read in the background. If folders were added, **Refresh** adds new files and removes missing ones from the list, without deleting anything from disk.
- **Clear list** cancels pending addition/decoding and releases UI selections and thumbnails. The bounded in-memory decode cache survives clearing the list, so unchanged files can be re-added without decoding again. Source files and the disk cache are not deleted. Late results from canceled jobs cannot repopulate the list.
- Reorder by dragging rows or using **↑ / ↓** at the bottom. A/B selections are tied to IDs, not row positions.
- **Single** (`1`) shows A; hold the left mouse button or Space to show B. Left-drag pans while showing B; middle-drag pans without switching. **Slider** (`2`) shows both images with a divider. **Diff** (`3`) shows absolute RGB differences with **1/4/16/64/256×** gain. Buttons have brief hover tooltips.
- The wheel zooms at the cursor. Drag to pan; middle-drag works in every mode. Drag the divider handle in Slider mode. Controls include **Fit to screen / 100% / 200% / 400% / 800%**; `F` or `0` selects Fit, `8` selects 800%. Zoom ranges from ½ Fit to 800% (Fit is also limited to 800% for very small images). Above 200%, pixel interiors stay flat and only a one-screen-pixel boundary is smoothed.
- Zoom and normalized camera center persist across image changes and A/B swaps, constrained to the nearest valid position: small images are centered on each axis, and large images cannot expose space beyond their edges. The same constraint applies after loading settings or resizing the window. 100% is one original pixel per physical screen pixel, accounting for DPI.
- During zoom/pan, GPUI transforms the prepared texture without compute/readback for every event. A refinement render follows 10 ms without movement. This is not interpolation between two complete RAW developments; newly exposed areas become detailed after refinement.
- The bottom-right **Navigator · A** shows the reference image and the visible viewport rectangle. Click outside the rectangle to recenter, drag it to pan, or use its wheel to zoom; camera limits remain synchronized with the main view. Its small thumbnail is sampled on a separate worker, never used for comparison or statistics.
- B is mapped to A's geometry using bilinear interpolation. Different resolutions produce a warning; this does not automatically align the scene or estimate displacement.
- DNG applies its **DefaultCrop** before orientation without resampling RGB16; JPEG respects EXIF orientation. The tested DNG/JPEG pair produces matching **5504×8256** dimensions. Intentionally resized or further cropped exports are not stretched to match the DNG. Previous mismatch, checks and limitations: [dng.md](dng.md).
- **High pass filter** at the top computes `I − Gσ(I)` separately for both images, then compares residuals. σ ranges from 0.3 to 20 A pixels; kernels stop at ±3σ and extend edges using the nearest pixel. B's σ is scaled on both axes. Slider/Single show a zero residual as middle grey; Diff remains absolute with zero shown as black.
- `C` / **Color picker** always shows original pixels at 1600% (16×16 physical screen pixels per sample, accounting for DPI), without interpolation or smoothing. Diff/gain/High pass use a separate background local computation; numeric A/B colors remain original RGB16. Formats include RGB8/16, HEX, HSL/HSV, OKLCH, Lab/XYZ D65 and linear RGB.
- `S` / **Statistics** shows PSNR, SSIM, MS-SSIM, SSIMULACRA2, GMSD, LPIPS, MAE/RMSE and three A/B noise estimates. Rows appear independently as results become available. Memory caches retain 100 pairs (LFU, including partial results) and 20 resized copies; returning to a pair does not repeat completed stages. Formulas and analysis sizes: [metrics.md](metrics.md).
- `Ctrl+O` adds files; `Ctrl+Shift+O` adds a folder. `F1` or **Help** opens a separate controls window. `←/→` changes the last-selected image; `Home/End` goes to the beginning/end of the list.

## Settings

`settings.json` lives in the user configuration directory from `directories::ProjectDirs("app", "Scrutivue", "Scrutivue")` (Windows: `%APPDATA%\Scrutivue\Scrutivue\config\settings.json`). Override it with `--settings-file`. On first use of the default profile, legacy Lumen Diff preferences can be imported; the original file is never modified or deleted. Explicit profiles do not import unrelated preferences. Saved values include the preferred mode, gain, High pass/σ, split, zoom/center, recursion, window size, library/Statistics/Color picker visibility and analysis resolution. Startup still shows Single with one file; the file list and selection are not saved. `--recursive` enables recursion in addition to the saved value; `--cpu`/`--cache-dir` affect only the current run. CLI probe/benchmark/statistics do not change user preferences.

Changes are saved by a separate worker after 400 ms of inactivity and on close, using a temporary file and atomic replacement. Invalid values are clamped to safe ranges; corrupt JSON or an unknown version loads defaults with a warning, and the original file **is not overwritten**. To reset settings, close the application and move this JSON elsewhere.

## Performance and limitations

- GPUI `uniform_list` creates only visible rows. The scanner emits batches at 128 paths or after 25 ms of traversal; opening a folder neither decodes full frames nor creates 10,000 decode jobs.
- Two background workers gradually build thumbnails. Visible rows take priority; the bounded queue holds at most four pending jobs. RAW thumbnails use the smallest suitable embedded JPEG, without full development when no preview exists.
- The in-memory thumbnail LRU holds up to 1024 images no larger than 160×112 (about 70 MiB of pixels). The disk cache, keyed by path/size/mtime, is capped at 512 MiB with periodic cleanup. A 10,000-file folder does not allocate 10,000 GPU textures.
- `--cache-dir` sets the cache parent directory. Writes/cleanup affect only hashed cache files in the application's marked `scrutivue-v1` subdirectory. Nonempty unmarked directories and old application caches are not cleaned; source photographs are never changed.
- LibRaw decodes and develops RAW in two background threads: 16-bit sRGB, as-shot white balance, no auto-brightness, denoising or sharpening, and fast bilinear demosaic. This is **not** Redlamp's color pipeline. Initial RAW decoding is not promised within 100 ms. Some cameras, including Nikon Z5 II/Z50 II, may lack a complete color matrix in the pinned fork.
- `jxl-oxide` decompresses JPEG XL DNG 1.7 (compression 52546) with unsigned 16-bit grey/RGB chunky tiles in memory. The same LibRaw receives a virtual uncompressed DNG, preserving camera matrices, white balance, black/white levels and crop. Nothing is written to disk, and the full image is not replaced with a JPEG preview. Float/planar/BigTIFF variants are unsupported by this path. Extra memory is the source-file size plus unpacked samples, with a 1 GiB sample limit. Thumbnails are extracted separately from JPEG SubIFDs without a full decode.
- DNG `DefaultCropOrigin`/`DefaultCropSize` are applied relative to `ActiveArea`, accounting for crop/orientation already performed by LibRaw; RGB16 is not stretched to match a neighboring JPEG. SHORT/LONG/RATIONAL are supported. Non-unit `DefaultScale` requires separate resampling and currently returns an error for this pixel-preserving crop. Dimension mismatch, measurements and limitations: [dng.md](dng.md).
- RAW uses read-only memory mapping: thumbnail extraction does not copy the complete compressed file into RAM. Do not modify or truncate sources while they are being read. A decoded-frame LRU is limited to six frames and roughly 1 GiB (at least two frames), keyed by canonical path/size/modification/creation times rather than library IDs. It survives clear/re-add, protects selected frames, and checks revisions on workers. Selected A/B loads take priority. Full-resolution neighbors are prepared only with an A/B pair and Statistics closed; opening Statistics cancels speculative work without cancelling selected loads. A single selection never decodes unseen neighbors. Measured DNG loading/cache boundaries: [loading-performance.md](loading-performance.md); statistics timing and memory: [statistics-performance.md](statistics-performance.md).
- Two active RGB16 frames remain in GPU memory. Gaussian filtering uses two separable compute passes, float32 residuals and a shared scratch buffer; results are cached until the frame/σ changes. Diff caches a pyramid of **absolute differences before resampling**, using 2×2 area averages and trilinear transitions; gain follows averaging. This reduces moiré and prevents opposite differences from canceling during downsampling. Initial Diff entry requires preparation; pan/zoom/gain do not repeat it. The extra pyramid takes about **0.68 GiB for 45 MP**; high-pass + Diff uses roughly **2.7 GiB VRAM** beyond GPUI/driver overhead. Ordinary A/B without prepared Diff uses about 0.5 GiB. CPU fallback uses the same mathematics.
- The separate compute device still transfers the viewport result through readback/upload to GPUI. **Zero-copy GPUI integration is not implemented**: this GPUI uses DX11 on Windows. Why DX12 rather than OpenCL: [compute-decision.md](compute-decision.md).
- The render worker keeps only the latest pending job; stale results never replace a newer image. Expensive work stays off the UI thread. Slider and Single prepare **two complete viewport textures**, A/B. The divider only changes GPU clipping in GPUI; it does not trigger decoding, Gaussian filtering, compute, readback or new image handles. Single switching also uses prepared textures. Zoom/pan transform them first, then refine after a pause.
- An unavailable GPU or compute failure activates background CPU/Rayon fallback with a message. `--cpu` enables it explicitly. This is a compatibility mode, **not** a guarantee of latency below 100 ms.
- Differences use 16-bit RGB code values, not the original Bayer mosaic. Development, exposure, white balance, geometry and embedded-profile differences also appear in Diff. ICC conversion into a common raster working space and HDR tone mapping are not implemented. EXR is converted to RGB16; values outside 0…1 are clipped.
- Damaged RAW files are processed inside the application, without a separate sandbox process. Do not open untrusted RAW files in this version.

## Validation and measurements

```powershell
cargo fmt --check
cargo test
cargo test gpu_matches_cpu -- --ignored --nocapture
cargo run --release -- --benchmark --iterations 30
cargo run --release -- --benchmark "D:\Photos\A.NEF" "D:\Photos\B.NEF"
cargo run --release -- --benchmark --cpu
cargo run --release -- --probe "D:\Photos\HE.NEF" "D:\Photos\HEstar.NEF"
```

Without files, the benchmark creates an 8256×5504 pair (~45.4 MP), warms the GPU, and measures cold upload, initial Gaussian filtering and median/p95 pan/zoom/gain times with and without high-pass. Output is JSON. Timings include compute + readback + copying the viewport into a host Vec, **not** end-to-end mouse-to-screen latency. A 100 ms target must account for the GPU, viewport size, cold/warm caches, σ and system load; choosing DX12 alone does not guarantee it.

Reproducible local checks and actual results: [validation.md](validation.md). `python scripts/fixtures.py library` creates a 10,000-PNG stress-test folder; `python scripts/fixtures.py raw` downloads two CC0 Nikon Z9 HE/HE* files with SHA-256 verification. On Windows, `python scripts/ui_smoke.py --tail` checks the native window and saves screenshots (requires Pillow).

## Licenses

Application code is MIT. LibRaw is used under CDDL-1.0; the unmodified fork's source is available at the URL in `config/libraw.json`. Notices and license texts are in `LICENSES/`. GPUI and GPUI Kit use Apache-2.0; wgpu uses MIT/Apache-2.0. The image from the Redlamp example is not distributed with this application.
