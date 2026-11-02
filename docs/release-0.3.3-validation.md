# Scrutivue 0.3.3: local validation

Validated on Windows x64 on **2026-11-02**. These results describe the local
release candidate; distributed assets are built separately by Windows CI from
the committed/tagged revision, with their own embedded provenance.

| Check | Result |
| --- | --- |
| Formatting and diff whitespace | Passed |
| Default-feature tests | 197 passed, 22 ignored |
| No-default-feature tests | 185 passed, 18 ignored |
| Explicit native distance-0.05 JPEG XL roundtrip/corruption/invalidation | Passed |
| Explicit native GPU/CPU comparisons | 3 passed |
| Python CI provenance / JXL runtime / model guards | 6 / 13 / 3 passed |
| Normal locked/offline release build | Passed |
| Standalone CLI, native tiled DNG and external LPIPS smoke | Passed |

Seven isolated registry tests cover the shared 27-extension list, Unicode and
quoted command lines, repeat registration/removal, preserving other viewers and
defaults, foreign registrations, moved copies, edited commands, and refusal to
remove a handler subsequently selected as a per-user default. Test registry trees
are separate from production associations and are cleaned after each test.

## Native Windows integration

The real Settings **Register / Remove** buttons were exercised against the normal
release EXE with an isolated settings profile. Startup and opening Settings did
not register automatically. Register created the quoted absolute executable
command with `-- "%1"`; Windows `SHAssocEnumHandlers` independently enumerated
Scrutivue for **all 27 supported extensions**. Remove deleted the owned handlers
and candidate values while preserving defaults, other candidates and the user's
preferences. The production registration was absent again after testing.

A native Settings screenshot was captured and inspected, including the 0.05
policy text and the Register/Remove section. Standard-size captures also verified
that scrolling reaches the new controls while Apply/Close remain pinned. The button/registry run recorded 18
WM_NULL responsiveness probes with no 100 ms timeouts; this is not a physical
presentation-latency measurement.

## Cache and frame correctness

The extended native Settings/cache integration passed against the 0.05 release:
Apply/reopen/restart persistence, Close discarding pending edits, Prefetch and
build-all, exactly three eligible previews including the 10,000,000-pixel boundary,
2560 px long edges, payload checksums, all four independent thumbnails, disk-budget
enforcement, and reset preserving unrelated files/thumbnails and targeting the
applied rather than edited location. The isolated cache was explicitly warmed
with the new policy, not treated as a hit from older 0.03 files. Its cached preview
was accepted before full development; 62 responsiveness probes had no 100 ms
timeouts. This integration used `--no-capture` and does not establish new timings
for the historical D4 performance table.

Seven D4 final-frame RGB16 hashes, dimensions, sample sums, byte sizes and thumbnail
dimensions match the earlier baseline. Source hashes for those files and size/
mtime records for all **499** corpus files are unchanged. Exact decoder/runtime/
model pins and default features remain unchanged.

## Local build identity

The prior normal EXE/runtime/model assets were preserved under
`target/release-0.3.3-validation/previous-release-0.3.2/`. The new normal build
explicitly used the main checkout's pinned `vendor/libraw` through
`DIFF_LIBRAW_SOURCE`; the EXE contains no experimental `AB_METRIC ` marker. Only
the existing unused `crop`/`unpack` warnings remain.

Local candidate: **37,673,472 bytes**, SHA-256:

```text
d23833b1849fbe376d17a19c7b90baef533f2355a348bdfccb9b3c1ed08abc63
```

This local EXE is **not substituted for the CI release archive**. Generated logs,
native screenshots, frame probes and integration reports are under
`target/release-0.3.3-validation/`; no generated EXE/DLL or evidence is committed.
Repository-root application binaries and the published 0.3.2 release are untouched.
