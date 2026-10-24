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
    let (base, default_branch) = if uses("{base}") || uses("{default_branch}") {
        let default = git::default_branches(&worktree.repo)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::NoDefaultBranch(worktree.repo.clone()))?;
        let base = if uses("{base}") {
            git::run(&worktree.path, &["merge-base", &default, "HEAD"])?
                .trim()
                .to_owned()
        } else {
            String::new()
        };
        (base, git::short_ref(&default).to_owned())
    } else {
        (String::new(), String::new())
    };
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
}
