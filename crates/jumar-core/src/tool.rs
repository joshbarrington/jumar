use crate::git;
use crate::model::{Head, Worktree};
use crate::{Error, Result};

#[derive(Debug)]
struct Vars<'a> {
    path: &'a str,
    repo: &'a str,
    branch: &'a str,
    base: String,
    default_branch: String,
}

/// Fills `{path}`, `{repo}`, `{branch}`, `{base}` and `{default_branch}` in each argument.
/// Git is only consulted for the placeholders that are used.
///
/// # Errors
///
/// Fails when `{base}` or `{default_branch}` is used and the repo has no default branch,
/// or git fails.
pub fn render(command: &[String], worktree: &Worktree) -> Result<Vec<String>> {
    let uses = |placeholder: &str| command.iter().any(|arg| arg.contains(placeholder));
    let default = if uses("{base}") || uses("{default_branch}") {
        Some(default_ref(worktree)?)
    } else {
        None
    };
    let base = match &default {
        Some(default) if uses("{base}") => merge_base(worktree, default)?,
        _ => String::new(),
    };
    let default_branch = default.as_deref().map_or("", git::short_ref).to_owned();
    let vars = Vars {
        path: utf8(&worktree.path)?,
        repo: utf8(&worktree.repo)?,
        branch: match &worktree.head {
            Some(Head::Branch(name) | Head::Detached(name)) => name,
            None => "HEAD",
        },
        base,
        default_branch,
    };
    Ok(command.iter().map(|arg| fill(arg, &vars)).collect())
}

const PLACEHOLDERS: [&str; 5] = ["path", "repo", "branch", "base", "default_branch"];

/// The first `{name}` in `arg` that is not a placeholder. Braces around anything other than
/// a lowercase name, as in a `jq` filter, are left alone.
#[must_use]
pub fn unknown_placeholder(arg: &str) -> Option<&str> {
    let mut rest = arg;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let end = after.find('}')?;
        let name = &after[..end];
        let looks_like_one =
            !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        if looks_like_one && !PLACEHOLDERS.contains(&name) {
            return Some(name);
        }
        rest = &after[end + 1..];
    }
    None
}

/// Where the worktree's HEAD left its repo's default branch.
///
/// # Errors
///
/// Fails when the repo has no default branch, or git fails.
pub fn base(worktree: &Worktree) -> Result<String> {
    merge_base(worktree, &default_ref(worktree)?)
}

fn default_ref(worktree: &Worktree) -> Result<String> {
    git::default_branches(&worktree.repo, worktree.default_branch.as_deref())?
        .into_iter()
        .next()
        .ok_or_else(|| Error::NoDefaultBranch(worktree.repo.clone()))
}

fn merge_base(worktree: &Worktree, default: &str) -> Result<String> {
    Ok(git::run(&worktree.path, &["merge-base", default, "HEAD"])?
        .trim()
        .to_owned())
}

fn fill(arg: &str, vars: &Vars) -> String {
    arg.replace("{path}", vars.path)
        .replace("{repo}", vars.repo)
        .replace("{branch}", vars.branch)
        .replace("{base}", &vars.base)
        .replace("{default_branch}", &vars.default_branch)
}

fn utf8(path: &std::path::Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::NonUtf8Path(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_should_replace_every_placeholder() {
        let vars = Vars {
            path: "/w",
            repo: "/r",
            branch: "fix",
            base: "abc".into(),
            default_branch: "main".into(),
        };

        assert_eq!(
            fill("{path} {repo} {branch} {base} {default_branch}", &vars),
            "/w /r fix abc main"
        );
    }

    #[test]
    fn unknown_placeholder_should_find_typo() {
        assert_eq!(unknown_placeholder("--from={bsae}"), Some("bsae"));
    }

    #[test]
    fn unknown_placeholder_should_ignore_known_and_non_placeholder_braces() {
        assert_eq!(unknown_placeholder("{path} {.name} {}"), None);
    }
}
