# Contributing

Issues and pull requests are welcome. For a larger change, open an issue first so we can agree
on the approach.

## Setup

`rust-toolchain.toml` pins the Rust version; rustup installs it on the first `cargo` call. The
tests create real git repos, so they need `git` on your `PATH`.

```sh
just check   # fmt, clippy and tests, the same as CI
```

Without `just`, run the three commands in the `justfile`.

## Guidelines

- The workspace lints are strict: no `unwrap`, `expect` or `unsafe`, and clippy warnings fail CI.
- Add a test for a behavior change. `jumar-core` holds discovery, planning and safety logic;
  `jumar` holds the CLI and TUI.
- Keep `herdr-plugin.toml` and `.claude-plugin/*.json` versions in step with `Cargo.toml`.
- Add a line to `CHANGELOG.md` for anything a user would notice.
