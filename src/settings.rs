//! The handful of knobs the bridge, the relay and `exec` all have to agree on.

use std::path::PathBuf;

/// Loopback port the host bridge publishes herdr's socket on.
pub const DEFAULT_PORT: u16 = 47100;

/// Fallback only — not the path `exec` actually uses. `exec`'s bootstrap
/// script (`exec::RELAY_BOOTSTRAP`) resolves `$HOME` *inside* the target
/// container and passes the result as `HERDR_SOCKET_PATH` per exec, since
/// `remoteUser` isn't always `vscode`. This constant only matters for a
/// manual `relay serve`/`relay start` run with no `HERDR_SOCKET_PATH` in the
/// environment at all — e.g. this repo's own dogfood container, where the
/// user genuinely is `vscode`.
pub const CONTAINER_SOCKET: &str = "/home/vscode/.herdr/herdr.sock";

/// Docker Desktop forwards this to the host's 127.0.0.1.
pub const RELAY_HOST: &str = "host.docker.internal";

/// Address the host bridge binds. Loopback by default and deliberately: the
/// herdr control socket is unauthenticated, so whatever can reach this port can
/// drive herdr. See `HERDR_BRIDGE_BIND` in the README before widening it.
pub const DEFAULT_BIND: &str = "127.0.0.1";

pub fn port() -> u16 {
    std::env::var("HERDR_RELAY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

pub fn relay_host() -> String {
    std::env::var("HERDR_RELAY_HOST")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| RELAY_HOST.to_string())
}

/// Mirror of `relay_host` for the host end: where the bridge listens, rather
/// than where the container dials. Native Linux Docker needs both moved off
/// their Docker Desktop defaults, and moving only one is worse than moving
/// neither — see the `forward` module docs.
pub fn bridge_bind() -> String {
    std::env::var("HERDR_BRIDGE_BIND")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| DEFAULT_BIND.to_string())
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The herdr API socket on the host — herdr exports `HERDR_SOCKET_PATH` into
/// each pane, and the default is where the server puts it.
pub fn host_socket() -> PathBuf {
    crate::forward::socket_path(|| home().join(".config/herdr/herdr.sock"))
}

/// Where the relay listens, read from the same variable: "the herdr socket as
/// seen from here". In the container that's what `exec` passed in.
pub fn relay_socket() -> PathBuf {
    crate::forward::socket_path(|| PathBuf::from(CONTAINER_SOCKET))
}

/// Pidfiles and logs for the host bridge. `TMPDIR` on macOS is per-user.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env vars are process-global, so assert on the default branch only
    /// when the caller's environment hasn't already set one — same shape as
    /// `forward::tests::socket_path_prefers_the_environment`.
    #[test]
    fn bridge_bind_defaults_to_loopback() {
        match std::env::var("HERDR_BRIDGE_BIND") {
            Ok(v) if !v.is_empty() => assert_eq!(bridge_bind(), v),
            _ => assert_eq!(bridge_bind(), DEFAULT_BIND),
        }
    }

    /// The two ends are separate knobs on purpose: setting one without the
    /// other is the misconfiguration this feature exists to make explicable.
    #[test]
    fn the_two_ends_default_independently() {
        if std::env::var_os("HERDR_RELAY_HOST").is_none() {
            assert_eq!(relay_host(), RELAY_HOST);
        }
        if std::env::var_os("HERDR_BRIDGE_BIND").is_none() {
            assert_eq!(bridge_bind(), DEFAULT_BIND);
        }
    }
}
