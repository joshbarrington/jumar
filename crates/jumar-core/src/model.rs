use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Head {
    /// Short name: `feature/x`, not `refs/heads/feature/x`.
    Branch(String),
    Detached(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    /// Git has metadata but the directory is gone; `git worktree prune` cleans it up.
    Prunable,
    /// The directory exists but its `.git` file points at missing metadata.
    Orphaned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Worktree {
    pub path: PathBuf,
    /// Where git commands run: the main checkout, or the git dir of a bare repo.
    pub repo: PathBuf,
    /// `None` puts it in the "other" group.
    pub source: Option<String>,
    pub head: Option<Head>,
    pub health: Health,
    /// Set by `git worktree lock`, holding its reason (empty without one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
}
