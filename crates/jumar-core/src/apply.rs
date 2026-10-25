use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::config::Source;
use crate::discovery;
use crate::git;
use crate::herdr::{self, Herdr};
use crate::inspect;
use crate::model::{Head, Health, Worktree};
use crate::plan::{EntryId, Operation};
use crate::safety::Tier;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Remove(Remove),
    Move(Move),
    Create(Create),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remove {
    pub worktree: Worktree,
    pub tier: Tier,
    /// `git status --short` lines for a dirty worktree: what force would discard.
    pub changes: Vec<String>,
    pub force: bool,
    pub delete_branch: bool,
    /// The herdr workspace showing this worktree; removing goes through herdr then.
    pub workspace: Option<herdr::Workspace>,
}

impl Remove {
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        match &self.worktree.head {
            Some(Head::Branch(name)) => Some(name),
            _ => None,
        }
    }

    #[must_use]
    pub fn agents(&self) -> usize {
        self.workspace.as_ref().map_or(0, |w| w.agents)
    }

    /// Uncommitted changes, running agents and a lock all need an explicit force.
    #[must_use]
    pub fn needs_force(&self) -> bool {
        self.tier == Tier::Dirty || self.agents() > 0 || self.worktree.locked.is_some()
    }

    #[must_use]
    pub fn is_runnable(&self) -> bool {
        !self.needs_force() || self.force
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub worktree: Worktree,
    pub to: PathBuf,
    pub rename_branch: Option<String>,
    /// herdr has no move, so its workspace is closed first and reopened at `to`.
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Create {
    pub repo: PathBuf,
    pub source: String,
    pub path: PathBuf,
    pub branch: String,
    pub start: Start,
    pub open_in_herdr: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The branch exists and is checked out as is.
    Existing,
    /// A new branch from this revision.
    From(String),
}

/// An operation that cannot run as written; the confirm screen shows why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub operation: Operation,
    pub reason: String,
}

pub struct Context<'a> {
    pub worktrees: &'a [Worktree],
    /// Tiers already known from inspection, by entry; missing ones are computed.
    pub tiers: &'a [Option<Tier>],
    pub sources: &'a [Source],
    pub default_source: &'a str,
    pub home: &'a Path,
    /// herdr's open workspaces, when a herdr server answered.
    pub herdr: Option<&'a herdr::State>,
}

/// Resolves planned operations against git so the confirm screen can show exactly what
/// will happen.
#[must_use]
pub fn prepare(context: &Context, operations: Vec<Operation>) -> Vec<Result<Step, Blocked>> {
    operations
        .into_iter()
        .map(|operation| {
            let resolved = resolve(context, &operation);
            resolved.map_err(|reason| Blocked { operation, reason })
        })
        .collect()
}

fn resolve(context: &Context, operation: &Operation) -> Result<Step, String> {
    match operation {
        Operation::Remove { id } => remove(context, *id),
        Operation::Move {
            id,
            to,
            rename_branch,
        } => {
            let worktree = &context.worktrees[id.0];
            if worktree.locked.is_some() {
                return Err("it is locked; run `git worktree unlock` to move it".into());
            }
            let workspace = workspace_for(context, worktree);
            if workspace.is_some_and(|w| w.agents > 0) {
                return Err("an agent is running in its herdr workspace".into());
            }
            Ok(Step::Move(Move {
                worktree: worktree.clone(),
                to: to.clone(),
                rename_branch: rename_branch.clone(),
                workspace: workspace.map(|w| w.id.clone()),
            }))
        }
        Operation::Create { target, base, near } => {
            create(context, target, base.as_deref(), *near).map(Step::Create)
        }
    }
}

fn workspace_for<'a>(context: &Context<'a>, worktree: &Worktree) -> Option<&'a herdr::Workspace> {
    context.herdr?.workspace_for(&worktree.path)
}

fn remove(context: &Context, id: EntryId) -> Result<Step, String> {
    let worktree = &context.worktrees[id.0];
    if worktree.health == Health::Orphaned {
        return Err("git no longer tracks this directory; delete it by hand".into());
    }
    let tier = match context.tiers.get(id.0).copied().flatten() {
        Some(tier) => tier,
        None => inspect::inspect(worktree).map_err(|e| e.to_string())?.tier,
    };
    let changes = if tier == Tier::Dirty {
        git::run(&worktree.path, &["status", "--short"])
            .map(|out| out.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let has_branch = matches!(worktree.head, Some(Head::Branch(_)));
    Ok(Step::Remove(Remove {
        worktree: worktree.clone(),
        tier,
        changes,
        force: false,
        delete_branch: has_branch && tier.deletes_branch_by_default(),
        workspace: workspace_for(context, worktree).cloned(),
    }))
}

fn create(
    context: &Context,
    target: &str,
    base: Option<&str>,
    near: Option<EntryId>,
) -> Result<Create, String> {
    let (repo_name, name) = split_target(target);
    if name.contains('/') {
        return Err("a name cannot contain `/`; write repo/name".into());
    }
    let repo = match repo_name {
        Some(repo_name) => find_repo(context.worktrees, repo_name)?,
        None => near
            .map(|id| context.worktrees[id.0].repo.clone())
            .ok_or("no repo to create in; write repo/name")?,
    };
    let source = create_source(context, near)?;
    let template = source
        .create_path
        .as_deref()
        .ok_or_else(|| format!("source `{}` cannot create worktrees", source.name))?;
    let path = expand_template(template, &repo, name, context.home);
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let git_err = |err: Error| err.to_string();
    if !git::is_valid_branch_name(&repo, name).map_err(git_err)? {
        return Err(format!("`{name}` is not a valid branch name"));
    }
    let start = if git::branch_exists(&repo, name).map_err(git_err)? {
        if base.is_some() {
            return Err(format!(
                "branch `{name}` already exists; drop @base to check it out"
            ));
        }
        Start::Existing
    } else {
        Start::From(match base {
            Some(base) => base.to_owned(),
            None => default_base(&repo).map_err(git_err)?,
        })
    };
    Ok(Create {
        repo,
        source: source.name.clone(),
        path,
        branch: name.to_owned(),
        start,
        open_in_herdr: source.opens_in_herdr() && context.herdr.is_some(),
    })
}

fn split_target(target: &str) -> (Option<&str>, &str) {
    match target.split_once('/') {
        Some((repo, name)) => (Some(repo), name),
        None => (None, target),
    }
}

fn find_repo(worktrees: &[Worktree], name: &str) -> Result<PathBuf, String> {
    let mut matches = discovery::repos(worktrees)
        .into_iter()
        .filter(|repo| repo.file_name().is_some_and(|n| n == name));
    match (matches.next(), matches.next()) {
        (Some(repo), None) => Ok(repo),
        (Some(_), Some(_)) => Err(format!("more than one repo is named `{name}`")),
        (None, _) => Err(format!("no known repo named `{name}`")),
    }
}

/// The nearby line's source when it can create worktrees, else the default source.
fn create_source<'a>(context: &'a Context, near: Option<EntryId>) -> Result<&'a Source, String> {
    let find = |name: &str| context.sources.iter().find(|s| s.name == name);
    near.and_then(|id| context.worktrees[id.0].source.as_deref())
        .and_then(find)
        .filter(|source| source.create_path.is_some())
        .or_else(|| find(context.default_source))
        .ok_or_else(|| format!("no source named `{}`", context.default_source))
}

fn default_base(repo: &Path) -> Result<String> {
    Ok(git::default_branches(repo)?
        .first()
        .map_or_else(|| "HEAD".to_owned(), |r| git::short_ref(r).to_owned()))
}

/// Fills `{repo}`, `{repo_name}` and `{name}`, and expands a leading `~`.
#[must_use]
pub fn expand_template(template: &str, repo: &Path, name: &str, home: &Path) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let filled = template
        .replace("{repo_name}", &repo_name)
        .replace("{repo}", &repo.to_string_lossy())
        .replace("{name}", name);
    match filled.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(filled),
    }
}

/// # Errors
///
/// Fails when a git or herdr command fails; earlier commands of the step are not rolled
/// back.
pub fn execute(step: &Step, herdr: &Herdr) -> Result<()> {
    match step {
        Step::Remove(remove) => {
            let worktree = &remove.worktree;
            // Even `--force` refuses a locked worktree, and herdr would hit the same.
            if worktree.locked.is_some() {
                git::run(
                    &worktree.repo,
                    &["worktree", "unlock", utf8(&worktree.path)?],
                )?;
            }
            match &remove.workspace {
                Some(workspace) if worktree.health == Health::Healthy => {
                    herdr.remove(&workspace.id, remove.force)?;
                }
                // A prunable worktree's directory is gone, which `worktree remove` refuses.
                _ if worktree.health == Health::Prunable => {
                    git::run(&worktree.repo, &["worktree", "prune"])?;
                }
                _ => {
                    let mut args = vec!["worktree", "remove"];
                    if remove.force {
                        args.push("--force");
                    }
                    args.push(utf8(&worktree.path)?);
                    git::run(&worktree.repo, &args)?;
                }
            }
            // herdr may already have deleted the branch along with the checkout.
            if remove.delete_branch
                && let Some(branch) = remove.branch()
                && git::branch_exists(&worktree.repo, branch)?
            {
                git::run(&worktree.repo, &["branch", "-D", branch])?;
            }
        }
        Step::Move(step) => {
            let worktree = &step.worktree;
            if let Some(workspace) = &step.workspace {
                herdr.close_workspace(workspace)?;
            }
            let args = ["worktree", "move", utf8(&worktree.path)?, utf8(&step.to)?];
            git::run(&worktree.repo, &args)?;
            if let (Some(new), Some(Head::Branch(old))) = (&step.rename_branch, &worktree.head) {
                git::run(&worktree.repo, &["branch", "-m", old, new])?;
            }
            if step.workspace.is_some() {
                herdr.open(&step.to, &worktree.repo, false)?;
            }
        }
        Step::Create(create) => {
            let path = utf8(&create.path)?;
            match &create.start {
                Start::Existing => {
                    git::run(&create.repo, &["worktree", "add", path, &create.branch])?;
                }
                // `--no-track` keeps a branch made from `origin/main` from pushing to main.
                Start::From(base) => {
                    let args = [
                        "worktree",
                        "add",
                        "--no-track",
                        "-b",
                        &create.branch,
                        path,
                        base,
                    ];
                    git::run(&create.repo, &args)?;
                }
            }
            if create.open_in_herdr {
                herdr.open(&create.path, &create.repo, false)?;
            }
        }
    }
    Ok(())
}

/// Runs `steps` one at a time on a background thread, in order, sending each result
/// tagged with its index. Later steps still run after a failure.
#[must_use]
pub fn execute_all(steps: Vec<(usize, Step)>, herdr: Herdr) -> Receiver<(usize, Result<()>)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for (index, step) in steps {
            if tx.send((index, execute(&step, &herdr))).is_err() {
                break;
            }
        }
    });
    rx
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::NonUtf8Path(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_template_should_fill_placeholders() {
        let path = expand_template(
            "{repo}/.claude/worktrees/{name}",
            Path::new("/r/app"),
            "fix",
            Path::new("/h"),
        );

        assert_eq!(path, Path::new("/r/app/.claude/worktrees/fix"));
    }

    #[test]
    fn expand_template_should_fill_repo_name_and_home() {
        let path = expand_template(
            "~/wt/{repo_name}/{name}",
            Path::new("/r/app"),
            "fix",
            Path::new("/h"),
        );

        assert_eq!(path, Path::new("/h/wt/app/fix"));
    }

    #[test]
    fn split_target_should_take_repo_before_slash() {
        assert_eq!(split_target("app/fix"), (Some("app"), "fix"));
    }

    #[test]
    fn split_target_should_leave_bare_name_without_repo() {
        assert_eq!(split_target("fix"), (None, "fix"));
    }
}
