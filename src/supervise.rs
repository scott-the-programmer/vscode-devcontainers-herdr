//! start / stop / status for the two long-lived forwarders in `forward`.
//!
//! Ownership is deliberately loose: several herdr panes share one bridge, so no
//! single pane owns its lifetime, and `start` is idempotent — if something
//! already answers on the endpoint, that's either this forwarder from an earlier
//! pane or from an earlier container start, and either way there's nothing to do.
//!
//! No launchd/systemd unit: a forwarder is only useful while herdr is up, and
//! the socket path is whatever the running herdr server chose.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use crate::settings;

/// How long to wait for a freshly spawned forwarder to answer, in 100ms steps.
const READY_STEPS: u32 = 20;

/// The host end: `<bind>:<port>` -> herdr's unix socket.
pub fn bridge() -> Daemon {
    let port = settings::port();
    let run = settings::runtime_dir();
    Daemon {
        label: "bridge",
        endpoint: format!(
            "{}:{port} -> {}",
            settings::bridge_bind(),
            settings::host_socket().display()
        ),
        pidfile: run.join(format!("herdr-tcp-bridge-{port}.pid")),
        log: run.join(format!("herdr-tcp-bridge-{port}.log")),
        serve_args: vec!["bridge".into(), "serve".into()],
    }
}

/// The container end: a local unix socket -> the host bridge port. Runs in the
/// container, so its pid and log live in the container's home.
pub fn relay() -> Daemon {
    let sock = settings::relay_socket();
    let dir = sock.parent().map_or_else(settings::home, Path::to_path_buf);
    // The log file has to be creatable before the forwarder is spawned, and on a
    // fresh container nothing has made this directory yet.
    let _ = std::fs::create_dir_all(&dir);
    Daemon {
        label: "relay",
        endpoint: format!(
            "{} -> {}:{}",
            sock.display(),
            settings::relay_host(),
            settings::port()
        ),
        pidfile: dir.join("relay.pid"),
        log: dir.join("relay.log"),
        serve_args: vec!["relay".into(), "serve".into()],
    }
}

/// One supervised forwarder: the `serve` invocation of this same binary, plus
/// where its pid and output land.
pub struct Daemon {
    /// Prefix on every message, and the name in the usage text ("bridge", "relay").
    pub label: &'static str,
    /// Human-readable endpoint, for messages only.
    pub endpoint: String,
    pub pidfile: PathBuf,
    pub log: PathBuf,
    /// Arguments to re-exec ourselves with, e.g. `["bridge", "serve"]`.
    pub serve_args: Vec<String>,
}

impl Daemon {
    /// Spawn the forwarder unless the endpoint already answers. `ready` is the
    /// endpoint probe — `forward::tcp_answers` or `forward::unix_answers`.
    pub fn start(&self, ready: &dyn Fn() -> bool) -> Result<String, String> {
        if ready() {
            return Ok(format!(
                "{}: already listening on {}",
                self.label, self.endpoint
            ));
        }

        let exe = std::env::current_exe().map_err(|e| format!("{}: {e}", self.label))?;
        let log = File::create(&self.log)
            .map_err(|e| format!("{}: {}: {e}", self.label, self.log.display()))?;
        let mut cmd = Command::new(exe);
        cmd.args(&self.serve_args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log.try_clone()
                    .map_err(|e| format!("{}: {e}", self.label))?,
            ))
            .stderr(Stdio::from(log));

        // setsid: leave the pane's session so the forwarder outlives the shell
        // that started it — what `nohup … &` bought in the shell version. Failing
        // means we're already a session leader, which is just as good.
        //
        // SAFETY: setsid is async-signal-safe, which is the bar for pre_exec.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        let child = cmd
            .spawn()
            .map_err(|e| format!("{}: failed to spawn: {e}", self.label))?;
        let _ = std::fs::write(&self.pidfile, child.id().to_string());

        // Bind failures (port taken, socket path gone) happen after the fork, so
        // a live pid is not proof of a working forwarder — wait for the endpoint.
        for _ in 0..READY_STEPS {
            if ready() {
                return Ok(format!("{}: listening on {}", self.label, self.endpoint));
            }
            sleep(Duration::from_millis(100));
        }
        Err(format!(
            "{}: failed to start; last log lines:\n{}",
            self.label,
            tail(&self.log, 5)
        ))
    }

    pub fn stop(&self) -> String {
        match self.pid() {
            Some(pid) if alive(pid) => {
                // SAFETY: kill with a valid signal number; no memory involved.
                unsafe { libc::kill(pid, libc::SIGTERM) };
                let _ = std::fs::remove_file(&self.pidfile);
                format!("{}: stopped pid {pid}", self.label)
            }
            _ => {
                let _ = std::fs::remove_file(&self.pidfile);
                format!(
                    "{}: not running from {}",
                    self.label,
                    self.pidfile.display()
                )
            }
        }
    }

    /// Err when nothing answers, so the caller can exit non-zero — `make relay`
    /// and any script wrapping it read the exit code, not the message.
    pub fn status(&self, ready: &dyn Fn() -> bool) -> Result<String, String> {
        if ready() {
            Ok(format!("{}: listening on {}", self.label, self.endpoint))
        } else {
            Err(format!(
                "{}: nothing listening on {}",
                self.label, self.endpoint
            ))
        }
    }

    fn pid(&self) -> Option<i32> {
        std::fs::read_to_string(&self.pidfile)
            .ok()?
            .trim()
            .parse()
            .ok()
    }
}

/// True if a process with this pid exists — including one we don't own, which is
/// why the pidfile alone never decides whether a forwarder is up.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 performs error checking only.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Last `n` lines of a log, for the failure message. Reads the tail of the file
/// rather than all of it: a long-lived forwarder's log can grow.
fn tail(path: &Path, n: usize) -> String {
    const WINDOW: i64 = 8192;
    let Ok(mut file) = File::open(path) else {
        return format!("({}: unreadable)", path.display());
    };
    let _ = file.seek(SeekFrom::End(-WINDOW));
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        // Not utf-8 from that offset; fall back to whatever the start gives us.
        buf.clear();
        let _ = File::open(path).and_then(|mut f| f.read_to_string(&mut buf));
    }
    let lines: Vec<&str> = buf.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(dir: &Path) -> Daemon {
        Daemon {
            label: "bridge",
            endpoint: "127.0.0.1:47100".into(),
            pidfile: dir.join("bridge.pid"),
            log: dir.join("bridge.log"),
            serve_args: vec!["bridge".into(), "serve".into()],
        }
    }

    #[test]
    fn start_is_a_no_op_when_the_endpoint_already_answers() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemon(dir.path());
        let msg = d.start(&|| true).unwrap();
        assert!(msg.contains("already listening"), "{msg}");
        // Nothing spawned, so nothing to record.
        assert!(!d.pidfile.exists());
    }

    #[test]
    fn start_reports_the_log_when_the_endpoint_never_answers() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = daemon(dir.path());
        // A "forwarder" that exits immediately without binding anything.
        d.serve_args = vec!["--version-that-does-not-exist".into()];
        let err = d.start(&|| false).unwrap_err();
        assert!(err.contains("failed to start"), "{err}");
        assert!(d.log.exists());
    }

    #[test]
    fn stop_without_a_pidfile_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemon(dir.path());
        assert!(d.stop().contains("not running"));
    }

    #[test]
    fn stop_clears_a_stale_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemon(dir.path());
        // pid 0 is never a real process to signal here; the point is that a
        // pidfile naming something dead doesn't wedge stop.
        std::fs::write(&d.pidfile, "2147483647").unwrap();
        assert!(d.stop().contains("not running"));
        assert!(!d.pidfile.exists());
    }

    #[test]
    fn status_is_err_when_nothing_answers() {
        let dir = tempfile::tempdir().unwrap();
        let d = daemon(dir.path());
        assert!(d.status(&|| false).is_err());
        assert!(d.status(&|| true).is_ok());
    }

    #[test]
    fn tail_returns_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("x.log");
        std::fs::write(&log, "a\nb\nc\nd\n").unwrap();
        assert_eq!(tail(&log, 2), "c\nd");
        assert_eq!(tail(&log, 99), "a\nb\nc\nd");
        assert!(tail(&dir.path().join("gone"), 5).contains("unreadable"));
    }

    #[test]
    fn alive_recognises_this_process() {
        assert!(alive(std::process::id() as i32));
    }
}
