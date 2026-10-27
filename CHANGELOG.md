# Changelog

## 0.2.0

- `M` moves the main checkout's branch to a new worktree, and puts the main checkout back on
  the default branch. Uncommitted changes move with it, and a failed move rolls back.
- The worktree table fits the pane: long names are cut and columns hide in narrow panes, while
  the cursor line stays in view.
- Esc drops a new line that is still blank.
- A new line's hint shows short examples, like `branch@base` and `repo:branch`, and wraps
  to fit the pane instead of running off its edge.

## 0.1.0

First release.

- Edit git worktrees as a vim-style buffer: delete a line to remove a worktree, add a line to
  create one, then `:w` to review and apply.
- Each line shows the directory, then the branch. Edit the directory to move the worktree,
  and edit the branch to rename the branch.
- A new line takes a branch, or a directory and then a branch, such as `tui feat/tui`. Given
  only a branch, the directory is its name with each `/` as `-`. A hint under each new line
  shows what it will create, or why it can't.
- A new line that names a branch on the remote only, such as `fix` for `origin/fix`, makes a
  local branch that tracks it. An unknown base, such as `fix@nope`, is blocked on the confirm
  screen.
- `o` and `O` create in the repo of the line under the cursor. `app:fix` creates in another
  listed repo, and `~/code/app:fix` in the repo at that path, so you can create a worktree
  even when jumar lists no repos.
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
