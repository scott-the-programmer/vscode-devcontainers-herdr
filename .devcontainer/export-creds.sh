#!/bin/sh
# Runs on the HOST (devcontainer.json "initializeCommand"), before the container
# starts. Writes the live Claude credentials to a file the container can mount.
#
# Why this exists: on macOS, Claude Code keeps its OAuth record in the login
# Keychain, and ~/.claude/.credentials.json is a side-copy it stops updating.
# Seeding the container from that file hands it an access token that expired
# hours ago plus a refresh token that has since been rotated away — the refresh
# fails, Claude rewrites the record with empty tokens, and you get a login
# prompt inside the container. Reading the Keychain here means the container is
# seeded with whatever is actually valid right now.
#
# On Linux there is no Keychain and the JSON file *is* the store, so this just
# copies it. Never fatal: with no output file, seed-claude.sh falls back to
# /host-claude/.credentials.json and the worst case is the old behaviour.
set -eu

OUT="$HOME/.claude/.container-credentials.json"
SERVICE="Claude Code-credentials"

mkdir -p "$HOME/.claude"

# If a previous run found no credentials, Docker will have created $OUT as an
# empty *directory* to satisfy the bind mount. Left in place, the mv below would
# move the token file inside it instead of replacing it.
[ -d "$OUT" ] && rmdir "$OUT" 2>/dev/null || true

if security find-generic-password -s "$SERVICE" -w >/dev/null 2>&1; then
  # Write via a temp file so a mounted reader never sees a half-written token.
  tmp="$OUT.tmp.$$"
  security find-generic-password -s "$SERVICE" -w > "$tmp"
  chmod 600 "$tmp"
  mv "$tmp" "$OUT"
  echo "export-creds: exported Keychain credentials to ${OUT#$HOME/}"
elif [ -f "$HOME/.claude/.credentials.json" ]; then
  cp "$HOME/.claude/.credentials.json" "$OUT"
  chmod 600 "$OUT"
  echo "export-creds: no Keychain entry — copied .credentials.json"
else
  echo "export-creds: no credentials found; the container will prompt for login"
fi
