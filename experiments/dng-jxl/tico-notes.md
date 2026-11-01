# Local intoPIX recognition probe (not a JPEG XL decoder)

## Scope and source evidence

`tico_probe.cpp` is a standalone Windows MSVC x64 Release executable. It reads
an already extracted, complete DNG tile payload; it does not parse or modify
DNGs or original photographs. It calls **only `IsIpxData`**, through the exact
function pointer and decorated export already used by:

`<pipeline-graph>/native/dxo_ipx_helper.cpp` in the user-supplied local project.

- Lines 25–27: x64/MSVC Release container ABI checks.
- Lines 155–186: `bool (__cdecl*)(const std::vector<unsigned char>&)` and the
  `?IsIpxData@@YA_NAEBV?$vector@EV?$allocator@E@std@@@std@@@Z` export.
- Lines 269–275: bounded input read, minimum 32-byte header, expected
  `CONTACT_INTOPIX` Nikon marker, then recognition. This probe retains the
  minimum length but deliberately does **not** pre-reject a non-IPX marker:
  the experiment must actually ask the recognizer about a JXL tile.
- Lines 352–365: read lock, SHA-256 pin and restricted `LoadLibraryExW`.
- Lines 432–440: preserving the JSON/protocol stdout handle and redirecting
  vendor diagnostics to stderr.

Additional evidence:

- `crates/raw/README.md`, lines 55–87, explicitly limits the verified adapter
  to Nikon Z8 HE/HE* NEF IPX and disclaims proprietary redistribution rights.
- `crates/raw/src/ipx.rs`, lines 23–25 and 133–165, pins the helper, framework,
  manifest and complete runtime file set. This probe does not launch the helper.
- The existing helper's `DecodeIpxData` use is **not a verified JPEG XL ABI**.
  This program never resolves it, constructs a foreign image, allocates pixel
  output, extracts keys, patches code, or attempts another private entry point.

No broader claim that every intoPIX product or every part of the framework
lacks JPEG XL is established by this narrowly scoped test. If a different
TicoRAW/TicoPIX/JPEG XL library was intended, its identity and documented API
are still needed. No new runtime is downloaded or redistributed here.

A bounded filename search within the supplied `pipeline-graph` found no
`*tico*` files. Its JPEG XL implementation is instead
`crates/io/src/jxl.rs` (documented public libjxl C ABI), with an existing
`resources/native/libjxl-0.12.0/jxl.dll` bundle and another
`resources/native/codecs/jxl.dll`. This supports keeping the IPX recognition
experiment separate from the parent's actual libjxl decode benchmark.

## Existing runtime, identity and dependencies

Discovery location (read-only; pass it explicitly, no application search):

```text
<pipeline-graph>/resources/native/ipx/dxo-6.8.0.7/DxOFramework.dll
```

Local read-only inspection confirmed:

| Item | Identity |
| --- | --- |
| Framework bytes | `142098568` |
| Framework SHA-256 | `f3064b4bf321402e6cce16388e135bb6b15de824ed717a6074a53784c9c662c0` |
| Manifest bytes | `2510` |
| Manifest SHA-256 | `59aa78cce27ee5a0b2ac36d767e75da4f5083e6431f2e422f1e060fa0dc4321b` |
| Manifest runtime label | `6.8.0.7` |
| Framework FileVersion / ProductVersion strings | `26.13.0.260928112933` |

The manifest label is **not** the DLL's FileVersion string. The probe checks
the exact file hashes/sizes, verifies the FileVersion string, and holds
share-read-only handles until after framework unload. Its fixed pin table
matches the separately pinned manifest, avoiding a general JSON parser or
trusting an editable manifest. The other pinned directory files are
`raw_r.dll`, `concrt140.dll`, `heif.dll`, `msvcp140.dll`,
`msvcp140_atomic_wait.dll`, `vcruntime140.dll`, and `vcruntime140_1.dll`.
Every listed local file was independently size/hash-checked during inspection.
The five Microsoft runtime files report `14.44.35211.0`; `raw_r.dll` and
`heif.dll` have no FileVersion string. Their full hashes are in the probe table
and the existing local manifest.

Loading permits only the DLL's directory and Windows System32. Windows may
reuse an already resident process CRT instead of the directory's CRT; the
probe therefore reports the effective loaded paths of those named modules
and does **not** claim that all effective dependencies necessarily match the
local pins. No DLL is copied next to the probe. The manifest lists `LIBAF.dll`
and `MFPlat.DLL` as delay imports not required for IPX recognition/decoding;
there is no attempt to source missing optional libraries.

## Build and invocation

Use an existing **x64 MSVC Developer** environment, with compatible MSVC STL.
Do not use MinGW, a debug CRT, `_ITERATOR_DEBUG_LEVEL=2`, or `/MT`.
The parent benchmark owns compilation and representative tile runs. This
scope does not build the application or modify external resources.

Example, assuming `target/dng-jxl` already exists:

```powershell
cl.exe /nologo /std:c++17 /O2 /EHsc /MD /W4 /DNDEBUG /D_ITERATOR_DEBUG_LEVEL=0 /Fe:target/dng-jxl/tico_probe.exe /Fo:target/dng-jxl/tico_probe.obj experiments/dng-jxl/tico_probe.cpp /link bcrypt.lib version.lib
& ./target/dng-jxl/tico_probe.exe --self-test
& ./target/dng-jxl/tico_probe.exe --framework 'D:/Tools/pipeline-graph/resources/native/ipx/dxo-6.8.0.7/DxOFramework.dll' --payload 'D:/Scrutivue/target/dng-jxl/tile-000000.jxl'
```

Explicit non-default link requirements are **`bcrypt.lib`** (SHA-256) and
**`version.lib`** (version resources), plus the usual Windows/MSVC defaults.
No proprietary import library or helper executable is linked or shipped.
Use separate processes with a parent-enforced timeout: this is not a sandbox,
and C++ exception handling does not recover from native access violations.

## JSON and timing interpretation

- Stdout is one JSON object; native diagnostics go to stderr.
- Exit `0` means a completed recognizer query (including **false**), or a
  passed explicit `--self-test`. It never means JPEG XL decoding succeeded.
  Exit `2` means a probe/validation error; `3` means reporting setup/write failed.
- `ipx_recognized` is `false`, `true`, or `null` if not called. False yields
  `status: "rejected_by_ipx_recognizer"`. An unexpected true yields only
  `"recognized_as_ipx_not_decoded"` and never triggers a decode submission.
- Always: `native_decode_attempted: false`, `native_decode_success: null`,
  `decode_seconds: null`, `jpeg_xl_decode_tested: false`.
- `payload_signature` reports only the standard raw-JXL/container signature;
  it is not a full JPEG XL validation. Parent DNG extraction/libjxl establish
  actual compression semantics.
- Separate seconds: runtime manifest/dependency size+hash verification,
  framework version-resource read, payload open/lock, explicit vector reserve,
  vector resize/zero-initialization (first-touch proxy), `ReadFile`, payload
  SHA-256, DLL load, post-load setup (loaded-path identity/dependency inspection
  and symbol lookup), exact symbol lookup, and the single recognizer call.
  Symbol lookup is nested inside post-load setup; do not sum both fields.
- `payload_first_touch_seconds` is not guaranteed cold physical page-fault
  cost: the allocator may reuse committed pages. It zeros the complete live
  byte vector before the separately timed read. Reserve may itself touch
  allocator metadata. No decoder-output allocation exists in this probe.
- `payload_read_seconds` is normal **buffered OS-cache** I/O into the pre-sized
  vector, not a cold-disk benchmark. DLL/hash verification can warm the runtime
  before load; all such work stays outside `recognize_seconds`. Do not compare
  that recognizer duration or historical NEF decode times to libjxl decode
  throughput as if they measured equivalent work.

## Observed JPEG XL recognition results

The standalone MSVC x64 build and `--self-test` passed. The parent matrix then
extracted original tile indices **0, 363 and 725** from each requested DNG
(`AK8_0005_halo0.dng` and `AK8_0005_halo24.dng`). All **six** actual native
`IsIpxData` calls returned **false**, with the complete pinned runtime verified.
Each query took 3.1–3.4 microseconds; runtime verification was 84.7–89.3 ms and
DLL loading 12.3–13.9 ms. These are recognition/setup times, **not decode times**.

No decode was attempted, no pixels were returned and no foreign image ABI was
guessed. This confirms rejection by this specific Nikon IPX entry point for the
tested actual JXL payloads, not a product-wide statement about all intoPIX APIs.
Full local observations: `target/dng-jxl-results-20261101/results.json`.
