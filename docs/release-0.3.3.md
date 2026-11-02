# Scrutivue 0.3.3

## Changes

- Add a separate **Settings** window with a configurable RAM-cache budget, optional
  neighbor **Prefetch**, disk-preview location/limit, lazy **Build previews for all
  added files**, and a safe disk-preview reset. Changes apply without restarting.
- Cache display-only JPEG XL previews for sources of at least 10,000,000 pixels:
  **2560 px long edge, distance 0.05, effort 3**, stored in an owned SHA-256 tree.
  Encoder policy is part of the key; older distance-0.03 previews regenerate
  lazily. Default RAM budget is 2048 MiB, split equally between complete frames
  and decoded previews; the disk limit defaults to 1024 MiB under Temp.
- Display cached previews as bounded source textures positioned/scaled by GPUI,
  removing a viewport-sized CPU interpolation pass. Full RAW development remains
  in the background. Historical distance-0.03 D4 measurements and their limits are
  documented in [the latency investigation](cache-preview-latency.md).
- Keep thumbnails independent and lazy for every supported file, including
  images below the display-preview threshold. Selected images and visible rows
  take priority over background work.
- Add **Settings → Windows Open with → Register / Remove** for all 27 supported
  extensions. Registration is explicit, per-user and administrator-free; startup
  never registers automatically and default apps are not changed. Register again
  after moving the portable folder. Unrelated registrations and manually changed
  commands are protected.
- Produce versioned portable archives, checksums and clean-commit/build-run
  provenance in the manual Windows CI workflow. Release assets come from the
  successful CI run, without substituting a local rebuild.

## Correctness and limitations

Display previews are lossy and never replace complete authoritative RGB16 data
for Diff, High pass, Statistics or Color picker. Source images remain unchanged.
The exact Redlamp LibRaw fork, JPEG XL/ONNX Runtime pins, LPIPS model and default
decoder features are unchanged. Cache budgets are not hard process-memory limits.
See [cache behavior](cache-settings.md) and the [manual](manual.md).

Local automated/native checks and their limits: [0.3.3 validation](release-0.3.3-validation.md).

## Windows portable package

Extract `Scrutivue-0.3.3-windows-x64.zip` completely. Keep `onnxruntime.dll`, `models/`
and the complete `libjxl/` directory beside `scrutivue.exe`. Python and 7-Zip are not
needed at runtime. The Microsoft Visual C++ x64 runtime may be required.

`SHA256SUMS.txt` covers the ZIP and `release.json`; the ZIP embeds the same
provenance at `Scrutivue/build-info/release.json`, including source commit and
GitHub Actions run URL. The application is unsigned; Windows SmartScreen may warn.
This release is prepared as an **unpublished draft** for review.
