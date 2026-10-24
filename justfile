# Run every check CI runs.
check: fmt-check lint test

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --all-targets --all-features --locked -- -D warnings

test:
    cargo test --all-features --locked
