#!/bin/sh
# Cargo.toml, herdr-plugin.toml, Cargo.lock and the release tag must agree:
# build.sh reads the version out of Cargo.toml and asks GitHub for exactly
# that tag, so any drift here is a plugin install that silently falls back
# to compiling from source.
#
# usage: check-version.sh [tag]
#   with no argument, only Cargo.toml/herdr-plugin.toml/Cargo.lock are
#   compared (run on every PR); with a tag (e.g. from the release workflow),
#   the tag is checked against them too.
set -eu
cd "$(dirname "$0")/../.."

field() {
  sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | head -1
}

cargo="$(field Cargo.toml)"
plugin="$(field herdr-plugin.toml)"
lock="$(awk '
  /^name = "herdr-devcontainer-status"$/ { f = 1; next }
  f && /^version/ { gsub(/"/, "", $3); print $3; exit }
' Cargo.lock)"

fail=0
[ -n "$cargo" ] || { echo "Cargo.toml: no [package] version found" >&2; fail=1; }
[ "$cargo" = "$plugin" ] || {
  echo "herdr-plugin.toml says $plugin, Cargo.toml says $cargo" >&2
  fail=1
}
[ "$cargo" = "$lock" ] || {
  echo "Cargo.lock says $lock, Cargo.toml says $cargo (run: cargo update -p herdr-devcontainer-status)" >&2
  fail=1
}

if [ $# -gt 0 ]; then
  tag="${1#refs/tags/}"
  [ "$tag" = "v$cargo" ] || {
    echo "tag $tag does not match v$cargo" >&2
    fail=1
  }
fi

[ "$fail" -eq 0 ] || exit 1
echo "version $cargo (tag v$cargo)"
