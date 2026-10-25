use std::path::PathBuf;

use anyhow::{Context, Result};
use jumar_core::config::{self, Config};
use jumar_core::discovery::{repos, scan_repo, scan_sources};
use jumar_core::model::Worktree;

pub const WORKERS: usize = 8;

pub struct Env {
    pub home: PathBuf,
    pub config: Config,
}

impl Env {
    pub fn load() -> Result<Self> {
        let home = std::env::home_dir().context("no home directory")?;
        let config = match config::default_path() {
            Some(path) => Config::load(&path)?,
            None => Config::default(),
        };
        Ok(Self { home, config })
    }
}

pub struct Scan {
    /// Grouped by repo; within a repo, the main checkout comes first, then source matches,
    /// then the "other" group.
    pub worktrees: Vec<Worktree>,
    /// Repos that failed to list; they are skipped rather than failing the whole scan.
    pub warnings: Vec<String>,
}

pub fn scan(env: &Env) -> Result<Scan> {
    let mut worktrees = scan_sources(&env.config.sources(), &env.home)?;
    let mut others = Vec::new();
    let mut warnings = Vec::new();
    for repo in repos(&worktrees) {
        match scan_repo(&repo, &worktrees) {
            Ok(found) => others.extend(found),
            Err(err) => warnings.push(format!("skipping {}: {err}", repo.display())),
        }
    }
    worktrees.extend(others);
    worktrees.sort_by(|a, b| {
        (a.repo.file_name(), &a.repo, !a.main).cmp(&(b.repo.file_name(), &b.repo, !b.main))
    });
    Ok(Scan {
        worktrees,
        warnings,
    })
}
