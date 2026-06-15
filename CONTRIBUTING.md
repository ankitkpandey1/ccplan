# Contributing

ccplan is built as a small, auditable Rust CLI. Keep changes focused, covered by tests, and aligned
with [CONVENTIONS.md](CONVENTIONS.md).

## Development

Use the pinned toolchain from `rust-toolchain.toml`.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --workspace
cargo deny check
```

Coverage is a release gate:

```sh
RUSTFLAGS="--cfg coverage_nightly" cargo +nightly llvm-cov --all-features --workspace --fail-under-lines 100
```

## Commits

Use Conventional Commits, for example:

```text
feat: add release installer metadata
fix: preserve done blocks during whole-plan import
docs: document agent JSON contract
```

Do not add generated noise or unrelated formatting churn to a feature commit.

## Pull requests

Every PR should describe behavior changes, tests run, and user-facing docs changed. Changes that
touch scheduling, automation, storage, or release packaging need focused tests and a short note about
the failure mode being protected.
