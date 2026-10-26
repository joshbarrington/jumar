use std::collections::HashSet;
use std::fmt;
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
    /// Raw text, parsed by [`parse_target`]; the target is resolved to a repo and source
    /// on apply.
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
        target: Target,
        /// The closest existing line above (or else below), whose repo and source a
        /// bare `name` inherits.
        near: Option<EntryId>,
    },
}

/// A new line, `[repo:]name[@start]`, where `#123` alone is PR 123 as branch `pr-123`
/// and `name@#123` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub repo: Option<String>,
    pub name: String,
    /// Where a new branch starts; never set for a PR.
    pub base: Option<String>,
    pub pull: Option<u32>,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(repo) = &self.repo {
            write!(f, "{repo}:")?;
        }
        match (self.pull, &self.base) {
            (Some(n), _) if self.name == format!("pr-{n}") => write!(f, "#{n}"),
            (Some(n), _) => write!(f, "{}@#{n}", self.name),
            (None, Some(base)) => write!(f, "{}@{base}", self.name),
            (None, None) => write!(f, "{}", self.name),
        }
    }
}

/// Parses a new line's text. Splits on the first `@`, then the first `:`; git forbids
/// both in branch names, so neither split is ambiguous.
///
/// # Errors
///
/// Returns what is missing or malformed, reported against line `line`.
pub fn parse_target(text: &str, line: usize) -> Result<Target, PlanError> {
    let (target, base) = match text.split_once('@') {
        Some((target, base)) => (target.trim(), Some(base.trim())),
        None => (text.trim(), None),
    };
    let (repo, name) = match target.split_once(':') {
        Some((repo, name)) => (Some(repo.trim()), name.trim()),
        None => (None, target),
    };
    if name.is_empty() {
        return Err(PlanError::MissingTarget { line });
    }
    if base == Some("") {
        return Err(PlanError::MissingBase { line });
    }
    if repo == Some("") {
        return Err(PlanError::MissingRepo { line });
    }
    let number = |text: &str| {
        text.parse::<u32>().map_err(|_| PlanError::BadPull {
            line,
            text: text.to_owned(),
        })
    };
    let (name, base, pull) = match (name.strip_prefix('#'), base.map(|b| b.strip_prefix('#'))) {
        (Some(n), None) => {
            let n = number(n)?;
            (format!("pr-{n}"), None, Some(n))
        }
        (Some(_), Some(_)) => return Err(PlanError::NamedPull { line }),
        (None, Some(Some(n))) => (name.to_owned(), None, Some(number(n)?)),
        (None, _) => (name.to_owned(), base.map(str::to_owned), None),
    };
    Ok(Target {
        repo: repo.map(str::to_owned),
        name,
        base,
        pull,
    })
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
    #[error("line {line}: write a repo before `:`, or leave out the `:`")]
    MissingRepo { line: usize },
    #[error("line {line}: `#{text}` is not a PR number")]
    BadPull { line: usize, text: String },
    #[error("line {line}: write name@#123 to name a PR's worktree")]
    NamedPull { line: usize },
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
                let dir = dir_name_for(name);
                let same_dir = is_dir_of(&dir, &worktree.path);
                let rename_branch = renamed_branch(worktree, name);
                // The buffer shows directories, so an untouched line is a no-op, while
                // `jb-a` to `jb/a` keeps the directory but can still rename the branch.
                if same_dir && (Some(name) == dir_name(&worktree.path) || rename_branch.is_none()) {
                    continue;
                }
                if !is_valid_dir_name(&dir) {
                    errors.push(PlanError::InvalidName {
                        line: line_no,
                        name: name.to_owned(),
                    });
                } else if worktree.health == Health::Healthy {
                    let to = (!same_dir).then(|| worktree.path.with_file_name(dir));
                    moves.push((line_no, *id, to, rename_branch));
                } else {
                    errors.push(PlanError::Unhealthy { line: line_no });
                }
            }
            Line::New { text, near: opened } => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                match parse_target(text, line_no) {
                    Ok(target) => creates.push(Operation::Create {
                        target,
                        near: opened.or(near),
                    }),
                    Err(err) => errors.push(err),
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
    for (line, id, to, rename_branch) in moves {
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
            rename_branch,
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
            | Self::MissingBase { line }
            | Self::MissingRepo { line }
            | Self::BadPull { line, .. }
            | Self::NamedPull { line } => *line,
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
            if branch != name && is_dir_of(&dir_name_for(branch), &worktree.path) =>
        {
            Some(name.to_owned())
        }
        _ => None,
    }
}

fn dir_name(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

fn is_dir_of(dir: &str, path: &Path) -> bool {
    dir_name(path) == Some(dir)
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
            target: Target {
                repo: Some("app".into()),
                name: "fix".into(),
                base: None,
                pull: None,
            },
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

        assert!(
            matches!(&ops[0], Operation::Create { target, .. } if target.base.as_deref() == Some("dev"))
        );
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

    fn parsed(text: &str) -> Result<Target, PlanError> {
        parse_target(text, 1)
    }

    #[test]
    fn parse_target_should_read_repo_name_and_start() {
        let target = parsed(" api : jb/fix @ origin/dev ").unwrap();

        assert_eq!(
            target,
            Target {
                repo: Some("api".into()),
                name: "jb/fix".into(),
                base: Some("origin/dev".into()),
                pull: None,
            }
        );
    }

    #[test]
    fn parse_target_should_name_a_bare_pr_after_its_number() {
        let target = parsed("#7").unwrap();

        assert_eq!((target.name.as_str(), target.pull), ("pr-7", Some(7)));
    }

    #[test]
    fn parse_target_should_keep_a_given_name_for_a_pr() {
        let target = parsed("review@#7").unwrap();

        assert_eq!(
            (target.name.as_str(), target.base, target.pull),
            ("review", None, Some(7))
        );
    }

    #[test]
    fn parse_target_should_reject_malformed_lines() {
        let errors =
            ["app:", ":fix", "fix@", "#seven", "#1@#2"].map(|text| parsed(text).unwrap_err());

        assert_eq!(
            errors,
            [
                PlanError::MissingTarget { line: 1 },
                PlanError::MissingRepo { line: 1 },
                PlanError::MissingBase { line: 1 },
                PlanError::BadPull {
                    line: 1,
                    text: "seven".into()
                },
                PlanError::NamedPull { line: 1 },
            ]
        );
    }

    #[test]
    fn target_should_display_as_written() {
        let shown = ["api:fix@v2", "#7", "review@#7"].map(|text| parsed(text).unwrap().to_string());

        assert_eq!(shown, ["api:fix@v2", "#7", "review@#7"]);
    }
}
