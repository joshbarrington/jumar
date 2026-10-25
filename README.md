# jumar

Manage git worktrees from an editable, vim-style buffer, in the spirit of
[oil.nvim](https://github.com/stevearc/oil.nvim). Delete a line to remove a worktree, edit a
name to rename it, add a line to create one, then `:w` to review and apply.

jumar finds Claude Code and herdr worktrees out of the box, plus any other worktree of the
repos it finds. It runs in the terminal or as a [herdr](https://herdr.dev) plugin in a
floating pane.

## Install

```sh
brew install joshbarrington/tap/jumar
# or
curl -LsSf https://github.com/joshbarrington/jumar/releases/latest/download/jumar-installer.sh | sh
# or, as a herdr plugin
herdr plugin install joshbarrington/jumar
```

To `cd` into a worktree when you press Enter, add the shell function:

```sh
eval "$(jumar init zsh)"   # or bash
jumar init fish | source
```

## Use

`jumar` opens the buffer. `jumar list [--json]` prints the same list for scripts.

| Keys | Action |
|---|---|
| `j` `k` `gg` `G` `h` `l` `w` `b` `0` `$` | move (counts work: `3j`, `5G`) |
| `/` | filter by name, repo, source or branch |
| `dd` | remove the worktree |
| `v` `V` | select lines, then `d` to remove them all (`o` jumps to the other end) |
| `cw` `ciw` `x` `i` `a` `A` `I` | rename it |
| `o` `O` | create one: `name`, `repo/name`, or `name@base` |
| `u` `Ctrl-r` | undo, redo |
| `:w` `:wq` `:q` `:q!` | apply, apply and quit, quit |
| `Enter` | go to the worktree (its herdr workspace inside herdr) |
| `yp` | copy the worktree's path |
| `R` `:e` | reload, refreshing every status (`:e!` drops unsaved edits) |
| `<Space>d` | open revdiff on the worktree's changes |
| `<Space>r` | open reviewr in the worktree's herdr workspace |
| `?` `:help` | show these keys |

`:w` shows every change before anything runs. Removes are graded:

-  (green) clean and merged or fully pushed: removes the checkout and its branch.
-  (yellow) clean but with commits found nowhere else: keeps the branch (`D` deletes it too).
-  (red) uncommitted or untracked changes: needs force (`f`), and lists what would be lost.

Each repo's own checkout is listed first as `(main)`: you can go to it or run tools on it,
but not remove or rename it.

A rename moves the directory, and renames the branch when it had the same name. Worktrees
open in a herdr workspace go through herdr; one with a running agent needs force to remove
and cannot be renamed.

## Configure

`~/.config/jumar/config.toml`:

```toml
default_create_source = "work"

[[source]]
name = "work"
globs = ["~/work/*/.worktrees/*"]
create_path = "{repo}/.worktrees/{name}"

[[tool]]
name = "lazygit"
key = "g"
command = ["lazygit"]
mode = "suspend"   # suspend | herdr-workspace | detach
```

Sources named `claude` or `herdr` adjust the presets. A tool runs with `<Space>` and its
key. Tool commands can use `{path}`, `{repo}`, `{branch}`, `{base}` (the merge-base with
the default branch) and `{default_branch}`.

The `revdiff` and `reviewr` tools are presets. A `[[tool]]` with the same name changes only
the fields it gives, and `command = []` removes it:

```toml
[[tool]]
name = "revdiff"
key = "v"

[[tool]]
name = "difftastic"
key = "t"
command = ["git", "-c", "diff.external=difft", "diff", "{base}"]
```

A config with two tools on one key, a new tool without a key or command, or an unknown
placeholder fails to load with an error naming the tool.

## License

MIT
