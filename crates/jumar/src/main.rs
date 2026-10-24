//! The `jumar` binary: TUI, CLI and herdr plugin entry point.

use clap::Parser;

/// Manage git worktrees from an editable, vim-style buffer.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {}

fn main() {
    Cli::parse();
}
