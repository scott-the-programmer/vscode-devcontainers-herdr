//! Socket forwarding for herdr agent state, host side and container side.
//!
//! Claude's herdr integration hook (`~/.claude/hooks/herdr-agent-state.sh`)
//! reports a pane's agent session by connecting to `$HERDR_SOCKET_PATH` as an
//! AF_UNIX socket. A container cannot reach the host's socket directly: Docker
//! Desktop's file sharing does not carry unix sockets across the VM boundary
//! (`/var/run/docker.sock` works only because Docker Desktop special-cases it),
//! so bind-mounting `~/.config/herdr/herdr.sock` into the container gets you a
//! path that exists and never connects.
//!
//! Two hops bridge it instead, and they are mirror images of each other:
//!
//! ```text
//! container:  $HOME/.herdr/herdr.sock  ->  host.docker.internal:47100   (relay)
//! host:       127.0.0.1:47100          ->  ~/.config/herdr/herdr.sock   (bridge)
//! ```
//!
//! The host end binds loopback by default — Docker Desktop forwards
//! `host.docker.internal` to the host's `127.0.0.1` — so the herdr control
//! socket never reaches the LAN.
//!
//! Native Linux Docker provides neither half of that arrangement. There is no
//! `host.docker.internal` unless the container was started with
//! `--add-host=host.docker.internal:host-gateway`, and a listener bound to
//! `127.0.0.1` does not accept connections arriving over the docker bridge —
//! so fixing only the name turns a resolution failure into a refused
//! connection. `HERDR_RELAY_HOST` already moves the container end;
//! `HERDR_BRIDGE_BIND` moves the host end. Both default to today's behaviour.
//!
//! Widening the bind is opt-in because the herdr control socket is not
//! authenticated: whatever reaches the port can drive herdr. Binding the docker
//! bridge address exposes it to every container on that bridge. That is a real
//! trade, and it belongs to the person making it rather than to a default.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const BUFFER_SIZE: usize = 65536;

/// A byte stream we can forward, of either address family.
///
/// The half-close is the reason this is a trait rather than plain `Read + Write`:
/// forwarding has to propagate EOF, and `shutdown` lives on the concrete types.
pub trait Stream: Read + Write + Send {
    /// Close the write half, so the peer sees EOF while still able to reply.
    fn shutdown_write(&self) -> io::Result<()>;
    /// A second handle on the same connection, for the opposite direction.
    fn dup(&self) -> io::Result<Box<dyn Stream>>;
}

impl Stream for TcpStream {
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
    fn dup(&self) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(self.try_clone()?))
    }
}

impl Stream for UnixStream {
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(Shutdown::Write)
    }
    fn dup(&self) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(self.try_clone()?))
    }
}

/// Copy `src` into `dst` until EOF, then half-close `dst` so the peer sees it.
///
/// The half-close matters: the hook sends one JSON request, then blocks reading
/// herdr's reply. Without `SHUT_WR` propagating in each direction, both ends
/// wait on a connection neither will close.
fn pump(mut src: Box<dyn Stream>, mut dst: Box<dyn Stream>) {
    let mut buf = vec![0u8; BUFFER_SIZE];
    loop {
        match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = dst.shutdown_write();
}

/// Join two connections into one bidirectional pipe, returning when both
/// directions have seen EOF.
pub fn splice(a: Box<dyn Stream>, b: Box<dyn Stream>) -> io::Result<()> {
    let a_back = a.dup()?;
    let b_back = b.dup()?;
    let outbound = thread::spawn(move || pump(a, b));
    pump(b_back, a_back);
    let _ = outbound.join();
    Ok(())
}

/// Host side: accept on `listener`, forward each connection to the unix socket
/// at `target`. One thread per connection, so concurrent hook calls don't queue.
pub fn serve_tcp_to_unix(listener: TcpListener, target: &Path) -> io::Result<()> {
    for conn in listener.incoming() {
        let conn = match conn {
            Ok(c) => c,
            Err(e) => {
                eprintln!("bridge: accept: {e}");
                continue;
            }
        };
        let target = target.to_path_buf();
        thread::spawn(move || match UnixStream::connect(&target) {
            Ok(upstream) => {
                let _ = splice(Box::new(conn), Box::new(upstream));
            }
            // herdr stopped, or a restart replaced the socket. Log and drop the
            // connection; the hook swallows the failure either way.
            Err(e) => eprintln!("bridge: {}: {e}", target.display()),
        });
    }
    Ok(())
}

/// Container side: accept on the local unix socket, forward each connection to
/// the host's bridge port.
pub fn serve_unix_to_tcp(listener: UnixListener, host: &str, port: u16) -> io::Result<()> {
    for conn in listener.incoming() {
        let conn = match conn {
            Ok(c) => c,
            Err(e) => {
                eprintln!("relay: accept: {e}");
                continue;
            }
        };
        let host = host.to_string();
        thread::spawn(move || match TcpStream::connect((host.as_str(), port)) {
            Ok(upstream) => {
                let _ = splice(Box::new(conn), Box::new(upstream));
            }
            // No host bridge: costs agent state in herdr, not the session.
            Err(e) => eprintln!("relay: {host}:{port}: {e}"),
        });
    }
    Ok(())
}

/// Bind the bridge port. Loopback by default and deliberately — see the module
/// docs and `settings::bridge_bind` before widening `addr`.
pub fn bind_tcp(addr: &str, port: u16) -> io::Result<TcpListener> {
    TcpListener::bind((addr, port))
}

/// Bind the relay's unix socket, replacing a stale one left by an earlier
/// container start (socat's `unlink-early`), and keep it owner-only.
pub fn bind_unix(path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// True if anything accepts a connection on the bridge port at `addr`.
///
/// Liveness is probed by connecting rather than by reading a pidfile: a pid can
/// be recycled, and a bridge started by another pane never wrote our pidfile.
///
/// The probe has to target the address the bridge actually bound. A bridge on
/// the docker bridge address does not answer on loopback, and probing the wrong
/// one makes `start` believe it failed and spawn another forwarder on every
/// exec.
pub fn tcp_answers(addr: &str, port: u16) -> bool {
    match (addr, port).to_socket_addrs() {
        Ok(mut addrs) => {
            addrs.any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok())
        }
        // An unresolvable bind address answers nothing, which is the same
        // observable state as a bridge that is not running.
        Err(_) => false,
    }
}

/// True if anything accepts a connection on the relay's unix socket.
pub fn unix_answers(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Where the relay listens inside the container, and where the bridge finds
/// herdr on the host — "the herdr socket as seen from here", either way.
pub fn socket_path(fallback: impl Fn() -> PathBuf) -> PathBuf {
    std::env::var_os("HERDR_SOCKET_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Stand-in for herdr: read one request to EOF, reply, close. Reading to EOF
    /// is the point — it only completes if the client's half-close propagated.
    fn fake_herdr(listener: UnixListener, replies: usize) {
        for conn in listener.incoming().take(replies) {
            let mut conn = conn.unwrap();
            let mut req = String::new();
            conn.read_to_string(&mut req).unwrap();
            conn.write_all(format!("reply:{req}").as_bytes()).unwrap();
        }
    }

    fn request(mut conn: TcpStream, body: &str) -> String {
        conn.write_all(body.as_bytes()).unwrap();
        conn.shutdown(Shutdown::Write).unwrap();
        let mut got = String::new();
        conn.read_to_string(&mut got).unwrap();
        got
    }

    #[test]
    fn bridges_tcp_to_unix_with_half_close_in_both_directions() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("herdr.sock");
        let upstream = bind_unix(&sock).unwrap();
        thread::spawn(move || fake_herdr(upstream, 1));

        let listener = bind_tcp("127.0.0.1", 0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = sock.clone();
        thread::spawn(move || serve_tcp_to_unix(listener, &target));

        let conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert_eq!(request(conn, r#"{"id":1}"#), r#"reply:{"id":1}"#);
    }

    #[test]
    fn relays_unix_to_tcp_end_to_end_through_the_bridge() {
        // Both hops at once: unix -> tcp -> unix, the shape a container session
        // actually takes.
        let dir = tempfile::tempdir().unwrap();
        let herdr_sock = dir.path().join("herdr.sock");
        let upstream = bind_unix(&herdr_sock).unwrap();
        thread::spawn(move || fake_herdr(upstream, 1));

        let bridge = bind_tcp("127.0.0.1", 0).unwrap();
        let port = bridge.local_addr().unwrap().port();
        let target = herdr_sock.clone();
        thread::spawn(move || serve_tcp_to_unix(bridge, &target));

        let container_sock = dir.path().join("container/herdr.sock");
        let relay = bind_unix(&container_sock).unwrap();
        thread::spawn(move || serve_unix_to_tcp(relay, "127.0.0.1", port));

        let mut conn = UnixStream::connect(&container_sock).unwrap();
        conn.write_all(br#"{"id":2}"#).unwrap();
        conn.shutdown(Shutdown::Write).unwrap();
        let mut got = String::new();
        conn.read_to_string(&mut got).unwrap();
        assert_eq!(got, r#"reply:{"id":2}"#);
    }

    #[test]
    fn missing_upstream_drops_one_connection_not_the_listener() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("herdr.sock");

        let listener = bind_tcp("127.0.0.1", 0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = sock.clone();
        thread::spawn(move || serve_tcp_to_unix(listener, &target));

        // herdr isn't there yet: the connection is accepted, then dropped — as
        // EOF, or as a reset if our request was still in flight. Either way the
        // hook gets no reply and swallows it.
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let _ = conn.write_all(b"lost");
        let mut got = String::new();
        match conn.read_to_string(&mut got) {
            Ok(_) => assert!(got.is_empty(), "unexpected reply: {got}"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::ConnectionReset),
        }

        // herdr comes up; the bridge is still serving.
        let (up, ready) = (bind_unix(&sock).unwrap(), mpsc::channel());
        thread::spawn(move || {
            ready.0.send(()).unwrap();
            fake_herdr(up, 1);
        });
        ready.1.recv().unwrap();
        let conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert_eq!(request(conn, "again"), "reply:again");
    }

    #[test]
    fn rebinding_replaces_a_stale_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("herdr.sock");
        let first = bind_unix(&sock).unwrap();
        drop(first); // a container restart leaves the file behind
        assert!(sock.exists());
        assert!(bind_unix(&sock).is_ok());
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn probes_report_whether_anything_answers() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("herdr.sock");
        assert!(!unix_answers(&sock));
        let _listener = bind_unix(&sock).unwrap();
        assert!(unix_answers(&sock));

        let listener = bind_tcp("127.0.0.1", 0).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_answers("127.0.0.1", port));
        drop(listener);
        assert!(!tcp_answers("127.0.0.1", port));
    }

    #[test]
    fn a_bridge_is_not_found_on_an_address_it_did_not_bind() {
        // The regression behind HERDR_BRIDGE_BIND: probing loopback for a
        // bridge bound elsewhere reports "not running" forever.
        let listener = bind_tcp("127.0.0.1", 0).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp_answers("127.0.0.1", port));
        assert!(!tcp_answers("192.0.2.1", port)); // TEST-NET-1, RFC 5737
    }

    #[test]
    fn an_unresolvable_address_is_false_rather_than_a_panic() {
        assert!(!tcp_answers("no-such-host.invalid", 47100)); // RFC 2606
    }

    #[test]
    fn socket_path_prefers_the_environment() {
        let fallback = || PathBuf::from("/fallback/herdr.sock");
        // The env var is process-global; assert on the fallback branch only when
        // herdr hasn't set it for this test run.
        match std::env::var_os("HERDR_SOCKET_PATH") {
            Some(v) if !v.is_empty() => assert_eq!(socket_path(fallback), PathBuf::from(v)),
            _ => assert_eq!(socket_path(fallback), PathBuf::from("/fallback/herdr.sock")),
        }
    }
}
