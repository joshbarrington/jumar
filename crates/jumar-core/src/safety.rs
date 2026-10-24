use crate::Result;
use crate::git::{self, Status};
use crate::model::{Head, Health, Worktree};

/// How much would be lost by removing a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Clean, and the branch is merged or fully pushed: remove checkout and branch.
    Safe,
    /// Clean, but the branch holds commits found nowhere else: keep the branch.
    Unmerged,
    /// Uncommitted or untracked changes: only removable with force.
    Dirty,
}

impl Tier {
    #[must_use]
    pub fn deletes_branch_by_default(self) -> bool {
        self == Self::Safe
    }
}

#[must_use]
pub fn classify(status: &Status, merged: bool) -> Tier {
    if !status.is_clean() {
        Tier::Dirty
    } else if merged || (status.upstream.is_some() && status.ahead == 0) {
        Tier::Safe
    } else {
        Tier::Unmerged
    }
}

/// # Errors
///
/// Fails when git fails.
pub fn assess(worktree: &Worktree) -> Result<Tier> {
    let defaults = git::default_branches(&worktree.repo)?;
    let revision = match &worktree.head {
        Some(Head::Branch(name)) => {
            let local = format!("refs/heads/{name}");
            // The default branch is "merged" by definition but must never be deleted.
            if defaults.contains(&local) {
                return Ok(Tier::Unmerged);
            }
            local
        }
        Some(Head::Detached(oid)) => oid.clone(),
        None => return Ok(Tier::Unmerged),
    };
    let status = match worktree.health {
        Health::Healthy => git::status(&worktree.path)?,
        Health::Prunable | Health::Orphaned => Status::default(),
    };
    let mut merged = false;
    for default in &defaults {
        if git::is_ancestor(&worktree.repo, &revision, default)? {
            merged = true;
            break;
        }
    }
    Ok(classify(&status, merged))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> Status {
        Status::default()
    }

    #[test]
    fn classify_should_be_safe_when_clean_and_merged() {
        assert_eq!(classify(&clean(), true), Tier::Safe);
    }

    #[test]
    fn classify_should_be_safe_when_clean_and_fully_pushed() {
        let status = Status {
            upstream: Some("origin/x".into()),
            ..clean()
        };

        assert_eq!(classify(&status, false), Tier::Safe);
    }

    #[test]
    fn classify_should_be_unmerged_when_ahead_of_upstream() {
        let status = Status {
            upstream: Some("origin/x".into()),
            ahead: 3,
            ..clean()
        };

        assert_eq!(classify(&status, false), Tier::Unmerged);
    }

    #[test]
    fn classify_should_be_unmerged_when_no_upstream_and_not_merged() {
        assert_eq!(classify(&clean(), false), Tier::Unmerged);
    }

    #[test]
    fn classify_should_be_dirty_when_untracked_files_even_if_merged() {
        let status = Status {
            untracked: 1,
            ..clean()
        };

        assert_eq!(classify(&status, true), Tier::Dirty);
    }

    #[test]
    fn classify_should_be_dirty_when_conflicted() {
        let status = Status {
            conflicted: 1,
            ..clean()
        };

        assert_eq!(classify(&status, true), Tier::Dirty);
    }

    #[test]
    fn deletes_branch_by_default_only_when_safe() {
        let deletes: Vec<_> = [Tier::Safe, Tier::Unmerged, Tier::Dirty]
            .into_iter()
            .map(Tier::deletes_branch_by_default)
            .collect();

        assert_eq!(deletes, [true, false, false]);
    }
}
