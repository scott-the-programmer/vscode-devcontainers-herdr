#!/bin/sh
# Runs on the HOST. Supervises herdr-tcp-bridge.py, which publishes the herdr
# API unix socket on 127.0.0.1:$HERDR_RELAY_PORT so the container can reach it.
# See the docstring in herdr-tcp-bridge.py for why a bind mount can't do this.
#
#   herdr-host-relay.sh [start|stop|status]
#
# start is idempotent: if something already answers on the port, that's either
# this bridge from an earlier pane or an earlier container start, and either way
# there's nothing to do. Ownership is deliberately loose — several herdr panes
# share one bridge, so no single pane owns its lifetime.
set -eu

DIR="$(cd "$(dirname "$0")" && pwd)"

PORT="${HERDR_RELAY_PORT:-47100}"
SOCK="${HERDR_SOCKET_PATH:-$HOME/.config/herdr/herdr.sock}"
RUNDIR="${TMPDIR:-/tmp}"
PIDFILE="$RUNDIR/herdr-tcp-bridge-$PORT.pid"
LOG="$RUNDIR/herdr-tcp-bridge-$PORT.log"

PYTHON="${PYTHON:-python3}"

# port_open — true if anything accepts a connection on the bridge port. Checked
# instead of the pidfile because the pid can be recycled and because a bridge
# started by another pane won't have written *this* pidfile.
port_open() {
  "$PYTHON" - "$PORT" <<'PY'
import socket, sys
try:
    with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=0.3):
        pass
except OSError:
    sys.exit(1)
PY
}

case "${1:-start}" in
start)
  if port_open; then
    echo "herdr-host-relay: already listening on 127.0.0.1:$PORT"
    exit 0
  fi

  if [ ! -S "$SOCK" ]; then
    echo "herdr-host-relay: no herdr socket at $SOCK — is herdr running?" >&2
    exit 1
  fi

  # nohup + & rather than a launchd job: the bridge is only useful while herdr
  # is up, and herdr's socket path is whatever the running server chose.
  nohup "$PYTHON" "$DIR/herdr-tcp-bridge.py" --socket "$SOCK" --port "$PORT" \
    >"$LOG" 2>&1 &
  echo $! >"$PIDFILE"

  # Bind failures (port taken by something else, socket vanished) happen after
  # the fork, so a live pid isn't proof of a working bridge — wait for the port.
  i=0
  while [ "$i" -lt 20 ]; do
    if port_open; then
      echo "herdr-host-relay: listening on 127.0.0.1:$PORT -> $SOCK"
      exit 0
    fi
    sleep 0.1
    i=$((i + 1))
  done

  echo "herdr-host-relay: bridge failed to start; last log lines:" >&2
  tail -5 "$LOG" >&2 2>/dev/null || true
  exit 1
  ;;

stop)
  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    kill "$(cat "$PIDFILE")"
    echo "herdr-host-relay: stopped pid $(cat "$PIDFILE")"
  else
    echo "herdr-host-relay: not running from $PIDFILE"
  fi
  rm -f "$PIDFILE"
  ;;

status)
  if port_open; then
    echo "herdr-host-relay: listening on 127.0.0.1:$PORT -> $SOCK"
  else
    echo "herdr-host-relay: nothing listening on 127.0.0.1:$PORT"
    exit 1
  fi
  ;;

*)
  echo "usage: $(basename "$0") [start|stop|status]" >&2
  exit 2
  ;;
esac
