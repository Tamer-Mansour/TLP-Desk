# Summary

[Introduction](introduction.md)

# User guide (English)

- [Overview](user-guide/en/index.md)
  - [Installing](user-guide/en/installing.md)
  - [First run and the firewall](user-guide/en/first-run.md)
  - [Your device ID and one-time code](user-guide/en/device-id-and-code.md)

# Security

- [Security model](security.md)

# Contributor guide

- [Architecture](dev/architecture/index.md)
  - [Video pipeline](dev/architecture/video.md)
  - [Input path](dev/architecture/input.md)
  - [Connection and authentication](dev/architecture/auth.md)
- [Crates](dev/crates/index.md)
  - [tlp-desk-proto](dev/crates/proto.md)
  - [tlp-desk-crypto](dev/crates/crypto.md)
  - [tlp-desk-net](dev/crates/net.md)
  - [tlp-desk-capture](dev/crates/capture.md)
  - [tlp-desk-codec](dev/crates/codec.md)
  - [tlp-desk-input](dev/crates/input.md)
  - [tlp-desk-audio](dev/crates/audio.md)
  - [tlp-desk-files](dev/crates/files.md)
  - [tlp-desk-host](dev/crates/host.md)
  - [tlp-desk-viewer](dev/crates/viewer.md)
  - [tlp-desk-app](dev/crates/app.md)
  - [tlp-desk-cli](dev/crates/cli.md)
- [Building on Windows](dev/building.md)
- [Testing](dev/testing.md)
- [Performance notes](dev/performance.md)
- [i18n and Arabic](dev/i18n.md)
- [Release process](dev/release.md)

# Architecture decisions

- [Decision records](adr/index.md)
  - [Template](adr/template.md)
  - [0001 One QUIC stream per video frame](adr/0001-quic-stream-per-video-frame.md)
  - [0002 SPAKE2 with channel binding](adr/0002-spake2-with-channel-binding.md)
  - [0003 Identity key and device ID](adr/0003-ecdsa-identity-and-device-id.md)
  - [0004 Pinning and trust on first use](adr/0004-tofu-and-fingerprint-pinning.md)
  - [0005 postcard framing](adr/0005-postcard-framing.md)
  - [0006 OpenH264 first](adr/0006-openh264-first.md)
  - [0007 egui with a bidi pass](adr/0007-egui-with-bidi-workaround.md)
  - [0008 Inno Setup instead of MSI](adr/0008-inno-setup-instead-of-msi.md)
  - [0009 Rename to TLP-Desk](adr/0009-rename-to-tlp-desk.md)
  - [0010 Local input priority](adr/0010-local-input-priority.md)
  - [0011 Pointer marker excluded from capture](adr/0011-capture-exclusion-for-pointer-marker.md)
  - [0012 Capture size cap](adr/0012-capture-size-cap.md)
- [Coding conventions](dev/conventions.md)

# Reference

- [Wire protocol](protocol.md)
- [Glossary](glossary.md)
- [Changelog](changelog.md)
