use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::config::{self, Config, Source};
use crate::discovery;
use crate::git;
use crate::herdr::{self, Herdr};
use crate::inspect;
use crate::model::{Head, Health, Worktree};
use crate::plan::{self, EntryId, Operation, Target};
use crate::safety::Tier;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Remove(Remove),
    Move(Move),
    Create(Create),
    Split(Box<Split>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remove {
    pub worktree: Worktree,
    pub tier: Tier,
    /// `git status --short` lines for a dirty worktree: what force would discard.
    pub changes: Vec<String>,
    /// An unmerged branch whose remote branch was deleted, often after its PR merged.
    pub upstream_gone: bool,
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
    /// `None` keeps the directory and only renames the branch.
    pub to: Option<PathBuf>,
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
    /// Files copied from the main checkout, relative to it.
    pub copy: Vec<PathBuf>,
    /// Shell commands run in the new worktree once it exists.
    pub on_create: Vec<String>,
    /// The remote ref a new branch starts from, fetched first.
    pub fetch: Option<Fetch>,
}

/// Moves the branch of a repo's main checkout into a new worktree, carrying any
/// uncommitted changes in a stash, and switches the main checkout to the default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub main: Worktree,
    /// The local branch the main checkout switches to.
    pub default_branch: String,
    /// `git status --short` lines that move with the branch.
    pub changes: Vec<String>,
    /// The new worktree, which checks out the moved branch.
    pub create: Create,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch {
    pub remote: String,
    /// The ref on the remote, like `refs/heads/main` or `refs/pull/123/head`.
    pub from: String,
    /// Where it lands locally, which is also where the new branch starts.
    pub into: String,
    /// The PR's number. A PR can't be created without its fetch; a branch falls back to
    /// what was last fetched.
    pub pull: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// The branch exists and is checked out as is.
    Existing,
    /// A new branch from this revision.
    From(String),
    /// A new branch tracking this remote branch of the same name, like `origin/fix`.
    Tracking(String),
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
    /// For each repo's `copy` and `on_create`.
    pub config: &'a Config,
    /// herdr's open workspaces, when a herdr server answered.
    pub herdr: Option<&'a herdr::State>,
}

/// Resolves planned operations against git so the confirm screen can show exactly what
/// will happen.
#[must_use]
pub fn prepare(context: &Context, operations: Vec<Operation>) -> Vec<Result<Step, Blocked>> {
    let mut paths = HashSet::new();
    // Each step is checked against git as it is now, so it must not depend on a branch
    // that another line takes, or that an earlier step removes or renames.
    let mut taken: Vec<(PathBuf, String)> = Vec::new();
    let mut changed: Vec<(PathBuf, String, &str)> = Vec::new();
    let mut refs = RefCache::default();
    let mut results = Vec::new();
    for operation in operations {
        let resolved = resolve(context, &mut refs, &operation).and_then(|step| {
            // `jb/fix` and `jb-fix` share a directory, which only one line can have.
            let (path, branch) = claims(&step);
            if let Some(path) = path
                && paths.contains(path)
            {
                return Err(format!("another line also uses {}", path.display()));
            }
            if let Some((repo, branch)) = branch {
                let clashes = |r: &Path, b: &str| r == repo && git::branches_clash(b, branch);
                if let Some((_, other)) = taken.iter().find(|(r, b)| clashes(r, b)) {
                    return Err(if other == branch {
                        format!("another line also uses branch `{branch}`")
                    } else {
                        format!("another line's branch `{other}` stops git from making `{branch}`")
                    });
                }
                if let Some((_, other, what)) = changed.iter().find(|(r, b, _)| clashes(r, b)) {
                    return Err(format!(
                        "this save also {what} branch `{other}`; apply that first"
                    ));
                }
                // git checks a branch out in only one worktree at a time.
                if let Step::Create(create) = &step
                    && matches!(create.start, Start::Existing)
                    && let Some(other) = context.worktrees.iter().find(|w| {
                        w.repo == repo && w.head.as_ref() == Some(&Head::Branch(branch.to_owned()))
                    })
                {
                    return Err(format!(
                        "branch `{branch}` is checked out in {}",
                        other.path.display()
                    ));
                }
                taken.push((repo.to_path_buf(), branch.to_owned()));
            }
            paths.extend(path.map(Path::to_path_buf));
            changed.extend(frees(&step));
            Ok(step)
        });
        results.push(resolved.map_err(|reason| Blocked { operation, reason }));
    }
    results
}

/// Each repo's refs, read the first time a step needs them.
#[derive(Default)]
struct RefCache(HashMap<PathBuf, git::Refs>);

impl RefCache {
    fn get(&mut self, repo: &Path) -> Result<&git::Refs, String> {
        if !self.0.contains_key(repo) {
            let refs = git::Refs::read(repo).map_err(git_err)?;
            self.0.insert(repo.to_path_buf(), refs);
        }
        Ok(&self.0[repo])
    }
}

/// The branch a step takes away from where it is now, and how.
fn frees(step: &Step) -> Option<(PathBuf, String, &'static str)> {
    let (worktree, what) = match step {
        Step::Remove(remove) => (&remove.worktree, "removes the worktree of"),
        Step::Move(step) if step.rename_branch.is_some() => (&step.worktree, "renames"),
        _ => return None,
    };
    match &worktree.head {
        Some(Head::Branch(branch)) => Some((worktree.repo.clone(), branch.clone(), what)),
        _ => None,
    }
}

/// The directory and the repo's branch a step takes for itself.
fn claims(step: &Step) -> (Option<&Path>, Option<(&Path, &str)>) {
    match step {
        Step::Move(step) => (
            step.to.as_deref(),
            step.rename_branch
                .as_deref()
                .map(|b| (step.worktree.repo.as_path(), b)),
        ),
        Step::Create(step) => (
            Some(&step.path),
            Some((step.repo.as_path(), step.branch.as_str())),
        ),
        Step::Split(split) => (
            Some(&split.create.path),
            Some((split.create.repo.as_path(), split.create.branch.as_str())),
        ),
        Step::Remove(_) => (None, None),
    }
}

fn resolve(context: &Context, refs: &mut RefCache, operation: &Operation) -> Result<Step, String> {
    match operation {
        Operation::Remove { id } => remove(context, *id),
        Operation::Move {
            id,
            to,
            rename_branch,
        } => {
            let worktree = &context.worktrees[id.0];
            // git only refuses to move a locked worktree; its branch can still be renamed.
            if to.is_some() && worktree.locked.is_some() {
                return Err("it is locked; run `git worktree unlock` to move it".into());
            }
            // An agent could commit or push to the old branch name, so renaming it waits too.
            let workspace = workspace_for(context, worktree);
            if workspace.is_some_and(|w| w.agents > 0) {
                return Err("an agent is running in its herdr workspace".into());
            }
            if let (Some(new), Some(Head::Branch(old))) = (rename_branch, &worktree.head) {
                check_new_branch(refs.get(&worktree.repo)?, &worktree.repo, new, old)?;
            }
            Ok(Step::Move(Move {
                worktree: worktree.clone(),
                to: to.clone(),
                rename_branch: rename_branch.clone(),
                // herdr only needs closing and reopening when the directory moves.
                workspace: workspace.filter(|_| to.is_some()).map(|w| w.id.clone()),
            }))
        }
        Operation::Create { target, near } => {
            create(context, refs, target, *near).map(Step::Create)
        }
        Operation::Split { id } => split(context, refs, *id).map(|s| Step::Split(Box::new(s))),
    }
}

fn split(context: &Context, refs: &mut RefCache, id: EntryId) -> Result<Split, String> {
    let main = &context.worktrees[id.0];
    let Some(Head::Branch(branch)) = &main.head else {
        return Err("the main checkout has no branch to move".into());
    };
    if workspace_for(context, main).is_some_and(|w| w.agents > 0) {
        return Err("an agent is running in its herdr workspace".into());
    }
    let base = default_base(context, &main.repo).map_err(git_err)?;
    if base == "HEAD" {
        return Err(format!(
            "no default branch to switch to; set default_branch for {}",
            main.repo.display()
        ));
    }
    let remotes = &refs.get(&main.repo)?.remotes;
    let default_branch = git::split_remote(remotes, &base)
        .map_or(base.as_str(), |(_, name)| name)
        .to_owned();
    if *branch == default_branch {
        return Err(format!(
            "the main checkout is already on `{default_branch}`"
        ));
    }
    let on_default = Some(Head::Branch(default_branch.clone()));
    if let Some(other) = context
        .worktrees
        .iter()
        .find(|w| w.repo == main.repo && w.head == on_default)
    {
        return Err(format!(
            "branch `{default_branch}` is checked out in {}",
            other.path.display()
        ));
    }
    let target = Target {
        repo: None,
        dir: plan::dir_name_for(branch),
        name: branch.clone(),
        base: None,
        pull: None,
    };
    let create = create(context, refs, &target, Some(id))?;
    let changes = git::run(&main.path, &["status", "--short"])
        .map_err(git_err)?
        .lines()
        .map(str::to_owned)
        .collect();
    Ok(Split {
        main: main.clone(),
        default_branch,
        changes,
        create,
    })
}

/// Checked before anything runs, since a failed `git branch -m` would leave a move half
/// done. `old` is the branch being renamed, which git lets the new name replace.
fn check_new_branch(refs: &git::Refs, repo: &Path, branch: &str, old: &str) -> Result<(), String> {
    check_branch_name(repo, branch)?;
    match refs.clashing_branch(branch, Some(old)) {
        Some(clash) if clash == branch => Err(format!("branch `{branch}` already exists")),
        Some(clash) => Err(clash_reason(&clash, branch)),
        None => Ok(()),
    }
}

fn check_branch_name(repo: &Path, branch: &str) -> Result<(), String> {
    if git::is_valid_branch_name(repo, branch).map_err(git_err)? {
        Ok(())
    } else {
        Err(format!("`{branch}` is not a valid branch name"))
    }
}

fn clash_reason(clash: &str, branch: &str) -> String {
    format!("branch `{clash}` stops git from making `{branch}`")
}

#[expect(clippy::needless_pass_by_value, reason = "fits `map_err` directly")]
fn git_err(err: Error) -> String {
    err.to_string()
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
        None => inspect::inspect(worktree).map_err(git_err)?.tier,
    };
    let changes = if tier == Tier::Dirty {
        git::run(&worktree.path, &["status", "--short"])
            .map(|out| out.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let upstream_gone = tier == Tier::Unmerged
        && worktree.health == Health::Healthy
        && git::status(&worktree.path).is_ok_and(|s| s.upstream_gone);
    let has_branch = matches!(worktree.head, Some(Head::Branch(_)));
    Ok(Step::Remove(Remove {
        worktree: worktree.clone(),
        tier,
        changes,
        upstream_gone,
        force: false,
        delete_branch: has_branch && tier.deletes_branch_by_default(),
        workspace: workspace_for(context, worktree).cloned(),
    }))
}

fn create(
    context: &Context,
    refs: &mut RefCache,
    target: &Target,
    near: Option<EntryId>,
) -> Result<Create, String> {
    let name = target.name.as_str();
    let repo = match &target.repo {
        Some(repo) => find_repo(context, repo)?,
        None => near
            .map(|id| context.worktrees[id.0].repo.clone())
            .ok_or("no repo to create in; write its path first, like ~/code/app:feat")?,
    };
    let source = create_source(context, near)?;
    let template = source
        .create_path
        .as_deref()
        .ok_or_else(|| format!("source `{}` cannot create worktrees", source.name))?;
    let path = expand_template(template, &repo, &target.dir, context.home);
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    check_branch_name(&repo, name)?;
    let refs = refs.get(&repo)?;
    let exists = match refs.clashing_branch(name, None) {
        Some(clash) if clash == name => true,
        Some(clash) => return Err(clash_reason(&clash, name)),
        None => false,
    };
    let base = target.base.as_deref();
    let mut fetch = None;
    let start = if exists {
        if base.is_some() || target.pull.is_some() {
            return Err(format!(
                "branch `{name}` already exists; write just `{name}` to check it out"
            ));
        }
        Start::Existing
    } else {
        let remotes = &refs.remotes;
        if let Some(number) = target.pull {
            let pull = pull_fetch(&repo, remotes, number)?;
            let start = Start::From(pull.into.clone());
            fetch = Some(pull);
            start
        } else if base.is_none()
            && let Some(remote) = refs.remote_branch(name)
        {
            if context.config.fetch_on_create() {
                fetch = fetch_for(remotes, &remote);
            }
            Start::Tracking(remote)
        } else {
            let start = match base {
                Some(base) => base.to_owned(),
                None => default_base(context, &repo).map_err(git_err)?,
            };
            if context.config.fetch_on_create() {
                fetch = fetch_for(remotes, &start);
            }
            // A remote branch not fetched yet only exists once its fetch runs.
            if base.is_some()
                && fetch.is_none()
                && !git::is_commit(&repo, &start).map_err(git_err)?
            {
                return Err(format!("`{start}` is not a branch, tag or commit"));
            }
            Start::From(start)
        }
    };
    let setup = context.config.repo(&repo, context.home);
    let copy = match setup {
        Some(setup) => matching_files(&repo, &setup.copy)?,
        None => Vec::new(),
    };
    Ok(Create {
        source: source.name.clone(),
        path,
        branch: name.to_owned(),
        start,
        open_in_herdr: source.opens_in_herdr() && context.herdr.is_some(),
        copy,
        on_create: setup.map(|s| s.on_create.clone()).unwrap_or_default(),
        fetch,
        repo,
    })
}

/// PR `number`'s head from `upstream`, where a fork's PRs live, or else `origin`.
fn pull_fetch(repo: &Path, remotes: &[String], number: u32) -> Result<Fetch, String> {
    let remote = ["upstream", "origin"]
        .into_iter()
        .find(|name| remotes.iter().any(|r| r == name))
        .ok_or("no upstream or origin remote to fetch the PR from")?;
    let url = git::run(repo, &["remote", "get-url", remote]).unwrap_or_default();
    let from = if url.contains("gitlab") {
        format!("refs/merge-requests/{number}/head")
    } else {
        format!("refs/pull/{number}/head")
    };
    Ok(Fetch {
        remote: remote.to_owned(),
        from,
        into: format!("refs/remotes/{remote}/pr/{number}"),
        pull: Some(number),
    })
}

/// `base` as a branch of one of the repo's remotes, like `origin/main`.
fn fetch_for(remotes: &[String], base: &str) -> Option<Fetch> {
    let (remote, branch) = git::split_remote(remotes, base)?;
    Some(Fetch {
        remote: remote.to_owned(),
        from: format!("refs/heads/{branch}"),
        into: format!("refs/remotes/{remote}/{branch}"),
        pull: None,
    })
}

/// Files in `repo` matching `patterns`, relative to it. Git's own directory and paths
/// outside the checkout are never matched.
fn matching_files(repo: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, String> {
    let base = glob::Pattern::escape(&repo.to_string_lossy());
    let mut files: Vec<PathBuf> = Vec::new();
    for pattern in patterns {
        let paths = glob::glob(&format!("{base}/{pattern}"))
            .map_err(|err| format!("invalid copy pattern `{pattern}`: {err}"))?;
        for path in paths.flatten() {
            let Ok(relative) = path.strip_prefix(repo) else {
                continue;
            };
            let inside = relative
                .components()
                .all(|c| matches!(c, Component::Normal(_)) && c.as_os_str() != ".git");
            if inside && path.is_file() && !files.iter().any(|f| f == relative) {
                files.push(relative.to_path_buf());
            }
        }
    }
    Ok(files)
}

fn find_repo(context: &Context, name: &str) -> Result<PathBuf, String> {
    if plan::is_repo_path(name) {
        let path = config::expand_home(name, context.home);
        return discovery::repo_containing(&path)
            .ok_or_else(|| format!("{name} is not a git repo"));
    }
    let mut found = discovery::repos(context.worktrees);
    found.retain(|repo| repo.file_name().is_some_and(|n| n == name));
    match &found[..] {
        [repo] => Ok(repo.clone()),
        [] => Err(format!("no known repo named `{name}`")),
        _ => Err(format!("more than one repo is named `{name}`")),
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

fn default_base(context: &Context, repo: &Path) -> Result<String> {
    let configured = context
        .worktrees
        .iter()
        .find(|w| w.repo == repo)
        .and_then(|w| w.default_branch.as_deref());
    Ok(git::default_branches(repo, configured)?
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

/// Returns notes on anything that went less well than planned but did not stop the step.
///
/// # Errors
///
/// Fails when a git or herdr command fails; earlier commands of the step are not rolled
/// back.
pub fn execute(step: &Step, herdr: &Herdr) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    match step {
        Step::Remove(remove) => execute_remove(remove, herdr, &mut notes)?,
        Step::Move(step) => execute_move(step, herdr)?,
        Step::Create(create) => execute_create(create, herdr, &mut notes)?,
        Step::Split(split) => execute_split(split, herdr, &mut notes)?,
    }
    Ok(notes)
}

fn execute_remove(remove: &Remove, herdr: &Herdr, notes: &mut Vec<String>) -> Result<()> {
    let worktree = &remove.worktree;
    // The grade can be minutes old by now, and an agent may have committed since.
    let mut delete_branch = remove.delete_branch;
    if delete_branch
        && remove.tier == Tier::Safe
        && worktree.health == Health::Healthy
        && !inspect::inspect(worktree).is_ok_and(|i| i.tier == Tier::Safe)
        && let Some(branch) = remove.branch()
    {
        delete_branch = false;
        notes.push(format!(
            "kept branch {branch}: it changed since jumar checked it, so deleting it \
             might lose work"
        ));
    }
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
    if delete_branch
        && let Some(branch) = remove.branch()
        && git::branch_exists(&worktree.repo, branch)?
    {
        git::run(&worktree.repo, &["branch", "-D", branch])?;
    }
    Ok(())
}

fn execute_move(step: &Move, herdr: &Herdr) -> Result<()> {
    let worktree = &step.worktree;
    if let Some(workspace) = &step.workspace {
        herdr.close_workspace(workspace)?;
    }
    if let Some(to) = &step.to {
        let args = ["worktree", "move", utf8(&worktree.path)?, utf8(to)?];
        git::run(&worktree.repo, &args)?;
    }
    if let (Some(new), Some(Head::Branch(old))) = (&step.rename_branch, &worktree.head) {
        git::run(&worktree.repo, &["branch", "-m", old, new])?;
    }
    if let Some(to) = &step.to
        && step.workspace.is_some()
    {
        herdr.open(to, &worktree.repo, false)?;
    }
    Ok(())
}

fn execute_create(create: &Create, herdr: &Herdr, notes: &mut Vec<String>) -> Result<()> {
    if let Some(fetch) = &create.fetch
        && let Err(err) = git::fetch(&create.repo, &fetch.remote, &fetch.from, &fetch.into)
    {
        if let Some(number) = fetch.pull {
            return Err(Error::PullFetch {
                number,
                remote: fetch.remote.clone(),
                source: Box::new(err),
            });
        }
        let start = git::short_ref(&fetch.into).to_owned();
        if !git::is_commit(&create.repo, &fetch.into)? {
            return Err(Error::StartFetch {
                start,
                source: Box::new(err),
            });
        }
        notes.push(format!(
            "could not fetch {start}, so it starts from the last fetched commit: {err}"
        ));
    }
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
        Start::Tracking(remote) => {
            let args = [
                "worktree",
                "add",
                "--track",
                "-b",
                &create.branch,
                path,
                remote,
            ];
            git::run(&create.repo, &args)?;
        }
    }
    // The worktree exists now, so it opens in herdr even when its setup failed.
    let setup = set_up(create);
    if create.open_in_herdr {
        herdr.open(&create.path, &create.repo, false)?;
    }
    setup?;
    Ok(())
}

fn execute_split(split: &Split, herdr: &Herdr, notes: &mut Vec<String>) -> Result<()> {
    let main = split.main.path.as_path();
    let create = &split.create;
    // Always stash, since files can change while the review is open. `stash push` succeeds
    // without making a stash when there is nothing to save.
    let before = git::resolve(main, "refs/stash")?;
    let message = format!("jumar: moving {} to its own worktree", create.branch);
    git::run(
        main,
        &["stash", "push", "--include-untracked", "-m", &message],
    )?;
    let after = git::resolve(main, "refs/stash")?;
    let stash = after.filter(|after| before.as_ref() != Some(after));
    let stash = stash.as_deref();

    if let Err(err) = git::run(main, &["switch", &split.default_branch]) {
        return Err(restore(main, None, stash, err));
    }
    let added = utf8(&create.path)
        .and_then(|path| git::run(main, &["worktree", "add", path, &create.branch]));
    if let Err(err) = added {
        return Err(restore(main, Some(&create.branch), stash, err));
    }
    if let Some(stash) = stash
        && let Err(err) = unstash(&create.path, stash)
    {
        notes.push(format!(
            "your changes are still in the stash as {stash}; run `git stash apply {stash}` \
             in the new worktree: {err}"
        ));
    }
    let setup = set_up(create);
    if create.open_in_herdr {
        herdr.open(&create.path, &create.repo, false)?;
    }
    setup
}

/// Puts the main checkout back on `branch` with its changes after a failed split, and
/// adds to `err` whatever could not be put back.
fn restore(main: &Path, branch: Option<&str>, stash: Option<&str>, err: Error) -> Error {
    let mut lost = Vec::new();
    let switched = match branch.map(|branch| (branch, git::run(main, &["switch", branch]))) {
        Some((branch, Err(switch))) => {
            lost.push(format!(
                "the main checkout stays on the default branch, not {branch} ({switch})"
            ));
            false
        }
        _ => true,
    };
    if let Some(stash) = stash {
        // The changes belong on the branch, so they wait in the stash if that is not back.
        let unstashed = if switched {
            unstash(main, stash)
        } else {
            Ok(())
        };
        if !switched || unstashed.is_err() {
            let why = unstashed
                .err()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default();
            lost.push(format!(
                "your changes are still in the stash as {stash}{why}"
            ));
        }
    }
    if lost.is_empty() {
        err
    } else {
        Error::Rollback {
            source: Box::new(err),
            note: lost.join("; "),
        }
    }
}

/// Applies the stash entry with commit `stash` in `dir`, then drops it. The stash list is
/// shared by every worktree, so the entry is found by its commit, not as the newest.
fn unstash(dir: &Path, stash: &str) -> Result<()> {
    git::run(dir, &["stash", "apply", stash])?;
    let list = git::run(dir, &["stash", "list", "--format=%H"])?;
    if let Some(index) = list.lines().position(|line| line == stash) {
        git::run(dir, &["stash", "drop", &format!("stash@{{{index}}}")])?;
    }
    Ok(())
}

fn set_up(create: &Create) -> Result<()> {
    for relative in &create.copy {
        let to = create.path.join(relative);
        // A tracked file is already checked out.
        if to.exists() {
            continue;
        }
        let copied = to
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::copy(create.repo.join(relative), &to));
        copied.map_err(|source| Error::Copy {
            path: relative.clone(),
            source,
        })?;
    }
    for command in &create.on_create {
        let failed = |output: String| Error::Setup {
            command: command.clone(),
            output,
        };
        let output = Command::new("sh")
            .args(["-c", command])
            .current_dir(&create.path)
            .env("JUMAR_REPO", &create.repo)
            .env("JUMAR_WORKTREE", &create.path)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| failed(err.to_string()))?;
        if !output.status.success() {
            return Err(failed(last_lines(&output)));
        }
    }
    Ok(())
}

/// The end of a failed command's stderr, or of its stdout when stderr is empty, on one line.
fn last_lines(output: &Output) -> String {
    let stream = if output.stderr.trim_ascii().is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let text = String::from_utf8_lossy(stream);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    match lines.len() {
        0 => format!("exited with {}", output.status),
        n => lines[n.saturating_sub(3)..].join("; "),
    }
}

/// Runs `steps` one at a time on a background thread, in order, sending each result
/// tagged with its index. Later steps still run after a failure.
#[must_use]
pub fn execute_all(
    steps: Vec<(usize, Step)>,
    herdr: Herdr,
) -> Receiver<(usize, Result<Vec<String>>)> {
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
}
