#!/bin/sh
# Seed the container's Claude config from the read-only host mounts.
#
# devcontainer.json puts the host's ~/.claude at /host-claude and ~/.claude.json
# at /host-claude.json, both read-only, and gives /home/vscode/.claude its own
# named volume. This copies across the parts worth sharing and leaves the rest.
#
# Deliberately NOT copied: history.jsonl, projects/, todos/, shell-snapshots/,
# ide/, file-history/, backups/, cache/ — per-machine session state that would
# only confuse a container session (and is most of the host dir's 300MB).
#
# Idempotent: existing container-side entries win, so anything you change in
# here survives a re-run. Pass --force to overwrite from the host copies.
set -eu

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

SRC=/host-claude
DEST="$HOME/.claude"

# Where the credentials come from. /host-claude-credentials.json is written on
# the host by export-creds.sh immediately before the container starts, and is
# the only source that reflects the macOS Keychain — the .credentials.json
# inside /host-claude is a side-copy Claude stops updating there, so on macOS it
# is typically hours out of date. Fall back to it only when the export is
# absent (older container, or the mount was dropped).
CREDS_SRC=/host-claude-credentials.json
[ -f "$CREDS_SRC" ] || CREDS_SRC="$SRC/.credentials.json"

# Two files are exempt from "existing wins", because the thing they hold can go
# bad on its own and then the idempotence pins the bad state in the named volume
# for the life of the container:
#
#   .credentials.json — a failed OAuth refresh makes Claude write the record
#     back with empty accessToken/refreshToken. That looks like a file to keep,
#     but it's a logged-out session, and every restart keeps it.
#   .claude.json — the VS Code extension can start before postCreateCommand
#     finishes and write a virgin 400-byte config first. Keeping that loses the
#     host's MCP servers, trusted folders and onboarding flags.
#
# Both get re-seeded when they're detectably empty rather than genuinely local.

# creds_stale FILE — true if the file is missing, unparseable, holds a
# claudeAiOauth record with no access token, or holds one that has expired.
# Expiry counts because the container's refresh token may have been rotated out
# from under it by the host, in which case Claude can't recover on its own; a
# freshly exported host record is strictly better. An unexpired token is left
# alone — overwriting a live session's credentials is how you cause the very
# rotation mismatch this is trying to avoid.
creds_stale() {
  [ -f "$1" ] || return 0
  python3 - "$1" <<'PY'
import json, sys, time
try:
    oauth = json.load(open(sys.argv[1])).get("claudeAiOauth") or {}
except Exception:
    sys.exit(0)
fresh = oauth.get("accessToken") and oauth.get("expiresAt", 0) > time.time() * 1000
sys.exit(1 if fresh else 0)
PY
}

# virgin_config FILE — true if .claude.json is missing or is the stub Claude
# writes on a first run (no projects/mcpServers of its own yet).
virgin_config() {
  [ -f "$1" ] || return 0
  python3 - "$1" <<'PY'
import json, sys
try:
    cfg = json.load(open(sys.argv[1]))
except Exception:
    sys.exit(0)
sys.exit(0 if not cfg.get("projects") and not cfg.get("mcpServers") else 1)
PY
}

if [ ! -d "$SRC" ]; then
  echo "seed-claude: $SRC not mounted — skipping (Claude will start fresh)"
  exit 0
fi

# The named volume comes up root-owned.
sudo chown "$(id -u):$(id -g)" "$DEST"
mkdir -p "$DEST"

# Credentials first, and from CREDS_SRC rather than $SRC — see above. This is
# the one entry that gets replaced when it already exists, because "already
# exists" here can mean "expired" or "logged out", and keeping either pins a
# broken login into the named volume for the life of the container.
if [ -f "$CREDS_SRC" ]; then
  if [ "$FORCE" -eq 1 ] || creds_stale "$DEST/.credentials.json"; then
    cp "$CREDS_SRC" "$DEST/.credentials.json"
    echo "seed-claude: seeded .credentials.json from ${CREDS_SRC}"
  else
    echo "seed-claude: keeping existing .credentials.json (still valid)"
  fi
else
  echo "seed-claude: no host credentials mounted — Claude will prompt for login"
fi

# settings.json references $HOME/.claude/hooks/... and a statusline script, and
# enables plugins from a marketplace — carry all of them so the seeded settings
# don't point at things that aren't here.
for entry in \
  settings.json \
  CLAUDE.md \
  statusline-command.sh \
  commands \
  agents \
  skills \
  hooks \
  plugins; do

  [ -e "$SRC/$entry" ] || continue
  if [ -e "$DEST/$entry" ] && [ "$FORCE" -eq 0 ]; then
    echo "seed-claude: keeping existing $entry"
    continue
  fi
  rm -rf "$DEST/$entry"
  cp -R "$SRC/$entry" "$DEST/$entry"
  echo "seed-claude: seeded $entry"
done

# MCP servers, trusted-folder state and onboarding flags live here. Copying it
# once means the container session doesn't re-run onboarding. It also carries
# per-project history for host paths that don't exist in here; harmless.
# A stub written by an extension that beat postCreate to the punch counts as
# absent — see virgin_config above.
if [ -f /host-claude.json ] && { virgin_config "$HOME/.claude.json" || [ "$FORCE" -eq 1 ]; }; then
  cp /host-claude.json "$HOME/.claude.json"
  echo "seed-claude: seeded .claude.json"
else
  echo "seed-claude: keeping existing .claude.json"
fi

# The OAuth token is a secret; keep it owner-only however the host had it.
[ -f "$DEST/.credentials.json" ] && chmod 600 "$DEST/.credentials.json"
[ -f "$HOME/.claude.json" ] && chmod 600 "$HOME/.claude.json"

echo "seed-claude: done"
