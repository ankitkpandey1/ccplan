## Summary

## Tests

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --all-features --workspace`
- [ ] `cargo deny check`
- [ ] coverage gate run when code changes require it

## Definition of Done

- [ ] Behavior is covered by focused tests.
- [ ] User-facing docs are updated when CLI behavior changes.
- [ ] Release packaging impact is considered.
- [ ] Security-sensitive `run:` or scheduler changes are called out explicitly.
