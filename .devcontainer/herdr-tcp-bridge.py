#!/usr/bin/env python3
"""Expose the herdr API unix socket on the host loopback as a TCP port.

Runs on the HOST. Claude's herdr integration hook
(~/.claude/hooks/herdr-agent-state.sh) reports pane agent state by connecting to
an AF_UNIX socket named by $HERDR_SOCKET_PATH. A container cannot reach the
host's socket directly: Docker Desktop's file sharing does not carry unix
sockets across the VM boundary (/var/run/docker.sock works only because Docker
Desktop special-cases it), so bind-mounting ~/.config/herdr/herdr.sock into the
container gets you a path that exists and never connects.

This bridges the socket to 127.0.0.1:<port> instead. The container reaches it as
host.docker.internal:<port>, which Docker Desktop forwards to the host loopback
— so the herdr control socket stays off the LAN. On the container side,
.devcontainer/herdr-relay.sh turns that port back into a unix socket for the
hook to connect to.

Deliberately stdlib-only: this is the one piece that has to run on the host, and
the host is not guaranteed to have socat (macOS ships without it).
"""

import argparse
import socket
import socketserver
import sys
import threading

BUFFER_SIZE = 65536


def pump(src, dst):
    """Copy src -> dst until EOF, then half-close dst so the peer sees the EOF.

    The half-close matters: the hook sends one JSON request, then blocks in
    recv() for herdr's reply. Without SHUT_WR propagating in each direction,
    both ends wait on a connection neither will close.
    """
    try:
        while True:
            data = src.recv(BUFFER_SIZE)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass


class BridgeHandler(socketserver.BaseRequestHandler):
    def handle(self):
        upstream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            upstream.connect(self.server.herdr_socket)
        except OSError as exc:
            # herdr stopped, or the socket was replaced by a restart. Log and
            # drop the connection; the hook swallows the failure either way.
            print(f"bridge: {self.server.herdr_socket}: {exc}", flush=True)
            upstream.close()
            return
        with upstream:
            outbound = threading.Thread(
                target=pump, args=(self.request, upstream), daemon=True
            )
            outbound.start()
            pump(upstream, self.request)
            outbound.join()


class BridgeServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True, help="path to herdr.sock")
    parser.add_argument("--port", type=int, required=True, help="loopback port to listen on")
    parser.add_argument(
        "--bind",
        default="127.0.0.1",
        help="address to listen on (default 127.0.0.1; do not widen this)",
    )
    args = parser.parse_args()

    server = BridgeServer((args.bind, args.port), BridgeHandler)
    server.herdr_socket = args.socket
    print(f"bridge: {args.bind}:{args.port} -> {args.socket}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
