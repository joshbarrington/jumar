mod discover;
mod list;

use clap::{Parser, Subcommand};

/// Manage git worktrees from an editable, vim-style buffer.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List worktrees with their status and removal safety.
    List {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> anyhow::Result<()> {
    // Until the TUI lands, a bare `jumar` lists.
    match Cli::parse()
        .command
        .unwrap_or(Command::List { json: false })
    {
        Command::List { json } => list::run(json),
    }
}
