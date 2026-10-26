use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::model::{Head, Health, Worktree};

/// Index into the worktree list the buffer was rendered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// `dir branch`, split by [`existing_columns`]. A worktree without a branch, such as a
    /// detached checkout, has only its directory.
    Existing { id: EntryId, text: String },
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

/// An existing line's directory and branch. A branch cannot contain spaces, so it is the
/// last word, and the directory is everything before it, spaces and all. A line for a
/// worktree without a branch is all directory.
#[must_use]
pub fn existing_columns(text: &str, has_branch: bool) -> (&str, &str) {
    let text = text.trim();
    match text.rfind(char::is_whitespace) {
        Some(at) if has_branch => (text[..at].trim_end(), text[at..].trim_start()),
        _ => (text, ""),
    }
}

/// A new line's directory and the rest, split at the first run of spaces.
#[must_use]
pub fn columns(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(end) => (&text[..end], text[end..].trim()),
        None => (text, ""),
    }
}

/// A new line, `[repo:][dir ]branch[@start]`. Without a directory, the branch names it,
/// with each `/` as `-`. `#123` as the branch is PR 123 as branch `pr-123`, and
/// `branch@#123` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub repo: Option<String>,
    pub dir: String,
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
        if self.dir != dir_name_for(&self.name) {
            write!(f, "{} ", self.dir)?;
        }
        match (self.pull, &self.base) {
            (Some(n), _) if self.name == format!("pr-{n}") => write!(f, "#{n}"),
            (Some(n), _) => write!(f, "{}@#{n}", self.name),
            (None, Some(base)) => write!(f, "{}@{base}", self.name),
            (None, None) => write!(f, "{}", self.name),
        }
    }
}

/// Why a new line's text cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    #[error("type a branch, or a directory and then a branch")]
    MissingBranch,
    #[error("write a start after `@`, or leave out the `@`")]
    MissingBase,
    #[error("write a repo before `:`, or leave out the `:`")]
    MissingRepo,
    #[error("write a directory and a branch, with nothing after them but @start")]
    TooManyWords,
    #[error("`{0}` is not a valid directory name")]
    InvalidDir(String),
    #[error("`#{0}` is not a PR number")]
    BadPull(String),
    #[error("a PR starts from its own head; write name@#123 to name its branch")]
    PullStart,
    #[error("write the repo and `:` at the start of the line, like app:tui feat")]
    LateRepo,
}

/// Whether the part before `:` is a path rather than a repo's name, which has no `/`.
#[must_use]
pub fn is_repo_path(repo: &str) -> bool {
    repo.contains('/') || repo.starts_with('~') || repo == "." || repo == ".."
}

/// Parses a new line's text. Splits on the first `@`, then on a first word's `:`, then on
/// spaces. git forbids spaces and `:` in branch names, but not `@`, so a branch such as
/// `jb@fix` cannot be written here.
///
/// # Errors
///
/// Returns what is missing or malformed.
pub fn parse_target(text: &str) -> Result<Target, TargetError> {
    let (target, base) = match text.split_once('@') {
        Some((target, base)) => (target.trim(), Some(base.trim())),
        None => (text.trim(), None),
    };
    // Only a first word can be a repo, so `tui api:fix` is not one.
    let (repo, target) = match target.split_once(':') {
        Some((repo, rest)) if !repo.contains(char::is_whitespace) => (Some(repo), rest),
        _ => (None, target),
    };
    let words: Vec<&str> = target.split_whitespace().collect();
    let (dir, branch) = match words[..] {
        [] => return Err(TargetError::MissingBranch),
        [branch] => (None, branch),
        [dir, branch] => (Some(dir), branch),
        _ => return Err(TargetError::TooManyWords),
    };
    if words.iter().any(|word| word.contains(':')) {
        return Err(TargetError::LateRepo);
    }
    if repo == Some("") {
        return Err(TargetError::MissingRepo);
    }
    if branch.is_empty() {
        return Err(TargetError::MissingBranch);
    }
    if base == Some("") {
        return Err(TargetError::MissingBase);
    }
    let number = |text: &str| {
        text.parse::<u32>()
            .map_err(|_| TargetError::BadPull(text.to_owned()))
    };
    let (name, base, pull) = match (branch.strip_prefix('#'), base.map(|b| b.strip_prefix('#'))) {
        (Some(n), None) => {
            let n = number(n)?;
            (format!("pr-{n}"), None, Some(n))
        }
        (Some(_), Some(_)) => return Err(TargetError::PullStart),
        (None, Some(Some(n))) => (branch.to_owned(), None, Some(number(n)?)),
        (None, _) => (branch.to_owned(), base.map(str::to_owned), None),
    };
    let dir = dir.map_or_else(|| dir_name_for(&name), str::to_owned);
    if !is_valid_dir_name(&dir) {
        return Err(TargetError::InvalidDir(dir));
    }
    Ok(Target {
        repo: repo.map(str::to_owned),
        dir,
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
    #[error("line {line}: write the branch after the directory")]
    MissingBranch { line: usize },
    #[error("line {line}: {source}")]
    Target {
        line: usize,
        #[source]
        source: TargetError,
    },
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
    for (index, line) in lines.iter().enumerate() {
        let line_no = index + 1;
        match line {
            Line::Existing { id, text } => {
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
                match edit(worktree, text, line_no) {
                    Ok(Some(edit)) => moves.push((line_no, *id, edit)),
                    Ok(None) => {}
                    Err(err) => errors.push(err),
                }
            }
            Line::New { text, near } => {
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                match parse_target(text) {
                    Ok(target) => creates.push(Operation::Create {
                        target,
                        near: *near,
                    }),
                    Err(source) => errors.push(PlanError::Target {
                        line: line_no,
                        source,
                    }),
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
    for (line, id, Edit { to, rename_branch }) in moves {
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
            | Self::MissingBranch { line }
            | Self::Target { line, .. } => *line,
        }
    }
}

/// What an edited `dir branch` line asks of its worktree.
struct Edit {
    to: Option<PathBuf>,
    rename_branch: Option<String>,
}

/// The edit a `dir branch` line asks for, or `None` when it changes neither.
fn edit(worktree: &Worktree, text: &str, line: usize) -> Result<Option<Edit>, PlanError> {
    let has_branch = matches!(worktree.head, Some(Head::Branch(_)));
    let (dir, branch) = existing_columns(text, has_branch);
    let rename_branch = match &worktree.head {
        Some(Head::Branch(_)) if branch.is_empty() => {
            return Err(PlanError::MissingBranch { line });
        }
        Some(Head::Branch(old)) => (branch != old).then(|| branch.to_owned()),
        _ => None,
    };
    let to = (Some(dir) != dir_name(&worktree.path)).then(|| worktree.path.with_file_name(dir));
    if to.is_none() && rename_branch.is_none() {
        Ok(None)
    } else if !is_valid_dir_name(dir) {
        Err(PlanError::InvalidName {
            line,
            name: dir.to_owned(),
        })
    } else if worktree.health == Health::Healthy {
        Ok(Some(Edit { to, rename_branch }))
    } else {
        Err(PlanError::Unhealthy { line })
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
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
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

    fn keep(id: usize, text: &str) -> Line {
        Line::Existing {
            id: EntryId(id),
            text: text.into(),
        }
    }

    fn new(text: &str) -> Line {
        Line::New {
            text: text.into(),
            near: None,
        }
    }

    fn unchanged() -> Vec<Line> {
        vec![keep(0, "a a"), keep(1, "b feature/b")]
    }

    fn moved(id: usize, to: Option<&str>, branch: Option<&str>) -> Operation {
        Operation::Move {
            id: EntryId(id),
            to: to.map(PathBuf::from),
            rename_branch: branch.map(Into::into),
        }
    }

    #[test]
    fn columns_should_split_at_the_first_run_of_spaces() {
        assert_eq!(columns("  tui   feat/x @main "), ("tui", "feat/x @main"));
    }

    #[test]
    fn plan_should_be_empty_when_buffer_unchanged() {
        assert_eq!(plan(&original(), &unchanged()), Ok(vec![]));
    }

    #[test]
    fn plan_should_remove_deleted_line() {
        let ops = plan(&original(), &[keep(1, "b feature/b")]).unwrap();

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
    fn plan_should_move_only_the_directory_when_it_changes() {
        let ops = plan(&original(), &[keep(0, "a2 a"), keep(1, "b feature/b")]).unwrap();

        assert_eq!(ops, [moved(0, Some("/w/a2"), None)]);
    }

    #[test]
    fn plan_should_rename_only_the_branch_when_it_changes() {
        let ops = plan(&original(), &[keep(0, "a a"), keep(1, "b feat/b")]).unwrap();

        assert_eq!(ops, [moved(1, None, Some("feat/b"))]);
    }

    #[test]
    fn plan_should_move_and_rename_when_both_change() {
        let ops = plan(&original(), &[keep(0, "a2 jb/a"), keep(1, "b feature/b")]).unwrap();

        assert_eq!(ops, [moved(0, Some("/w/a2"), Some("jb/a"))]);
    }

    #[test]
    fn plan_should_ignore_spacing_between_and_around_columns() {
        assert_eq!(
            plan(&original(), &[keep(0, " a   a "), keep(1, "b\tfeature/b")]),
            Ok(vec![])
        );
    }

    #[test]
    fn plan_should_leave_a_detached_checkout_with_only_its_directory() {
        let mut original = original();
        original[0].head = Some(Head::Detached("0123".into()));

        assert_eq!(
            plan(&original, &[keep(0, "a"), keep(1, "b feature/b")]),
            Ok(vec![])
        );
    }

    #[test]
    fn plan_should_read_a_detached_line_as_all_directory() {
        let mut original = original();
        original[0].head = Some(Head::Detached("0123".into()));

        let ops = plan(&original, &[keep(0, "a x"), keep(1, "b feature/b")]).unwrap();

        assert_eq!(ops, [moved(0, Some("/w/a x"), None)]);
    }

    #[test]
    fn plan_should_reject_a_line_missing_its_branch() {
        let errors = plan(&original(), &[keep(0, "a"), keep(1, "b feature/b")]).unwrap_err();

        assert_eq!(errors, [PlanError::MissingBranch { line: 1 }]);
    }

    #[test]
    fn plan_should_leave_a_directory_with_spaces_alone() {
        let original = vec![worktree("/w/my dir", "feat")];

        assert_eq!(plan(&original, &[keep(0, "my dir feat")]), Ok(vec![]));
    }

    #[test]
    fn plan_should_take_the_last_word_as_the_branch() {
        let original = vec![worktree("/w/my dir", "feat")];

        let ops = plan(&original, &[keep(0, "my dir feat/x")]).unwrap();

        assert_eq!(ops, [moved(0, None, Some("feat/x"))]);
    }

    #[test]
    fn plan_should_reject_a_slash_in_the_directory() {
        let errors = plan(&original(), &[keep(0, "x/y a"), keep(1, "b feature/b")]).unwrap_err();

        assert_eq!(
            errors,
            [PlanError::InvalidName {
                line: 1,
                name: "x/y".into(),
            }]
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
                dir: "fix".into(),
                name: "fix".into(),
                base: None,
                pull: None,
            },
            near: None,
        };
        assert_eq!(ops, [expected]);
    }

    #[test]
    fn plan_should_take_context_from_the_line_it_was_opened_on() {
        let opened = Line::New {
            text: "x".into(),
            near: Some(EntryId(1)),
        };
        let lines = [keep(0, "a a"), opened, keep(1, "b feature/b")];

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
    fn plan_should_leave_a_new_line_without_context_when_it_was_opened_on_none() {
        let lines = [keep(0, "a a"), new("x"), keep(1, "b feature/b")];

        let ops = plan(&original(), &lines).unwrap();

        assert!(matches!(&ops[0], Operation::Create { near: None, .. }));
    }

    #[test]
    fn plan_should_ignore_blank_new_lines() {
        let mut lines = unchanged();
        lines.push(new("  "));

        assert_eq!(plan(&original(), &lines), Ok(vec![]));
    }

    #[test]
    fn plan_should_order_removes_then_moves_then_creates() {
        let lines = [new("c"), keep(1, "b2 feature/b")];

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
        let ops = plan(&original(), &[keep(1, "a feature/b")]).unwrap();

        assert_eq!(ops.len(), 2);
    }

    #[test]
    fn plan_should_reject_moving_onto_surviving_worktree() {
        let errors = plan(&original(), &[keep(0, "a a"), keep(1, "a feature/b")]).unwrap_err();

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
        let errors = plan(&original(), &[keep(0, "b a"), keep(1, "a feature/b")]).unwrap_err();

        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn plan_should_reject_dot_dot_name() {
        let errors = plan(&original(), &[keep(0, ".. a"), keep(1, "b feature/b")]).unwrap_err();

        assert_eq!(
            errors,
            [PlanError::InvalidName {
                line: 1,
                name: "..".into(),
            }]
        );
    }

    #[test]
    fn plan_should_reject_duplicated_line() {
        let mut lines = unchanged();
        lines.push(keep(0, "a a"));

        let errors = plan(&original(), &lines).unwrap_err();

        assert_eq!(errors, [PlanError::Duplicate { line: 3 }]);
    }

    #[test]
    fn plan_should_reject_unknown_entry() {
        let mut lines = unchanged();
        lines.push(keep(7, "z z"));

        let errors = plan(&original(), &lines).unwrap_err();

        assert_eq!(errors, [PlanError::UnknownEntry { line: 3 }]);
    }

    #[test]
    fn plan_should_reject_renaming_unhealthy_worktree() {
        let mut original = original();
        original[0].health = Health::Prunable;

        let errors = plan(&original, &[keep(0, "a2 a"), keep(1, "b feature/b")]).unwrap_err();

        assert_eq!(errors, [PlanError::Unhealthy { line: 1 }]);
    }

    #[test]
    fn plan_should_report_a_new_line_it_cannot_read() {
        let mut lines = unchanged();
        lines.push(new("fix @ "));

        let errors = plan(&original(), &lines).unwrap_err();

        assert_eq!(
            errors,
            [PlanError::Target {
                line: 3,
                source: TargetError::MissingBase
            }]
        );
    }

    #[test]
    fn plan_should_report_every_invalid_line() {
        let lines = [keep(0, ".. a"), keep(1, "b"), new("@b")];

        let errors = plan(&original(), &lines).unwrap_err();

        let lines: Vec<_> = errors.iter().map(PlanError::line).collect();
        assert_eq!(lines, [1, 2, 3]);
    }

    fn target(repo: Option<&str>, dir: &str, name: &str, base: Option<&str>) -> Target {
        Target {
            repo: repo.map(Into::into),
            dir: dir.into(),
            name: name.into(),
            base: base.map(Into::into),
            pull: None,
        }
    }

    #[test]
    fn parse_target_should_name_the_directory_after_a_lone_branch() {
        assert_eq!(
            parse_target("feat/tui-improvements"),
            Ok(target(
                None,
                "feat-tui-improvements",
                "feat/tui-improvements",
                None
            ))
        );
    }

    #[test]
    fn parse_target_should_read_a_directory_then_a_branch() {
        assert_eq!(
            parse_target(" api:tui  feat/tui-improvements @ origin/dev "),
            Ok(target(
                Some("api"),
                "tui",
                "feat/tui-improvements",
                Some("origin/dev")
            ))
        );
    }

    #[test]
    fn parse_target_should_give_a_lone_branch_its_repo() {
        assert_eq!(
            [
                parse_target("api:feat/x"),
                parse_target("api: feat/x"),
                parse_target("api: tui feat/x"),
            ],
            [
                Ok(target(Some("api"), "feat-x", "feat/x", None)),
                Ok(target(Some("api"), "feat-x", "feat/x", None)),
                Ok(target(Some("api"), "tui", "feat/x", None)),
            ]
        );
    }

    #[test]
    fn parse_target_should_name_a_bare_pr_after_its_number() {
        let target = parse_target("#7").unwrap();

        assert_eq!(
            (target.dir.as_str(), target.name.as_str(), target.pull),
            ("pr-7", "pr-7", Some(7))
        );
    }

    #[test]
    fn parse_target_should_keep_a_given_name_for_a_pr() {
        let target = parse_target("rev review@#7").unwrap();

        assert_eq!(
            (
                target.dir.as_str(),
                target.name.as_str(),
                target.base,
                target.pull
            ),
            ("rev", "review", None, Some(7))
        );
    }

    #[test]
    fn parse_target_should_reject_malformed_lines() {
        let errors = [
            "app:",
            ":fix",
            "fix@",
            "#seven",
            "#1@#2",
            "a b c",
            "x/y fix",
            "@dev",
            "#7@dev",
            "tui api:fix",
        ]
        .map(|text| parse_target(text).unwrap_err());

        assert_eq!(
            errors,
            [
                TargetError::MissingBranch,
                TargetError::MissingRepo,
                TargetError::MissingBase,
                TargetError::BadPull("seven".into()),
                TargetError::PullStart,
                TargetError::TooManyWords,
                TargetError::InvalidDir("x/y".into()),
                TargetError::MissingBranch,
                TargetError::PullStart,
                TargetError::LateRepo,
            ]
        );
    }

    #[test]
    fn is_repo_path_should_read_a_dotted_name_as_a_repo_name() {
        assert_eq!(
            [".dotfiles", "~/code/app", "./app", ".", "app"].map(is_repo_path),
            [false, true, true, true, false]
        );
    }

    #[test]
    fn target_should_display_as_written() {
        let shown = ["api:fix@v2", "#7", "review@#7", "tui feat/x"]
            .map(|text| parse_target(text).unwrap().to_string());

        assert_eq!(shown, ["api:fix@v2", "#7", "review@#7", "tui feat/x"]);
    }
}
