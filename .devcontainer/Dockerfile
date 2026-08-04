ARG BASE_IMAGE=mcr.microsoft.com/devcontainers/rust:1-bookworm
FROM ${BASE_IMAGE}

# Claude Code is baked into the image instead of installed by post-create.sh.
# The native install is a ~260MB download; as a post-create step every rebuild
# paid for it again, and a failed download left a container with no `claude`
# (which is what post-create's non-fatal `|| echo` did on first run here).
#
# Installed as `vscode`, not root: the native installer is $HOME-relative, so
# as root the launcher lands in /root/.local/bin where the remoteUser can't
# reach it. Set CLAUDE_VERSION to a version number to pin instead of tracking
# the latest release.
ARG CLAUDE_VERSION=latest
USER vscode
COPY --chown=vscode:vscode install-claude.sh /tmp/install-claude.sh
RUN /tmp/install-claude.sh "${CLAUDE_VERSION}" && rm -f /tmp/install-claude.sh

# The launcher lands in ~/.local/bin, which this image does not have on PATH.
# Set it here rather than in devcontainer.json's remoteEnv so `docker run` on
# this image finds claude too, not just the devcontainer lifecycle.
#
# Deliberately *after* the RUN. Moving it earlier would silence the installer's
# "~/.local/bin is not in your PATH" note, but that note is exactly what makes
# it append the export to vscode's shell rc — and that append is what keeps
# claude reachable in a login shell, where Debian's /etc/profile overwrites
# PATH and would otherwise drop this ENV.
ENV PATH="/home/vscode/.local/bin:${PATH}"

# Back to the base image's default user: devcontainer features and the
# postCreateCommand wrapper expect to start from root, and devcontainer.json's
# remoteUser is what selects `vscode` for the actual session.
USER root
