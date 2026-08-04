#!/bin/sh
# Runs once after the container is created.
set -eu

cd "$(dirname "$0")/.."

# The named volumes come up root-owned; cargo needs to write to two of them,
# and seed-claude.sh below writes into the third.
sudo chown -R "$(id -u):$(id -g)" target /usr/local/cargo/registry "$HOME/.claude"

rustup component add clippy rustfmt

# Claude Code itself is not installed here — it's baked into the image by
# .devcontainer/Dockerfile, so a rebuild doesn't re-download it. Only the
# config seeding below is a per-container step.

# Copy the shareable parts of the host's Claude config into this container's
# own ~/.claude volume. Non-fatal: a missing host mount just means a fresh login.
.devcontainer/seed-claude.sh || echo "post-create: claude seed skipped"

# Same for git/SSH/gh: copy the host credentials into container-owned files.
# Non-fatal — an unmounted host dir just means git/gh here are unauthenticated.
.devcontainer/seed-creds.sh || echo "post-create: creds seed skipped"

# Warm the dependency cache so the first cargo check in the editor is quick.
# No Cargo.lock is committed yet, so resolve rather than --locked.
# Non-fatal: the crate has no src/main.rs or src/lib.rs yet, so cargo cannot
# resolve a target and this fails until one exists. Don't block container setup.
cargo fetch || echo "post-create: cargo fetch skipped (no crate target yet)"
