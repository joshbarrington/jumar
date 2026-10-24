use anyhow::{Context, Result};
use jumar_core::config::{self, Config};
use jumar_core::discovery::{repos, scan_repo, scan_sources};
use jumar_core::model::Worktree;

pub const WORKERS: usize = 8;

/// Source matches first, then the "other" group. Repos that fail to list are reported on
/// stderr and skipped rather than failing the whole scan.
pub fn worktrees() -> Result<Vec<Worktree>> {
    let home = std::env::home_dir().context("no home directory")?;
    let config = match config::default_path() {
        Some(path) => Config::load(&path)?,
        None => Config::default(),
    };
    let mut worktrees = scan_sources(&config.sources(), &home)?;
    let mut others = Vec::new();
    for repo in repos(&worktrees) {
        match scan_repo(&repo, &worktrees) {
            Ok(found) => others.extend(found),
            Err(err) => eprintln!("jumar: skipping {}: {err}", repo.display()),
        }
    }
    worktrees.extend(others);
    Ok(worktrees)
}
