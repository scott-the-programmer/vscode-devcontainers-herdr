//! Reports the plugin's own findings back to herdr: the focused pane's real
//! cwd (herdr's injected env doesn't hand us one directly — see `pane_cwd`),
//! and the devcontainer status as pane/workspace metadata (`herdr pane
//! report-metadata` / `herdr workspace report-metadata`), so `hook`/`refresh`
//! produce more than a JSON line in the plugin log.
//!
//! Everything here is best-effort: a missing id, a missing `$HERDR_BIN_PATH`,
//! or a failed call just leaves the badge stale. `print_status`'s JSON on
//! stdout is the real contract in `main.rs` and never depends on any of this.

use std::path::PathBuf;
use std::process::Command;

use crate::Status;

const SOURCE: &str = "devcontainer-status";
const TOKEN: &str = "devcontainer";

/// The directory herdr actually wants status for.
///
/// herdr does **not** set `HERDR_PANE_CWD` — it runs plugin commands with the
/// *plugin's own* directory as their working directory (herdr's plugin docs,
/// "Commands and environment"). Falling straight back to `current_dir()`
/// therefore silently resolves every pane to wherever this plugin is
/// installed — for a GitHub checkout of this very crate, a project with its
/// own `.devcontainer/`, i.e. exactly wrong. Ask herdr instead, cheapest
/// first:
///
///   1. `HERDR_PLUGIN_CONTEXT_JSON`, already in hand — no process spawned.
///   2. `$HERDR_BIN_PATH pane get <pane_id>`, herdr's own answer for the pane
///      id we do have.
///   3. `HERDR_PANE_CWD`/`current_dir()`, for a manual run outside herdr.
pub fn pane_cwd() -> PathBuf {
    std::env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .as_deref()
        .and_then(parse_context_cwd)
        .or_else(queried_cwd)
        .or_else(|| std::env::var_os("HERDR_PANE_CWD").map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Best-effort parse of `HERDR_PLUGIN_CONTEXT_JSON`. The plugin docs describe
/// its contents ("can include workspace, tab, focused pane, ... fields") but
/// not a schema, so this tries the field names herdr's own `pane get`/`pane
/// list` document (`cwd`, `foreground_cwd`) under the plausible nestings and
/// gives up cleanly on anything else — `pane_cwd`'s next fallback covers a
/// wrong guess here.
fn parse_context_cwd(raw: &str) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    ["pane", "focused_pane"]
        .into_iter()
        .find_map(|key| cwd_field(value.get(key)))
        .or_else(|| cwd_field(value.get("workspace")))
        .or_else(|| cwd_field(Some(&value)))
}

fn cwd_field(v: Option<&serde_json::Value>) -> Option<PathBuf> {
    let v = v?;
    v.get("foreground_cwd")
        .or_else(|| v.get("cwd"))
        .and_then(|c| c.as_str())
        .map(PathBuf::from)
}

/// `herdr pane get <pane_id>` via `$HERDR_BIN_PATH`, parsed the same way.
fn queried_cwd() -> Option<PathBuf> {
    let bin = std::env::var("HERDR_BIN_PATH").ok()?;
    let pane = std::env::var("HERDR_PANE_ID").ok()?;
    let out = Command::new(bin)
        .args(["pane", "get", &pane])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    cwd_field(Some(&value))
}

/// Push `status` onto the pane as display-only metadata — a `$devcontainer`
/// token a user can add to their sidebar rows (see the README). `hook --all`
/// (the `workspace.focused` event) has no single pane in view, so it reports
/// on the workspace instead; `--source` scopes both to this plugin so they
/// never collide with another source's tokens.
pub fn publish(status: &Status, workspace_wide: bool) {
    let Ok(bin) = std::env::var("HERDR_BIN_PATH") else {
        return;
    };
    let pane = std::env::var("HERDR_PANE_ID").ok();
    let workspace = std::env::var("HERDR_WORKSPACE_ID").ok();
    let Some(mut cmd) = report_command(
        &bin,
        workspace_wide,
        pane.as_deref(),
        workspace.as_deref(),
        status.status,
    ) else {
        return;
    };
    let _ = cmd.status();
}

/// Build the `herdr pane|workspace report-metadata … --token devcontainer=…`
/// invocation, or `None` when the id it needs isn't available.
fn report_command(
    bin: &str,
    workspace_wide: bool,
    pane: Option<&str>,
    workspace: Option<&str>,
    status: &str,
) -> Option<Command> {
    let (kind, id) = if workspace_wide {
        ("workspace", workspace?)
    } else {
        ("pane", pane?)
    };
    let mut cmd = Command::new(bin);
    cmd.args([kind, "report-metadata", id, "--source", SOURCE]);
    if status == "none" {
        cmd.args(["--clear-token", TOKEN]);
    } else {
        cmd.args(["--token", &format!("{TOKEN}={status}")]);
    }
    Some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cwd_from_a_nested_pane_object() {
        let raw = r#"{"pane": {"foreground_cwd": "/p/api", "cwd": "/p"}}"#;
        assert_eq!(parse_context_cwd(raw), Some(PathBuf::from("/p/api")));
    }

    #[test]
    fn falls_back_to_plain_cwd_when_foreground_cwd_is_absent() {
        let raw = r#"{"pane": {"cwd": "/p/api"}}"#;
        assert_eq!(parse_context_cwd(raw), Some(PathBuf::from("/p/api")));
    }

    #[test]
    fn falls_back_to_the_workspace_when_there_is_no_pane() {
        let raw = r#"{"workspace": {"cwd": "/p/api"}}"#;
        assert_eq!(parse_context_cwd(raw), Some(PathBuf::from("/p/api")));
    }

    #[test]
    fn gives_up_cleanly_on_unrecognised_shapes() {
        assert_eq!(parse_context_cwd(r#"{"tab": {"id": "t1"}}"#), None);
        assert_eq!(parse_context_cwd("not json"), None);
        assert_eq!(parse_context_cwd(""), None);
    }

    #[test]
    fn reports_a_running_status_as_a_pane_token() {
        let cmd = report_command("herdr", false, Some("w9:p3"), None, "running").unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "pane",
                "report-metadata",
                "w9:p3",
                "--source",
                "devcontainer-status",
                "--token",
                "devcontainer=running",
            ]
        );
    }

    #[test]
    fn clears_the_token_when_there_is_no_devcontainer() {
        let cmd = report_command("herdr", false, Some("w9:p3"), None, "none").unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[args.len() - 2..], ["--clear-token", "devcontainer"]);
    }

    #[test]
    fn workspace_wide_reports_target_the_workspace() {
        let cmd = report_command("herdr", true, Some("w9:p3"), Some("w9"), "running").unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "workspace");
        assert_eq!(args[2], "w9");
    }

    #[test]
    fn no_command_without_the_id_it_needs() {
        assert!(report_command("herdr", false, None, Some("w9"), "running").is_none());
        assert!(report_command("herdr", true, Some("w9:p3"), None, "running").is_none());
    }
}
