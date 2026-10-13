# Architecture decision records

An ADR records one decision that shapes the code: what we chose, what else we considered, and
what follows from it. They explain *why* the code looks the way it does, so a later change can
revisit the reasoning instead of rediscovering it.

| # | Decision | Status |
|---|---|---|
| [0001](0001-quic-stream-per-video-frame.md) | One QUIC stream per video frame | Accepted |
| [0002](0002-spake2-with-channel-binding.md) | SPAKE2 with TLS channel binding for codes and passwords | Accepted |
| [0003](0003-ecdsa-identity-and-device-id.md) | ECDSA P-256 identity key and 9-digit device ID derived from it | Accepted |
| [0004](0004-tofu-and-fingerprint-pinning.md) | Pin by device ID; trust on first use for addresses | Accepted |
| [0005](0005-postcard-framing.md) | `postcard` with a `u32` length prefix for messages | Accepted |
| [0006](0006-openh264-first.md) | Software H.264 with OpenH264 first | Accepted |
| [0007](0007-egui-with-bidi-workaround.md) | egui for the GUI, with our own bidi pass | Accepted |
| [0008](0008-inno-setup-instead-of-msi.md) | Inno Setup `setup.exe` instead of an MSI | Accepted |
| [0009](0009-rename-to-tlp-desk.md) | Rename the product to TLP-Desk, keep wire identifiers | Accepted |
| [0010](0010-local-input-priority.md) | The person at the host always has input priority | Accepted |
| [0011](0011-capture-exclusion-for-pointer-marker.md) | Remote-pointer marker excluded from capture | Accepted |
| [0012](0012-capture-size-cap.md) | Cap capture at 1920×1200, scaled on the GPU | Accepted |

## Writing a new ADR

1. Copy the [template](template.md) to `NNNN-short-title.md` with the next free number.
2. Fill in context, decision, alternatives and consequences. Link code and tests.
3. Add it to this table and to `SUMMARY.md`, in the same pull request as the change it
   describes.
4. Never rewrite an accepted ADR's decision. If it changes, write a new ADR that supersedes it and
   set the old one's status to `Superseded by NNNN`.
