// Shelling out (rather than linking a git library) honours the user's git config, hooks
// and fsmonitor exactly as `git` itself would.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::model::Head;
use crate::{Error, Result};

/// Runs `git -C <dir> <args>` and returns stdout.
///
/// # Errors
///
/// Fails when git cannot start, exits unsuccessfully, or prints non-UTF-8.
pub fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let output = spawn(dir, args)?;
    if !output.status.success() {
        return Err(failure(dir, args, &output));
    }
    String::from_utf8(output.stdout).map_err(|_| Error::GitParse("non-UTF-8 output".into()))
}

fn spawn(dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(Error::GitSpawn)
}

fn failure(dir: &Path, args: &[&str], output: &Output) -> Error {
    Error::Git {
        dir: dir.to_path_buf(),
        args: args.join(" "),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

/// Fetches `from`, a ref on `remote`, into the local ref `into`. Never prompts for
/// credentials, since the terminal belongs to the TUI.
///
/// # Errors
///
/// Fails when git cannot start or the fetch fails.
pub fn fetch(repo: &Path, remote: &str, from: &str, into: &str) -> Result<()> {
    let refspec = format!("+{from}:{into}");
    let args = ["fetch", "--quiet", "--no-tags", remote, &refspec];
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(Error::GitSpawn)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(failure(repo, &args, &output))
    }
}

/// For commands that answer yes/no with exit status 0/1.
fn yes_no(dir: &Path, args: &[&str]) -> Result<bool> {
    let output = spawn(dir, args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(failure(dir, args, &output)),
    }
}

/// The commit `rev` names, or `None` when it does not exist.
///
/// # Errors
///
/// Fails when git cannot start or fails for another reason.
pub fn resolve(dir: &Path, rev: &str) -> Result<Option<String>> {
    let args = ["rev-parse", "--verify", "--quiet", rev];
    let output = spawn(dir, &args)?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        )),
        Some(1) => Ok(None),
        _ => Err(failure(dir, &args, &output)),
    }
}

/// # Errors
///
/// Fails when either revision does not exist.
pub fn is_ancestor(dir: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    yes_no(dir, &["merge-base", "--is-ancestor", ancestor, descendant])
}

/// # Errors
///
/// Fails when git fails.
pub fn branch_exists(repo: &Path, name: &str) -> Result<bool> {
    ref_exists(repo, &format!("refs/heads/{name}"))
}

/// A repo's branches, fetched remote branches and remotes, read once so that every line
/// of a save is checked against the same refs.
#[derive(Debug, Clone, Default)]
pub struct Refs {
    pub heads: Vec<String>,
    /// Like `origin/main`.
    pub remote_branches: Vec<String>,
    pub remotes: Vec<String>,
}

impl Refs {
    /// # Errors
    ///
    /// Fails when git fails.
    pub fn read(repo: &Path) -> Result<Self> {
        let mut refs = Self {
            remotes: run(repo, &["remote"])?.lines().map(str::to_owned).collect(),
            ..Self::default()
        };
        let all = run(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/",
                "refs/remotes/",
            ],
        )?;
        for line in all.lines() {
            if let Some(head) = line.strip_prefix("refs/heads/") {
                refs.heads.push(head.to_owned());
            } else if let Some(remote) = line.strip_prefix("refs/remotes/") {
                refs.remote_branches.push(remote.to_owned());
            }
        }
        Ok(refs)
    }

    /// `remote/name` when one of the remotes, `origin` first, has a fetched branch `name`.
    #[must_use]
    pub fn remote_branch(&self, name: &str) -> Option<String> {
        let mut found: Vec<&String> = self
            .remote_branches
            .iter()
            .filter(|full| split_remote(&self.remotes, full).is_some_and(|(_, b)| b == name))
            .collect();
        found.sort_by_key(|full| !full.starts_with("origin/"));
        found.first().map(|full| (*full).clone())
    }

    /// A branch whose ref stops git from making branch `name`: `name` itself, or a branch
    /// it would nest under or over, since `jb` blocks `jb/a` and `jb/a` blocks `jb`.
    /// `except` is left out, as git lets a rename replace its own old name.
    #[must_use]
    pub fn clashing_branch(&self, name: &str, except: Option<&str>) -> Option<String> {
        self.heads
            .iter()
            .filter(|branch| Some(branch.as_str()) != except)
            .filter(|branch| branches_clash(branch, name))
            // The name itself first, so an existing branch reads as one.
            .max_by_key(|branch| *branch == name)
            .cloned()
    }
}

/// `full`, like `origin/main`, split into one of `remotes` and its branch. With remotes `a`
/// and `a/b`, `a/b/x` is branch `x` of `a/b`, not `b/x` of `a`.
#[must_use]
pub fn split_remote<'a>(remotes: &'a [String], full: &'a str) -> Option<(&'a str, &'a str)> {
    let full = full.strip_prefix("refs/remotes/").unwrap_or(full);
    remotes
        .iter()
        .filter_map(|remote| {
            let branch = full.strip_prefix(remote.as_str())?.strip_prefix('/')?;
            Some((remote.as_str(), branch))
        })
        .max_by_key(|(remote, _)| remote.len())
}

/// Whether branches `a` and `b` cannot both exist: they are the same, or one would nest
/// under the other, as `jb` and `jb/a` do.
#[must_use]
pub fn branches_clash(a: &str, b: &str) -> bool {
    let nested = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    a == b || nested(a, b) || nested(b, a)
}

/// # Errors
///
/// Fails when git cannot start.
pub fn is_commit(repo: &Path, rev: &str) -> Result<bool> {
    // No revision starts with `-`, and git would read it as an option.
    if rev.starts_with('-') {
        return Ok(false);
    }
    let rev = format!("{rev}^{{commit}}");
    Ok(spawn(repo, &["rev-parse", "--verify", "--quiet", &rev])?
        .status
        .success())
}

fn ref_exists(repo: &Path, full_name: &str) -> Result<bool> {
    yes_no(repo, &["show-ref", "--verify", "--quiet", full_name])
}

/// # Errors
///
/// Fails when git cannot start.
pub fn is_valid_branch_name(repo: &Path, name: &str) -> Result<bool> {
    Ok(spawn(repo, &["check-ref-format", "--branch", name])?
        .status
        .success())
}

/// The repo's default branch as remote-tracking and local refs, whichever exist:
/// e.g. `["refs/remotes/origin/main", "refs/heads/main"]`. A `configured` name wins;
/// otherwise `origin/HEAD`, falling back to a local `main` or `master`.
///
/// # Errors
///
/// Fails when git fails.
pub fn default_branches(repo: &Path, configured: Option<&str>) -> Result<Vec<String>> {
    if let Some(name) = configured {
        let candidates = [
            format!("refs/remotes/origin/{name}"),
            format!("refs/heads/{name}"),
        ];
        let mut refs = Vec::new();
        for candidate in candidates {
            if ref_exists(repo, &candidate)? {
                refs.push(candidate);
            }
        }
        return Ok(refs);
    }
    let remote_head = spawn(
        repo,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )?;
    let mut refs = Vec::new();
    let local_names = if remote_head.status.success() {
        let target = String::from_utf8_lossy(&remote_head.stdout)
            .trim()
            .to_owned();
        let name = target.trim_start_matches("refs/remotes/origin/").to_owned();
        refs.push(target);
        vec![name]
    } else {
        vec!["main".to_owned(), "master".to_owned()]
    };
    for name in local_names {
        if branch_exists(repo, &name)? {
            refs.push(format!("refs/heads/{name}"));
            break;
        }
    }
    Ok(refs)
}

/// `main` for `refs/heads/main`, `origin/main` for `refs/remotes/origin/main`.
#[must_use]
pub fn short_ref(name: &str) -> &str {
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/remotes/"))
        .unwrap_or(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<Head>,
    pub bare: bool,
    pub prunable: bool,
    pub locked: Option<String>,
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
    let mut locked = None;
    for line in block.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => path = Some(PathBuf::from(value)),
            "HEAD" => oid = Some(value),
            "branch" => branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value)),
            "bare" => bare = true,
            "prunable" => prunable = true,
            "locked" => locked = Some(value.to_owned()),
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
        locked,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// The upstream is set but its remote branch is gone, as after a merged PR's branch
    /// is deleted; `ahead` and `behind` are then unknown and left at 0.
    pub upstream_gone: bool,
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
    let mut divergence = false;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            divergence |= header.starts_with("branch.ab ");
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
    status.upstream_gone = status.upstream.is_some() && !divergence;
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

    #[test]
    fn short_ref_should_strip_remote_prefix() {
        assert_eq!(short_ref("refs/remotes/origin/main"), "origin/main");
    }

    mod parse_worktree_list {
        use super::*;

        const LIST: &str = "\
worktree /r
HEAD 1111
branch refs/heads/main

worktree /r/.claude/worktrees/a
HEAD 2222
branch refs/heads/feature/a
locked on a usb drive

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
        fn reads_lock_reason() {
            let entries = parse_worktree_list(LIST).unwrap();

            assert_eq!(entries[1].locked.as_deref(), Some("on a usb drive"));
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
        fn flags_upstream_gone_without_divergence() {
            let status =
                parse_status("# branch.head feat\n# branch.upstream origin/feat\n").unwrap();

            assert!(status.upstream_gone);
        }

        #[test]
        fn leaves_upstream_gone_unset_with_divergence() {
            let text = "# branch.upstream origin/feat\n# branch.ab +0 -0\n";

            assert!(!parse_status(text).unwrap().upstream_gone);
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
