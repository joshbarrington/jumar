# Changelog

## 0.1.0

First release.

- Edit git worktrees as a vim-style buffer: delete a line to remove a worktree, edit a name to
  rename it, add a line to create one, then `:w` to review and apply.
- Graded removes: clean and merged, clean with unpushed commits, or dirty (needs force).
- Finds Claude Code and herdr worktrees out of the box, plus configurable sources.
- Runs in the terminal or as a herdr plugin in a floating pane, and shows herdr agent status.
- `jumar list [--json]` for scripts, and `jumar init` shell functions to `cd` on Enter.
- `jumar pick` and a Claude Code plugin to pick worktrees from inside an agent session.
- Tools on `<Space>` keys, with `revdiff` and `reviewr` presets.
- Auto-refresh and a per-repo default branch.
