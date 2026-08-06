//! Run a command in this repo's devcontainer as the calling herdr pane's agent.
//!
//! ```text
//! herdr-devcontainer-status exec claude
//! herdr-devcontainer-status exec bash
//! ```
//!
//! Four things have to line up, and none of them do by default:
//!
//!   1. `HERDR_ENV` / `HERDR_PANE_ID` / `HERDR_SOCKET_PATH` must be set in the
//!      container. herdr sets them in the pane's shell on the host; nothing
//!      carries them across `devcontainer exec`, and the hook exits 0 on the
//!      first missing one. `--remote-env` passes them per-exec, which is also why
//!      they can't live in `devcontainer.json`: `containerEnv` is fixed at create
//!      time, and the pane id differs per pane.
//!   2. `HERDR_SOCKET_PATH` must name a socket that exists in the container — the
//!      relay's, not the host path herdr set.
//!   3. Something must answer on the other end — the host bridge.
//!   4. herdr must recognise the pane as running an agent at all, which it
//!      decides by argv0, not by the reported session. See `spoof_argv0`.
//!
//! Kept on the devcontainer CLI rather than a raw `docker exec`: the CLI does
//! `userEnvProbe: loginInteractiveShell`, so a container shell here has the same
//! environment as one opened from VS Code, and `exec bash` and `exec claude` stay
//! on one mechanism.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::settings;
use crate::supervise;

/// Set on the re-exec so the spoofed process doesn't spoof itself again.
const ARGV0_GUARD: &str = "HERDR_EXEC_ARGV0";

/// The only agent this repo's container runs, and the argv0 herdr matches on.
const DEFAULT_AGENT: &str = "claude";

/// Runs in the container in one `devcontainer exec` round trip. The relay is this
/// same binary built for linux, and `target/` is a named volume the host cannot
/// see — so "is it built yet?" has to be asked in here, not on the host. Round
/// trips are what cost: the CLI starts node each time.
const RELAY_BOOTSTRAP: &str = r#"set -eu
bin=target/release/herdr-devcontainer-status
# Always build, rather than building only when the file is missing: after any
# change to this crate what's in there is last session's binary, which may not
# have the subcommand we're about to run. Quiet, because in the steady state this
# is a no-op — but say something before the one build that takes a minute.
[ -x "$bin" ] || echo "relay: building the container-side relay (first run only)" >&2
cargo build --release --locked --quiet
exec "$bin" relay "$1"
"#;

pub fn run(args: &[String]) -> i32 {
    let (claim, args) = split_agent_flag(args);
    if args.is_empty() {
        eprintln!("usage: herdr-devcontainer-status exec [--agent] <command> [args...]");
        return 2;
    }

    // The workspace comes from discovery, not from this binary's location: herdr
    // links the plugin from its own directory, so argv[0] says nothing about
    // which project the pane is in.
    let Some(root) = workspace_root() else {
        eprintln!("exec: no devcontainer configuration above the pane's cwd");
        return 1;
    };

    // Same problem build.sh has with cargo: herdr starts a pane's command without
    // an interactive shell, so PATH is whatever .zprofile left behind — and the
    // npm global bin that holds `devcontainer` is usually only added by .zshrc.
    let Some(cli) = resolve_cli() else {
        eprintln!("exec: devcontainer CLI not found on PATH.");
        eprintln!("install it with:  npm install -g @devcontainers/cli");
        return 127;
    };

    let pane = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|p| !p.is_empty());
    let Some(pane) = pane else {
        // Not in a herdr pane (or herdr's shell integration isn't active): there
        // is no pane to report to, so skip the relay work and run the command.
        eprintln!("exec: HERDR_PANE_ID unset — running without agent reporting");
        return wait(devcontainer(&cli, &root, &[], args));
    };

    spoof_argv0(args, claim);

    // Non-fatal, both of them: a missing bridge or relay costs agent state in
    // herdr, not the session.
    report(supervise::bridge().start(&|| crate::forward::tcp_answers(settings::port())));
    report(container_relay(&cli, &root, "start"));

    // No exec: this process is the pane's `claude` for detection purposes, so it
    // has to outlive the call. Claude puts the tty in raw mode, so Ctrl-C reaches
    // it as a keypress rather than a signal to this process group — no signal
    // plumbing here.
    wait(devcontainer(&cli, &root, &session_env(&pane), args))
}

/// herdr decides *which* agent a pane is running by scanning the pane's
/// foreground process group for a known argv0 ("claude", "codex", …). Reporting a
/// session over the socket is not enough on its own: without a match here the
/// pane stays `agent=none status=unknown`, and the output rules that produce
/// idle/working never run. Everything the host can see in this pane is the
/// devcontainer CLI under node, so put one process named `claude` in the group.
///
/// It has to be a re-exec of ourselves rather than a spoofed child process: the
/// name has to sit on a process that stays alive as the *parent* of the container
/// command, and the devcontainer CLI is a `/bin/sh` script that execs node, so a
/// spoof applied to it would be lost.
///
/// Returns when the spoof doesn't apply; a failed exec is downgraded to a
/// warning, since an undetected session still beats no session.
fn spoof_argv0(args: &[String], claim: bool) {
    if std::env::var_os(ARGV0_GUARD).is_some() {
        return;
    }
    let Some(name) = claimed_argv0(&args[0], claim) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let err = Command::new(exe)
        .arg0(name)
        .arg("exec")
        .args(args)
        .env(ARGV0_GUARD, "done")
        .exec();
    eprintln!("exec: argv0 re-exec failed ({err}) — herdr won't see an agent here");
}

/// Which argv0 to claim, if any.
///
/// Normally it comes from the command: `exec claude` claims the pane, and
/// anything else leaves it alone. `--agent` claims it regardless, which is what
/// an interactive shell needs — herdr only ever sees the *host* side of the pane,
/// so a `claude` started later inside the container changes nothing it can
/// observe. The claim has to be in place before the shell is.
fn claimed_argv0(command: &str, claim: bool) -> Option<&'static str> {
    if claim {
        return Some(DEFAULT_AGENT);
    }
    match Path::new(command).file_name()?.to_str()? {
        "claude" => Some(DEFAULT_AGENT),
        _ => None,
    }
}

/// Strip a leading `--agent`, which claims the pane whatever we end up running.
fn split_agent_flag(args: &[String]) -> (bool, &[String]) {
    match args.split_first() {
        Some((flag, rest)) if flag == "--agent" => (true, rest),
        _ => (false, args),
    }
}

/// The variables the hook needs; it exits 0 on the first one missing.
fn session_env(pane: &str) -> Vec<(&'static str, String)> {
    vec![
        ("HERDR_ENV", "1".to_string()),
        ("HERDR_PANE_ID", pane.to_string()),
        ("HERDR_SOCKET_PATH", settings::CONTAINER_SOCKET.to_string()),
        ("HERDR_RELAY_PORT", settings::port().to_string()),
    ]
}

/// Build `devcontainer exec --workspace-folder <root> [--remote-env K=V …] cmd…`.
fn devcontainer(
    cli: &Path,
    root: &Path,
    env: &[(&'static str, String)],
    command: &[String],
) -> Command {
    let mut cmd = Command::new(cli);
    cmd.arg("exec").arg("--workspace-folder").arg(root);
    for (key, value) in env {
        cmd.arg("--remote-env").arg(format!("{key}={value}"));
    }
    cmd.args(command);
    cmd
}

/// Host side of `relay <verb> --container`: drive the container's relay through
/// the CLI. `run` calls it before each session — `start` is idempotent in there,
/// so it also recovers a relay that died since the last one.
pub fn relay_in_container(verb: &str) -> i32 {
    let Some(root) = workspace_root() else {
        eprintln!("exec: no devcontainer configuration above the pane's cwd");
        return 1;
    };
    let Some(cli) = resolve_cli() else {
        eprintln!("exec: devcontainer CLI not found on PATH.");
        return 127;
    };
    match container_relay(&cli, &root, verb) {
        Ok(_) => 0,
        Err(msg) => {
            eprintln!("{msg}");
            1
        }
    }
}

/// Run one relay verb in the container, passing the socket path and port the host
/// side advertises so the two ends can't disagree about either.
fn container_relay(cli: &Path, root: &Path, verb: &str) -> Result<String, String> {
    let env = vec![
        ("HERDR_SOCKET_PATH", settings::CONTAINER_SOCKET.to_string()),
        ("HERDR_RELAY_PORT", settings::port().to_string()),
        ("HERDR_RELAY_HOST", settings::relay_host()),
    ];
    let bootstrap = [
        "sh".to_string(),
        "-c".to_string(),
        RELAY_BOOTSTRAP.to_string(),
        "relay-bootstrap".to_string(),
        verb.to_string(),
    ];
    match devcontainer(cli, root, &env, &bootstrap).status() {
        Ok(s) if s.success() => Ok(String::new()),
        Ok(_) => Err("exec: container relay unavailable — agent state won't reach herdr".into()),
        Err(e) => Err(format!("exec: cannot run the devcontainer CLI: {e}")),
    }
}

/// Forward one of the relay steps' outcomes without failing the session. The
/// success messages are the forwarders' own, already printed by their `start`.
fn report(outcome: Result<String, String>) {
    match outcome {
        Ok(msg) if !msg.is_empty() => eprintln!("{msg}"),
        Ok(_) => {}
        Err(msg) => eprintln!("{msg}"),
    }
}

/// Run to completion and hand back its exit code. A signalled child reports 1:
/// nothing downstream distinguishes it, and the pane is gone either way.
fn wait(mut cmd: Command) -> i32 {
    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("exec: {e}");
            1
        }
    }
}

fn workspace_root() -> Option<PathBuf> {
    let cwd = std::env::var_os("HERDR_PANE_CWD")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    crate::discover::find(&cwd).map(|d| d.project_root)
}

fn resolve_cli() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    which_in("devcontainer", std::env::split_paths(&path)).or_else(|| {
        cli_candidates(&settings::home())
            .into_iter()
            .find(|p| is_executable(p))
    })
}

/// Where `npm install -g` and homebrew put it. `~/.local/bin/devcontainer` is the
/// one on this host.
fn cli_candidates(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/bin/devcontainer"),
        home.join(".npm-global/bin/devcontainer"),
        PathBuf::from("/opt/homebrew/bin/devcontainer"),
        PathBuf::from("/usr/local/bin/devcontainer"),
    ]
}

fn which_in(name: &str, dirs: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.map(|dir| dir.join(name)).find(|p| is_executable(p))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_claude_claims_the_pane_by_command_name() {
        assert_eq!(claimed_argv0("claude", false), Some("claude"));
        assert_eq!(
            claimed_argv0("/usr/local/bin/claude", false),
            Some("claude")
        );
        assert_eq!(claimed_argv0("bash", false), None);
        assert_eq!(claimed_argv0("/bin/zsh", false), None);
        assert_eq!(claimed_argv0("", false), None);
    }

    #[test]
    fn the_agent_flag_claims_the_pane_for_any_command() {
        // What `make shell` needs: the claim is in place before the shell is,
        // because a `claude` started inside the container is invisible to herdr.
        assert_eq!(claimed_argv0("bash", true), Some("claude"));
        assert_eq!(claimed_argv0("/bin/zsh", true), Some("claude"));
    }

    #[test]
    fn agent_flag_is_stripped_from_the_container_command() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let shell = args(&["--agent", "bash"]);
        assert_eq!(split_agent_flag(&shell), (true, &shell[1..]));

        let session = args(&["claude", "--continue"]);
        assert_eq!(split_agent_flag(&session), (false, &session[..]));

        // Only leading, so it can't swallow a flag meant for the command itself.
        let passthrough = args(&["claude", "--agent"]);
        assert_eq!(split_agent_flag(&passthrough), (false, &passthrough[..]));
    }

    #[test]
    fn session_env_carries_everything_the_hook_requires() {
        let env = session_env("w9:p3");
        let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"HERDR_ENV"));
        assert!(keys.contains(&"HERDR_PANE_ID"));
        assert!(keys.contains(&"HERDR_SOCKET_PATH"));
        let sock = env.iter().find(|(k, _)| *k == "HERDR_SOCKET_PATH").unwrap();
        // The container's socket, never the host's.
        assert_eq!(sock.1, settings::CONTAINER_SOCKET);
        let pane = env.iter().find(|(k, _)| *k == "HERDR_PANE_ID").unwrap();
        assert_eq!(pane.1, "w9:p3");
    }

    #[test]
    fn builds_a_remote_env_flag_per_variable() {
        let cmd = devcontainer(
            Path::new("/bin/devcontainer"),
            Path::new("/p/api"),
            &session_env("w9:p3"),
            &["claude".to_string(), "--continue".to_string()],
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "exec");
        assert_eq!(args[1..3], ["--workspace-folder", "/p/api"]);
        assert_eq!(args.iter().filter(|a| *a == "--remote-env").count(), 4);
        assert!(args.contains(&"HERDR_ENV=1".to_string()));
        assert!(args.contains(&"HERDR_PANE_ID=w9:p3".to_string()));
        // The command stays last and intact, arguments included.
        assert_eq!(args[args.len() - 2..], ["claude", "--continue"]);
    }

    #[test]
    fn no_remote_env_flags_without_a_pane() {
        let cmd = devcontainer(
            Path::new("/bin/devcontainer"),
            Path::new("/p/api"),
            &[],
            &["bash".to_string()],
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["exec", "--workspace-folder", "/p/api", "bash"]);
    }

    #[test]
    fn which_in_skips_non_executables_and_missing_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("devcontainer");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();

        let dirs = || vec![dir.path().join("gone"), empty.clone(), bin.clone()].into_iter();
        // Present but not executable yet.
        assert_eq!(which_in("devcontainer", dirs()), None);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(which_in("devcontainer", dirs()), Some(script));
    }

    #[test]
    fn cli_candidates_are_absolute_and_include_the_npm_global_bin() {
        let c = cli_candidates(Path::new("/Users/x"));
        assert!(c.iter().all(|p| p.is_absolute()));
        assert!(c.contains(&PathBuf::from("/Users/x/.local/bin/devcontainer")));
    }

    #[test]
    fn relay_bootstrap_builds_then_runs_the_verb_it_is_given() {
        // Exercised as `sh -c <script> relay-bootstrap start`, so $1 is the verb.
        assert!(RELAY_BOOTSTRAP.contains("cargo build --release --locked"));
        assert!(RELAY_BOOTSTRAP.contains(r#"exec "$bin" relay "$1""#));
        // Unconditional: a stale binary is the failure mode a file check misses.
        assert!(!RELAY_BOOTSTRAP.contains("if [ ! -x"));
    }
}
