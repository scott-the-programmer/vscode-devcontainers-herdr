mod discover;
mod docker;
mod exec;
mod forward;
mod settings;
mod supervise;

use std::path::{Path, PathBuf};

/// What we report to herdr for one pane's project.
#[derive(Debug, PartialEq, serde::Serialize)]
struct Status {
    /// "running" | "stopped" | "none" | "error"
    status: &'static str,
    /// Project root that carries the devcontainer config, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    project_root: Option<PathBuf>,
    /// Container name backing the status, when one exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

const USAGE: &str = "\
usage: herdr-devcontainer-status <command>

  refresh | hook [--all]              print this pane's devcontainer status as JSON
  exec <command> [args...]            run a command in the container as this pane's agent
  bridge  <start|stop|status|serve>   host: publish herdr's socket on the loopback port
  relay   <start|stop|status|serve>   container: present that port as a unix socket
          [--container]               ... drive the container's relay from the host

See .devcontainer/README.md § \"Seeing the container's Claude session in herdr\".";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        // Both entry points do the same detection today; herdr decides when to
        // call us. `hook --all` re-detects for the focused workspace, which is
        // still just "the cwd herdr gave us".
        Some("refresh" | "hook") => print_status(),
        Some("exec") => exec::run(&args[1..]),
        Some("bridge") => bridge(&args[1..]),
        Some("relay") => relay(&args[1..]),
        other => {
            eprintln!("{USAGE}");
            if let Some(cmd) = other {
                eprintln!("\nunknown command: {cmd:?}");
            }
            2
        }
    };
    std::process::exit(code);
}

fn print_status() -> i32 {
    // herdr exports the pane's cwd; fall back to our own for manual runs.
    let cwd = std::env::var_os("HERDR_PANE_CWD")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let status = detect(&cwd);
    println!(
        "{}",
        serde_json::to_string(&status).expect("status serializes")
    );
    0
}

/// The host end of the agent-state bridge. `serve` is the forwarder itself;
/// the other verbs supervise it.
fn bridge(args: &[String]) -> i32 {
    let port = settings::port();
    let ready = || forward::tcp_answers(port);
    match verb(args) {
        "start" => match herdr_socket() {
            Ok(_) => say(supervise::bridge().start(&ready)),
            Err(msg) => say(Err(msg)),
        },
        "stop" => {
            println!("{}", supervise::bridge().stop());
            0
        }
        "status" => say(supervise::bridge().status(&ready)),
        "serve" => match herdr_socket() {
            Ok(socket) => serve_bridge(port, &socket),
            Err(msg) => say(Err(msg)),
        },
        other => {
            eprintln!("{USAGE}\n\nunknown bridge verb: {other:?}");
            2
        }
    }
}

fn serve_bridge(port: u16, socket: &Path) -> i32 {
    match forward::bind_tcp("127.0.0.1", port) {
        Ok(listener) => {
            println!("bridge: 127.0.0.1:{port} -> {}", socket.display());
            if let Err(e) = forward::serve_tcp_to_unix(listener, socket) {
                eprintln!("bridge: {e}");
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("bridge: 127.0.0.1:{port}: {e}");
            1
        }
    }
}

/// The container end. `--container` makes the host drive the one in the
/// container instead, which is what `make relay` and `exec` use.
fn relay(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--container") {
        return exec::relay_in_container(verb(args));
    }
    let socket = settings::relay_socket();
    let ready = || forward::unix_answers(&socket);
    match verb(args) {
        "start" => say(supervise::relay().start(&ready)),
        "stop" => {
            println!("{}", supervise::relay().stop());
            0
        }
        "status" => say(supervise::relay().status(&ready)),
        "serve" => serve_relay(&socket),
        other => {
            eprintln!("{USAGE}\n\nunknown relay verb: {other:?}");
            2
        }
    }
}

fn serve_relay(socket: &Path) -> i32 {
    let (host, port) = (settings::relay_host(), settings::port());
    match forward::bind_unix(socket) {
        Ok(listener) => {
            println!("relay: {} -> {host}:{port}", socket.display());
            if let Err(e) = forward::serve_unix_to_tcp(listener, &host, port) {
                eprintln!("relay: {e}");
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("relay: {}: {e}", socket.display());
            1
        }
    }
}

/// The herdr API socket, or why we can't serve it.
fn herdr_socket() -> Result<PathBuf, String> {
    let socket = settings::host_socket();
    if socket.exists() {
        Ok(socket)
    } else {
        Err(format!(
            "bridge: no herdr socket at {} — is herdr running?",
            socket.display()
        ))
    }
}

/// First non-flag argument, defaulting to `start` like the shell scripts did.
fn verb(args: &[String]) -> &str {
    args.iter()
        .map(String::as_str)
        .find(|a| !a.starts_with('-'))
        .unwrap_or("start")
}

/// Print a supervisor outcome, exiting non-zero when nothing is listening.
fn say(outcome: Result<String, String>) -> i32 {
    match outcome {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(msg) => {
            eprintln!("{msg}");
            1
        }
    }
}

fn detect(cwd: &Path) -> Status {
    let Some(found) = discover::find(cwd) else {
        return Status {
            status: "none",
            project_root: None,
            container: None,
            error: None,
        };
    };

    match docker::list() {
        Ok(containers) => {
            let matched = best_match(&containers, &found.project_root)
                // Dogfood path: when we run *inside* a devcontainer against the
                // host daemon, labels carry host paths while discovery sees the
                // /workspaces bind mount. HERDR_HOST_WORKSPACE (set by our own
                // devcontainer.json) bridges the two.
                .or_else(|| {
                    let host_root = PathBuf::from(std::env::var_os("HERDR_HOST_WORKSPACE")?);
                    best_match(&containers, &host_root)
                });
            Status {
                status: match matched {
                    Some(c) if c.is_running() => "running",
                    Some(_) => "stopped",
                    None => "none",
                },
                project_root: Some(found.project_root),
                container: matched.map(|c| c.name.clone()),
                error: None,
            }
        }
        Err(e) => Status {
            status: "error",
            project_root: Some(found.project_root),
            container: None,
            error: Some(e),
        },
    }
}

/// Pick the container labelled for `root`. Compose projects put several
/// containers on one local_folder; prefer a running one so the status doesn't
/// flap to "stopped" while a sidecar is down.
fn best_match<'a>(
    containers: &'a [docker::Container],
    root: &Path,
) -> Option<&'a docker::Container> {
    containers
        .iter()
        .filter(|c| Path::new(&c.local_folder) == root)
        .max_by_key(|c| c.is_running())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(state: &str, folder: &str, name: &str) -> docker::Container {
        docker::Container {
            state: state.into(),
            local_folder: folder.into(),
            config_file: format!("{folder}/.devcontainer/devcontainer.json"),
            name: name.into(),
        }
    }

    #[test]
    fn prefers_running_container_in_compose_project() {
        let cs = vec![
            container("exited", "/p/api", "api-db-1"),
            container("running", "/p/api", "api-devcontainer-1"),
        ];
        let m = best_match(&cs, Path::new("/p/api")).unwrap();
        assert_eq!(m.name, "api-devcontainer-1");
    }

    #[test]
    fn falls_back_to_stopped_container() {
        let cs = vec![container("exited", "/p/api", "api-db-1")];
        let m = best_match(&cs, Path::new("/p/api")).unwrap();
        assert_eq!(m.name, "api-db-1");
        assert!(!m.is_running());
    }

    #[test]
    fn verb_defaults_to_start_and_ignores_flags() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(verb(&args(&[])), "start");
        assert_eq!(verb(&args(&["--container"])), "start");
        assert_eq!(verb(&args(&["stop"])), "stop");
        assert_eq!(verb(&args(&["--container", "status"])), "status");
        assert_eq!(verb(&args(&["status", "--container"])), "status");
    }

    #[test]
    fn no_match_for_other_project() {
        let cs = vec![container("running", "/p/api", "api-1")];
        assert!(best_match(&cs, Path::new("/p/web")).is_none());
    }
}
