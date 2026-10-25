#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]

mod common;

use std::fs;
use std::path::PathBuf;

use jumar_core::config::Source;
use jumar_core::discovery::{repo_containing, repos, scan_repo, scan_sources};
use jumar_core::model::{Head, Health, Worktree};
use tempfile::TempDir;

use common::{git, init_repo};

struct Fixture {
    home: TempDir,
}

impl Fixture {
    /// `~/github/app` with a claude worktree `feat` and an unmatched worktree `~/elsewhere/other`.
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("github/app");
        init_repo(&repo);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                ".claude/worktrees/feat",
            ],
        );
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "other",
                "../../elsewhere/other",
            ],
        );
        Self { home }
    }

    fn path(&self, rel: &str) -> PathBuf {
        fs::canonicalize(self.home.path().join(rel)).unwrap()
    }

    fn scan(&self, globs: &[&str]) -> Vec<Worktree> {
        let sources = [Source {
            name: "claude".into(),
            globs: globs.iter().map(|g| (*g).to_owned()).collect(),
            create_path: None,
            open_in_herdr: None,
        }];
        scan_sources(&sources, self.home.path()).unwrap()
    }
}

const CLAUDE: &str = "~/github/*/.claude/worktrees/*";

#[test]
fn scan_sources_should_find_worktree_matching_glob() {
    let fx = Fixture::new();

    let found = fx.scan(&[CLAUDE]);

    assert_eq!(found[0].path, fx.path("github/app/.claude/worktrees/feat"));
}

#[test]
fn scan_sources_should_tag_worktree_with_source_name() {
    let fx = Fixture::new();

    let found = fx.scan(&[CLAUDE]);

    assert_eq!(found[0].source.as_deref(), Some("claude"));
}

#[test]
fn scan_sources_should_resolve_repo_to_main_checkout() {
    let fx = Fixture::new();

    let found = fx.scan(&[CLAUDE]);

    assert_eq!(found[0].repo, fx.path("github/app"));
}

#[test]
fn scan_sources_should_read_branch_head() {
    let fx = Fixture::new();

    let found = fx.scan(&[CLAUDE]);

    assert_eq!(found[0].head, Some(Head::Branch("feat".into())));
}

#[test]
fn scan_sources_should_skip_directories_that_are_not_worktrees() {
    let fx = Fixture::new();
    fs::create_dir_all(fx.home.path().join("github/app/.claude/worktrees/plain")).unwrap();

    let found = fx.scan(&[CLAUDE]);

    assert_eq!(found.len(), 1);
}

#[test]
fn scan_sources_should_report_each_directory_once_when_globs_overlap() {
    let fx = Fixture::new();

    let found = fx.scan(&[CLAUDE, "~/github/app/.claude/worktrees/*"]);

    assert_eq!(found.len(), 1);
}

#[test]
fn scan_sources_should_mark_orphaned_when_metadata_missing() {
    let fx = Fixture::new();
    fs::remove_dir_all(fx.home.path().join("github/app/.git/worktrees/feat")).unwrap();

    let found = fx.scan(&[CLAUDE]);

    assert_eq!(found[0].health, Health::Orphaned);
}

#[test]
fn scan_sources_should_resolve_repo_when_orphaned() {
    let fx = Fixture::new();
    fs::remove_dir_all(fx.home.path().join("github/app/.git/worktrees/feat")).unwrap();

    let found = fx.scan(&[CLAUDE]);

    assert_eq!(found[0].repo, fx.path("github/app"));
}

#[test]
fn scan_repo_should_return_only_unmatched_worktrees() {
    let fx = Fixture::new();
    let known = fx.scan(&[CLAUDE]);

    let others = scan_repo(&repos(&known)[0], &known).unwrap();

    let paths: Vec<_> = others.iter().map(|w| w.path.clone()).collect();
    assert_eq!(paths, [fx.path("github/app"), fx.path("elsewhere/other")]);
}

#[test]
fn scan_repo_should_mark_main_checkout() {
    let fx = Fixture::new();
    let known = fx.scan(&[CLAUDE]);

    let others = scan_repo(&repos(&known)[0], &known).unwrap();

    assert_eq!((others[0].main, others[1].main), (true, false));
}

#[test]
fn scan_repo_should_leave_source_empty() {
    let fx = Fixture::new();
    let known = fx.scan(&[CLAUDE]);

    let others = scan_repo(&repos(&known)[0], &known).unwrap();

    assert_eq!(others[1].source, None);
}

#[test]
fn scan_repo_should_mark_prunable_when_directory_deleted() {
    let fx = Fixture::new();
    let known = fx.scan(&[CLAUDE]);
    fs::remove_dir_all(fx.home.path().join("elsewhere/other")).unwrap();

    let others = scan_repo(&repos(&known)[0], &known).unwrap();

    assert_eq!(others[1].health, Health::Prunable);
}

#[test]
fn repo_containing_should_return_main_checkout_from_subdirectory() {
    let fx = Fixture::new();
    let dir = fx.home.path().join("github/app/src");
    fs::create_dir_all(&dir).unwrap();

    assert_eq!(repo_containing(&dir), Some(fx.path("github/app")));
}

#[test]
fn repo_containing_should_return_main_checkout_from_linked_worktree() {
    let fx = Fixture::new();

    let repo = repo_containing(&fx.path("elsewhere/other"));

    assert_eq!(repo, Some(fx.path("github/app")));
}

#[test]
fn repo_containing_should_be_none_outside_a_repo() {
    let fx = Fixture::new();

    assert_eq!(repo_containing(&fx.path("elsewhere")), None);
}

#[test]
fn scan_repo_should_list_main_checkout_of_repo_without_worktrees() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("app");
    init_repo(&repo);
    let repo = repo_containing(&repo).unwrap();

    let found = scan_repo(&repo, &[]).unwrap();

    assert_eq!((found.len(), found[0].main), (1, true));
}
