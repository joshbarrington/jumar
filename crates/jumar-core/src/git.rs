// Shelling out (rather than linking a git library) honours the user's git config, hooks
// and fsmonitor exactly as `git` itself would.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::Head;
use crate::{Error, Result};

/// Runs `git -C <dir> <args>` and returns stdout.
///
/// # Errors
///
/// Fails when git cannot start, exits unsuccessfully, or prints non-UTF-8.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(Error::GitSpawn)?;
    if !output.status.success() {
        return Err(Error::Git {
            dir: dir.to_path_buf(),
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|_| Error::GitParse("non-UTF-8 output".into()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<Head>,
    pub bare: bool,
    pub prunable: bool,
}

/// The first entry is the main worktree.
///
/// # Errors
///
/// Fails when git fails or its output does not parse.
pub fn worktree_list(repo: &Path) -> Result<Vec<WorktreeEntry>> {
    parse_worktree_list(&run(repo, &["worktree", "list", "--porcelain"])?)
}

/// # Errors
///
/// Fails when an entry has no `worktree` line.
pub fn parse_worktree_list(text: &str) -> Result<Vec<WorktreeEntry>> {
    text.split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .map(parse_worktree_block)
        .collect()
}

fn parse_worktree_block(block: &str) -> Result<WorktreeEntry> {
    let mut path = None;
    let mut oid = None;
    let mut branch = None;
    let mut bare = false;
    let mut prunable = false;
    for line in block.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => path = Some(PathBuf::from(value)),
            "HEAD" => oid = Some(value),
            "branch" => branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value)),
            "bare" => bare = true,
            "prunable" => prunable = true,
            _ => {}
        }
    }
    let path =
        path.ok_or_else(|| Error::GitParse(format!("worktree entry without path: {block:?}")))?;
    let head = match (branch, oid) {
        (Some(name), _) => Some(Head::Branch(name.to_owned())),
        (None, Some(oid)) => Some(Head::Detached(oid.to_owned())),
        (None, None) => None,
    };
    Ok(WorktreeEntry {
        path,
        head,
        bare,
        prunable,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changed: usize,
    pub conflicted: usize,
    pub untracked: usize,
}

impl Status {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.changed == 0 && self.conflicted == 0 && self.untracked == 0
    }
}

/// # Errors
///
/// Fails when git fails or its output does not parse.
pub fn status(path: &Path) -> Result<Status> {
    parse_status(&run(path, &["status", "--porcelain=v2", "--branch"])?)
}

/// # Errors
///
/// Fails on a malformed `# branch.ab` header.
pub fn parse_status(text: &str) -> Result<Status> {
    let mut status = Status::default();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            parse_status_header(header, &mut status)?;
        } else {
            match line.as_bytes().first() {
                Some(b'1' | b'2') => status.changed += 1,
                Some(b'u') => status.conflicted += 1,
                Some(b'?') => status.untracked += 1,
                _ => {}
            }
        }
    }
    Ok(status)
}

fn parse_status_header(header: &str, status: &mut Status) -> Result<()> {
    let Some((key, value)) = header.split_once(' ') else {
        return Ok(());
    };
    match key {
        "branch.head" if value != "(detached)" => status.branch = Some(value.to_owned()),
        "branch.upstream" => status.upstream = Some(value.to_owned()),
        "branch.ab" => {
            let bad = || Error::GitParse(format!("bad branch.ab header: {value:?}"));
            let (ahead, behind) = value.split_once(' ').ok_or_else(bad)?;
            status.ahead = ahead
                .strip_prefix('+')
                .and_then(|n| n.parse().ok())
                .ok_or_else(bad)?;
            status.behind = behind
                .strip_prefix('-')
                .and_then(|n| n.parse().ok())
                .ok_or_else(bad)?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod parse_worktree_list {
        use super::*;

        const LIST: &str = "\
worktree /r
HEAD 1111
branch refs/heads/main

worktree /r/.claude/worktrees/a
HEAD 2222
branch refs/heads/feature/a

worktree /gone
HEAD 3333
detached
prunable gitdir file points to non-existent location

";

        #[test]
        fn returns_every_entry() {
            assert_eq!(parse_worktree_list(LIST).unwrap().len(), 3);
        }

        #[test]
        fn strips_refs_heads_from_branch() {
            let entries = parse_worktree_list(LIST).unwrap();

            assert_eq!(entries[1].head, Some(Head::Branch("feature/a".into())));
        }

        #[test]
        fn reads_detached_head_as_oid() {
            let entries = parse_worktree_list(LIST).unwrap();

            assert_eq!(entries[2].head, Some(Head::Detached("3333".into())));
        }

        #[test]
        fn flags_prunable_entry() {
            let entries = parse_worktree_list(LIST).unwrap();

            assert!(entries[2].prunable);
        }

        #[test]
        fn reads_bare_entry_without_head() {
            let entries = parse_worktree_list("worktree /r.git\nbare\n").unwrap();

            assert_eq!((entries[0].bare, &entries[0].head), (true, &None));
        }

        #[test]
        fn rejects_entry_without_path() {
            assert!(parse_worktree_list("HEAD 1111\n").is_err());
        }
    }

    mod parse_status {
        use super::*;

        #[test]
        fn reads_branch_upstream_and_divergence() {
            let text = "# branch.oid 1111\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -3\n";

            let expected = Status {
                branch: Some("main".into()),
                upstream: Some("origin/main".into()),
                ahead: 2,
                behind: 3,
                ..Status::default()
            };
            assert_eq!(parse_status(text).unwrap(), expected);
        }

        #[test]
        fn leaves_branch_empty_when_detached() {
            let status = parse_status("# branch.head (detached)\n").unwrap();

            assert_eq!(status.branch, None);
        }

        #[test]
        fn counts_changed_conflicted_and_untracked() {
            let text = "\
1 .M N... 100644 100644 100644 aa bb src/a.rs
2 R. N... 100644 100644 100644 aa bb R100 new.rs\told.rs
u UU N... 100644 100644 100644 100644 aa bb cc c.rs
? notes.txt
? tmp/
! target/
";
            let status = parse_status(text).unwrap();

            assert_eq!(
                (status.changed, status.conflicted, status.untracked),
                (2, 1, 2)
            );
        }

        #[test]
        fn is_clean_when_only_headers() {
            let status = parse_status("# branch.head main\n").unwrap();

            assert!(status.is_clean());
        }

        #[test]
        fn rejects_malformed_divergence() {
            assert!(parse_status("# branch.ab 2 3\n").is_err());
        }
    }
}
