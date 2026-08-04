#!/bin/sh
# Runs IN THE CONTAINER. Presents the host's herdr API socket as a local unix
# socket, so Claude's herdr hook — which only knows how to connect to
# $HERDR_SOCKET_PATH as AF_UNIX — can reach it.
#
#   $HOME/.herdr/herdr.sock  ->  host.docker.internal:$HERDR_RELAY_PORT
#                            ->  (host bridge) ~/.config/herdr/herdr.sock
#
# The host end is .devcontainer/herdr-host-relay.sh; without it the socket here
# exists but every connection is refused, which the hook swallows silently.
#
# Started from postStartCommand and again by herdr-exec.sh before each session,
# so it survives a socat crash. Idempotent.
set -eu

PORT="${HERDR_RELAY_PORT:-47100}"
TARGET_HOST="${HERDR_RELAY_HOST:-host.docker.internal}"
SOCK="$HOME/.herdr/herdr.sock"
PIDFILE="$HOME/.herdr/relay.pid"
LOG="$HOME/.herdr/relay.log"

mkdir -p "$HOME/.herdr"

if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null && [ -S "$SOCK" ]; then
  echo "herdr-relay: already listening on $SOCK"
  exit 0
fi

if ! command -v socat >/dev/null 2>&1; then
  echo "herdr-relay: socat not installed in this image — no agent reporting" >&2
  exit 1
fi

# fork: one socat child per connection, so concurrent hook calls don't queue.
# unlink-early: a container restart leaves the previous socket file behind.
rm -f "$SOCK"
nohup socat "UNIX-LISTEN:$SOCK,fork,unlink-early,mode=600" \
  "TCP:$TARGET_HOST:$PORT" >"$LOG" 2>&1 &
echo $! >"$PIDFILE"

i=0
while [ "$i" -lt 20 ]; do
  if [ -S "$SOCK" ]; then
    echo "herdr-relay: $SOCK -> $TARGET_HOST:$PORT"
    exit 0
  fi
  sleep 0.1
  i=$((i + 1))
done

echo "herdr-relay: socat failed to start; last log lines:" >&2
tail -5 "$LOG" >&2 2>/dev/null || true
exit 1
