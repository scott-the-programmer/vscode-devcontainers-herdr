//! Run a command in this repo's devcontainer as the calling herdr pane's agent.
//!
//! ```text
//! herdr-devcontainer-status exec opencode
//! herdr-devcontainer-status exec --agent opencode bash
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
//! environment as one opened from VS Code, and every agent stays on one
//! mechanism whether it's named on the command line or claimed with `--agent`.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::docker;
use crate::settings;
use crate::supervise;

/// Set on the re-exec so the spoofed process doesn't spoof itself again.
const ARGV0_GUARD: &str = "HERDR_EXEC_ARGV0";

/// Agent names herdr's process-scan detector recognises, canonical label first
/// in each group. Mirrors `herdr/src/detect/mod.rs`'s alias table — herdr
/// exposes no runtime query for this list, so it has to be a static copy here,
/// and it will drift as herdr adds agents. An unrecognised `--agent` name is
/// still accepted and forwarded verbatim (see `parse_args`), so a new herdr
/// agent works here without a release.
const AGENTS: &[&[&str]] = &[
    &["pi"],
    &["claude", "claude-code"],
    &["codex"],
    &["gemini"],
    &["cursor", "cursor-agent"],
    &["devin", "devin-cli"],
    &["agy", "antigravity", "antigravity-cli"],
    &["cline"],
    &["omp"],
    &["mastracode", "mastra-code"],
    &["opencode", "opencode2", "open-code"],
    &["copilot", "github-copilot", "ghcs"],
    &["kimi", "kimi-code"],
    &["kiro", "kiro-cli"],
    &["droid"],
    &["amp", "amp-local"],
    &["grok", "grok-build"],
    &["hermes", "hermes-agent"],
    &["kilo", "kilo-code"],
    &["qodercli", "qoderclicn", "qoder", "qodercn"],
    &["qwen", "qwen-code"],
    &["maki"],
];

/// Lowercase, trim, and strip one trailing script/launcher extension — mirrors
/// herdr's own normalization (`herdr/src/detect/mod.rs`) so this table can't
/// disagree with the matcher it's feeding.
fn normalize(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    for suffix in [".exe", ".cmd", ".bat", ".ps1", ".js"] {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    lower
}

/// The canonical agent label `name` resolves to, if any. `name` may already be
/// a canonical label, an alias (`cursor-agent`), or carry an extension
/// (`Codex.exe`) — a leading path is not stripped here; callers that pass a
/// whole command path extract the basename first (see `claimed_argv0`).
fn canonical_agent(name: &str) -> Option<&'static str> {
    let needle = normalize(name);
    AGENTS
        .iter()
        .find(|group| group.contains(&needle.as_str()))
        .map(|group| group[0])
}

/// This binary's own version — what a container-side candidate has to report
/// from `--version` before we'll trust it, and the suffix on the path we
/// `docker cp` a bundled binary to. Same string `build.sh` compares after a
/// download, so a stale or wrong-arch relay can't be picked up silently.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One `devcontainer exec` round trip: resolve the container's own `$HOME`
/// (killing the old hard-coded `/home/vscode` assumption — any `remoteUser`
/// works), find a relay binary that reports our exact version, and either run
/// it or report which CPU architecture needs one. `container_relay` docker-cps
/// one in on the first miss and retries once.
///
/// Lookup order is deliberately open to a future devcontainer feature that
/// installs `herdr-devcontainer-status` onto `PATH` itself: candidate 2 finds
/// that with no plugin change at all, and nothing is pushed when it does.
const RELAY_BOOTSTRAP: &str = r#"set -eu
ver="$1"; verb="$2"
sock="${HOME:-/root}/.herdr/herdr.sock"
mkdir -p "$(dirname "$sock")" 2>/dev/null || true
echo "herdr-socket=$sock"
for cand in ${HERDR_RELAY_BIN:-} "$(command -v herdr-devcontainer-status 2>/dev/null || true)" "/tmp/herdr-devcontainer-status-$ver"; do
  [ -n "$cand" ] && [ -x "$cand" ] || continue
  [ "$("$cand" --version 2>/dev/null)" = "herdr-devcontainer-status $ver" ] || continue
  HERDR_SOCKET_PATH="$sock" exec "$cand" relay "$verb"
done
echo "herdr-need=$(uname -m)"
exit 42
"#;

/// Fallback for a source install (or a fork with no release assets, so
/// `build.sh` never bundled a linux binary): build in the container, but only
/// when it is unmistakably *our own* crate, never an unrelated project's —
/// this is what used to run unconditionally and broke on any other project's
/// container.
const SOURCE_FALLBACK: &str = r#"set -eu
verb="$1"
sock="${HOME:-/root}/.herdr/herdr.sock"
if [ ! -f Cargo.toml ] || ! grep -q '^name = "herdr-devcontainer-status"' Cargo.toml; then
  echo "exec: no bundled relay binary, and this container isn't the plugin's own crate" >&2
  exit 1
fi
mkdir -p "$(dirname "$sock")" 2>/dev/null || true
bin=target/release/herdr-devcontainer-status
[ -x "$bin" ] || echo "relay: building the container-side relay (first run only)" >&2
cargo build --release --locked --quiet
echo "herdr-socket=$sock"
HERDR_SOCKET_PATH="$sock" exec "$bin" relay "$verb"
"#;

pub fn run(args: &[String]) -> i32 {
    let (claim, args) = match parse_args(args) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    if args.is_empty() {
        eprintln!("usage: herdr-devcontainer-status exec [--agent <name>] <command> [args...]");
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

    if let Some(name) = claim.as_deref() {
        if canonical_agent(name).is_none() {
            eprintln!(
                "exec: unknown agent {name:?} — claiming it anyway; herdr will ignore it unless it recognises the name"
            );
        }
    }
    spoof_argv0(args, claim.as_deref());

    // Non-fatal, both of them: a missing bridge or relay costs agent state in
    // herdr, not the session. A relay failure still needs *some* socket path
    // to advertise, so the hook has a consistent (if unreachable) target to
    // fail against rather than none at all.
    report(supervise::bridge().start(&|| crate::forward::tcp_answers(settings::port())));
    let socket = container_relay(&cli, &root, "start").unwrap_or_else(|msg| {
        eprintln!("{msg}");
        settings::CONTAINER_SOCKET.to_string()
    });

    // No exec: this process is the pane's claimed agent for detection purposes,
    // so it has to outlive the call. Interactive agents put the tty in raw
    // mode, so Ctrl-C reaches them as a keypress rather than a signal to this
    // process group — no signal plumbing here.
    wait(devcontainer(
        &cli,
        &root,
        &session_env(&pane, &socket),
        args,
    ))
}

/// herdr decides *which* agent a pane is running by scanning the pane's
/// foreground process group for a known argv0 ("claude", "codex", …). Reporting a
/// session over the socket is not enough on its own: without a match here the
/// pane stays `agent=none status=unknown`, and the output rules that produce
/// idle/working never run. Everything the host can see in this pane is the
/// devcontainer CLI under node, so put one process named after the agent's
/// canonical label in the group.
///
/// It has to be a re-exec of ourselves rather than a spoofed child process: the
/// name has to sit on a process that stays alive as the *parent* of the container
/// command, and the devcontainer CLI is a `/bin/sh` script that execs node, so a
/// spoof applied to it would be lost.
///
/// Returns when the spoof doesn't apply; a failed exec is downgraded to a
/// warning, since an undetected session still beats no session.
fn spoof_argv0(args: &[String], claim: Option<&str>) {
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
/// Normally it comes from the command: `exec opencode` claims the pane as
/// `opencode`, and a command that names no recognised agent (`bash`, `npm`, …)
/// leaves it alone. `--agent <name>` overrides the command entirely, which is
/// what an interactive shell needs — herdr only ever sees the *host* side of
/// the pane, so an agent started later inside the container changes nothing it
/// can observe. The claim has to be in place before the shell is.
fn claimed_argv0(command: &str, claim: Option<&str>) -> Option<String> {
    if let Some(name) = claim {
        return Some(name.to_string());
    }
    let file_name = Path::new(command).file_name()?.to_str()?;
    canonical_agent(file_name).map(str::to_string)
}

/// Split a leading `--agent <name>` off the front of `args`, returning the
/// agent to claim (resolved to its canonical label when recognised, forwarded
/// verbatim otherwise — see `AGENTS`) and the remaining command. Only a
/// *leading* `--agent` is recognised, so one meant for the command itself
/// (`exec claude --agent`) passes through untouched.
///
/// `--agent` used to be a bare flag meaning "claim as claude"; the old
/// `exec --agent bash` form now reads `bash` as the agent name and has no
/// command left, so it errors instead of silently doing the wrong thing.
fn parse_args(args: &[String]) -> Result<(Option<String>, &[String]), String> {
    let Some((flag, rest)) = args.split_first() else {
        return Ok((None, args));
    };
    if flag != "--agent" {
        return Ok((None, args));
    }
    let Some((name, command)) = rest.split_first() else {
        return Err("exec: --agent needs an agent name, e.g. --agent claude bash".to_string());
    };
    if command.is_empty() {
        return Err(
            "exec: --agent takes an agent name now, and still needs a command\n  \
             exec --agent claude bash   claims the pane and runs bash\n  \
             exec bash                  runs it unclaimed"
                .to_string(),
        );
    }
    let claim = canonical_agent(name)
        .map(str::to_string)
        .unwrap_or_else(|| name.clone());
    Ok((Some(claim), command))
}

/// The variables the hook needs; it exits 0 on the first one missing. `socket`
/// is whatever `container_relay` resolved the container's own home to be —
/// never assume `/home/vscode`, since `remoteUser` isn't always `vscode`.
fn session_env(pane: &str, socket: &str) -> Vec<(&'static str, String)> {
    vec![
        ("HERDR_ENV", "1".to_string()),
        ("HERDR_PANE_ID", pane.to_string()),
        ("HERDR_SOCKET_PATH", socket.to_string()),
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

/// What `RELAY_BOOTSTRAP` found, parsed from its stdout.
enum Probe {
    /// A candidate ran; this is the socket it bound (`HOME/.herdr/herdr.sock`
    /// resolved *inside* the container).
    Ready(String),
    /// No candidate matched our version; the container's `uname -m`, so the
    /// caller knows which musl triple to push.
    NeedsBinary(String),
}

/// Resolve a working container-side relay and run `verb` against it, pushing
/// a bundled musl binary in via `docker cp` on the first miss and retrying
/// once. Returns the `HERDR_SOCKET_PATH` the relay ended up bound to.
fn container_relay(cli: &Path, root: &Path, verb: &str) -> Result<String, String> {
    let arch = match probe_relay(cli, root, verb)? {
        Probe::Ready(socket) => return Ok(socket),
        Probe::NeedsBinary(arch) => arch,
    };
    if let Err(push_err) = push_relay_binary(root, &arch) {
        // No bundled binary for this arch (or the push itself failed): the
        // only other way to get a relay running is to build it in place, and
        // only when the container is unmistakably our own crate.
        return probe_source_fallback(cli, root, verb).map_err(|_| push_err);
    }
    match probe_relay(cli, root, verb)? {
        Probe::Ready(socket) => Ok(socket),
        Probe::NeedsBinary(arch) => Err(format!(
            "exec: pushed a relay binary but the container ({arch}) still can't run it"
        )),
    }
}

/// Run `RELAY_BOOTSTRAP` and interpret what it found.
fn probe_relay(cli: &Path, root: &Path, verb: &str) -> Result<Probe, String> {
    let script = [
        "sh".to_string(),
        "-c".to_string(),
        RELAY_BOOTSTRAP.to_string(),
        "relay-bootstrap".to_string(),
        VERSION.to_string(),
        verb.to_string(),
    ];
    let out = run_bootstrap(cli, root, &script)?;
    let (socket, need) = parse_bootstrap_output(&out.stdout);
    if let Some(arch) = need {
        return Ok(Probe::NeedsBinary(arch));
    }
    if out.success {
        socket
            .map(Probe::Ready)
            .ok_or_else(|| "exec: relay bootstrap produced no socket path".to_string())
    } else {
        Err("exec: container relay unavailable — agent state won't reach herdr".into())
    }
}

/// Run `SOURCE_FALLBACK` — only reached when no bundled binary could be
/// pushed in. Guarded by the script itself against building an unrelated
/// project.
fn probe_source_fallback(cli: &Path, root: &Path, verb: &str) -> Result<String, String> {
    let script = [
        "sh".to_string(),
        "-c".to_string(),
        SOURCE_FALLBACK.to_string(),
        "relay-source-fallback".to_string(),
        verb.to_string(),
    ];
    let out = run_bootstrap(cli, root, &script)?;
    if !out.success {
        return Err("exec: container relay unavailable — agent state won't reach herdr".into());
    }
    let (socket, _) = parse_bootstrap_output(&out.stdout);
    socket.ok_or_else(|| "exec: relay bootstrap produced no socket path".to_string())
}

/// One `devcontainer exec` round trip running a bootstrap script, with its
/// stdout captured for parsing and everything else — the relay's own
/// start/stop/status messages, cargo's build banner, stderr — forwarded live
/// so the session doesn't go quiet during the one build that takes a minute.
struct BootstrapOutput {
    success: bool,
    stdout: String,
}

fn run_bootstrap(cli: &Path, root: &Path, script: &[String]) -> Result<BootstrapOutput, String> {
    let env = vec![
        ("HERDR_RELAY_PORT", settings::port().to_string()),
        ("HERDR_RELAY_HOST", settings::relay_host()),
    ];
    let out = devcontainer(cli, root, &env, script)
        .output()
        .map_err(|e| format!("exec: cannot run the devcontainer CLI: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    for line in stdout.lines() {
        if !line.starts_with("herdr-socket=") && !line.starts_with("herdr-need=") {
            eprintln!("{line}");
        }
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.trim().is_empty() {
        eprint!("{stderr}");
    }
    Ok(BootstrapOutput {
        success: out.status.success(),
        stdout,
    })
}

/// Pull `herdr-socket=`/`herdr-need=` markers back out of a bootstrap
/// script's stdout — see `RELAY_BOOTSTRAP`/`SOURCE_FALLBACK`.
fn parse_bootstrap_output(stdout: &str) -> (Option<String>, Option<String>) {
    let mut socket = None;
    let mut need = None;
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("herdr-socket=") {
            socket = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("herdr-need=") {
            need = Some(v.to_string());
        }
    }
    (socket, need)
}

/// Push a bundled musl binary matching `arch` into the container's `/tmp` by
/// streaming it over `docker exec`'s stdin, then mark it executable. `/tmp` is
/// deliberately not persisted: a restarted container needs a restarted relay
/// anyway, and the next `exec` re-pushes automatically via `probe_relay` above.
///
/// Deliberately not `docker cp`: it fails with `no such device or address` on
/// containers that bind-mount a live Unix socket (e.g. docker-outside-of-docker's
/// `docker.sock`), since its tar machinery walks the whole changed-file set.
/// Streaming over `exec -i` stdin avoids that.
fn push_relay_binary(root: &Path, arch: &str) -> Result<(), String> {
    let triple = musl_triple(arch)
        .ok_or_else(|| format!("exec: no relay binary published for container arch {arch}"))?;
    let local = bundled_relay_binary(triple)?;
    let container = container_name(root)?;
    let dest = format!("/tmp/herdr-devcontainer-status-{VERSION}");

    let file = std::fs::File::open(&local)
        .map_err(|e| format!("exec: cannot open {}: {e}", local.display()))?;

    let write = Command::new("docker")
        .args([
            "exec",
            "-i",
            &container,
            "sh",
            "-c",
            &format!("cat > {dest} && chmod +x {dest}"),
        ])
        .stdin(Stdio::from(file))
        .status()
        .map_err(|e| format!("exec: docker exec: {e}"))?;
    if !write.success() {
        return Err(format!(
            "exec: pushing the relay binary to {container} failed"
        ));
    }
    Ok(())
}

/// Which container `root`'s devcontainer runs as — `docker::container_for`,
/// the same match (dogfood fallback included) `main::detect` uses for
/// status, so `exec` and the status hook can never disagree about which
/// container a project's pane means.
fn container_name(root: &Path) -> Result<String, String> {
    let containers = docker::list()?;
    docker::container_for(&containers, root)
        .map(|c| c.name.clone())
        .ok_or_else(|| format!("exec: no container labelled for {}", root.display()))
}

/// `build.sh` bundles both linux targets next to the host binary at
/// `bin/linux/<triple>/`, resolved relative to *this* executable rather than
/// `$HERDR_PLUGIN_ROOT` — unset when `make claude` runs the binary directly.
fn bundled_relay_binary(triple: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("exec: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "exec: current executable has no parent directory".to_string())?;
    let path = dir
        .join("linux")
        .join(triple)
        .join("herdr-devcontainer-status");
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "exec: no bundled relay binary at {} — reinstall with `herdr plugin install`, \
             or set HERDR_RELAY_BIN in the container to one you provide yourself",
            path.display()
        ))
    }
}

/// The two targets `build.yml`'s release matrix publishes for linux — static
/// musl, so they run in any container regardless of distro or libc.
fn musl_triple(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" | "amd64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Some("aarch64-unknown-linux-musl"),
        _ => None,
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
    fn a_command_that_names_an_agent_claims_the_pane() {
        assert_eq!(claimed_argv0("claude", None), Some("claude".to_string()));
        assert_eq!(
            claimed_argv0("/usr/local/bin/claude", None),
            Some("claude".to_string())
        );
        assert_eq!(
            claimed_argv0("opencode", None),
            Some("opencode".to_string())
        );
        assert_eq!(claimed_argv0("bash", None), None);
        assert_eq!(claimed_argv0("/bin/zsh", None), None);
        assert_eq!(claimed_argv0("", None), None);
    }

    #[test]
    fn the_agent_flag_claims_the_pane_for_any_command() {
        // What `make shell` needs: the claim is in place before the shell is,
        // because an agent started inside the container is invisible to herdr.
        assert_eq!(
            claimed_argv0("bash", Some("opencode")),
            Some("opencode".to_string())
        );
        assert_eq!(
            claimed_argv0("/bin/zsh", Some("claude")),
            Some("claude".to_string())
        );
    }

    #[test]
    fn claimed_argv0_resolves_aliases_to_their_canonical_label() {
        assert_eq!(
            claimed_argv0("cursor-agent", None),
            Some("cursor".to_string())
        );
        assert_eq!(
            claimed_argv0("claude-code", None),
            Some("claude".to_string())
        );
        assert_eq!(claimed_argv0("ghcs", None), Some("copilot".to_string()));
    }

    #[test]
    fn canonical_agent_strips_one_trailing_extension_and_ignores_case() {
        assert_eq!(canonical_agent("claude.cmd"), Some("claude"));
        assert_eq!(canonical_agent("opencode.js"), Some("opencode"));
        // Only one suffix is stripped, so a double extension still fails to match.
        assert_eq!(canonical_agent("opencode.js.exe"), None);
        assert_eq!(canonical_agent("CLAUDE"), Some("claude"));
        // A leading path isn't stripped here — callers extract the basename first.
        assert_eq!(canonical_agent("/usr/local/bin/Codex"), None);
    }

    #[test]
    fn every_canonical_label_resolves_to_itself_with_no_duplicate_aliases() {
        let mut seen = std::collections::HashSet::new();
        for group in AGENTS {
            assert_eq!(canonical_agent(group[0]), Some(group[0]));
            for alias in *group {
                assert!(
                    seen.insert(*alias),
                    "duplicate alias across groups: {alias}"
                );
            }
        }
    }

    #[test]
    fn parse_args_claims_a_recognised_or_unknown_agent_name() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let recognised = args(&["--agent", "opencode", "bash"]);
        let (claim, command) = parse_args(&recognised).unwrap();
        assert_eq!(claim.as_deref(), Some("opencode"));
        assert_eq!(command, &recognised[2..]);

        // Forwarded verbatim so a herdr agent this table doesn't know about yet
        // still works; `run` is what warns about it.
        let unknown = args(&["--agent", "hypothetical", "bash"]);
        let (claim, _) = parse_args(&unknown).unwrap();
        assert_eq!(claim.as_deref(), Some("hypothetical"));

        // An alias resolves to its canonical label.
        let alias = args(&["--agent", "cursor-agent", "bash"]);
        let (claim, _) = parse_args(&alias).unwrap();
        assert_eq!(claim.as_deref(), Some("cursor"));
    }

    #[test]
    fn parse_args_leaves_a_command_with_no_leading_agent_flag_untouched() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let session = args(&["claude", "--continue"]);
        let (claim, command) = parse_args(&session).unwrap();
        assert_eq!(claim, None);
        assert_eq!(command, &session[..]);

        // Only leading, so it can't swallow a flag meant for the command itself.
        let passthrough = args(&["claude", "--agent"]);
        let (claim, command) = parse_args(&passthrough).unwrap();
        assert_eq!(claim, None);
        assert_eq!(command, &passthrough[..]);
    }

    #[test]
    fn parse_args_rejects_agent_flag_with_no_name_or_no_command() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert!(parse_args(&args(&["--agent"])).is_err());
        // The old bare-flag form: `--agent bash` now reads "bash" as the agent
        // name and has no command left, so it must error, not silently claim.
        assert!(parse_args(&args(&["--agent", "bash"])).is_err());
    }

    #[test]
    fn session_env_carries_everything_the_hook_requires() {
        let env = session_env("w9:p3", "/home/node/.herdr/herdr.sock");
        let keys: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"HERDR_ENV"));
        assert!(keys.contains(&"HERDR_PANE_ID"));
        assert!(keys.contains(&"HERDR_SOCKET_PATH"));
        let sock = env.iter().find(|(k, _)| *k == "HERDR_SOCKET_PATH").unwrap();
        // Whatever container_relay resolved, e.g. a non-vscode remoteUser —
        // never a hard-coded home directory.
        assert_eq!(sock.1, "/home/node/.herdr/herdr.sock");
        let pane = env.iter().find(|(k, _)| *k == "HERDR_PANE_ID").unwrap();
        assert_eq!(pane.1, "w9:p3");
    }

    #[test]
    fn builds_a_remote_env_flag_per_variable() {
        let cmd = devcontainer(
            Path::new("/bin/devcontainer"),
            Path::new("/p/api"),
            &session_env("w9:p3", "/home/vscode/.herdr/herdr.sock"),
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
    fn relay_bootstrap_resolves_home_and_reports_what_it_needs() {
        // Exercised as `sh -c <script> relay-bootstrap <version> <verb>`.
        assert!(RELAY_BOOTSTRAP.contains(r#"sock="${HOME:-/root}/.herdr/herdr.sock""#));
        assert!(RELAY_BOOTSTRAP.contains(r#"exec "$cand" relay "$verb""#));
        assert!(RELAY_BOOTSTRAP.contains("herdr-need=$(uname -m)"));
        assert!(RELAY_BOOTSTRAP.contains("exit 42"));
        // No unconditional cargo build left in the fast path — that's what
        // broke on any project that isn't this crate.
        assert!(!RELAY_BOOTSTRAP.contains("cargo build"));
    }

    #[test]
    fn source_fallback_refuses_an_unrelated_project() {
        assert!(SOURCE_FALLBACK.contains(r#"grep -q '^name = "herdr-devcontainer-status"'"#));
        assert!(SOURCE_FALLBACK.contains("cargo build --release --locked"));
    }

    #[test]
    fn musl_triple_maps_known_arches_and_rejects_the_rest() {
        assert_eq!(musl_triple("x86_64"), Some("x86_64-unknown-linux-musl"));
        assert_eq!(musl_triple("amd64"), Some("x86_64-unknown-linux-musl"));
        assert_eq!(musl_triple("aarch64"), Some("aarch64-unknown-linux-musl"));
        assert_eq!(musl_triple("arm64"), Some("aarch64-unknown-linux-musl"));
        assert_eq!(musl_triple("armv7l"), None);
        assert_eq!(musl_triple(""), None);
    }

    #[test]
    fn parses_the_resolved_socket_when_a_candidate_ran() {
        let stdout = "herdr-socket=/home/node/.herdr/herdr.sock\nrelay: listening on ...\n";
        assert_eq!(
            parse_bootstrap_output(stdout),
            (Some("/home/node/.herdr/herdr.sock".to_string()), None)
        );
    }

    #[test]
    fn parses_the_needed_arch_when_no_candidate_matched() {
        let stdout = "herdr-socket=/root/.herdr/herdr.sock\nherdr-need=aarch64\n";
        assert_eq!(
            parse_bootstrap_output(stdout),
            (
                Some("/root/.herdr/herdr.sock".to_string()),
                Some("aarch64".to_string())
            )
        );
    }

    #[test]
    fn parses_nothing_from_unrelated_output() {
        assert_eq!(
            parse_bootstrap_output("relay: already listening\n"),
            (None, None)
        );
        assert_eq!(parse_bootstrap_output(""), (None, None));
    }
}
