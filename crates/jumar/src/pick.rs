//! `jumar pick`: choose worktrees for an agent, which reads the result from a file.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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

impl Use {
    fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Context => "context",
        }
    }
}

/// One block of `key: value` lines per worktree and its status.
pub fn describe<'a>(use_: Use, picked: impl IntoIterator<Item = (&'a Worktree, String)>) -> String {
    let use_ = use_.as_str();
    let mut text = String::new();
    for (worktree, status) in picked {
        let _ = writeln!(text, "use: {use_}");
        let _ = writeln!(text, "path: {}", worktree.path.display());
        let _ = writeln!(text, "repo: {}", worktree.repo.display());
        let _ = writeln!(text, "branch: {}", format::branch(worktree.head.as_ref()));
        let _ = writeln!(text, "status: {status}");
        // A repo without a default branch has no base; the agent falls back to recent log.
        if let Ok(base) = tool::base(worktree) {
            let _ = writeln!(text, "base: {base}");
        }
        if worktree.main {
            let _ = writeln!(text, "main: true");
        }
        text.push('\n');
    }
    text
}

/// Writes an empty pick on drop unless [`Output::finish`] ran, so a launcher waiting for
/// the file is released however jumar ends, panics included.
pub struct Output(Option<PathBuf>);

impl Output {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self(path)
    }

    /// Writes `text` to the file, or prints it without one.
    pub fn finish(mut self, text: &str) -> Result<()> {
        if let Some(path) = self.0.take() {
            return write(&path, text);
        }
        print!("{text}");
        Ok(())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = write(path, "");
        }
    }
}

/// Writes all at once, so a reader waiting for the file never sees part of it.
fn write(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .with_context(|| format!("could not write {}", path.display()))
}
