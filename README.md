<p align="center">
  <img src="assets/logo.png" alt="jumar logo: a rope ascender clamped on a rope" width="160">
</p>

# jumar

Manage git worktrees from an editable, vim-style buffer, in the spirit of
[oil.nvim](https://github.com/stevearc/oil.nvim). Delete a line to remove a worktree, edit a
name to rename it, add a line to create one, then `:w` to review and apply.

```
    NAME          BRANCH       STATUS        SOURCE   HERDR
 app  ~/github/app
    (main)        main         clean
 ✓  fix-login     fix-login    clean         claude
 ↑  search        search       ↑2            claude   ● working
 !  new-api       new-api      3 changed     herdr    ▲ blocked
 ↑  old-spike     old-spike    upstream gone claude
```

jumar finds Claude Code and herdr worktrees out of the box, plus every other worktree of the
repos it finds. Before anything is removed it checks what you would lose, and it runs in the
terminal, as a [herdr](https://herdr.dev) plugin, or as a picker for coding agents.

A jumar is a rope ascender: a clamp that slides up a rope and grips when you stop.

## Requirements

- macOS or Linux
- git 2.17 or newer
- A [Nerd Font](https://www.nerdfonts.com) in your terminal, for the status icons
- For `jumar pick` inside an agent session: herdr, tmux or WezTerm, to open the overlay
- Optional: [herdr](https://herdr.dev), for the HERDR column and workspaces;
  [revdiff](https://github.com/umputun/revdiff) and the reviewr herdr plugin, for the tool
  presets

## Install

```sh
brew install joshbarrington/tap/jumar
# or
curl -LsSf https://github.com/joshbarrington/jumar/releases/latest/download/jumar-installer.sh | sh
# or, from source
cargo install --locked --git https://github.com/joshbarrington/jumar jumar
```

### Shell integration

To `cd` into a worktree when you press Enter, add the shell function to your shell config:

```sh
eval "$(jumar init zsh)"   # ~/.zshrc, or bash in ~/.bashrc
jumar init fish | source   # ~/.config/fish/config.fish
```

Without it, Enter prints the path and a reminder.

### herdr

```sh
herdr plugin install joshbarrington/jumar
```

jumar then opens in a floating pane, Enter goes to the worktree's herdr workspace, and the
HERDR column shows each worktree's agent: working, blocked, done or idle.

### Claude Code and other agents

```sh
/plugin install jumar --marketplace joshbarrington/jumar
```

Then ask Claude to "pick a worktree". jumar opens over the session (in herdr, tmux or
WezTerm). Enter tells Claude to work in the worktree; Tab, or `V` then Tab for several,
passes them to the conversation as context.

The skill in `skills/jumar` is plain instructions plus a launcher script around `jumar pick
--output <file>`, so other agents that read skills can use it too.

## Use

`jumar` opens the buffer. Edit it like text, then `:w` to see every change before anything
runs.

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
| `Tab` | under `jumar pick`: pass the worktree (or `V` selection) to the agent as context |
| `R` `:e` | reload, refreshing every status (`:e!` drops unsaved edits) |
| `<Space>d` | open revdiff on the worktree's changes |
| `<Space>r` | open reviewr in the worktree's herdr workspace |
| `?` `:help` | show these keys |

### Removing

Each worktree is graded by what removing it would lose:

| Mark | Meaning | On remove |
|---|---|---|
| green check | clean, and merged or fully pushed | removes the checkout and its branch |
| yellow arrow | clean, but with commits found nowhere else | keeps the branch |
| red mark | uncommitted or untracked changes | needs force, and lists what would be lost |

A branch whose remote branch was deleted shows "upstream gone". That usually means its PR
was merged, but jumar can't be sure, so it stays yellow and keeps the branch.

The confirm screen after `:w` lists each change:

| Keys | Action |
|---|---|
| `j` `k` | move between changes |
| `D` | toggle deleting the branch too |
| `f` | toggle force, for dirty or locked worktrees and running herdr agents |
| `y` | apply |
| `q` `Esc` | cancel and go back to the buffer |

### Renaming and creating

A rename moves the directory, and renames the branch when it had the same name. Worktrees
open in a herdr workspace go through herdr; one with a running agent needs force to remove
and cannot be renamed.

`o` takes `name`, `repo/name` or `name@base`. A new name makes a new branch from the repo's
default branch, or from `base`; the name of an existing branch checks that branch out.

Each repo's own checkout is listed first as `(main)`: you can go to it or run tools on it,
but not remove or rename it. The repo you run jumar from, and every `[[repo]]` in the
config, is listed even before it has any worktrees, so `o` on its `(main)` line creates the
first one.

## What jumar finds

| Source | Finds | Creates in |
|---|---|---|
| `claude` | `~/*/.claude/worktrees/*`, `~/github/*/.claude/worktrees/*` | `{repo}/.claude/worktrees/{name}` |
| `herdr` | `~/.herdr/worktrees/*/*` | `~/.herdr/worktrees/{repo_name}/{name}` |
| other | every other worktree of a repo found above, or of the current or a configured repo | |

New worktrees go to the `herdr` source inside herdr and to `claude` outside it, unless you
set `default_create_source`. If your repos live somewhere else, add a source or change the
preset's globs.

## Configure

`~/.config/jumar/config.toml`:

```toml
default_create_source = "work"
refresh_seconds = 5   # re-read statuses and herdr agents; 0 turns it off

[[repo]]
path = "~/work/app"
default_branch = "develop"   # instead of origin/HEAD, main or master

[[source]]
name = "work"
globs = ["~/work/*/.worktrees/*"]
create_path = "{repo}/.worktrees/{name}"

[[source]]
name = "claude"   # same name as a preset: replaces its globs
globs = ["~/code/*/.claude/worktrees/*"]

[[tool]]
name = "lazygit"
key = "g"
command = ["lazygit"]
mode = "suspend"   # suspend | herdr-workspace | detach
```

While jumar is open it refreshes every `refresh_seconds`. It adds and drops worktrees that
came or went, but leaves the list alone while you have unsaved edits. A repo's
`default_branch` decides what counts as merged, and is where `{base}` and new worktrees
start from.

A `[[source]]` named `claude` or `herdr` replaces that preset's globs (empty disables it),
and its `create_path` and `open_in_herdr` when given. `create_path` can use `{repo}`,
`{repo_name}` and `{name}`.

### Tools

A tool runs with `<Space>` and its key. Tool commands can use `{path}`, `{repo}`,
`{branch}`, `{base}` (the merge-base with the default branch) and `{default_branch}`.

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

## Scripting

`jumar list` prints the worktrees as a table, and `jumar list --json` as JSON. Each entry has
`path`, `repo`, `source`, `head`, `health`, `tier` (`safe`, `unmerged` or `dirty`) and
`status`, which holds `branch`, `upstream`, `upstream_gone`, `ahead`, `behind`, `changed`,
`untracked` and `conflicted`. Repos' own checkouts are left out.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE)
