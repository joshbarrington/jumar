use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// Short name: `feature/x`, not `refs/heads/feature/x`.
    Branch(String),
    Detached(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    /// Git has metadata but the directory is gone; `git worktree prune` cleans it up.
    Prunable,
    /// The directory exists but its `.git` file points at missing metadata.
    Orphaned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Where git commands run: the main checkout, or the git dir of a bare repo.
    pub repo: PathBuf,
    /// `None` puts it in the "other" group.
    pub source: Option<String>,
    pub head: Option<Head>,
    pub health: Health,
}
