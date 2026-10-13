# Summary

[Introduction](introduction.md)

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
- [Coding conventions](dev/conventions.md)

# Reference

- [Wire protocol](protocol.md)
- [Glossary](glossary.md)
- [Changelog](changelog.md)
