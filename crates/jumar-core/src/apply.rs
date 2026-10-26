use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::config::{Config, Source};
use crate::discovery;
use crate::git;
use crate::herdr::{self, Herdr};
use crate::inspect;
use crate::model::{Head, Health, Worktree};
use crate::plan::{self, EntryId, Operation};
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
    /// Files copied from the main checkout, relative to it.
    pub copy: Vec<PathBuf>,
    /// Shell commands run in the new worktree once it exists.
    pub on_create: Vec<String>,
    /// The remote ref a new branch starts from, fetched first.
    pub fetch: Option<Fetch>,
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
    target: &str,
    base: Option<&str>,
    near: Option<EntryId>,
) -> Result<Create, String> {
    let (repo_name, name) = split_target(target);
    let (name, pull) = pull_request(name, base)?;
    let name = name.as_str();
    let base = base.filter(|_| pull.is_none());
    // `app/fix` meant repo `app` before `:` took over.
    if repo_name.is_none()
        && let Some((prefix, rest)) = name.split_once('/')
        && find_repo(context.worktrees, prefix).is_ok()
    {
        return Err(format!(
            "`{prefix}` is a repo; write `{prefix}:{rest}` to create there"
        ));
    }
    let repo = match repo_name {
        Some(repo_name) => find_repo(context.worktrees, repo_name)?,
        None => near
            .map(|id| context.worktrees[id.0].repo.clone())
            .ok_or("no repo to create in; write repo:name")?,
    };
    let source = create_source(context, near)?;
    let template = source
        .create_path
        .as_deref()
        .ok_or_else(|| format!("source `{}` cannot create worktrees", source.name))?;
    let path = expand_template(template, &repo, &plan::dir_name_for(name), context.home);
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let git_err = |err: Error| err.to_string();
    if !git::is_valid_branch_name(&repo, name).map_err(git_err)? {
        return Err(format!("`{name}` is not a valid branch name"));
    }
    let mut fetch = None;
    let start = if git::branch_exists(&repo, name).map_err(git_err)? {
        if base.is_some() || pull.is_some() {
            return Err(format!(
                "branch `{name}` already exists; write just `{name}` to check it out"
            ));
        }
        Start::Existing
    } else if let Some(number) = pull {
        let pull = pull_fetch(&repo, number)?;
        let start = Start::From(pull.into.clone());
        fetch = Some(pull);
        start
    } else if base.is_none()
        && let Some(remote) = git::remote_branch(&repo, name).map_err(git_err)?
    {
        if context.config.fetch_on_create() {
            fetch = remote_branch(&repo, &remote);
        }
        Start::Tracking(remote)
    } else {
        let explicit = base.is_some();
        let base = match base {
            Some(base) => base.to_owned(),
            None => default_base(context, &repo).map_err(git_err)?,
        };
        if context.config.fetch_on_create() {
            fetch = remote_branch(&repo, &base);
        }
        // A remote branch not fetched yet only exists once its fetch runs.
        if explicit && fetch.is_none() && !git::is_commit(&repo, &base).map_err(git_err)? {
            return Err(format!("`{base}` is not a branch, tag or commit"));
        }
        Start::From(base)
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

/// The branch name and PR number of a create line: `#123` is branch `pr-123`, and
/// `name@#123` names it.
fn pull_request(name: &str, base: Option<&str>) -> Result<(String, Option<u32>), String> {
    let number = |text: &str| {
        text.parse::<u32>()
            .map_err(|_| format!("`#{text}` is not a PR number"))
    };
    match (name.strip_prefix('#'), base.map(|b| b.strip_prefix('#'))) {
        (Some(n), None) => {
            let n = number(n)?;
            Ok((format!("pr-{n}"), Some(n)))
        }
        (Some(_), Some(_)) => Err("write name@#123 to name a PR's worktree".into()),
        (None, Some(Some(n))) => Ok((name.to_owned(), Some(number(n)?))),
        (None, _) => Ok((name.to_owned(), None)),
    }
}

/// PR `number`'s head from `upstream`, where a fork's PRs live, or else `origin`.
fn pull_fetch(repo: &Path, number: u32) -> Result<Fetch, String> {
    let remotes = git::run(repo, &["remote"]).map_err(|e| e.to_string())?;
    let remote = ["upstream", "origin"]
        .into_iter()
        .find(|name| remotes.lines().any(|r| r == *name))
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
fn remote_branch(repo: &Path, base: &str) -> Option<Fetch> {
    let base = base.strip_prefix("refs/remotes/").unwrap_or(base);
    let remotes = git::run(repo, &["remote"]).ok()?;
    remotes
        .lines()
        .filter_map(|remote| {
            let branch = base.strip_prefix(remote)?.strip_prefix('/')?;
            Some(Fetch {
                remote: remote.to_owned(),
                from: format!("refs/heads/{branch}"),
                into: format!("refs/remotes/{remote}/{branch}"),
                pull: None,
            })
        })
        // A remote named `a/b` beats `a` for `a/b/c`.
        .max_by_key(|found| found.remote.len())
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

/// `repo:name`; git forbids `:` in branch names, so the split is never ambiguous.
fn split_target(target: &str) -> (Option<&str>, &str) {
    match target.split_once(':') {
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
    let args = ["worktree", "move", utf8(&worktree.path)?, utf8(&step.to)?];
    git::run(&worktree.repo, &args)?;
    if let (Some(new), Some(Head::Branch(old))) = (&step.rename_branch, &worktree.head) {
        git::run(&worktree.repo, &["branch", "-m", old, new])?;
    }
    if step.workspace.is_some() {
        herdr.open(&step.to, &worktree.repo, false)?;
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
        notes.push(format!(
            "could not fetch {}, so it starts from the last fetched commit: {err}",
            git::short_ref(&fetch.into)
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

    #[test]
    fn split_target_should_take_repo_before_colon() {
        assert_eq!(split_target("app:jb/fix"), (Some("app"), "jb/fix"));
    }

    #[test]
    fn split_target_should_leave_bare_name_without_repo() {
        assert_eq!(split_target("fix"), (None, "fix"));
    }
}
