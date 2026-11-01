# Windows CI cost and portable releases

## Last observed Actions run

Reviewed on 2026-11-01. The last public run was
[Windows #1, 41228737944](https://github.com/AlexeyKupershtokh/Scrutivue/actions/runs/41228737944),
commit `1e9874c9fa990502482c6c8de2aa77f9c81adf2a`, on 2026-10-31.
Its build job succeeded in **30 minutes 52 seconds**; the overall run was
approximately 31 minutes including scheduling.

| Step | Duration |
| --- | ---: |
| Checkout | 7 s |
| Rust installation | 8 s |
| Native library preparation | 7 s |
| Formatting | 6 s |
| `cargo test --locked` | **19 min 27 s** |
| `cargo build --release --locked` | **10 min 39 s** |
| CLI smoke | under 1 s at the API's timestamp precision |
| Packaging and archive validation | 9 s |
| Artifact upload | 3 s |

Evidence: the public
[job/step API](https://api.github.com/repos/AlexeyKupershtokh/Scrutivue/actions/runs/41228737944/jobs)
and the full run log downloaded after the owner completed GitHub authentication.
There were **no cache steps**, and the repository's Actions cache API reported
**zero entries** when reviewed. The Cargo log confirms:

- `Finished test profile [optimized + debuginfo] ... in 19m 25s`.
- The tests themselves: **108 passed, 4 ignored, 0.73 seconds**.
- `Finished release profile [optimized] ... in 10m 38s`.
- **496 distinct crate/version entries were compiled in each profile**, with all
  496 appearing in both, including GPUI, wgpu, image, JXL, ONNX Runtime bindings
  and the application itself.

The runner image was `windows-2025-vs2026`, image version `20261021.278.1`, runner
version `2.338.2`. The raw archive and ANSI-stripped log are retained locally under
`target/ci-review/`; credentials were not saved or printed. The old test count
belongs to that earlier revision, not the latest 126-test implementation.

The workflow built two different Cargo profiles. Development dependencies use
optimization level 2, while release uses optimization level 3, thin LTO and one
codegen unit. Cargo cannot generally reuse those compiled libraries across the
two profiles. Native library downloads and ZIP upload were not the bottleneck.

## Changes made without spending Actions minutes

The workflow remains **manual-only** (`workflow_dispatch`), including after a
push or tag. No remote run was dispatched for this review.

- Run `cargo test --release --locked` before `cargo build --release --locked`.
  Dependencies and native build outputs share the release profile; the test
  harness and ordinary application still require distinct final builds.
- Cache Cargo registry/Git dependencies and **only `target/release`**, rather
  than experimental corpora, screenshots, fixture directories or all of `target`.
  This also keeps the compiled native outputs in Cargo's release build directory.
- Use a compatibility key containing the Rust compiler identity, Visual Studio
  installation version, architecture, dependency/build configuration and commit.
  Restore the closest compatible previous commit, with a broader same-toolchain
  fallback for dependency changes. Cargo still checks freshness and rebuilds what
  changed; **a cache hit never skips tests or packaging validation**.
- Cache pinned LibRaw/JPEG XL sources and ONNX Runtime separately. Their exact pin/preparer
  key has no broad restore fallback; preparation still runs after restoration.
- Save the native cache as soon as preparation succeeds, and useful Cargo outputs
  even if subsequent tests/packaging fail. Do not save from a cancelled run.
- Pin the official `actions/cache` v5 implementation by commit. Retain the existing
  read-only repository permissions and two-worker build budget.
- Do not recompress the already compressed application ZIP during artifact upload.
  Limit a job to 30 minutes rather than the previous 60.

The **first cache-populating run is still a cold build**. Toolchain changes and
cache eviction can also make later runs cold. Checkout timestamps may cause the
workspace/native build script to run again, and final thin-LTO linking, archive
restoration and saving still cost time. A warm few-minute run is a target, **not
a measured two-minute guarantee**. Cache size/transfer time must be measured
before adding more cache layers. Do not change release quality settings or drop
tests merely to achieve a headline number.

For scale, the current local `target/release` tree was approximately **2.35 GiB
before compression** (1.95 GiB in `deps`, 0.30 GiB in `build`). This is an
accumulated developer checkout, not a fresh-runner cache-size measurement; it is
another reason to measure restore/save overhead rather than promise two minutes.

Local validation of the new workflow: YAML/trigger/permission/action-pin checks
passed, all PowerShell blocks parsed, and the compiler-identity step executed
successfully on Windows. `cargo test --release --locked --offline --no-fail-fast`
passed **126 tests, 6 ignored** (test execution 0.98 s; the new release test harness
took 1 min 32 s to compile/link with existing dependencies). Hosted cache hits and
remote end-to-end duration have **not** been validated yet.

### Bounded next experiment

After pushing and explicitly choosing to spend the minutes:

1. Manually run Windows once to populate caches; do not launch parallel variants
   or repeatedly retry a cold build. Its hard job limit is 30 minutes.
2. Re-run that revision once with warm caches, and record cache hit, restore/save
   durations, test compilation, application linking and ZIP validation separately.
3. If warm time is still excessive, stop rather than trying a long matrix. Inspect
   Cargo timings first. Consider a separately versioned native static-library cache
   or sccache only if native rebuilds are the demonstrated remaining bottleneck.
4. Keep ordinary pushes and tags free of automatic Windows builds. A persistent
   trusted Windows build runner is another option, but needs isolation, maintenance
   and explicit approval; it is not a free speedup for public pull requests.

## What Redlamp actually publishes

The reference [Redlamp releases](https://github.com/pdcgomes/redlamp/releases)
contain versioned application ZIPs and DMGs, separate macOS debug symbols, release
notes and a Sparkle `appcast.xml`. For example `v0.3.4-alpha` includes
`Redlamp-0.3.4-alpha.zip` and `Redlamp-0.3.4-alpha.dmg`.

Its public [release task](https://github.com/pdcgomes/redlamp/blob/main/mise/tasks/release)
builds a selected committed revision in a clean worktree, signs/notarizes it,
checks the packaged app, creates a tag and publishes assets with
`gh release create`. This is **not** a release job in its ordinary public CI
workflow. macOS signing, DMG creation and Sparkle are not requirements for our
Windows portable ZIP.

## Scrutivue release process

Start with a manually approved **draft prerelease**, not an automatically published
release on every push. Do not create a tag or public release without choosing a
version and approval. The initial review created no release/tag; the owner later
published 0.3.0. The explicitly approved [0.3.1 draft](release-0.3.1.md) follows
the clean-revision local-build path below, without altering 0.3.0 or triggering CI.

1. Choose an immutable version/tag that matches `Cargo.toml` and the executable's
   version resource. Build from the exact clean committed revision; never attach
   an arbitrary stale `target/release/scrutivue.exe` to a new tag. Record source SHA,
   toolchain, dependency pins and build/validation results.
2. Reuse the **same tested package** from a successful build instead of triggering
   another full compilation to publish it. Initially this can be a validated local
   Windows release, as Redlamp's release task is separate from hosted CI. Later a
   small manual publication job can download an explicitly selected successful
   workflow run's artifact, verify its revision and attach it without rebuilding.
3. Publish `Scrutivue-<version>-windows-x64.zip` and a `SHA256SUMS.txt`. The ZIP
   contains the executable, its matching ONNX Runtime, pinned native JXL runtime,
   notices/licenses and build provenance. Keep all generated files under `target/`.
   The existing `scripts/package_windows.py` and `scripts/test_package.py` already
   provide a portable ZIP plus an extracted-binary/LPIPS validation path; versioned
   filenames, provenance and checksum publication are the next small additions.
4. **Native JXL distribution is now configured:** the official Windows x64
   `v0.12.0/jxl-x64-windows.zip` exactly matches the existing audited DLL/notice
   hashes. `config/libjxl.json` records its URL, whole-archive SHA-256 and all ten
   notice hashes. The hosted workflow prepares/caches it, then installs it after
   Cargo cache restoration. Packaging requires the five pinned DLLs and notices;
   the extracted executable must actually decode a generated tiled DNG through
   the native backend. There is no new startup download, arbitrary latest-DLL
   selection, release publication or automatic Actions trigger.
5. Create an annotated tag and a draft release with the ZIP, checksums, concise
   changes, supported Windows/CPU architecture and unsigned-build/SmartScreen
   caveat. Review/download-test the draft before publishing it as Latest.
   Release asset publication requires `contents: write` only for that explicit
   publication operation/job; normal Windows validation stays read-only.
6. Add a README link to `releases/latest` after the first approved published
   release. Source-code ZIP/TAR links generated by GitHub are **not** application
   binaries. Keep PDB symbols as a separate optional asset, not in the user ZIP.

For a future automatic updater, design signature verification and rollback
separately. A SHA-256 file on the same release helps detect corrupt downloads but
is not a substitute for signed updates or Windows code signing.

## Native JXL distribution validation, 2026-11-01

The official source archive was downloaded and independently compared with the
prior local runtime: all five DLLs and ten notices matched, with no DLL pin changes.
The new online preparation path and offline pinned-archive path both passed local
download/extraction/installation checks. Thirteen isolated Python tests cover
missing/corrupt notices and DLLs, non-overwriting installation, failed/partial
downloads, archive identity/entry validation and mandatory packaging.

The release build and its headless native tiled-DNG/statistics/LPIPS smoke passed.
`scripts/package_windows.py --output target/libjxl-distribution-review/Scrutivue-windows-x64.zip`
produced a portable ZIP, and `scripts/test_package.py --archive` verified its
embedded pins/notices and ran the extracted executable's native decode plus LPIPS
checks. Records are under `target/libjxl-distribution-review/`. No new hosted
Actions run was dispatched for this validation.
