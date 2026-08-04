#!/bin/sh
# herdr plugin build step: compile the release binary into ./bin/ on the HOST.
# The binary must run on the host (macOS/Linux), so this never builds in Docker.
set -eu

cd "$(dirname "$0")"

find_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    command -v cargo
    return 0
  fi
  if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
    if command -v cargo >/dev/null 2>&1; then
      command -v cargo
      return 0
    fi
  fi
  for candidate in \
    "$HOME/.cargo/bin/cargo" \
    /opt/homebrew/opt/rustup/bin/cargo \
    /opt/homebrew/bin/cargo \
    /usr/local/bin/cargo; do
    if [ -x "$candidate" ]; then
      echo "$candidate"
      return 0
    fi
  done
  return 1
}

CARGO="$(find_cargo)" || {
  echo "error: no Rust toolchain found on the host." >&2
  echo "install one with:  brew install rustup && rustup default stable" >&2
  echo "then re-link:      herdr plugin link $(pwd)" >&2
  exit 1
}

echo "building with $CARGO"
"$CARGO" build --release --locked

mkdir -p bin
# Install by rename, never by writing through the existing path: macOS caches a
# code signature per file, and overwriting a binary that has already been run
# leaves the cache mismatched — every later exec then dies with SIGKILL
# ("Killed: 9") before main, with no output to explain it. The rename hands the
# kernel a new file to validate. Only visible since `make claude`/`shell`/`relay`
# run this binary on every session.
cp target/release/herdr-devcontainer-status bin/.herdr-devcontainer-status.new
mv -f bin/.herdr-devcontainer-status.new bin/herdr-devcontainer-status
echo "installed bin/herdr-devcontainer-status"
