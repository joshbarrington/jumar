---
name: jumar
description: Pick a git worktree in jumar's overlay and either move into it to work there, or pass one or more worktrees to the conversation as context. Activates on "jumar", "pick a worktree", "switch worktree", "work in a worktree", "work in <name>", "add worktree as context", "use the <name> worktree as context", "which worktrees do I have".
argument-hint: 'optional: "work <name>" or "context <name>" to skip the picker'
allowed-tools: [Bash, Read]
---

# jumar: pick a worktree

jumar lists the git worktrees of the user's repos. This skill opens it as an overlay,
lets the user pick, and acts on the pick.

## Step 1: Get the pick

If `$ARGUMENTS` names a worktree (for example `work refactor` or `context fix-login`),
skip the overlay: run `jumar list --json`, find the entry whose path ends in that name,
and treat it as a pick with that use. Ask the user when several match or none do.

Otherwise run the launcher, with the bash timeout set to the maximum the harness allows
(it waits for the user):

```bash
"${CLAUDE_SKILL_DIR}/scripts/launch-jumar.sh"
```

In the overlay, Enter picks the worktree under the cursor to work in, and Tab passes it as
context (`V` then Tab passes several). The launcher prints one block per picked worktree:

```
use: work
path: /home/me/app/.claude/worktrees/fix-login
repo: /home/me/app
branch: fix-login
status: 2 changed
base: 3f2c1e9a...
```

`use` is `work` or `context`. `base` is where the branch left the repo's default branch;
it is missing when there is no default branch. A block for a repo's own checkout also has
`main: true`.

- **No output**: the user closed jumar without picking. Say so in one line and stop.
- **Exit 3** (no overlay: not in herdr, tmux or WezTerm): run `jumar list --json` and ask
  the user which worktree, and whether to work in it or use it as context.
- **Other failures**: report the launcher's stderr as it is.

## Step 2: Act on it

### `use: work`

Move the session into the worktree:

- If the path is under `<repo>/.claude/worktrees/` and an `EnterWorktree` tool is
  available, call it with `path` set to the picked path.
- Otherwise run every later command in that directory (`cd <path> && ...`, or `git -C
  <path>`), and use absolute paths under it for file edits, since the shell's directory
  may reset between commands.

Then tell the user, in one line, the branch and status you are now working on.

### `use: context`

Do not change directory. For each picked worktree, read enough to know what it is:

```bash
git -C <path> status --short
git -C <path> log --oneline <base>..HEAD
git -C <path> diff --stat <base>
```

Without a `base`, use `git -C <path> log --oneline -10` instead.

Give the user a short summary per worktree (branch, what its commits do, uncommitted
changes), and keep the paths in mind for the rest of the conversation.
