mod clipboard;
mod discover;
mod format;
mod list;
mod pick;
mod plugin;
mod shell;
mod tools;
mod tui;

use std::path::PathBuf;

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
    /// Pick worktrees for an agent: Enter to work in one, Tab to pass them as context.
    Pick {
        /// Write the pick here instead of stdout; written empty when nothing is picked.
        #[arg(long, env = "JUMAR_PICK_OUTPUT")]
        output: Option<PathBuf>,
    },
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
        Some(Command::Pick { output }) => {
            let text = match tui::run(true) {
                Ok(tui::Outcome::Picked(text)) => text,
                Ok(_) => String::new(),
                Err(err) => {
                    // A launcher waiting for the file must not hang on a failed run.
                    if let Some(path) = &output {
                        let _ = pick::write(path, "");
                    }
                    return Err(err);
                }
            };
            if let Some(path) = output {
                return pick::write(&path, &text);
            }
            print!("{text}");
            Ok(())
        }
        None => {
            let _pane = plugin::PaneRecord::register();
            if let tui::Outcome::Open(path) = tui::run(false)? {
                shell::hand_off(&path)?;
            }
            Ok(())
        }
    }
}
