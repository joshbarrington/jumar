use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Source;
use crate::git;
use crate::model::{Head, Health, Worktree};
use crate::{Error, Result};

/// Matches source globs against the filesystem without spawning git, so the result can be
/// rendered immediately. Directories matched by several sources keep the first source.
///
/// # Errors
///
/// Returns [`Error::Glob`] for an invalid pattern.
pub fn scan_sources(sources: &[Source], home: &Path) -> Result<Vec<Worktree>> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for source in sources {
        for pattern in &source.globs {
            let pattern = expand_home(pattern, home);
            let paths = glob::glob(&pattern).map_err(|err| Error::Glob {
                pattern: pattern.clone(),
                source: err,
            })?;
            for path in paths.flatten() {
                if let Some(worktree) = read_worktree(&path, &source.name)
                    && seen.insert(worktree.path.clone())
                {
                    found.push(worktree);
                }
            }
        }
    }
    Ok(found)
}

/// The distinct repositories of `worktrees`, in first-seen order.
#[must_use]
pub fn repos(worktrees: &[Worktree]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    worktrees
        .iter()
        .filter(|w| seen.insert(&w.repo))
        .map(|w| w.repo.clone())
        .collect()
}

/// The known repos whose directory is named `name`, as `repo:` picks them.
#[must_use]
pub fn repos_named<'a>(worktrees: &'a [Worktree], name: &str) -> Vec<&'a Path> {
    let mut found: Vec<&Path> = Vec::new();
    for worktree in worktrees {
        let repo = worktree.repo.as_path();
        if repo.file_name().is_some_and(|n| n == name) && !found.contains(&repo) {
            found.push(repo);
        }
    }
    found
}

/// The main checkout of the repo containing `dir`, or `None` when `dir` is not in one.
#[must_use]
pub fn repo_containing(dir: &Path) -> Option<PathBuf> {
    let common = git::run(dir, &["rev-parse", "--git-common-dir"]).ok()?;
    Some(repo_from_common(&canonical(&dir.join(common.trim()))))
}

/// Worktrees of `repo` that are not in `known`: its main checkout, linked ones no source
/// matched, and prunable ones whose directory is gone.
///
/// # Errors
///
/// Fails when `git worktree list` fails.
pub fn scan_repo(repo: &Path, known: &[Worktree]) -> Result<Vec<Worktree>> {
    let known: HashSet<&Path> = known.iter().map(|w| w.path.as_path()).collect();
    let others = git::worktree_list(repo)?
        .into_iter()
        .enumerate()
        .filter(|(_, entry)| !entry.bare)
        .map(|(index, entry)| Worktree {
            path: canonical(&entry.path),
            repo: repo.to_path_buf(),
            source: None,
            head: entry.head,
            health: if entry.prunable {
                Health::Prunable
            } else {
                Health::Healthy
            },
            locked: entry.locked,
            main: index == 0,
            default_branch: None,
        })
        .filter(|w| !known.contains(w.path.as_path()))
        .collect();
    Ok(others)
}

fn read_worktree(dir: &Path, source: &str) -> Option<Worktree> {
    let dot_git = fs::read_to_string(dir.join(".git")).ok()?;
    let git_dir = dir.join(dot_git.strip_prefix("gitdir:")?.trim());
    let (repo, head, health) = if git_dir.is_dir() {
        (repo_of(&git_dir), read_head(&git_dir), Health::Healthy)
    } else {
        // Metadata lives at `<common>/worktrees/<id>`, so the repo is still derivable.
        (
            repo_from_common(git_dir.parent()?.parent()?),
            None,
            Health::Orphaned,
        )
    };
    let locked = fs::read_to_string(git_dir.join("locked"))
        .ok()
        .map(|reason| reason.trim().to_owned());
    Some(Worktree {
        path: canonical(dir),
        repo,
        source: Some(source.to_owned()),
        head,
        health,
        locked,
        main: false,
        default_branch: None,
    })
}

fn repo_of(git_dir: &Path) -> PathBuf {
    let common = fs::read_to_string(git_dir.join("commondir"))
        .map_or_else(|_| git_dir.join("../.."), |c| git_dir.join(c.trim()));
    repo_from_common(&canonical(&common))
}

fn repo_from_common(common: &Path) -> PathBuf {
    match (common.file_name(), common.parent()) {
        (Some(name), Some(parent)) if name == ".git" => parent.to_path_buf(),
        _ => common.to_path_buf(),
    }
}

fn read_head(git_dir: &Path) -> Option<Head> {
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => Head::Branch(branch.to_owned()),
        None => Head::Detached(head.to_owned()),
    })
}

fn expand_home(pattern: &str, home: &Path) -> String {
    let home = glob::Pattern::escape(&home.to_string_lossy());
    match pattern.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => pattern.to_owned(),
    }
}

// Paths are compared across glob results, `.git` pointers and git's own output, which
// disagree on symlinks such as macOS's `/var` -> `/private/var`.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    mod expand_home {
        use super::*;

        #[test]
        fn replaces_leading_tilde_slash() {
            assert_eq!(expand_home("~/a/*", Path::new("/h")), "/h/a/*");
        }

        #[test]
        fn keeps_tilde_user_form() {
            assert_eq!(expand_home("~bob/*", Path::new("/h")), "~bob/*");
        }

        #[test]
        fn escapes_glob_characters_in_home() {
            assert_eq!(expand_home("~/*", Path::new("/h[1]")), "/h[[]1[]]/*");
        }
    }

    mod repo_from_common {
        use super::*;

        #[test]
        fn returns_checkout_for_dot_git() {
            assert_eq!(repo_from_common(Path::new("/r/.git")), Path::new("/r"));
        }

        #[test]
        fn returns_git_dir_for_bare_repo() {
            assert_eq!(repo_from_common(Path::new("/r.git")), Path::new("/r.git"));
        }
    }
}
