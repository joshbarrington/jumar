mod discover;
mod format;
mod list;
mod tui;

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
    match Cli::parse().command {
        Some(Command::List { json }) => list::run(json),
        None => {
            if let tui::Outcome::Open(path) = tui::run()? {
                println!("{}", path.display());
            }
            Ok(())
        }
    }
}
