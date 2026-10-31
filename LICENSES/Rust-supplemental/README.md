# Supplemental Rust dependency notices

These files supplement published Rust crate archives that contain no standalone
license or notice file. They are included in the Windows distribution's
`LICENSES` directory, alongside the crate-provided notices collected separately.
They do not change Scrutivue's own license or any dependency's license.

`index.json` maps every affected `package@version` to files relative to this
directory. It records full upstream repository URLs, immutable commits, original
paths, download URLs, and SHA-256 hashes. Package commits and paths come from the
published `.cargo_vcs_info.json`, with the exceptions described below. Identical
license bytes are shared across commits within the same repository only after
checking each pinned revision. `.gitattributes` preserves the downloaded bytes.
Two pinned upstream files intentionally retain trailing spaces; only those files
are excluded from Git whitespace checks instead of changing their verified bytes.

The reviewed diagnostic contains **29** packages without standalone notices.
Twenty-five are covered by license files retrieved at their published commits.
AccessKit's Chromium BSD license and distinct source copyright headers are also
retained. Zune's source license headers are retained without implementation code.
For packages offering a choice, `selected_license` records the distribution's
chosen alternative; additional upstream alternatives may also be reproduced.

## Upstream notice-text gaps requiring review

Four upstream trees and published archives contain a license declaration, but no
standalone license text or copyright notice:

| Package | Declared license | Supplemental material |
| --- | --- | --- |
| `hexf-parse@0.2.1` | CC0-1.0 | Exact manifest attribution and canonical CC0 terms |
| `mac@0.1.1` | MIT/Apache-2.0 | Exact manifest/README attribution and canonical Apache-2.0 terms |
| `seahash@4.1.0` | MIT | Exact manifest attribution and canonical MIT terms |
| `simd_helpers@0.1.0` | MIT | Exact manifest attribution and canonical MIT terms |

These are explicitly marked `metadata-only` and `review_required` in the
manifest, not represented as recovered upstream notices. `metadata-only/NOTICE.txt`
preserves the authors and declarations verbatim. Canonical texts are unmodified
copies from the SPDX License List's pinned revision. Their copyright placeholders
are templates, **not** asserted copyright notices for these packages. No copyright
holder or year has been invented. A release reviewer must decide whether these
upstream omissions need clarification before redistribution; this collection is
not a legal clearance of those omissions.

`mac@0.1.1` predates published Cargo VCS metadata. Its release tag `0.1.1` resolves
to the recorded commit, and all 12 tracked files at that commit were verified
byte-for-byte against the published archive using Git blob hashes. The other
three packages use their recorded publication commits. `hexf-parse` omits
`path_in_vcs`; its upstream `parse` directory was identified by the byte-identical
manifest. The absent path is recorded as `null`, not silently inferred metadata.

The repository for `zune-core` and `zune-inflate` is absent from their manifests;
the recorded Zune Image repository was verified at each publication commit and
the copied source headers were checked against the published files. Seahash's
provenance uses its official Redox GitLab repository, not an unrelated mirror.

To refresh this collection, resolve the locked dependency metadata again, review
new crate archives, retrieve notices from their exact publication revisions, and
update the package mapping and hashes. Do not substitute current default-branch
licenses for the recorded revisions.
