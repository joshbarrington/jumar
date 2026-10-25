//! `jumar pick`: choose worktrees for an agent, which reads the result from a file.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use jumar_core::model::Worktree;
use jumar_core::tool;

use crate::format;

#[derive(Debug, Clone, Copy)]
pub enum Use {
    /// The agent should move into the worktree and work there.
    Work,
    /// The agent should only know about the worktrees.
    Context,
}

/// One block of `key: value` lines per worktree.
pub fn describe(use_: Use, picked: &[(&Worktree, String)]) -> String {
    let use_ = match use_ {
        Use::Work => "work",
        Use::Context => "context",
    };
    let mut text = String::new();
    for (worktree, status) in picked {
        let _ = writeln!(text, "use: {use_}");
        let _ = writeln!(text, "path: {}", worktree.path.display());
        let _ = writeln!(text, "repo: {}", worktree.repo.display());
        let _ = writeln!(text, "branch: {}", format::branch(worktree.head.as_ref()));
        let _ = writeln!(text, "status: {status}");
        if let Some(base) = base(worktree) {
            let _ = writeln!(text, "base: {base}");
        }
        if worktree.main {
            let _ = writeln!(text, "main: true");
        }
        text.push('\n');
    }
    text
}

/// Where the branch left the default branch, as in a tool's `{base}`.
fn base(worktree: &Worktree) -> Option<String> {
    tool::render(&["{base}".to_owned()], worktree).ok()?.pop()
}

/// Writes all at once, so a reader waiting for the file never sees part of it.
pub fn write(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .with_context(|| format!("could not write {}", path.display()))
}
