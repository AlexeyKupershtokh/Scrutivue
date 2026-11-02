# Settings and caches

Open **Settings** from the floating controls to edit cache preferences in a separate window. **Apply** takes effect without restarting and persists through the existing version-1 settings profile. **Close** or Escape closes the window without applying pending edits. Cache reads, source decoding, preview generation, pruning and clearing run on background workers; source images remain unchanged.

## Defaults

| Setting | Default | Meaning |
| --- | --- | --- |
| Total RAM cache | 2048 MiB | Half for complete RGB16 frames; half for decoded display previews |
| Prefetch | Off | Read neighboring full frames after selected frames are ready |
| Maximum disk size | 1024 MiB | Limit for disk display previews and their sidecars; 0 disables disk storage |
| Location | System Temp / `Scrutivue` | Parent of the owned `scrutivue-previews-v1` directory |
| Build previews for all added files | Off | Extend lazy preview work to the entire added library during idle time |

The RAM budget is an application cache budget, **not half the computer's physical RAM**. Selected A/B frames remain protected even when they exceed the frame-cache allowance. Decode temporaries, queued handoffs, viewport images, thumbnails, analysis caches and GPU allocations are additional working memory; this setting is not a hard process-memory limit.

The total RAM setting accepts 64–1048576 MiB; the disk setting accepts 0–1048576 MiB. Disk locations must be absolute paths. Settings displays validation errors before applying invalid input.

## Lazy display previews

- Only sources with at least **10,000,000 pixels** are eligible. For example, 3200×3125 qualifies; 800×600 does not.
- Previews are developed sRGB16 images resized to a **2560 px long edge**, encoded by the existing pinned JPEG XL runtime at **distance 0.05**, **effort 3**. Rounded dimensions preserve original source geometry when displayed: a 5520×8280 image produces a 1707×2560 preview.
- Viewing or enabled neighbor prefetch offers complete frames for lazy preview creation. Enabling **Build previews for all added files** also schedules eligible files that have not been viewed. It does not eagerly decode the whole library at once.
- Prefetch supports Single and paired navigation. Selected frames take priority, and Statistics suppresses speculative neighbor decoding. Disabling Prefetch does not disable viewed-file preview creation or the independent build-all option.
- Cached previews can appear while the full frame is still loading in Single/Slider. They are lossy, display-only copies: **Diff, High pass, Statistics and Color picker still require complete authoritative frames**. Preview geometry and zoom use original source dimensions, not preview pixel dimensions.
- Decoded previews are converted to bounded source textures on a worker. GPUI scales, positions and clips these textures directly on its rendering device, without a separate CPU interpolation pass over every window pixel. This also keeps preview-buffer size independent of viewport size. Full source decoding still runs in the background; the cache improves the first display, not the RAW decoder itself. [Nikon D4 latency investigation](cache-preview-latency.md).
- Complete RGB16 frames and decoded previews have separate byte-bounded RAM LRUs. Disk previews are decoded into RAM only when requested; missing, incomplete or corrupt previews fall back to normal source decoding.

## Storage and reset safety

The selected location is a parent containing:

```text
scrutivue-previews-v1/
  .cache-owner
  aa/bb/<sha256>.jxl
  aa/bb/<sha256>.json
```

Keys include the canonical source path, file size, modification/creation times, processing policy and pinned LibRaw configuration. Sidecars record original/preview dimensions and the JPEG XL payload's SHA-256. Source revision checks prevent stale writes; writes use temporary files and atomic replacement.

Since 0.3.3, keys incorporate the encoder's distance and effort constants directly. Older distance-0.03 previews are not reused as 0.05 previews: replacements regenerate lazily. Old owned files remain subject to the disk limit and explicit cache reset; no source images or thumbnails need migration.

Disk hits update recency; background pruning removes the least recently used generated files to meet the applied limit. Pruning also recognizes owned temporary files. A nonempty unmarked cache directory is refused, and linked cache roots/shards/files are not used as cache deletion targets.

**Clear disk preview cache** resets generated preview files in the **currently applied location**, even if the location input contains unapplied edits. It preserves the ownership marker, unrelated files, source photographs and thumbnails. With lazy building still enabled, later viewing/prefetch/idle work can refill the cache.

Changing the location does not migrate or delete the old cache. Revisited or background-built files are persisted at the new location even when their preview was already in RAM. Clear the old location explicitly if it is no longer needed.

## Independent thumbnails

Every supported file can receive a lazy thumbnail, including images below the 10 MP preview threshold. Visible library rows take priority; remaining rows are warmed gradually in the background. RAW thumbnail extraction uses an embedded preview rather than full development when available.

This separate cache retains the existing application cache location, `--cache-dir` override, marked `scrutivue-v1` directory and approximately 512 MiB disk limit. Its in-memory LRU holds up to 1024 thumbnails. Changing or resetting display-preview storage does not relocate or clear thumbnails.

See also the [manual](manual.md) and [historical loading measurements](loading-performance.md). Those measurements describe their recorded builds, not a new performance guarantee for all cache configurations.

## Validation

```powershell
cargo fmt --all -- --check
cargo test --locked --offline --bin scrutivue
cargo test --locked --offline --no-default-features --bin scrutivue
cargo test --locked --offline --bin scrutivue cache::tests::native_jxl_disk_roundtrip_corruption_and_source_invalidation -- --ignored --nocapture
cargo test --locked --offline --bin scrutivue gpu_matches_cpu -- --ignored --nocapture
cargo build --release --locked --offline --bin scrutivue
```

The explicit JPEG XL test needs the prepared pinned runtime under `target/release/libjxl`; GPU checks require a working native backend. Cache tests cover the exact area threshold, byte budgets, source revisions, ownership/reset safety, cancellation, rounded preview geometry and persistence after a location change. Native encoding also checks real sRGB16 roundtrip, payload corruption and source invalidation. [Local implementation validation](cache-settings-validation.md).
