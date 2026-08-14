#!/bin/sh
# herdr plugin build step: put a release binary in ./bin/ for the HOST.
#
# Two ways to get one, in this order:
#   1. download the release built for this host's target triple, so a
#      `herdr plugin install` needs no Rust toolchain;
#   2. compile this checkout with cargo.
#
# The binary must run on the host (macOS/Linux), so this never builds in Docker.
set -eu

cd "$(dirname "$0")"

BIN=herdr-devcontainer-status
MODE="${HERDR_PLUGIN_BUILD:-auto}" # auto | prebuilt | source

for arg in "$@"; do
  case "$arg" in
    --source) MODE=source ;;
    --prebuilt) MODE=prebuilt ;;
    -h | --help)
      echo "usage: ./build.sh [--source | --prebuilt]"
      echo "  (default: try the published binary for this host, then cargo)"
      exit 0
      ;;
    *)
      echo "build.sh: unknown option: $arg" >&2
      exit 2
      ;;
  esac
done

# The version this checkout *is*. The release is tagged v$VERSION, the asset is
# named for it, and the binary reports it — which is how a stale or truncated
# download is caught before it replaces a working one. Anchored at column 0, so
# `serde = { version = "1" }` can't win.
VERSION="${HERDR_PLUGIN_VERSION:-$(
  sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1
)}"

# Whichever repo this checkout came from publishes its own releases: herdr
# clones before it builds, so origin is the right place to ask. A fork with no
# releases of its own just 404s and falls back to a source build.
slug_from_origin() {
  url="$(git config --get remote.origin.url 2>/dev/null)" || return 1
  url="${url%.git}"
  case "$url" in
    *github.com[:/]*)
      echo "${url##*github.com}" | sed 's|^[:/]||'
      ;;
    *) return 1 ;;
  esac
}
REPO="${HERDR_PLUGIN_REPO:-$(slug_from_origin || echo scott-the-programmer/vscode-devcontainers-herdr)}"

WORK=
cleanup() {
  [ -n "$WORK" ] && rm -rf "$WORK"
  return 0
}
trap cleanup EXIT INT TERM

note() { echo "build.sh: $*" >&2; }

## --- how we get bytes -------------------------------------------------------

fetch() { # url dest ; 0 only when the file really arrived
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --retry-connrefused -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    return 1
  fi
}

# macOS has shasum and no sha256sum; most Linuxes the other way round.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    return 1
  fi
}

target_triple() {
  case "$(uname -s)" in
    Darwin)
      case "$(uname -m)" in
        arm64 | aarch64) echo aarch64-apple-darwin ;;
        x86_64) echo x86_64-apple-darwin ;; # also Rosetta, correctly
        *) return 1 ;;
      esac
      ;;
    Linux)
      # musl, statically linked: one linux build per arch, no glibc floor.
      case "$(uname -m)" in
        x86_64 | amd64) echo x86_64-unknown-linux-musl ;;
        aarch64 | arm64) echo aarch64-unknown-linux-musl ;;
        *) return 1 ;;
      esac
      ;;
    *) return 1 ;;
  esac
}

## --- installing -------------------------------------------------------------

# Install by rename, never by writing through the existing path: macOS caches a
# code signature per file, and overwriting a binary that has already been run
# leaves the cache mismatched — every later exec then dies with SIGKILL
# ("Killed: 9") before main, with no output to explain it. The rename hands the
# kernel a new file to validate. Only visible since `make claude`/`shell`/`relay`
# run this binary on every session. Same reason a downloaded tarball is always
# extracted to a temp dir first, never straight over bin/.
install_binary() {
  mkdir -p bin
  cp "$1" "bin/.$BIN.new"
  chmod +x "bin/.$BIN.new"
  mv -f "bin/.$BIN.new" "bin/$BIN"
}

# Fetch + checksum-verify + unpack one release asset, leaving $2/$BIN in
# place on success. Shared by the host install path below (which then
# Gatekeeper-clears and version-checks its result) and install_relay_assets
# (which just copies its result into bin/linux/<triple>/).
download_verified() { # triple workdir
  triple="$1"
  workdir="$2"
  asset="$BIN-v$VERSION-$triple.tar.gz"
  base="${HERDR_PLUGIN_BASE_URL:-https://github.com/$REPO/releases/download/v$VERSION}"

  echo "fetching $asset"
  fetch "$base/$asset" "$workdir/$asset" || {
    note "no release asset $asset"
    return 1
  }
  fetch "$base/SHA256SUMS" "$workdir/SHA256SUMS" || {
    note "no SHA256SUMS for v$VERSION"
    return 1
  }

  want="$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1; exit }' "$workdir/SHA256SUMS")"
  [ -n "$want" ] || {
    note "$asset is not listed in SHA256SUMS"
    return 1
  }
  got="$(sha256_of "$workdir/$asset")" || {
    note "no sha256 tool here; not trusting the download"
    return 1
  }
  # Not a fallback case: a mismatch is corruption or tampering, and quietly
  # continuing (compiling instead, or shipping the file anyway) would bury it.
  [ "$want" = "$got" ] || {
    echo "error: checksum mismatch for $asset" >&2
    echo "  expected $want" >&2
    echo "  got      $got" >&2
    exit 1
  }

  (cd "$workdir" && tar -xzf "$asset") || {
    note "cannot unpack $asset"
    return 1
  }
  [ -f "$workdir/$BIN" ] || {
    note "$asset does not contain $BIN"
    return 1
  }
  chmod +x "$workdir/$BIN"
}

# Set by install_prebuilt on success, so install_relay_assets can reuse the
# download instead of fetching it a second time when the host itself is one
# of the two linux-musl targets.
HOST_TRIPLE=
HOST_BINARY=

install_prebuilt() {
  triple="$(target_triple)" || {
    note "no published build for $(uname -s)/$(uname -m)"
    return 1
  }

  WORK="$(mktemp -d "${TMPDIR:-/tmp}/$BIN.XXXXXX")" || return 1
  download_verified "$triple" "$WORK" || return 1

  # curl and wget don't set com.apple.quarantine — but a tarball a human fetched
  # in a browser does, and Gatekeeper refuses an ad-hoc-signed binary that
  # carries it. Clearing it is free and a no-op when the attribute (or xattr
  # itself) is absent.
  xattr -d com.apple.quarantine "$WORK/$BIN" 2>/dev/null || true

  # Run it before trusting it. Wrong arch, a half download and a Gatekeeper
  # refusal all surface here, while the binary already in bin/ is still intact.
  got_version="$("$WORK/$BIN" --version 2>/dev/null)" || {
    note "the downloaded binary does not run on this host"
    return 1
  }
  [ "$got_version" = "$BIN $VERSION" ] || {
    note "downloaded binary reports \"$got_version\", expected \"$BIN $VERSION\""
    return 1
  }

  install_binary "$WORK/$BIN"
  echo "installed bin/$BIN $VERSION (prebuilt, $triple)"
  HOST_TRIPLE="$triple"
  HOST_BINARY="$WORK/$BIN"
}

## --- relay binaries -----------------------------------------------------------

# The two targets build.yml's release matrix publishes for linux — static
# musl, so they run in any container regardless of distro or libc. `exec`
# `docker cp`s whichever one matches a target container's `uname -m` in, so it
# can report agent state into *any* project's container, not just a checkout
# of this crate with its own Rust toolchain (what `exec` used to require).
LINUX_TRIPLES="x86_64-unknown-linux-musl aarch64-unknown-linux-musl"

# Best-effort: status detection is the plugin's primary job and must still
# install whether or not this succeeds. HERDR_PLUGIN_RELAY=0 skips it — no
# `exec` on this host, no need for the extra download.
install_relay_assets() {
  if [ "${HERDR_PLUGIN_RELAY:-1}" = "0" ]; then
    note "HERDR_PLUGIN_RELAY=0: skipping the bundled relay binaries"
    return 0
  fi
  for triple in $LINUX_TRIPLES; do
    dest="bin/linux/$triple"
    if [ "$triple" = "$HOST_TRIPLE" ] && [ -n "$HOST_BINARY" ]; then
      mkdir -p "$dest"
      cp "$HOST_BINARY" "$dest/$BIN"
      chmod +x "$dest/$BIN"
      echo "installed $dest/$BIN $VERSION (reused host download)"
      continue
    fi
    relay_work="$(mktemp -d "${TMPDIR:-/tmp}/$BIN-relay.XXXXXX")" || continue
    if download_verified "$triple" "$relay_work"; then
      mkdir -p "$dest"
      cp "$relay_work/$BIN" "$dest/$BIN"
      chmod +x "$dest/$BIN"
      echo "installed $dest/$BIN $VERSION (relay binary)"
    else
      note "no relay binary for $triple — 'exec' will fall back to building one in the target container"
    fi
    rm -rf "$relay_work"
  done
}

## --- source -------------------------------------------------------------------

# herdr starts a pane's command without an interactive shell, so PATH is
# whatever .zprofile left behind — cargo is regularly not on it even when it's
# installed.
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

build_from_source() {
  CARGO="$(find_cargo)" || return 1
  echo "building with $CARGO"
  "$CARGO" build --release --locked
  install_binary "target/release/$BIN"
  echo "installed bin/$BIN (built from source)"
}

no_toolchain() {
  echo "error: no prebuilt binary for this host at v$VERSION, and no Rust toolchain to build one." >&2
  echo "" >&2
  echo "either install Rust:  brew install rustup && rustup default stable" >&2
  echo "then re-link:         herdr plugin link $(pwd)" >&2
  echo "" >&2
  echo "or see why the download failed:  HERDR_PLUGIN_BUILD=prebuilt ./build.sh" >&2
  echo "(releases: https://github.com/$REPO/releases/tag/v$VERSION)" >&2
  exit 1
}

case "$MODE" in
  source)
    build_from_source || no_toolchain
    install_relay_assets
    ;;
  prebuilt)
    install_prebuilt || {
      echo "error: no usable prebuilt binary for v$VERSION" >&2
      exit 1
    }
    install_relay_assets
    ;;
  auto)
    if install_prebuilt; then
      install_relay_assets
      exit 0
    fi
    note "falling back to a source build"
    build_from_source || no_toolchain
    install_relay_assets
    ;;
  *)
    echo "build.sh: HERDR_PLUGIN_BUILD must be auto|prebuilt|source" >&2
    exit 2
    ;;
esac
