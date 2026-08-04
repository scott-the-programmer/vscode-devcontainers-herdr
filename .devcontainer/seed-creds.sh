#!/bin/sh
# Seed git / SSH / gh credentials from the read-only host mounts.
#
# Same shape as seed-claude.sh: devcontainer.json bind-mounts the host copies
# read-only (/host-ssh, /host-gitconfig, /host-git, /host-gh) and this copies
# what's needed into container-owned files. Copying rather than writing through
# the mount is deliberate:
#   - ssh refuses a private key it doesn't own, and the bind mount carries the
#     host uid (501 on macOS), not vscode's 1000;
#   - `gh auth` and `git config --global` rewrite their files in place, which
#     would otherwise mutate the host's.
#
# Idempotent: existing container-side files win. Pass --force to overwrite.
set -eu

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

# Host $HOME, injected by devcontainer.json. Absolute host paths inside
# .gitconfig (signingkey, allowedSignersFile) have to be rewritten to the
# container's $HOME or git fails to sign.
HOST_HOME="${HERDR_HOST_HOME:-}"

# copy SRC DEST [mode] — skip if DEST exists and we're not forcing.
copy() {
  [ -e "$1" ] || return 0
  if [ -e "$2" ] && [ "$FORCE" -eq 0 ]; then
    echo "seed-creds: keeping existing ${2#$HOME/}"
    return 0
  fi
  rm -rf "$2"
  cp -Rf "$1" "$2"
  if [ -n "${3:-}" ]; then
    chmod "$3" "$2"
  fi
  echo "seed-creds: seeded ${2#$HOME/}"
}

## --- ssh ------------------------------------------------------------------
# Keys and known_hosts only. The agent sockets in ~/.ssh/agent are host-side
# and meaningless in here, and copying a live socket errors out.
if [ -d /host-ssh ]; then
  mkdir -p "$HOME/.ssh"
  chmod 700 "$HOME/.ssh"
  for f in /host-ssh/id_* /host-ssh/known_hosts /host-ssh/config; do
    [ -f "$f" ] || continue
    name=$(basename "$f")
    case "$name" in
      *.pub) copy "$f" "$HOME/.ssh/$name" 644 ;;
      id_*)  copy "$f" "$HOME/.ssh/$name" 600 ;;
      *)     copy "$f" "$HOME/.ssh/$name" 644 ;;
    esac
  done
else
  echo "seed-creds: /host-ssh not mounted — skipping ssh"
fi

## --- git ------------------------------------------------------------------
if [ -f /host-gitconfig ] && { [ ! -f "$HOME/.gitconfig" ] || [ "$FORCE" -eq 1 ]; }; then
  if [ -n "$HOST_HOME" ]; then
    sed "s|$HOST_HOME|$HOME|g" /host-gitconfig > "$HOME/.gitconfig"
  else
    cp /host-gitconfig "$HOME/.gitconfig"
  fi
  echo "seed-creds: seeded .gitconfig"
elif [ -f "$HOME/.gitconfig" ]; then
  echo "seed-creds: keeping existing .gitconfig"
fi

# allowed_signers backs `gpg.ssh.allowedSignersFile`; without it `git log
# --show-signature` can't verify what this container signs.
if [ -f /host-git/allowed_signers ]; then
  mkdir -p "$HOME/.config/git"
  copy /host-git/allowed_signers "$HOME/.config/git/allowed_signers" 644
fi

## --- gh -------------------------------------------------------------------
# hosts.yml holds an OAuth token, so keep it owner-only.
if [ -d /host-gh ]; then
  mkdir -p "$HOME/.config"
  copy /host-gh "$HOME/.config/gh"
  [ -f "$HOME/.config/gh/hosts.yml" ] && chmod 600 "$HOME/.config/gh/hosts.yml"
fi

echo "seed-creds: done"
