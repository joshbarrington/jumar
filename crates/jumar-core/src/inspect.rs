use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::Result;
use crate::git::{self, Status};
use crate::model::{Health, Worktree};
use crate::safety::{self, Tier};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Inspection {
    /// `None` when there is no checkout to run `git status` in.
    pub status: Option<Status>,
    pub tier: Tier,
    /// When HEAD was committed, in Unix seconds; `None` without a checkout or a commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_commit: Option<i64>,
}

/// # Errors
///
/// Fails when git fails.
pub fn inspect(worktree: &Worktree) -> Result<Inspection> {
    let status = match worktree.health {
        Health::Healthy => Some(git::status(&worktree.path)?),
        Health::Prunable | Health::Orphaned => None,
    };
    let tier = safety::assess(worktree, status.as_ref().unwrap_or(&Status::default()))?;
    let last_commit = match worktree.health {
        Health::Healthy => git::run(&worktree.path, &["log", "-1", "--format=%ct"])
            .ok()
            .and_then(|out| out.trim().parse().ok()),
        Health::Prunable | Health::Orphaned => None,
    };
    Ok(Inspection {
        status,
        tier,
        last_commit,
    })
}

/// Inspects every worktree on `workers` background threads. Results arrive in completion
/// order, tagged with the worktree's index; the channel closes when all are done.
#[must_use]
pub fn inspect_all(
    worktrees: &Arc<[Worktree]>,
    workers: usize,
) -> Receiver<(usize, Result<Inspection>)> {
    let (tx, rx) = mpsc::channel();
    let next = Arc::new(AtomicUsize::new(0));
    for _ in 0..workers.clamp(1, worktrees.len().max(1)) {
        let (tx, next, worktrees) = (tx.clone(), Arc::clone(&next), Arc::clone(worktrees));
        thread::spawn(move || {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(worktree) = worktrees.get(index) else {
                    break;
                };
                if tx.send((index, inspect(worktree))).is_err() {
                    break;
                }
            }
        });
    }
    rx
}
