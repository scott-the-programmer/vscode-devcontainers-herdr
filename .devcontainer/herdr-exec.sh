#!/bin/bash
# Runs on the HOST, from inside a herdr pane. Executes a command in this repo's
# devcontainer with the calling pane's herdr identity forwarded, so a Claude
# session started in there shows up in herdr as this pane's agent.
#
#   .devcontainer/herdr-exec.sh claude
#   .devcontainer/herdr-exec.sh bash
#
# Four things have to line up, and none of them do by default:
#
#   1. HERDR_ENV / HERDR_PANE_ID / HERDR_SOCKET_PATH must be set in the
#      container. herdr sets them in the pane's shell on the host; nothing
#      carries them across `devcontainer exec`, and the hook exits 0 on the
#      first missing one. --remote-env below passes them per-exec, which is also
#      why they can't live in devcontainer.json: containerEnv is fixed at create
#      time, and the pane id differs per pane.
#   2. HERDR_SOCKET_PATH must name a socket that exists in the container —
#      herdr-relay.sh, not the host path.
#   3. Something must answer on the other end — herdr-host-relay.sh.
#   4. herdr must recognise the pane as running an agent at all, which it decides
#      by argv0, not by the reported session — see the re-exec below.
#
# bash, not sh: `exec -a` is a bashism and the argv0 rewrite depends on it.
set -eu

DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$DIR")"
SELF="$DIR/$(basename "$0")"

PORT="${HERDR_RELAY_PORT:-47100}"
CONTAINER_SOCK="/home/vscode/.herdr/herdr.sock"

[ "$#" -gt 0 ] || { echo "usage: $(basename "$0") <command> [args...]" >&2; exit 2; }

cd "$ROOT"

# Same problem build.sh has with cargo: herdr starts a pane's command without an
# interactive shell, so PATH is whatever .zprofile left behind — and the npm
# global bin that holds `devcontainer` is usually only added by .zshrc. Resolve
# it here rather than depending on how the pane was spawned.
find_devcontainer() {
  if command -v devcontainer >/dev/null 2>&1; then
    command -v devcontainer
    return 0
  fi
  for candidate in \
    "$HOME/.local/bin/devcontainer" \
    "$HOME/.npm-global/bin/devcontainer" \
    /opt/homebrew/bin/devcontainer \
    /usr/local/bin/devcontainer; do
    if [ -x "$candidate" ]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

DEVCONTAINER="$(find_devcontainer)" || {
  echo "herdr-exec: devcontainer CLI not found on PATH." >&2
  echo "install it with:  npm install -g @devcontainers/cli" >&2
  exit 127
}

if [ -z "${HERDR_PANE_ID:-}" ]; then
  # Not in a herdr pane (or herdr's shell integration isn't active): there is no
  # pane to report to, so skip the relay work and just run the command.
  echo "herdr-exec: HERDR_PANE_ID unset — running without agent reporting" >&2
  exec "$DEVCONTAINER" exec --workspace-folder . "$@"
fi

# herdr decides *which* agent a pane is running by scanning the foreground
# process group for a known argv0 ("claude", "codex", ...). Reporting a session
# over the socket is not enough on its own: without a match here the pane stays
# agent=none/status=unknown, and the output rules that produce idle/working never
# run. Everything the host can see in this pane is the devcontainer CLI under
# node, so put one process named `claude` in the group by re-execing ourselves
# with argv0 rewritten. It has to stay alive as the *parent* of the container
# command — hence the plain call rather than an exec at the end of this script.
#
# Only for claude: `herdr-exec.sh bash` should not claim the pane is an agent.
if [ "${HERDR_EXEC_ARGV0:-}" != "done" ] && [ "$(basename "$1")" = "claude" ]; then
  export HERDR_EXEC_ARGV0=done
  exec -a claude /bin/bash "$SELF" "$@"
fi

# Non-fatal: a missing bridge costs agent state in herdr, not the session.
"$DIR/herdr-host-relay.sh" start || \
  echo "herdr-exec: host bridge unavailable — agent state won't reach herdr" >&2

"$DEVCONTAINER" exec --workspace-folder . \
  --remote-env "HERDR_RELAY_PORT=$PORT" \
  .devcontainer/herdr-relay.sh || \
  echo "herdr-exec: container relay unavailable — agent state won't reach herdr" >&2

# No exec: this process is the pane's `claude` for detection purposes, so it has
# to outlive the call. Claude puts the tty in raw mode, so Ctrl-C reaches it as a
# keypress rather than a signal to this process group — no signal plumbing here.
"$DEVCONTAINER" exec --workspace-folder . \
  --remote-env HERDR_ENV=1 \
  --remote-env "HERDR_PANE_ID=$HERDR_PANE_ID" \
  --remote-env "HERDR_SOCKET_PATH=$CONTAINER_SOCK" \
  --remote-env "HERDR_RELAY_PORT=$PORT" \
  "$@"
