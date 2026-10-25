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

CMD="$(sq "$JUMAR") pick --output $(sq "$OUT")"

# Waits until jumar writes its pick or the overlay is gone (closed by the user, or jumar
# killed), then prints the pick if there is one. "$@" checks the overlay is still open.
wait_for_pick() {
    while [ ! -e "$OUT" ] && "$@"; do
        sleep 0.2
    done
    if [ -e "$OUT" ]; then
        cat "$OUT"
    fi
}

pane_id() {
    grep -o '"pane_id":"[^"]*"' | head -1 | cut -d'"' -f4
}

herdr_pane_open() {
    herdr pane get "$1" >/dev/null 2>&1
}

wezterm_pane_open() {
    wezterm cli list --format json 2>/dev/null | grep "\"pane_id\": $1," >/dev/null
}

if [ "${HERDR_ENV:-}" = 1 ] && command -v herdr >/dev/null 2>&1; then
    if opened=$(herdr plugin pane open --plugin joshbarrington.jumar --entrypoint pick \
        --placement overlay --cwd "$PWD" --env "JUMAR_PICK_OUTPUT=$OUT" --focus 2>/dev/null); then
        pane=$(printf '%s' "$opened" | pane_id)
        if [ -n "$pane" ]; then
            wait_for_pick herdr_pane_open "$pane"
        else
            wait_for_pick true
        fi
        exit 0
    fi
    # Without jumar's herdr plugin installed, take over the agent's pane with a zoomed split.
    split=$(herdr pane split --pane "${HERDR_PANE_ID:-}" --direction down --cwd "$PWD" --focus 2>&1) || {
        echo "error: herdr pane split failed: $split" >&2
        exit 1
    }
    pane=$(printf '%s' "$split" | pane_id)
    if [ -z "$pane" ]; then
        echo "error: herdr pane split returned no pane id: $split" >&2
        exit 1
    fi
    herdr pane zoom "$pane" --on >/dev/null 2>&1 || true
    herdr pane run "$pane" "$CMD; exit" >/dev/null
    wait_for_pick herdr_pane_open "$pane"
    herdr pane close "$pane" >/dev/null 2>&1 || true
    exit 0
fi

if [ -n "${TMUX:-}" ] && command -v tmux >/dev/null 2>&1; then
    # The popup blocks until jumar exits or the user closes it.
    tmux display-popup -E -w 90% -h 90% -d "$PWD" "$CMD" || true
    wait_for_pick false
    exit 0
fi

if [ -n "${WEZTERM_PANE:-}" ] && command -v wezterm >/dev/null 2>&1; then
    pane=$(wezterm cli split-pane --bottom --percent 80 --pane-id "$WEZTERM_PANE" --cwd "$PWD" \
        -- sh -c "$CMD")
    wait_for_pick wezterm_pane_open "$pane"
    exit 0
fi

echo "error: no overlay here; jumar pick needs herdr, tmux or WezTerm" >&2
exit 3
