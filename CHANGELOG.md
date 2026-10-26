# Changelog

## Unreleased

- A new line that names a branch on the remote only, such as `fix` for `origin/fix`, now
  makes a local branch that tracks it, instead of a new branch from the default branch.
- An unknown start, such as `fix@nope`, is blocked on the confirm screen.
- Each line shows the directory, then the branch. Edit the directory to move the worktree,
  and edit the branch to rename the branch; one no longer renames the other.
- A new line takes a branch, or a directory and then a branch, such as `tui feat/tui`. Given
  only a branch, the directory is its name with each `/` as `-`. A hint under each new line
  shows what it will create, or why it can't.
- `o` and `O` create in the repo of the line under the cursor, and the new line shows under
  that repo. Before, `O` on a repo's first line used the repo above it.
- **Breaking:** write `app:fix`, not `app/fix`, to create a worktree in another repo.

## 0.1.0

First release.

- Edit git worktrees as a vim-style buffer: delete a line to remove a worktree, edit a name to
  rename it, add a line to create one, then `:w` to review and apply.
- Graded removes: clean and merged, clean with unpushed commits, or dirty (needs force). A
  branch whose remote branch was deleted is flagged "upstream gone" and never counts as pushed.
- Just before a safe branch is deleted, jumar grades it again, and keeps it if it changed.
- Finds Claude Code and herdr worktrees out of the box, plus configurable sources.
- Runs in the terminal or as a herdr plugin in a floating pane, and shows herdr agent status.
- `jumar list [--json]` for scripts, and `jumar init` shell functions to `cd` on Enter.
- `jumar pick` and a Claude Code plugin to pick worktrees from inside an agent session.
- Tools on `<Space>` keys, with `revdiff` and `reviewr` presets.
- Auto-refresh and a per-repo default branch.
- An AGE column, and `:clean` to remove every safe worktree shown.
- `o` `#123` checks out a GitHub PR or GitLab merge request into a new worktree.
- New worktrees fetch the remote branch they start from first (`fetch_on_create`).
- Per-repo `copy` and `on_create` to set up new worktrees, such as copying `.env` and
  running `npm install`.
- The current repo and `[[repo]]` entries are listed before they have worktrees, so you can
  create the first one.
