#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]

mod common;

use std::fs;

use jumar_core::git;
use jumar_core::model::{Head, Health, Worktree};
use jumar_core::tool::render;

use common::{git, init_repo};

#[test]
fn render_should_fill_base_with_merge_base_of_default_branch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fs::canonicalize(dir.path()).unwrap().join("app");
    init_repo(&repo);
    let main = git::run(&repo, &["rev-parse", "HEAD"]).unwrap();
    let path = dir.path().join("feat");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat",
            path.to_str().unwrap(),
        ],
    );
    git(&path, &["commit", "-q", "--allow-empty", "-m", "work"]);
    let worktree = Worktree {
        path,
        repo,
        source: None,
        head: Some(Head::Branch("feat".into())),
        health: Health::Healthy,
    };

    let command = render(&["revdiff".into(), "{base}".into()], &worktree).unwrap();

    assert_eq!(command, ["revdiff", main.trim()]);
}
