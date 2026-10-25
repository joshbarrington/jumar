#!/usr/bin/env bash
# Opens `jumar pick` in an overlay beside the agent, waits for it to close, and prints the
# pick: blocks of `key: value` lines, or nothing when the user picked nothing.
# Overlays: herdr (jumar's plugin pane, else a zoomed split), tmux popup, WezTerm split.
# Exit 3 means no overlay is available here.
set -euo pipefail

JUMAR=$(command -v jumar || true)
if [ -z "$JUMAR" ]; then
    echo "error: jumar not found in PATH (brew install joshbarrington/tap/jumar)" >&2
    exit 1
fi

DIR=$(mktemp -d "${TMPDIR:-/tmp}/jumar-pick-XXXXXX")
OUT="$DIR/pick"
trap 'rm -rf "$DIR"' EXIT

sq() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

# jumar writes an empty pick however it exits; this fallback covers it being killed.
CMD="$(sq "$JUMAR") pick --output $(sq "$OUT"); [ -e $(sq "$OUT") ] || : > $(sq "$OUT")"

wait_for_pick() {
    while [ ! -e "$OUT" ]; do
        sleep 0.2
    done
    cat "$OUT"
}

if [ "${HERDR_ENV:-}" = 1 ] && command -v herdr >/dev/null 2>&1; then
    if herdr plugin pane open --plugin joshbarrington.jumar --entrypoint pick \
        --placement overlay --cwd "$PWD" --env "JUMAR_PICK_OUTPUT=$OUT" --focus >/dev/null 2>&1; then
        wait_for_pick
        exit 0
    fi
    # Without jumar's herdr plugin installed, take over the agent's pane with a zoomed split.
    split=$(herdr pane split --pane "${HERDR_PANE_ID:-}" --direction down --cwd "$PWD" --focus 2>&1) || {
        echo "error: herdr pane split failed: $split" >&2
        exit 1
    }
    pane=$(printf '%s' "$split" | grep -o '"pane_id":"[^"]*"' | head -1 | cut -d'"' -f4)
    if [ -z "$pane" ]; then
        echo "error: herdr pane split returned no pane id: $split" >&2
        exit 1
    fi
    herdr pane zoom "$pane" --on >/dev/null 2>&1 || true
    herdr pane run "$pane" "sh -c $(sq "$CMD")" >/dev/null
    wait_for_pick
    herdr pane close "$pane" >/dev/null 2>&1 || true
    exit 0
fi

if [ -n "${TMUX:-}" ] && command -v tmux >/dev/null 2>&1; then
    tmux display-popup -E -w 90% -h 90% -d "$PWD" "sh -c $(sq "$CMD")"
    wait_for_pick
    exit 0
fi

if [ -n "${WEZTERM_PANE:-}" ] && command -v wezterm >/dev/null 2>&1; then
    wezterm cli split-pane --bottom --percent 80 --pane-id "$WEZTERM_PANE" --cwd "$PWD" \
        -- sh -c "$CMD" >/dev/null
    wait_for_pick
    exit 0
fi

echo "error: no overlay here; jumar pick needs herdr, tmux or WezTerm" >&2
exit 3
