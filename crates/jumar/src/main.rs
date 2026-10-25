mod clipboard;
mod discover;
mod format;
mod list;
mod plugin;
mod shell;
mod tools;
mod tui;

use clap::{Parser, Subcommand};

/// Manage git worktrees from an editable, vim-style buffer.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Run a herdr plugin action (used by herdr-plugin.toml).
    #[arg(long, hide = true)]
    action: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List worktrees with their status and removal safety.
    List {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print a shell function that runs jumar and cds into the worktree you open.
    Init { shell: shell::Shell },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(id) = cli.action {
        return plugin::action(&id);
    }
    match cli.command {
        Some(Command::List { json }) => list::run(json),
        Some(Command::Init { shell }) => {
            print!("{}", shell::init_script(shell));
            Ok(())
        }
        None => {
            let _pane = plugin::PaneRecord::register();
            if let tui::Outcome::Open(path) = tui::run()? {
                shell::hand_off(&path)?;
            }
            Ok(())
        }
    }
}
