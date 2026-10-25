#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use jumar_core::apply::{self, Blocked, Context, Start, Step};
use jumar_core::config::Config;
use jumar_core::git;
use jumar_core::herdr::{self, Herdr};
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::plan::{EntryId, Operation};
use jumar_core::safety::Tier;
use tempfile::TempDir;

use common::{git, init_repo};

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    worktrees: Vec<Worktree>,
    herdr: Option<herdr::State>,
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
        });
        path
    }

    fn prepare(&self, operation: Operation) -> Result<Step, Blocked> {
        let config = Config::default();
        let sources = config.sources();
        let context = Context {
            worktrees: &self.worktrees,
            tiers: &[],
            sources,
            default_source: "claude",
            home: &self.home,
            herdr: self.herdr.as_ref(),
        };
        apply::prepare(&context, vec![operation]).remove(0)
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
        self.prepare(Operation::Create {
            target: target.into(),
            base: base.map(Into::into),
            near: Some(EntryId(0)),
        })
        .map(|step| match step {
            Step::Create(create) => create,
            step => panic!("expected a create, got {step:?}"),
        })
    }

    /// Marks worktree `index` as open in a herdr workspace with `agents` agents.
    fn open_in_herdr(&mut self, index: usize, agents: usize) {
        self.herdr = Some(herdr::State {
            workspaces: vec![herdr::Workspace {
                id: "wB".into(),
                checkout: self.worktrees[index].path.clone(),
                agents,
            }],
        });
    }

    fn has_branch(&self, name: &str) -> bool {
        git::branch_exists(&self.repo(), name).unwrap()
    }
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
            to: fx.repo().join(".claude/worktrees/x"),
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
            to: fx.repo().join(".claude/worktrees/x"),
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

#[test]
fn execute_move_should_rename_directory_and_branch() {
    let fx = Fixture::new();
    let to = fx.repo().join(".claude/worktrees/feat2");
    let step = fx
        .prepare(Operation::Move {
            id: EntryId(0),
            to: to.clone(),
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
fn prepare_create_should_check_out_existing_branch() {
    let fx = Fixture::new();
    git(&fx.repo(), &["branch", "old"]);

    assert_eq!(fx.create("old", None).unwrap().start, Start::Existing);
}

#[test]
fn prepare_create_should_place_worktree_with_source_template() {
    let fx = Fixture::new();

    let create = fx.create("app/fix", None).unwrap();

    assert_eq!(create.path, fx.repo().join(".claude/worktrees/fix"));
}

#[test]
fn prepare_create_should_block_unknown_repo() {
    let blocked = Fixture::new().create("nope/fix", None).unwrap_err();

    assert_eq!(blocked.reason, "no known repo named `nope`");
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
            to: to.clone(),
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
        to: fx.repo().join(".claude/worktrees/x"),
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
