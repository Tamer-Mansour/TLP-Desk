## Summary

<!-- What changes and why, in a few lines. -->

## Lane / branch

<!-- e.g. TLP-files -> TLP-Desk -->

## Roadmap items

<!-- ROADMAP.md items this PR completes or advances (tick them in ROADMAP.md with evidence). -->

## Test evidence

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace` (new tests: name them here)
- [ ] Manual test (machines / Windows version / what was checked), if the change is visible or needs hardware
- [ ] Arabic checked: text shows, stores and travels correctly in Arabic (RTL layout, shaping, UTF-16 paths)

## Protocol

- [ ] No wire change, or: `PROTOCOL_VERSION` bumped, variants appended at the end only, `docs/protocol.md` updated, round-trip + index tests added

## Security impact

<!-- Does this touch auth, permissions, the allow-list, key storage, or what crosses the wire?
     Say what an attacker on the LAN could do with it and how it is gated. "None" is a valid answer. -->

## Clean-room

- [ ] No code, protocol, assets, names or UI text taken from other remote-desktop products
