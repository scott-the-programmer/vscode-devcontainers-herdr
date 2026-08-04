#!/bin/sh
# Install Claude Code into the image at build time.
#
# This is the same flow as https://claude.ai/install.sh — resolve the version,
# verify the binary against the release manifest checksum, then let the binary
# install itself — with one addition: the download resumes.
#
# Upstream's curl runs with no timeout and no retry. This ~260MB download
# stalls often enough from inside Docker that a plain `curl | bash` build step
# hangs forever rather than failing, so the transfer is driven here instead:
# --speed-limit/--speed-time turn a stall into an error, and -C - resumes from
# whatever already landed.
set -eu

TARGET="${1:-latest}"
BASE_URL=https://downloads.claude.ai/claude-code-releases
DOWNLOAD_DIR="$HOME/.claude/downloads"
MAX_ATTEMPTS=20

case "$(uname -m)" in
  aarch64 | arm64) arch=arm64 ;;
  x86_64 | amd64) arch=x64 ;;
  *)
    echo "install-claude: unsupported architecture $(uname -m)" >&2
    exit 1
    ;;
esac
platform="linux-${arch}"

fetch() { curl -fsSL --max-time 60 --retry 3 --retry-delay 2 "$1"; }

# `latest` and `stable` are channel aliases the release host resolves for us;
# anything else is expected to already be a version number.
version="$TARGET"
case "$TARGET" in
  latest | stable) version=$(fetch "$BASE_URL/$TARGET") ;;
esac
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "install-claude: bad version from $BASE_URL/$TARGET: '$version'" >&2
    exit 1
    ;;
esac

echo "install-claude: claude $version ($platform)"
mkdir -p "$DOWNLOAD_DIR"
binary="$DOWNLOAD_DIR/claude-$version-$platform"

attempt=0
while [ "$attempt" -lt "$MAX_ATTEMPTS" ]; do
  attempt=$((attempt + 1))
  if curl -fL --no-progress-meter -C - \
    --speed-limit 20480 --speed-time 30 \
    -o "$binary" "$BASE_URL/$version/$platform/claude"; then
    break
  fi
  echo "install-claude: attempt $attempt stalled at $(wc -c <"$binary" 2>/dev/null || echo 0) bytes, resuming"
  sleep 2
done

# The manifest lists every platform; "linux-arm64" carries its closing quote so
# it cannot also match the "linux-arm64-musl" entry.
expected=$(fetch "$BASE_URL/$version/manifest.json" |
  tr -d '\n' | grep -o "\"$platform\"[^}]*" | grep -o '[a-f0-9]\{64\}' | head -1)
actual=$(sha256sum "$binary" | cut -d' ' -f1)
if [ -z "$expected" ]; then
  echo "install-claude: no checksum for $platform in the $version manifest" >&2
  rm -f "$binary"
  exit 1
fi
if [ "$expected" != "$actual" ]; then
  echo "install-claude: checksum mismatch (expected $expected, got $actual)" >&2
  echo "install-claude: the download did not complete in $MAX_ATTEMPTS attempts, or the file is corrupt" >&2
  rm -f "$binary"
  exit 1
fi

chmod +x "$binary"
# No target argument: the binary installs itself, rather than re-resolving a
# channel and downloading a second copy.
"$binary" install
rm -f "$binary"

# $HOME/.claude is a named volume at runtime, so whatever the installer left
# under it is masked in the running container. Only ~/.local survives, so fail
# the build now if the launcher did not land there.
if [ ! -x "$HOME/.local/bin/claude" ]; then
  echo "install-claude: no launcher at \$HOME/.local/bin/claude after install" >&2
  exit 1
fi
"$HOME/.local/bin/claude" --version
