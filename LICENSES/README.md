# Third-party notices

LibRaw is used under its CDDL-1.0 option. Copyright and full licence are in this
directory. The unmodified source used to produce the executable is:

https://github.com/pdcgomes/redlamp-libraw/tree/f4956c404fba0c6035ce9e06353d4b4ffe068fc7

The source archive, SHA-256 and preparation command are recorded in
`config/libraw.json` and `scripts/prepare.py`. This includes Redlamp's Nikon HE/HE*
decoder; do not replace it with an ordinary upstream LibRaw binary.

GPUI, GPUI Kit and their GPUI snapshots retain their Apache-2.0 notices in their
crate distributions. wgpu is MIT/Apache-2.0. Cargo.lock records exact versions of
all Rust dependencies. Before distributing an installer, generate and include a
complete dependency licence inventory (for example with cargo-about), including
transitive dependencies and zlib.
