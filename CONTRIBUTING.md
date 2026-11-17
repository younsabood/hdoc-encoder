# Contributing

Bug reports, documentation fixes, and focused code changes are welcome. Keep
changes scoped to the encoder repository; it intentionally has no dependency on
the decoder crate.

## Before opening an issue

- Search existing issues for the same problem.
- Include the Rust version, operating system, relevant feature flags, and a
  minimal reproducible input.
- Do not attach private documents, credentials, or generated build artifacts.

## Local checks

Install the Rust version declared in `Cargo.toml`, then run:

```powershell
cargo fmt --check
cargo check --all-targets --all-features
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo package --no-verify
```

The PDF integration tests use `CH1.pdf`, `CH2.pdf`, `INT203-Ch02.pdf`, and
`INT203-Ch03.pdf` when available. Set `HDOC_PDF_TEST_DIR` to a directory that
contains those files to run them outside the original development checkout.
Without those optional fixtures, the tests report a skip and the rest of the
suite still runs.

## Pull requests

Describe the behavior change, public API impact, and checks you ran. Update the
relevant Markdown guide and Mermaid diagram when the format or ingest pipeline
changes. Keep binary test data out of pull requests unless it is small and
redistributable.
