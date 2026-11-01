# Third-party notices

LibRaw is used under its CDDL-1.0 option. Copyright and full licence are in this
directory. The unmodified source used to produce the executable is:

https://github.com/pdcgomes/redlamp-libraw/tree/f4956c404fba0c6035ce9e06353d4b4ffe068fc7

The source archive, SHA-256 and preparation command are recorded in
`config/libraw.json` and `scripts/prepare.py`. This includes Redlamp's Nikon HE/HE*
decoder; do not replace it with an ordinary upstream LibRaw binary.

`native/parallel_raw.cpp` and `native/parallel_raw.h` adapt orchestration and
processing loops from that exact source under the same CDDL-1.0 option (also
offered under LGPL-2.1). They retain Dmitri Sotnikov's Nikon HE and LibRaw LLC's
processing attribution. The original vendor source remains unchanged. Windows
ZIPs include these adapter sources under `build-info/native/` and their notice
in `LICENSES/LibRaw-adapters.txt`; the application adds no OpenMP runtime or
replacement RAW decoder.

GPUI, GPUI Kit and their GPUI snapshots retain their Apache-2.0 notices in their
crate distributions. wgpu is MIT/Apache-2.0. Cargo.lock records exact versions of
all Rust dependencies. The Windows packaging script includes the published
Rust crates' supplied license/notice files and manifests, a JSON dependency
inventory, and supplemental upstream notices for archives that omit them.
The inventory includes transitive dependencies and the vendored zlib notices.

Native SSIMULACRA2 uses rust-av/ssimulacra2 0.5.1 (BSD-2-Clause), with
its transitive notices in Cargo distributions. jxl-oxide is MIT/Apache-2.0.
ort is MIT/Apache-2.0; Microsoft's ONNX Runtime is MIT. Include the official
runtime's LICENSE and ThirdPartyNotices.txt from vendor/onnxruntime when
distributing its DLL.

Windows distributions include the separate libjxl 0.12.0 bundle
pinned in `config/libjxl.json` (BSD-3-Clause, with its dependency notices).
`scripts/prepare_jxl_runtime.py` fetches the official Windows x64 release archive,
verifies its pinned SHA-256 and each DLL/notice, and retains the upstream licenses
under `target/<profile>/libjxl/licenses`. The Windows packager requires all five
pinned DLLs and all ten supplied upstream notices. Downloading is build-time only;
the application does not fetch runtime components. No Redlamp LibRaw substitution
is involved.

The distributed `models/lpips-squeeze-v0.1.onnx` graph combines official LPIPS 0.1.4 learned Squeeze v0.1
weights (richzhang/PerceptualSimilarity, BSD-2-Clause) and torchvision's
SqueezeNet1_1_Weights.IMAGENET1K_V1 (BSD-3-Clause). Their full notices are
LPIPS.txt and torchvision.txt. Provenance, exporter versions and SHA-256 are
in assets/models/lpips-squeeze-v0.1.json. Python/PyTorch is used only for
development export/reference tests and is not part of the application runtime.
