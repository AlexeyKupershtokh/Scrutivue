# Actual-image partial-decoding experiments

Developer CLI probes only. Nothing changes the production viewer, its LibRaw
pin, source images, repository-root binaries or manual Windows workflow.
Embedded JPEG previews are **thumbnail-only**, never a substitute for partially
decoding the main image.

See [the actual-corpus report](../../docs/corpus-partial-decoding-experiments.md)
and [the initial library-capability report](../../docs/preview-decoding-experiments.md).
The latter explicitly separates synthetic controls and the original two-DNG
scope from the expanded 30 DNG / 30 JPEG / six NEF scope.

## Programs

| File | Scope |
| --- | --- |
| `capabilities.rs`, `dng_regions.rs` | Exact raw DNG tile ROI; monolithic oxide ROI/expansion; incremental rendering; current full Rust JPEG baseline. |
| `native_progressive.rs` | Public libjxl progressive events, full-size flushes, exact same-decoder completion; deterministic controls. |
| `jxl_downsampling_review.py` | djxl-compatible one-flush/pass-selection policy on actual tiles and positive controls; see the [follow-up correction](../../docs/jxl-downsampling-review.md). |
| `jpeg_probe.cpp` | Native libjpeg-turbo reduced IDCT and scanline ROI with explicit stage timing. |
| `jpeg_corpus.py` | All 30 actual JPEGs, scales and padded ROI; same-backend full-output comparisons. |
| `jpeg_current_corpus.py` | Current `image`/zune full-decoder timings on all 30 actual JPEGs. |
| `dng_corpus.py` | All 30 actual DNGs, five ROI geometries, selected-tile progression and raw equivalence. |
| `nef_libraw.cpp`, `nef_corpus.py` | Exact pinned LibRaw full/half/cropbox, quarter-after-half resize and retained-RAW reuse. |
| `nef_stripes.cpp`, `nef_stripes.h` | Source-level selective Nikon HE/HE* stripe reconstruction, not a public LibRaw API. |
| `nef_alternatives.py` | Existing pinned Imprint C++ DLL and supplied original-size IPX helper. |

## Execution rules

- Run builds and decoder matrices **sequentially**, not simultaneously. Decoder
  workers may use their explicitly bounded inner thread budget.
- All generated artifacts/evidence belong under `target/`; choose a new result
  directory rather than overwrite earlier measurements. Never commit EXE/DLLs.
- Inputs are read-only and source identities are checked. Some probes hash all
  source bytes after the decode-ready interval; their external process wall time
  therefore is not pure decode time and can warm mapped pages afterward.
- Exactness is established only in the stated backend/scale/color/raw domain.
  Reduced display output is not authoritative source data. Raw ROI is not yet
  equivalent to developed, cropped and oriented display RGB.
- Windows process peak working set/private commit is not codec-only allocation.
  Mmap creation is not a completed source read, and cache-warm I/O is not a cold
  drive measurement. API-combined stages remain explicitly combined.

## Build and run

Use the existing Windows x64/MSVC environment and prepared dependencies; no
production `Cargo.toml` or pin change is needed:

```powershell
python scripts/build_dng_experiments.py
python scripts/build_dng_experiments.py --preview
python experiments/preview/nef_corpus.py --build

# Run each command only after the previous matrix/build has finished:
python experiments/preview/jpeg_corpus.py --build --run-id NEW_JPEG_RUN
python experiments/preview/dng_corpus.py --run --output target/preview-dng-corpus/NEW_DNG_RUN
python experiments/preview/nef_corpus.py --directory 'D:/Photos/OriginalNEFs' --repeats 3 --output target/NEW_NEF_RUN
python experiments/preview/nef_alternatives.py --action probe --backend both --runs 3 --libraw-references target/NEW_NEF_RUN --output target/NEW_NEF_ALTERNATIVES
```

The corpus runners' documented defaults select only the user-supplied local
corpus and trusted existing native libraries. Consult each `*-notes.md` for exact
API boundaries, input identities, commands and validation status. No native
runtime redistribution is implied.
