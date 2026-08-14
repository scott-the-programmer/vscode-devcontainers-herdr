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
