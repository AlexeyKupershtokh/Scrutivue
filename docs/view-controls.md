# Floating controls and reduced-view panning

**0.3.2 status:** these controls and progressive NEF preview are now included in
the normal application. See [0.3.2 release notes](release-0.3.2.md). The isolated
build paths and validation below describe the preceding local prototype.

The original local experimental build includes these view-only changes alongside the
existing [NEF preview experiment](nef-progressive-preview.md):

- Statistics is **288 logical pixels** wide instead of 346. Analysis-size
  controls wrap when necessary; the two noise-value columns remain aligned.
- Images can be dragged at every zoom, including Fit and reduced views. The
  viewport centre stays within the reference image's normalized `[0, 1]` bounds
  on both axes. Empty space at image edges is intentional: details can be moved
  away from floating controls. The same rule applies after zooming, resizing,
  changing sources and using the Navigator. `F` / `0` recentres and fits the image.
- The color picker has one **56-pixel-high continuous swatch**: A on the left,
  B on the right. It has no gap, internal outline or rounded inner edge. Without
  B, A fills the whole field. Coordinate labels remain above the colors, and the
  numeric values and swatches still use original developed RGB16, not Diff or
  High pass output. The loupe and its independent 1600% zoom are unchanged.

No source images, decoder settings, dependency pins, model payloads or normal
0.3.1 application/release artifacts are changed by these UI adjustments.

## Validation

- 175 unit tests passed with the experiment (20 optional tests ignored).
- 168 passed with normal defaults (18 ignored); 163 without defaults (16 ignored).
- All three native DX12/CPU parity suites were explicitly run and passed,
  including panned Fit/reduced views, image-edge centres, A/B with unequal
  dimensions, all display modes and High pass. Render tolerance remains one
  eight-bit display step; authoritative source pixels are not changed.
- New checks cover reduced-view picker coordinates/background rejection,
  Navigator clipping, finite camera limits and joined-swatch geometry at
  fractional DPI scales.
- A targeted native 2x-DPI GUI check passed Fit panning with both mouse buttons,
  reduced-view panning at 44.36%, the `[0, 1]` boundary, Fit reset, open Statistics,
  paired picker input and persisted panned camera state. There were 162 event-loop
  probes, zero 100 ms timeouts, maximum 4.58 ms.
- All six actual NEFs still produce early snapshots and complete RGB16 hashes
  matching 0.3.1; source hashes, sizes and modification times remain unchanged.

Screen capture was blocked by an obscured desktop, so native input/state checks
are not a visual assertion of the card layout or color boundary. The earlier
broader NEF experiment's intermittent GUI-test caveats also remain unresolved;
this work does not claim to fix them.

## Original local build (before 0.3.2)

`target/release/scrutivue-nef-progressive.exe` is the updated experimental EXE.
Its predecessor is preserved under `target/ui-polish-review/previous/`, and the
previous experiment ZIP is unchanged. The new portable package is
`target/ui-polish-review/Scrutivue-NEF-preview-ui-windows-x64.zip`; extract its
entire folder and run `scrutivue-nef-progressive.exe` with its included runtime,
external model and JPEG XL assets.

The original isolated UI work did not commit, push, dispatch a remote build or
update a release. Later source publication and promotion are tracked in 0.3.2.
