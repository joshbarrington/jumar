use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::model::{Head, Health, Worktree};

/// Index into the worktree list the buffer was rendered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Existing {
        id: EntryId,
        name: String,
    },
    /// Raw `target[@base]` text; the target is resolved to a repo and source on apply.
    New {
        text: String,
        /// The worktree the cursor was on when the line was opened.
        near: Option<EntryId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    Remove {
        id: EntryId,
    },
    Move {
        id: EntryId,
        /// The new directory; `None` keeps it and only renames the branch.
        to: Option<PathBuf>,
        rename_branch: Option<String>,
    },
    Create {
        target: String,
        base: Option<String>,
        /// The closest existing line above (or else below), whose repo and source a
        /// bare `name` inherits.
        near: Option<EntryId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("line {line}: `{name}` is not a valid directory name")]
    InvalidName { line: usize, name: String },
    #[error("line {line}: the same worktree appears more than once")]
    Duplicate { line: usize },
    #[error("line {line}: no such worktree")]
    UnknownEntry { line: usize },
    #[error("line {line}: cannot rename a worktree whose directory or metadata is missing")]
    Unhealthy { line: usize },
    #[error("line {line}: {} is already taken", path.display())]
    Collision { line: usize, path: PathBuf },
    #[error("line {line}: new worktree needs a name")]
    MissingTarget { line: usize },
    #[error("line {line}: write a start after `@`, or leave out the `@`")]
    MissingBase { line: usize },
}

/// Diffs the edited buffer against `original`. Operations come back in apply order:
/// removes, then moves, then creates. All errors are reported, not just the first.
///
/// # Errors
///
/// Returns every invalid line when any line is invalid.
pub fn plan(original: &[Worktree], lines: &[Line]) -> Result<Vec<Operation>, Vec<PlanError>> {
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    let mut moves = Vec::new();
    let mut creates = Vec::new();
    let mut near = lines.iter().find_map(|line| existing_id(original, line));

    for (index, line) in lines.iter().enumerate() {
        if let Some(id) = existing_id(original, line) {
            near = Some(id);
        }
        let line_no = index + 1;
        match line {
            Line::Existing { id, name } => {
                let Some(worktree) = original.get(id.0) else {
                    errors.push(PlanError::UnknownEntry { line: line_no });
                    continue;
                };
                if !seen.insert(*id) {
                    errors.push(PlanError::Duplicate { line: line_no });
                    continue;
                }
                if worktree.main {
                    continue;
                }
                let name = name.trim();
                let dir = dir_name(&worktree.path);
                // `jb-a` to `jb/a` keeps the directory but can still rename the branch.
                if Some(name) == dir
                    || (Some(dir_name_for(name).as_str()) == dir
                        && renamed_branch(worktree, name).is_none())
                {
                    continue;
                }
                if !is_valid_dir_name(&dir_name_for(name)) {
                    errors.push(PlanError::InvalidName {
                        line: line_no,
                        name: name.to_owned(),
                    });
                } else if worktree.health == Health::Healthy {
                    moves.push((line_no, *id, worktree, name));
                } else {
                    errors.push(PlanError::Unhealthy { line: line_no });
                }
            }
            Line::New { text, near: opened } => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                let (target, base) = match text.split_once('@') {
                    Some((target, base)) => (target.trim(), Some(base.trim().to_owned())),
                    None => (text, None),
                };
                if target.is_empty() {
                    errors.push(PlanError::MissingTarget { line: line_no });
                } else if base.as_deref() == Some("") {
                    errors.push(PlanError::MissingBase { line: line_no });
                } else {
                    creates.push(Operation::Create {
                        target: target.to_owned(),
                        base,
                        near: opened.or(near),
                    });
                }
            }
        }
    }

    // Paths of worktrees that survive are taken even if they move away, so chains and
    // swaps are rejected rather than ordered.
    let mut taken: HashSet<PathBuf> = seen.iter().map(|id| original[id.0].path.clone()).collect();
    let mut operations: Vec<Operation> = (0..original.len())
        .map(EntryId)
        .filter(|id| !seen.contains(id) && !original[id.0].main)
        .map(|id| Operation::Remove { id })
        .collect();
    for (line, id, worktree, name) in moves {
        let path = worktree.path.with_file_name(dir_name_for(name));
        let to = (path != worktree.path).then_some(path);
        if let Some(to) = &to
            && !taken.insert(to.clone())
        {
            errors.push(PlanError::Collision {
                line,
                path: to.clone(),
            });
            continue;
        }
        operations.push(Operation::Move {
            id,
            to,
            rename_branch: renamed_branch(worktree, name),
        });
    }
    operations.extend(creates);

    if errors.is_empty() {
        Ok(operations)
    } else {
        errors.sort_by_key(PlanError::line);
        Err(errors)
    }
}

impl PlanError {
    #[must_use]
    pub fn line(&self) -> usize {
        match self {
            Self::InvalidName { line, .. }
            | Self::Duplicate { line }
            | Self::UnknownEntry { line }
            | Self::Unhealthy { line }
            | Self::Collision { line, .. }
            | Self::MissingTarget { line }
            | Self::MissingBase { line } => *line,
        }
    }
}

fn existing_id(original: &[Worktree], line: &Line) -> Option<EntryId> {
    match line {
        Line::Existing { id, .. } if id.0 < original.len() => Some(*id),
        _ => None,
    }
}

/// `name` when the branch is named after the directory, so renaming one renames both.
fn renamed_branch(worktree: &Worktree, name: &str) -> Option<String> {
    match &worktree.head {
        Some(Head::Branch(branch))
            if branch != name
                && Some(dir_name_for(branch).as_str()) == dir_name(&worktree.path) =>
        {
            Some(name.to_owned())
        }
        _ => None,
    }
}

fn dir_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

/// The directory for branch `name`: `jb/fix` lives in `jb-fix`, so every worktree stays
/// one directory deep.
#[must_use]
pub fn dir_name_for(name: &str) -> String {
    name.replace('/', "-")
}

fn is_valid_dir_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(path: &str, branch: &str) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            repo: PathBuf::from("/r"),
            source: Some("claude".into()),
            head: Some(Head::Branch(branch.into())),
            health: Health::Healthy,
            locked: None,
            main: false,
            default_branch: None,
        }
    }

    fn original() -> Vec<Worktree> {
        vec![worktree("/w/a", "a"), worktree("/w/b", "feature/b")]
    }

    fn keep(id: usize, name: &str) -> Line {
        Line::Existing {
            id: EntryId(id),
            name: name.into(),
        }
    }

    fn new(text: &str) -> Line {
        Line::New {
            text: text.into(),
            near: None,
        }
    }

    fn unchanged() -> Vec<Line> {
        vec![keep(0, "a"), keep(1, "b")]
    }

    #[test]
    fn plan_should_be_empty_when_buffer_unchanged() {
        assert_eq!(plan(&original(), &unchanged()), Ok(vec![]));
    }

    #[test]
    fn plan_should_remove_deleted_line() {
        let ops = plan(&original(), &[keep(1, "b")]).unwrap();

        assert_eq!(ops, [Operation::Remove { id: EntryId(0) }]);
    }

    #[test]
    fn plan_should_never_remove_or_move_main_checkout() {
        let mut original = original();
        original[0].main = true;

        assert_eq!(
            plan(&original, &[keep(0, "(main)")]),
            Ok(vec![Operation::Remove { id: EntryId(1) }])
        );
    }

    #[test]
    fn plan_should_rename_branch_when_it_matched_directory() {
        let ops = plan(&original(), &[keep(0, "a2"), keep(1, "b")]).unwrap();

        let expected = Operation::Move {
            id: EntryId(0),
            to: Some(PathBuf::from("/w/a2")),
            rename_branch: Some("a2".into()),
        };
        assert_eq!(ops, [expected]);
    }

    #[test]
    fn plan_should_keep_branch_when_it_differed_from_directory() {
        let ops = plan(&original(), &[keep(0, "a"), keep(1, "b2")]).unwrap();

        assert!(matches!(
            &ops[0],
            Operation::Move {
                rename_branch: None,
                ..
            }
        ));
    }

    #[test]
    fn plan_should_ignore_surrounding_whitespace_in_names() {
        assert_eq!(
            plan(&original(), &[keep(0, " a "), keep(1, "b\t")]),
            Ok(vec![])
        );
    }

    #[test]
    fn plan_should_create_from_new_line() {
        let mut lines = unchanged();
        lines.push(new("app:fix"));

        let ops = plan(&original(), &lines).unwrap();

        let expected = Operation::Create {
            target: "app:fix".into(),
            base: None,
            near: Some(EntryId(1)),
        };
        assert_eq!(ops, [expected]);
    }

    #[test]
    fn plan_should_take_context_from_line_above_new_line() {
        let lines = [keep(0, "a"), new("x"), keep(1, "b")];

        let ops = plan(&original(), &lines).unwrap();

        assert!(matches!(
            &ops[0],
            Operation::Create {
                near: Some(EntryId(0)),
                ..
            }
        ));
    }

    #[test]
    fn plan_should_take_context_from_the_line_it_was_opened_on() {
        let opened = Line::New {
            text: "x".into(),
            near: Some(EntryId(1)),
        };
        let lines = [keep(0, "a"), opened, keep(1, "b")];

        let ops = plan(&original(), &lines).unwrap();

        assert!(matches!(
            &ops[0],
            Operation::Create {
                near: Some(EntryId(1)),
                ..
            }
        ));
    }

    #[test]
    fn plan_should_take_context_from_line_below_when_new_line_is_first() {
        let lines = [new("x"), keep(1, "b"), keep(0, "a")];

        let ops = plan(&original(), &lines).unwrap();

        assert!(matches!(
            &ops[0],
            Operation::Create {
                near: Some(EntryId(1)),
                ..
            }
        ));
    }

    #[test]
    fn plan_should_have_no_context_when_buffer_has_no_existing_lines() {
        let ops = plan(&[], &[new("x")]).unwrap();

        assert!(matches!(&ops[0], Operation::Create { near: None, .. }));
    }

    #[test]
    fn plan_should_split_base_from_new_line() {
        let mut lines = unchanged();
        lines.push(new("fix@dev"));

        let ops = plan(&original(), &lines).unwrap();

        assert!(matches!(&ops[0], Operation::Create { base: Some(b), .. } if b == "dev"));
    }

    #[test]
    fn plan_should_ignore_blank_new_lines() {
        let mut lines = unchanged();
        lines.push(new("  "));

        assert_eq!(plan(&original(), &lines), Ok(vec![]));
    }

    #[test]
    fn plan_should_order_removes_then_moves_then_creates() {
        let lines = [new("c"), keep(1, "b2")];

        let ops = plan(&original(), &lines).unwrap();

        let kinds: Vec<_> = ops
            .iter()
            .map(|op| match op {
                Operation::Remove { .. } => "remove",
                Operation::Move { .. } => "move",
                Operation::Create { .. } => "create",
            })
            .collect();
        assert_eq!(kinds, ["remove", "move", "create"]);
    }

    #[test]
    fn plan_should_allow_moving_onto_removed_worktree_path() {
        let ops = plan(&original(), &[keep(1, "a")]).unwrap();

        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn plan_should_reject_moving_onto_surviving_worktree() {
        let errors = plan(&original(), &[keep(0, "a"), keep(1, "a")]).unwrap_err();

        assert_eq!(
            errors,
            [PlanError::Collision {
                line: 2,
                path: PathBuf::from("/w/a"),
            }]
        );
    }

    #[test]
    fn plan_should_reject_swapping_names() {
        let errors = plan(&original(), &[keep(0, "b"), keep(1, "a")]).unwrap_err();

        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn plan_should_move_name_with_slash_into_cleaned_directory() {
        let ops = plan(&original(), &[keep(0, "jb/a"), keep(1, "b")]).unwrap();

        let expected = Operation::Move {
            id: EntryId(0),
            to: Some(PathBuf::from("/w/jb-a")),
            rename_branch: Some("jb/a".into()),
        };
        assert_eq!(ops, [expected]);
    }

    #[test]
    fn plan_should_rename_branch_whose_cleaned_name_matched_directory() {
        let original = vec![worktree("/w/jb-a", "jb/a")];

        let ops = plan(&original, &[keep(0, "jb/a2")]).unwrap();

        assert!(matches!(
            &ops[0],
            Operation::Move { rename_branch: Some(b), .. } if b == "jb/a2"
        ));
    }

    #[test]
    fn plan_should_rename_only_the_branch_when_the_directory_stays() {
        let original = vec![worktree("/w/jb-a", "jb-a")];

        let ops = plan(&original, &[keep(0, "jb/a")]).unwrap();

        let expected = Operation::Move {
            id: EntryId(0),
            to: None,
            rename_branch: Some("jb/a".into()),
        };
        assert_eq!(ops, [expected]);
    }

    #[test]
    fn plan_should_leave_an_untouched_line_whose_branch_has_a_slash() {
        let original = vec![worktree("/w/jb-a", "jb/a")];

        assert_eq!(plan(&original, &[keep(0, "jb-a")]), Ok(vec![]));
    }

    #[test]
    fn plan_should_leave_worktree_when_name_cleans_to_its_directory() {
        let original = vec![worktree("/w/jb-a", "jb/a")];

        assert_eq!(plan(&original, &[keep(0, "jb/a")]), Ok(vec![]));
    }

    #[test]
    fn plan_should_reject_dot_dot_name() {
        let errors = plan(&original(), &[keep(0, ".."), keep(1, "b")]).unwrap_err();

        assert_eq!(
            errors,
            [PlanError::InvalidName {
                line: 1,
                name: "..".into(),
            }]
        );
    }

    #[test]
    fn plan_should_reject_empty_name() {
        let errors = plan(&original(), &[keep(0, ""), keep(1, "b")]).unwrap_err();

        assert!(matches!(
            errors[..],
            [PlanError::InvalidName { line: 1, .. }]
        ));
    }

    #[test]
    fn plan_should_reject_duplicated_line() {
        let errors = plan(&original(), &[keep(0, "a"), keep(1, "b"), keep(0, "a")]).unwrap_err();

        assert_eq!(errors, [PlanError::Duplicate { line: 3 }]);
    }

    #[test]
    fn plan_should_reject_unknown_entry() {
        let errors = plan(&original(), &[keep(0, "a"), keep(1, "b"), keep(7, "z")]).unwrap_err();

        assert_eq!(errors, [PlanError::UnknownEntry { line: 3 }]);
    }

    #[test]
    fn plan_should_reject_renaming_unhealthy_worktree() {
        let mut original = original();
        original[0].health = Health::Prunable;

        let errors = plan(&original, &[keep(0, "a2"), keep(1, "b")]).unwrap_err();

        assert_eq!(errors, [PlanError::Unhealthy { line: 1 }]);
    }

    #[test]
    fn plan_should_reject_new_line_without_target() {
        let mut lines = unchanged();
        lines.push(new("@dev"));

        let errors = plan(&original(), &lines).unwrap_err();

        assert_eq!(errors, [PlanError::MissingTarget { line: 3 }]);
    }

    #[test]
    fn plan_should_reject_new_line_with_empty_base() {
        let mut lines = unchanged();
        lines.push(new("fix @ "));

        let errors = plan(&original(), &lines).unwrap_err();

        assert_eq!(errors, [PlanError::MissingBase { line: 3 }]);
    }

    #[test]
    fn plan_should_report_every_invalid_line() {
        let lines = [keep(0, ".."), keep(1, ""), new("@b")];

        let errors = plan(&original(), &lines).unwrap_err();

        let lines: Vec<_> = errors.iter().map(PlanError::line).collect();
        assert_eq!(lines, [1, 2, 3]);
    }
}
