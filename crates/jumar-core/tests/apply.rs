#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]
#![expect(
    clippy::result_large_err,
    reason = "helpers return what `prepare` does"
)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use jumar_core::apply::{self, Blocked, Context, Start, Step};
use jumar_core::config::Config;
use jumar_core::git;
use jumar_core::herdr::{self, Herdr};
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::plan::{self, EntryId, Operation};
use jumar_core::safety::Tier;
use tempfile::TempDir;

use common::{git, init_repo};

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    worktrees: Vec<Worktree>,
    herdr: Option<herdr::State>,
    config: Config,
}

/// A create for line `text`, opened on worktree 0.
fn create_op(text: &str) -> Operation {
    Operation::Create {
        target: plan::parse_target(text).unwrap(),
        near: Some(EntryId(0)),
    }
}

impl Fixture {
    /// `~/app` with claude worktrees `feat` (no new commits) and `wip` (one commit).
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(dir.path()).unwrap();
        init_repo(&home.join("app"));
        let mut fx = Self {
            _dir: dir,
            home,
            worktrees: Vec::new(),
            herdr: None,
            config: Config::default(),
        };
        fx.add("feat");
        let wip = fx.add("wip");
        git(&wip, &["commit", "-q", "--allow-empty", "-m", "work"]);
        fx
    }

    fn repo(&self) -> PathBuf {
        self.home.join("app")
    }

    fn add(&mut self, name: &str) -> PathBuf {
        let path = self.repo().join(".claude/worktrees").join(name);
        git(
            &self.repo(),
            &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
        );
        self.worktrees.push(Worktree {
            path: path.clone(),
            repo: self.repo(),
            source: Some("claude".into()),
            head: Some(Head::Branch(name.into())),
            health: Health::Healthy,
            locked: None,
            main: false,
            default_branch: None,
        });
        path
    }

    /// Gives the repo a `[[repo]]` entry with these extra TOML lines.
    fn set_repo_config(&mut self, lines: &str) {
        let path = toml::Value::from(self.repo().to_str().unwrap());
        let text = format!("[[repo]]\npath = {path}\n{lines}\n");
        self.config = Config::from_toml(&text, Path::new("config.toml")).unwrap();
    }

    fn prepare(&self, operation: Operation) -> Result<Step, Blocked> {
        self.prepare_all(vec![operation]).remove(0)
    }

    fn prepare_all(&self, operations: Vec<Operation>) -> Vec<Result<Step, Blocked>> {
        let context = Context {
            worktrees: &self.worktrees,
            tiers: &[],
            sources: self.config.sources(),
            default_source: "claude",
            home: &self.home,
            config: &self.config,
            herdr: self.herdr.as_ref(),
        };
        apply::prepare(&context, operations)
    }

    fn remove(&self, index: usize) -> apply::Remove {
        match self
            .prepare(Operation::Remove { id: EntryId(index) })
            .unwrap()
        {
            Step::Remove(remove) => remove,
            step => panic!("expected a remove, got {step:?}"),
        }
    }

    fn create(&self, target: &str, base: Option<&str>) -> Result<apply::Create, Blocked> {
        let text = match base {
            Some(base) => format!("{target}@{base}"),
            None => target.to_owned(),
        };
        self.prepare(create_op(&text)).map(|step| match step {
            Step::Create(create) => create,
            step => panic!("expected a create, got {step:?}"),
        })
    }

    /// Pushes `main` to `origin` as `branch` and fetches it, so `branch` is only remote.
    fn with_remote_branch(&self, branch: &str) {
        self.add_remote_ahead();
        git(
            &self.repo(),
            &["push", "-q", "origin", &format!("main:{branch}")],
        );
        git(&self.repo(), &["fetch", "-q", "origin"]);
    }

    /// Marks worktree `index` as open in a herdr workspace with `agents` agents.
    fn open_in_herdr(&mut self, index: usize, agents: usize) {
        self.herdr = Some(herdr::State {
            workspaces: vec![herdr::Workspace {
                id: "wB".into(),
                checkout: self.worktrees[index].path.clone(),
                agents,
                status: herdr::AgentStatus::default(),
            }],
        });
    }

    /// Lists the main checkout, switched to a new branch `branch` with one commit.
    fn main_on(&mut self, branch: &str) -> EntryId {
        git(&self.repo(), &["switch", "-q", "-c", branch]);
        git(
            &self.repo(),
            &["commit", "-q", "--allow-empty", "-m", "side"],
        );
        self.worktrees.push(Worktree {
            path: self.repo(),
            repo: self.repo(),
            source: None,
            head: Some(Head::Branch(branch.into())),
            health: Health::Healthy,
            locked: None,
            main: true,
            default_branch: None,
        });
        EntryId(self.worktrees.len() - 1)
    }

    fn split(&self, id: EntryId) -> Result<apply::Split, Blocked> {
        self.prepare(Operation::Split { id })
            .map(|step| match step {
                Step::Split(split) => *split,
                step => panic!("expected a split, got {step:?}"),
            })
    }

    fn has_branch(&self, name: &str) -> bool {
        git::branch_exists(&self.repo(), name).unwrap()
    }
}

fn current_branch(dir: &Path) -> String {
    git::run(dir, &["branch", "--show-current"])
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn prepare_should_delete_branch_of_safe_worktree() {
    assert!(Fixture::new().remove(0).delete_branch);
}

#[test]
fn prepare_should_keep_branch_of_unmerged_worktree() {
    assert!(!Fixture::new().remove(1).delete_branch);
}

#[test]
fn prepare_should_list_changes_of_dirty_worktree() {
    let fx = Fixture::new();
    fs::write(fx.worktrees[0].path.join("notes.txt"), "wip").unwrap();

    assert_eq!(fx.remove(0).changes, ["?? notes.txt"]);
}

#[test]
fn prepare_should_block_dirty_worktree_until_forced() {
    let fx = Fixture::new();
    fs::write(fx.worktrees[0].path.join("notes.txt"), "wip").unwrap();

    let remove = fx.remove(0);

    assert_eq!((remove.tier, remove.is_runnable()), (Tier::Dirty, false));
}

#[test]
fn prepare_should_remove_through_herdr_when_open_in_workspace() {
    let mut fx = Fixture::new();
    fx.open_in_herdr(0, 0);

    assert_eq!(fx.remove(0).workspace.map(|w| w.id).as_deref(), Some("wB"));
}

#[test]
fn prepare_should_require_force_to_remove_worktree_with_running_agent() {
    let mut fx = Fixture::new();
    fx.open_in_herdr(0, 1);

    assert!(!fx.remove(0).is_runnable());
}

#[test]
fn prepare_should_block_moving_worktree_with_running_agent() {
    let mut fx = Fixture::new();
    fx.open_in_herdr(0, 1);

    let blocked = fx
        .prepare(Operation::Move {
            id: EntryId(0),
            to: Some(fx.repo().join(".claude/worktrees/x")),
            rename_branch: None,
        })
        .unwrap_err();

    assert_eq!(blocked.reason, "an agent is running in its herdr workspace");
}

#[test]
fn prepare_should_reopen_moved_worktree_in_herdr() {
    let mut fx = Fixture::new();
    fx.open_in_herdr(0, 0);

    let step = fx
        .prepare(Operation::Move {
            id: EntryId(0),
            to: Some(fx.repo().join(".claude/worktrees/x")),
            rename_branch: None,
        })
        .unwrap();

    assert!(matches!(step, Step::Move(m) if m.workspace.as_deref() == Some("wB")));
}

#[test]
fn prepare_should_block_removing_orphaned_worktree() {
    let mut fx = Fixture::new();
    fx.worktrees[0].health = Health::Orphaned;

    assert!(fx.prepare(Operation::Remove { id: EntryId(0) }).is_err());
}

#[test]
fn execute_remove_should_delete_checkout_and_branch() {
    let fx = Fixture::new();

    apply::execute(&Step::Remove(fx.remove(0)), &Herdr::new()).unwrap();

    assert_eq!(
        (fx.worktrees[0].path.exists(), fx.has_branch("feat")),
        (false, false)
    );
}

#[test]
fn execute_remove_should_keep_unmerged_branch() {
    let fx = Fixture::new();

    apply::execute(&Step::Remove(fx.remove(1)), &Herdr::new()).unwrap();

    assert!(fx.has_branch("wip"));
}

#[test]
fn execute_remove_should_discard_changes_when_forced() {
    let fx = Fixture::new();
    fs::write(fx.worktrees[0].path.join("notes.txt"), "wip").unwrap();
    let mut remove = fx.remove(0);
    remove.force = true;

    apply::execute(&Step::Remove(remove), &Herdr::new()).unwrap();

    assert!(!fx.worktrees[0].path.exists());
}

#[test]
fn execute_remove_should_prune_worktree_whose_directory_is_gone() {
    let mut fx = Fixture::new();
    fs::remove_dir_all(&fx.worktrees[0].path).unwrap();
    fx.worktrees[0].health = Health::Prunable;

    apply::execute(&Step::Remove(fx.remove(0)), &Herdr::new()).unwrap();

    let listed = git::worktree_list(&fx.repo()).unwrap();
    assert_eq!(listed.len(), 2);
}

/// Renames worktree 0's branch, moving it to `dir` when one is given.
fn rename(fx: &Fixture, dir: Option<&str>, branch: &str) -> Result<Step, Blocked> {
    fx.prepare(Operation::Move {
        id: EntryId(0),
        to: dir.map(|dir| fx.repo().join(".claude/worktrees").join(dir)),
        rename_branch: Some(branch.into()),
    })
}

#[test]
fn prepare_move_should_block_an_invalid_new_branch_name() {
    let blocked = rename(&Fixture::new(), Some("feat-"), "feat/").unwrap_err();

    assert_eq!(blocked.reason, "`feat/` is not a valid branch name");
}

#[test]
fn prepare_move_should_block_renaming_onto_an_existing_branch() {
    let fx = Fixture::new();
    git(&fx.repo(), &["branch", "old"]);

    let blocked = rename(&fx, Some("old"), "old").unwrap_err();

    assert_eq!(blocked.reason, "branch `old` already exists");
}

#[test]
fn execute_move_should_rename_only_the_branch_when_the_directory_stays() {
    let fx = Fixture::new();
    let step = rename(&fx, None, "jb/feat").unwrap();

    apply::execute(&step, &Herdr::new()).unwrap();

    let path = &fx.worktrees[0].path;
    let branch = git::run(path, &["branch", "--show-current"]).unwrap();
    assert_eq!(branch.trim(), "jb/feat");
}

#[test]
fn prepare_move_should_block_a_branch_nested_under_an_existing_one() {
    let fx = Fixture::new();
    git(&fx.repo(), &["branch", "jb"]);

    let blocked = rename(&fx, Some("jb-feat"), "jb/feat").unwrap_err();

    assert_eq!(
        blocked.reason,
        "branch `jb` stops git from making `jb/feat`"
    );
}

#[test]
fn prepare_move_should_block_a_branch_that_existing_ones_nest_under() {
    let fx = Fixture::new();
    git(&fx.repo(), &["branch", "jb/x"]);

    let blocked = rename(&fx, Some("jb"), "jb").unwrap_err();

    assert_eq!(blocked.reason, "branch `jb/x` stops git from making `jb`");
}

#[test]
fn execute_move_should_nest_a_branch_under_its_own_old_name() {
    let fx = Fixture::new();
    let step = rename(&fx, None, "feat/x").unwrap();

    apply::execute(&step, &Herdr::new()).unwrap();

    assert!(fx.has_branch("feat/x"));
}

#[test]
fn prepare_create_should_block_a_branch_nested_under_an_existing_one() {
    let fx = Fixture::new();
    git(&fx.repo(), &["branch", "jb"]);

    let blocked = fx.create("jb/fix", None).unwrap_err();

    assert_eq!(blocked.reason, "branch `jb` stops git from making `jb/fix`");
}

#[test]
fn prepare_should_block_a_second_line_using_the_same_branch() {
    let fx = Fixture::new();
    let operations = vec![
        Operation::Move {
            id: EntryId(0),
            to: None,
            rename_branch: Some("foo".into()),
        },
        create_op("foo"),
    ];

    let results = fx.prepare_all(operations);

    assert_eq!(
        results[1].as_ref().unwrap_err().reason,
        "another line also uses branch `foo`"
    );
}

#[test]
fn prepare_move_should_block_a_branch_rename_while_an_agent_runs() {
    let mut fx = Fixture::new();
    fx.open_in_herdr(0, 1);

    assert!(rename(&fx, None, "jb/feat").is_err());
}

#[test]
fn prepare_move_should_rename_the_branch_of_a_locked_worktree() {
    let mut fx = Fixture::new();
    fx.worktrees[0].locked = Some(String::new());

    assert!(rename(&fx, None, "jb/feat").is_ok());
}

#[test]
fn prepare_create_should_read_a_repo_named_prefix_as_part_of_the_branch() {
    let create = Fixture::new().create("app/v2", None).unwrap();

    assert_eq!(create.branch, "app/v2");
}

#[test]
fn prepare_should_block_a_second_line_whose_branch_nests_under_another() {
    let results = Fixture::new().prepare_all(vec![create_op("jb"), create_op("jb/a")]);

    assert_eq!(
        results[1].as_ref().unwrap_err().reason,
        "another line's branch `jb` stops git from making `jb/a`"
    );
}

#[test]
fn prepare_should_block_a_create_of_a_branch_an_earlier_remove_takes() {
    let fx = Fixture::new();

    let results = fx.prepare_all(vec![
        Operation::Remove { id: EntryId(0) },
        create_op("other feat"),
    ]);

    assert_eq!(
        results[1].as_ref().unwrap_err().reason,
        "this save also removes the worktree of branch `feat`; apply that first"
    );
}

#[test]
fn prepare_should_block_a_create_of_a_branch_an_earlier_move_renames() {
    let fx = Fixture::new();
    let operations = vec![
        Operation::Move {
            id: EntryId(0),
            to: None,
            rename_branch: Some("feat2".into()),
        },
        create_op("other feat"),
    ];

    let results = fx.prepare_all(operations);

    assert_eq!(
        results[1].as_ref().unwrap_err().reason,
        "this save also renames branch `feat`; apply that first"
    );
}

#[test]
fn prepare_should_block_a_second_line_using_the_same_directory() {
    let fx = Fixture::new();
    let results = fx.prepare_all(vec![create_op("jb/fix"), create_op("jb-fix")]);

    let reason = &results[1].as_ref().unwrap_err().reason;
    assert!(reason.starts_with("another line also uses"), "{reason}");
}

#[test]
fn prepare_should_block_a_create_into_a_moved_worktree_directory() {
    let fx = Fixture::new();
    let to = fx.repo().join(".claude/worktrees/jb-fix");
    let operations = vec![
        Operation::Move {
            id: EntryId(0),
            to: Some(to),
            rename_branch: None,
        },
        create_op("jb/fix"),
    ];

    let results = fx.prepare_all(operations);

    assert!(results[1].is_err());
}

#[test]
fn execute_move_should_rename_directory_and_branch() {
    let fx = Fixture::new();
    let to = fx.repo().join(".claude/worktrees/feat2");
    let step = fx
        .prepare(Operation::Move {
            id: EntryId(0),
            to: Some(to.clone()),
            rename_branch: Some("feat2".into()),
        })
        .unwrap();

    apply::execute(&step, &Herdr::new()).unwrap();

    assert_eq!((to.exists(), fx.has_branch("feat2")), (true, true));
}

#[test]
fn prepare_create_should_branch_from_default_branch() {
    let create = Fixture::new().create("fix", None).unwrap();

    assert_eq!(create.start, Start::From("main".into()));
}

#[test]
fn prepare_create_should_use_given_base() {
    let create = Fixture::new().create("fix", Some("wip")).unwrap();

    assert_eq!(create.start, Start::From("wip".into()));
}

#[test]
fn prepare_create_should_block_a_branch_checked_out_in_another_worktree() {
    let fx = Fixture::new();

    let blocked = fx.create("other feat", None).unwrap_err();

    assert_eq!(
        blocked.reason,
        format!(
            "branch `feat` is checked out in {}",
            fx.worktrees[0].path.display()
        )
    );
}

#[test]
fn prepare_create_should_check_out_existing_branch() {
    let fx = Fixture::new();
    git(&fx.repo(), &["branch", "old"]);

    assert_eq!(fx.create("old", None).unwrap().start, Start::Existing);
}

#[test]
fn prepare_create_should_place_worktree_with_source_template() {
    let fx = Fixture::new();

    let create = fx.create("app:fix", None).unwrap();

    assert_eq!(create.path, fx.repo().join(".claude/worktrees/fix"));
}

#[test]
fn prepare_create_should_block_unknown_repo() {
    let blocked = Fixture::new().create("nope:fix", None).unwrap_err();

    assert_eq!(blocked.reason, "no known repo named `nope`");
}

fn create_alone(fx: &Fixture, text: &str) -> Result<Step, Blocked> {
    fx.prepare(Operation::Create {
        target: plan::parse_target(text).unwrap(),
        near: None,
    })
}

#[test]
fn prepare_create_should_take_a_repo_by_path_when_none_is_listed() {
    let fx = Fixture::new();
    init_repo(&fx.home.join("other"));

    let Ok(Step::Create(create)) = create_alone(&fx, "~/other:fix") else {
        panic!("expected a create");
    };

    assert_eq!(
        (create.repo, create.path),
        (
            fx.home.join("other"),
            fx.home.join("other/.claude/worktrees/fix")
        )
    );
}

#[test]
fn prepare_create_should_block_a_path_outside_any_repo() {
    let fx = Fixture::new();

    let blocked = create_alone(&fx, "~/nowhere:fix").unwrap_err();

    assert_eq!(blocked.reason, "~/nowhere is not a git repo");
}

#[test]
fn prepare_create_should_ask_for_a_repo_path_when_the_line_has_no_repo() {
    let blocked = create_alone(&Fixture::new(), "fix").unwrap_err();

    assert_eq!(
        blocked.reason,
        "no repo to create in; write its path first, like ~/code/app:feat"
    );
}

#[test]
fn prepare_create_should_put_a_slashed_branch_in_a_cleaned_directory() {
    let fx = Fixture::new();

    let create = fx.create("app:jb/fix", None).unwrap();

    assert_eq!(create.branch, "jb/fix");
    assert_eq!(create.path, fx.repo().join(".claude/worktrees/jb-fix"));
}

#[test]
fn prepare_create_should_read_a_slashed_name_as_a_branch() {
    let create = Fixture::new().create("feature/login", None).unwrap();

    assert_eq!(create.branch, "feature/login");
}

#[test]
fn execute_create_should_check_out_an_existing_slashed_branch() {
    let fx = Fixture::new();
    git(&fx.repo(), &["branch", "alice/spike"]);
    let create = fx.create("alice/spike", None).unwrap();
    let path = create.path.clone();

    apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    let branch = git::run(&path, &["branch", "--show-current"]).unwrap();
    assert_eq!(branch.trim(), "alice/spike");
}

#[test]
fn prepare_create_should_block_existing_path() {
    let fx = Fixture::new();

    assert!(fx.create("feat", None).is_err());
}

#[test]
fn prepare_create_should_block_invalid_branch_name() {
    let blocked = Fixture::new().create("a..b", None).unwrap_err();

    assert_eq!(blocked.reason, "`a..b` is not a valid branch name");
}

#[test]
fn execute_create_should_add_worktree_on_new_branch() {
    let fx = Fixture::new();
    let create = fx.create("fix", None).unwrap();
    let path = create.path.clone();

    apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    assert_eq!((path.exists(), fx.has_branch("fix")), (true, true));
}

#[test]
fn prepare_create_should_list_matching_files_to_copy() {
    let mut fx = Fixture::new();
    fs::write(fx.repo().join(".env"), "KEY=1").unwrap();
    fs::create_dir_all(fx.repo().join("config")).unwrap();
    fs::write(fx.repo().join("config/app.local.toml"), "").unwrap();
    fx.set_repo_config(r#"copy = [".env", "config/*.local.toml", "missing"]"#);

    let create = fx.create("fix", None).unwrap();

    let expected = [
        PathBuf::from(".env"),
        PathBuf::from("config/app.local.toml"),
    ];
    assert_eq!(create.copy, expected);
}

#[test]
fn prepare_create_should_not_copy_from_outside_the_checkout_or_git_dir() {
    let mut fx = Fixture::new();
    fs::write(fx.home.join("secret"), "").unwrap();
    fx.set_repo_config(r#"copy = ["../*", ".git/*"]"#);

    let create = fx.create("fix", None).unwrap();

    assert!(create.copy.is_empty());
}

#[test]
fn prepare_create_should_block_invalid_copy_pattern() {
    let mut fx = Fixture::new();
    fx.set_repo_config(r#"copy = ["[x"]"#);

    let blocked = fx.create("fix", None).unwrap_err();

    assert!(blocked.reason.contains("invalid copy pattern"));
}

#[test]
fn execute_create_should_copy_files_and_run_commands_in_the_worktree() {
    let mut fx = Fixture::new();
    fs::write(fx.repo().join(".env"), "KEY=1").unwrap();
    let check_repo = format!(
        "test \"$JUMAR_REPO\" = '{}' && touch saw-repo",
        fx.repo().display()
    );
    let check_repo = toml::Value::from(check_repo);
    fx.set_repo_config(&format!(
        "copy = [\".env\"]\non_create = [\"touch made\", {check_repo}]"
    ));
    let create = fx.create("fix", None).unwrap();
    let path = create.path.clone();

    apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    let found = [".env", "made", "saw-repo"].map(|f| path.join(f).exists());
    assert_eq!(found, [true, true, true]);
}

#[test]
fn execute_create_should_report_a_failed_command_after_creating() {
    let mut fx = Fixture::new();
    fx.set_repo_config(r#"on_create = ["echo boom >&2; exit 1"]"#);
    let create = fx.create("fix", None).unwrap();
    let path = create.path.clone();

    let err = apply::execute(&Step::Create(create), &Herdr::new()).unwrap_err();

    assert!(err.to_string().contains("boom"), "{err}");
    assert!(path.exists());
}

#[test]
fn execute_create_should_keep_tracked_files_over_copies() {
    let mut fx = Fixture::new();
    fs::write(fx.repo().join("README"), "tracked").unwrap();
    git(&fx.repo(), &["add", "README"]);
    git(&fx.repo(), &["commit", "-q", "-m", "readme"]);
    fs::write(fx.repo().join("README"), "local edit").unwrap();
    fx.set_repo_config(r#"copy = ["README"]"#);
    let create = fx.create("fix", None).unwrap();
    let path = create.path.clone();

    apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    assert_eq!(fs::read_to_string(path.join("README")).unwrap(), "tracked");
}

impl Fixture {
    /// Gives the repo an `origin` with `main`, then pushes a commit to it from another
    /// clone, so the repo's `origin/main` is one commit behind. Returns that commit.
    fn add_remote_ahead(&self) -> String {
        let remote = self.home.join("remote.git");
        git(
            &self.home,
            &["init", "-q", "--bare", remote.to_str().unwrap()],
        );
        git(
            &self.repo(),
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&self.repo(), &["push", "-q", "origin", "main"]);
        git(&self.repo(), &["fetch", "-q", "origin"]);
        git(&self.repo(), &["remote", "set-head", "origin", "main"]);
        let other = self.home.join("other");
        git(
            &self.home,
            &[
                "clone",
                "-q",
                remote.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
        git(&other, &["commit", "-q", "--allow-empty", "-m", "upstream"]);
        git(&other, &["push", "-q", "origin", "main"]);
        git::run(&other, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned()
    }
}

fn head(path: &Path) -> String {
    git::run(path, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn prepare_create_should_fetch_the_remote_default_branch() {
    let fx = Fixture::new();
    fx.add_remote_ahead();

    let create = fx.create("fix", None).unwrap();

    let fetch = create.fetch.unwrap();
    assert_eq!(
        (fetch.from.as_str(), fetch.into.as_str()),
        ("refs/heads/main", "refs/remotes/origin/main")
    );
}

#[test]
fn prepare_create_should_not_fetch_a_local_base() {
    let fx = Fixture::new();
    fx.add_remote_ahead();

    let create = fx.create("fix", Some("main")).unwrap();

    assert_eq!(create.fetch, None);
}

#[test]
fn prepare_create_should_not_fetch_when_turned_off() {
    let mut fx = Fixture::new();
    fx.add_remote_ahead();
    fx.config = Config::from_toml("fetch_on_create = false\n", Path::new("config.toml")).unwrap();

    let create = fx.create("fix", None).unwrap();

    assert_eq!(create.fetch, None);
}

#[test]
fn execute_create_should_start_from_the_freshly_fetched_commit() {
    let fx = Fixture::new();
    let upstream = fx.add_remote_ahead();
    let create = fx.create("fix", None).unwrap();
    let path = create.path.clone();

    let notes = apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    assert_eq!((head(&path), notes.len()), (upstream, 0));
}

#[test]
fn execute_create_should_note_a_failed_fetch_and_still_create() {
    let fx = Fixture::new();
    fx.add_remote_ahead();
    let stale = head(&fx.repo());
    git(
        &fx.repo(),
        &["remote", "set-url", "origin", "/nonexistent/remote.git"],
    );
    let create = fx.create("fix", None).unwrap();
    let path = create.path.clone();

    let notes = apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    assert_eq!(head(&path), stale);
    assert!(
        notes[0].contains("could not fetch origin/main"),
        "{notes:?}"
    );
}

#[test]
fn prepare_create_should_track_a_branch_only_on_the_remote() {
    let fx = Fixture::new();
    fx.with_remote_branch("fix");

    let create = fx.create("fix", None).unwrap();

    assert_eq!(create.start, Start::Tracking("origin/fix".into()));
    assert_eq!(create.fetch.unwrap().from, "refs/heads/fix");
}

#[test]
fn execute_create_should_set_the_remote_branch_as_upstream() {
    let fx = Fixture::new();
    fx.with_remote_branch("fix");
    let create = fx.create("fix", None).unwrap();
    let path = create.path.clone();

    apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    let upstream = git::run(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]).unwrap();
    assert_eq!(upstream.trim(), "origin/fix");
}

#[test]
fn prepare_create_should_branch_from_a_given_base_over_the_remote_branch() {
    let fx = Fixture::new();
    fx.with_remote_branch("fix");

    let create = fx.create("fix", Some("wip")).unwrap();

    assert_eq!(create.start, Start::From("wip".into()));
}

#[test]
fn prepare_create_should_block_an_unknown_base() {
    let blocked = Fixture::new().create("fix", Some("nope")).unwrap_err();

    assert_eq!(blocked.reason, "`nope` is not a branch, tag or commit");
}

#[test]
fn execute_create_should_fail_cleanly_when_an_unfetched_start_cannot_be_fetched() {
    let fx = Fixture::new();
    fx.add_remote_ahead();
    let create = fx.create("fix", Some("origin/mian")).unwrap();
    let path = create.path.clone();

    let err = apply::execute(&Step::Create(create), &Herdr::new()).unwrap_err();

    assert!(
        err.to_string()
            .starts_with("could not fetch origin/mian, and it was never fetched before"),
        "{err}"
    );
    assert!(!path.exists());
}

#[test]
fn prepare_create_should_block_an_unfetched_remote_base_when_fetching_is_off() {
    let mut fx = Fixture::new();
    fx.add_remote_ahead();
    fx.config = Config::from_toml("fetch_on_create = false\n", Path::new("config.toml")).unwrap();

    assert!(fx.create("fix", Some("origin/later")).is_err());
}

#[test]
fn prepare_create_should_allow_a_remote_base_not_fetched_yet() {
    let fx = Fixture::new();
    fx.add_remote_ahead();

    let create = fx.create("fix", Some("origin/later")).unwrap();

    assert_eq!(create.fetch.unwrap().from, "refs/heads/later");
}

impl Fixture {
    /// Adds a remote named `remote` at `dir/remote.git` holding PR `number`'s head under
    /// `pulls` (`pull` or `merge-requests`), one commit past main. Returns that commit.
    fn add_pull(&self, remote: &str, dir: &str, pulls: &str, number: u32) -> String {
        let url = self.home.join(dir).join("remote.git");
        git(&self.home, &["init", "-q", "--bare", url.to_str().unwrap()]);
        git(
            &self.repo(),
            &["remote", "add", remote, url.to_str().unwrap()],
        );
        let work = self.home.join(format!("{dir}-pr"));
        git(
            &self.repo(),
            &["worktree", "add", "-q", "--detach", work.to_str().unwrap()],
        );
        git(&work, &["commit", "-q", "--allow-empty", "-m", "pr"]);
        let refspec = format!("HEAD:refs/{pulls}/{number}/head");
        git(&work, &["push", "-q", remote, &refspec]);
        head(&work)
    }
}

#[test]
fn prepare_create_should_name_a_pr_worktree_after_its_number() {
    let fx = Fixture::new();
    fx.add_pull("origin", "gh", "pull", 7);

    let create = fx.create("#7", None).unwrap();

    let fetch = create.fetch.unwrap();
    assert_eq!(
        (create.branch.as_str(), fetch.from.as_str(), fetch.pull),
        ("pr-7", "refs/pull/7/head", Some(7))
    );
}

#[test]
fn prepare_create_should_use_the_given_name_for_a_pr() {
    let fx = Fixture::new();
    fx.add_pull("origin", "gh", "pull", 7);

    let create = fx.create("review", Some("#7")).unwrap();

    assert_eq!(create.branch, "review");
}

#[test]
fn prepare_create_should_prefer_upstream_for_a_pr() {
    let fx = Fixture::new();
    fx.add_pull("origin", "fork", "pull", 1);
    fx.add_pull("upstream", "main-repo", "pull", 7);

    let create = fx.create("#7", None).unwrap();

    assert_eq!(create.fetch.unwrap().remote, "upstream");
}

#[test]
fn prepare_create_should_use_merge_requests_on_gitlab() {
    let fx = Fixture::new();
    fx.add_pull("origin", "gitlab", "merge-requests", 7);

    let create = fx.create("#7", None).unwrap();

    assert_eq!(create.fetch.unwrap().from, "refs/merge-requests/7/head");
}

#[test]
fn prepare_create_should_block_a_pr_without_a_remote() {
    let fx = Fixture::new();

    let blocked = fx.create("#7", None).unwrap_err();

    assert!(
        blocked.reason.contains("no upstream or origin"),
        "{}",
        blocked.reason
    );
}

#[test]
fn execute_create_should_check_out_the_pr_head() {
    let fx = Fixture::new();
    let pr = fx.add_pull("origin", "gh", "pull", 7);
    let create = fx.create("#7", None).unwrap();
    let path = create.path.clone();

    apply::execute(&Step::Create(create), &Herdr::new()).unwrap();

    assert_eq!(head(&path), pr);
}

#[test]
fn execute_create_should_fail_without_creating_when_the_pr_is_missing() {
    let fx = Fixture::new();
    fx.add_pull("origin", "gh", "pull", 7);
    let create = fx.create("#8", None).unwrap();
    let path = create.path.clone();

    let err = apply::execute(&Step::Create(create), &Herdr::new()).unwrap_err();

    assert!(err.to_string().contains("could not fetch PR #8"), "{err}");
    assert!(!path.exists());
}

#[test]
fn execute_remove_should_keep_a_branch_that_gained_commits_since_it_was_graded() {
    let fx = Fixture::new();
    let remove = fx.remove(0);
    assert!(remove.delete_branch);
    git(
        &fx.worktrees[0].path,
        &["commit", "-q", "--allow-empty", "-m", "late work"],
    );

    let notes = apply::execute(&Step::Remove(remove), &Herdr::new()).unwrap();

    assert!(fx.has_branch("feat"));
    assert!(notes[0].contains("kept branch feat"), "{notes:?}");
}

/// A herdr stand-in that logs its arguments and succeeds without touching anything.
fn fake_herdr(fx: &Fixture) -> (Herdr, PathBuf) {
    let log = fx.home.join("herdr.log");
    let bin = fx.home.join("herdr");
    fs::write(
        &bin,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display()),
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    (Herdr::with_bin(bin), log)
}

#[test]
fn execute_remove_should_go_through_herdr_when_open_in_workspace() {
    let mut fx = Fixture::new();
    fx.open_in_herdr(1, 0);
    let (herdr, log) = fake_herdr(&fx);

    apply::execute(&Step::Remove(fx.remove(1)), &herdr).unwrap();

    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "worktree remove --workspace wB\n"
    );
}

#[test]
fn execute_move_should_close_and_reopen_herdr_workspace() {
    let mut fx = Fixture::new();
    fx.open_in_herdr(0, 0);
    let (herdr, log) = fake_herdr(&fx);
    let to = fx.repo().join(".claude/worktrees/x");
    let step = fx
        .prepare(Operation::Move {
            id: EntryId(0),
            to: Some(to.clone()),
            rename_branch: None,
        })
        .unwrap();

    apply::execute(&step, &herdr).unwrap();

    assert_eq!(
        fs::read_to_string(log).unwrap(),
        format!(
            "workspace close wB\nworktree open --cwd {} --path {} --no-focus\n",
            fx.repo().display(),
            to.display()
        )
    );
}

#[test]
fn prepare_should_block_locked_worktree_until_forced() {
    let mut fx = Fixture::new();
    fx.worktrees[0].locked = Some(String::new());

    assert!(!fx.remove(0).is_runnable());
}

#[test]
fn prepare_should_block_moving_locked_worktree() {
    let mut fx = Fixture::new();
    fx.worktrees[0].locked = Some(String::new());

    let blocked = fx.prepare(Operation::Move {
        id: EntryId(0),
        to: Some(fx.repo().join(".claude/worktrees/x")),
        rename_branch: None,
    });

    assert!(blocked.is_err());
}

#[test]
fn execute_remove_should_unlock_locked_worktree_when_forced() {
    let mut fx = Fixture::new();
    let path = fx.worktrees[0].path.clone();
    git(&fx.repo(), &["worktree", "lock", path.to_str().unwrap()]);
    fx.worktrees[0].locked = Some(String::new());
    let mut remove = fx.remove(0);
    remove.force = true;

    apply::execute(&Step::Remove(remove), &Herdr::new()).unwrap();

    assert!(!path.exists());
}

#[test]
fn prepare_split_should_move_the_branch_to_a_new_worktree() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");

    let split = fx.split(id).unwrap();

    assert_eq!(
        (
            split.default_branch.as_str(),
            split.create.path.clone(),
            split.create.start.clone(),
            split.changes.len(),
        ),
        (
            "main",
            fx.repo().join(".claude/worktrees/side"),
            Start::Existing,
            0
        )
    );
}

#[test]
fn prepare_split_should_block_a_main_checkout_on_the_default_branch() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");
    git(&fx.repo(), &["switch", "-q", "main"]);
    fx.worktrees[id.0].head = Some(Head::Branch("main".into()));

    let reason = fx.split(id).unwrap_err().reason;

    assert!(reason.contains("already on `main`"), "{reason}");
}

#[test]
fn prepare_split_should_block_while_an_agent_runs_in_the_main_checkout() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");
    fx.open_in_herdr(id.0, 1);

    assert!(fx.split(id).is_err());
}

#[test]
fn execute_split_should_switch_main_back_and_check_the_branch_out_in_a_worktree() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");
    let split = fx.split(id).unwrap();

    apply::execute(&Step::Split(Box::new(split.clone())), &Herdr::new()).unwrap();

    assert_eq!(
        (
            current_branch(&fx.repo()),
            current_branch(&split.create.path)
        ),
        ("main".to_owned(), "side".to_owned())
    );
}

#[test]
fn execute_split_should_carry_uncommitted_changes_to_the_new_worktree() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");
    fs::write(fx.repo().join("notes.txt"), "draft").unwrap();
    let split = fx.split(id).unwrap();

    apply::execute(&Step::Split(Box::new(split.clone())), &Herdr::new()).unwrap();

    assert_eq!(
        (
            fx.repo().join("notes.txt").exists(),
            fs::read_to_string(split.create.path.join("notes.txt")).unwrap(),
            git::run(&fx.repo(), &["stash", "list"]).unwrap(),
        ),
        (false, "draft".to_owned(), String::new())
    );
}

#[test]
fn execute_split_should_carry_changes_made_after_the_review_opened() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");
    let split = fx.split(id).unwrap();
    fs::write(fx.repo().join("late.txt"), "late").unwrap();

    apply::execute(&Step::Split(Box::new(split.clone())), &Herdr::new()).unwrap();

    assert_eq!(
        (
            fx.repo().join("late.txt").exists(),
            split.create.path.join("late.txt").exists()
        ),
        (false, true)
    );
}

#[test]
fn execute_split_should_leave_other_stashes_alone() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");
    fs::write(fx.repo().join("older.txt"), "older").unwrap();
    git(&fx.repo(), &["stash", "push", "-q", "-u", "-m", "older"]);
    fs::write(fx.repo().join("notes.txt"), "draft").unwrap();
    let split = fx.split(id).unwrap();

    apply::execute(&Step::Split(Box::new(split.clone())), &Herdr::new()).unwrap();

    let stashes = git::run(&fx.repo(), &["stash", "list", "--format=%s"]).unwrap();
    assert_eq!(
        (
            split.create.path.join("notes.txt").exists(),
            split.create.path.join("older.txt").exists(),
            stashes.trim()
        ),
        (true, false, "On side: older")
    );
}

#[test]
fn execute_split_should_put_the_main_checkout_back_when_the_worktree_fails() {
    let mut fx = Fixture::new();
    let id = fx.main_on("side");
    fs::write(fx.repo().join("notes.txt"), "draft").unwrap();
    let split = fx.split(id).unwrap();
    fs::create_dir_all(&split.create.path).unwrap();
    fs::write(split.create.path.join("in-the-way"), "").unwrap();

    let result = apply::execute(&Step::Split(Box::new(split)), &Herdr::new());

    assert_eq!(
        (
            result.is_err(),
            current_branch(&fx.repo()),
            fs::read_to_string(fx.repo().join("notes.txt")).unwrap(),
            git::run(&fx.repo(), &["stash", "list"]).unwrap(),
        ),
        (true, "side".to_owned(), "draft".to_owned(), String::new())
    );
}
