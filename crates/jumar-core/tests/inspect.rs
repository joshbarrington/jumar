#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use jumar_core::inspect::inspect;
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::safety::Tier;
use tempfile::TempDir;

use common::{git, init_repo};

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().join("repo"));
        Self { dir }
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn add(&self, name: &str) -> Worktree {
        let path = self.dir.path().join(name);
        git(
            &self.repo(),
            &["worktree", "add", "-q", "-b", name, path.to_str().unwrap()],
        );
        Worktree {
            path,
            repo: self.repo(),
            source: None,
            head: Some(Head::Branch(name.into())),
            health: Health::Healthy,
            locked: None,
        }
    }
}

fn commit(path: &Path) {
    git(path, &["commit", "-q", "--allow-empty", "-m", "work"]);
}

#[test]
fn inspect_should_be_safe_when_branch_has_no_new_commits() {
    let fx = Fixture::new();
    let worktree = fx.add("feat");

    assert_eq!(inspect(&worktree).unwrap().tier, Tier::Safe);
}

#[test]
fn inspect_should_be_unmerged_when_branch_has_local_commits() {
    let fx = Fixture::new();
    let worktree = fx.add("feat");
    commit(&worktree.path);

    assert_eq!(inspect(&worktree).unwrap().tier, Tier::Unmerged);
}

#[test]
fn inspect_should_be_safe_when_branch_merged_into_main() {
    let fx = Fixture::new();
    let worktree = fx.add("feat");
    commit(&worktree.path);
    git(&fx.repo(), &["merge", "-q", "--ff-only", "feat"]);

    assert_eq!(inspect(&worktree).unwrap().tier, Tier::Safe);
}

#[test]
fn inspect_should_be_dirty_when_untracked_file_present() {
    let fx = Fixture::new();
    let worktree = fx.add("feat");
    fs::write(worktree.path.join("notes.txt"), "wip").unwrap();

    assert_eq!(inspect(&worktree).unwrap().tier, Tier::Dirty);
}

#[test]
fn inspect_should_keep_default_branch_even_when_checked_out_in_worktree() {
    let fx = Fixture::new();
    git(&fx.repo(), &["checkout", "-q", "-b", "parking"]);
    let path = fx.dir.path().join("main-wt");
    git(
        &fx.repo(),
        &["worktree", "add", "-q", path.to_str().unwrap(), "main"],
    );
    let worktree = Worktree {
        path,
        repo: fx.repo(),
        source: None,
        head: Some(Head::Branch("main".into())),
        health: Health::Healthy,
        locked: None,
    };

    assert_eq!(inspect(&worktree).unwrap().tier, Tier::Unmerged);
}

#[test]
fn inspect_should_be_safe_when_unmerged_branch_fully_pushed() {
    let fx = Fixture::new();
    let remote = fx.dir.path().join("remote.git");
    git(
        fx.dir.path(),
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(
        &fx.repo(),
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    let worktree = fx.add("feat");
    commit(&worktree.path);
    git(&worktree.path, &["push", "-q", "-u", "origin", "feat"]);

    assert_eq!(inspect(&worktree).unwrap().tier, Tier::Safe);
}
