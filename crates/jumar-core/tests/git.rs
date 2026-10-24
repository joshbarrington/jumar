#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]

mod common;

use jumar_core::git::default_branches;

use common::{git, init_repo};

#[test]
fn default_branches_should_include_origin_head_and_local_branch_when_cloned() {
    let dir = tempfile::tempdir().unwrap();
    let upstream = dir.path().join("upstream");
    init_repo(&upstream);
    git(dir.path(), &["clone", "-q", "upstream", "clone"]);

    let refs = default_branches(&dir.path().join("clone")).unwrap();

    assert_eq!(refs, ["refs/remotes/origin/main", "refs/heads/main"]);
}

#[test]
fn default_branches_should_fall_back_to_local_main_without_remote() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());

    assert_eq!(default_branches(dir.path()).unwrap(), ["refs/heads/main"]);
}
