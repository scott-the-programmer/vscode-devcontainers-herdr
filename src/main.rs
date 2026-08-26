mod discover;
mod docker;
mod exec;
mod forward;
mod report;
mod settings;
mod supervise;

use std::path::{Path, PathBuf};

/// What we report to herdr for one pane's project.
#[derive(Debug, PartialEq, serde::Serialize)]
pub(crate) struct Status {
    /// "running" | "stopped" | "none" | "error"
    pub(crate) status: &'static str,
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
  exec [--agent <name>] <command> [args...]
                                       run a command in the container as this pane's agent
                                       (a recognised agent name in <command> claims the
                                       pane; --agent <name> overrides it for any command)
  bridge  <start|stop|status|serve>   host: publish herdr's socket on the loopback port
  relay   <start|stop|status|serve>   container: present that port as a unix socket
          [--container]               ... drive the container's relay from the host
  --version                           print the version this binary was built from

See .devcontainer/README.md § \"Seeing the container's Claude session in herdr\".";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        // Both entry points do the same detection today; herdr decides when to
        // call us. `hook --all` re-detects for the focused workspace, which is
        // still just "the cwd herdr gave us" — but it reports to the
        // workspace rather than a pane, since `workspace.focused` has no
        // single pane in view.
        Some("refresh" | "hook") => print_status(args.iter().any(|a| a == "--all")),
        Some("exec") => exec::run(&args[1..]),
        Some("bridge") => bridge(&args[1..]),
        Some("relay") => relay(&args[1..]),
        // What build.sh compares against after a download: the same string on
        // both sides of the install, so a stale or wrong-arch asset can't land
        // in bin/.
        Some("--version" | "-V" | "version") => {
            println!("{}", version_line());
            0
        }
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

/// What `build.sh` compares against after a download: the same string on both
/// sides of the install, so a stale or wrong-arch asset can't land in bin/.
fn version_line() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

fn print_status(workspace_wide: bool) -> i32 {
    let cwd = report::pane_cwd();
    let status = detect(&cwd);
    println!(
        "{}",
        serde_json::to_string(&status).expect("status serializes")
    );
    // Best-effort: no pane/workspace id, no HERDR_BIN_PATH, or a failed call
    // just means the badge stays stale — the JSON above is the real contract
    // and is unaffected either way.
    report::publish(&status, workspace_wide);
    0
}

/// The host end of the agent-state bridge. `serve` is the forwarder itself;
/// the other verbs supervise it.
fn bridge(args: &[String]) -> i32 {
    let port = settings::port();
    let bind = settings::bridge_bind();
    let ready = || forward::tcp_answers(&bind, port);
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
            Ok(socket) => serve_bridge(&bind, port, &socket),
            Err(msg) => say(Err(msg)),
        },
        other => {
            eprintln!("{USAGE}\n\nunknown bridge verb: {other:?}");
            2
        }
    }
}

fn serve_bridge(bind: &str, port: u16, socket: &Path) -> i32 {
    match forward::bind_tcp(bind, port) {
        Ok(listener) => {
            println!("bridge: {bind}:{port} -> {}", socket.display());
            if let Err(e) = forward::serve_tcp_to_unix(listener, socket) {
                eprintln!("bridge: {e}");
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("bridge: {bind}:{port}: {e}");
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
            let matched = docker::container_for(&containers, &found.project_root);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_is_the_string_build_sh_matches() {
        assert_eq!(
            version_line(),
            format!("herdr-devcontainer-status {}", env!("CARGO_PKG_VERSION"))
        );
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
}
