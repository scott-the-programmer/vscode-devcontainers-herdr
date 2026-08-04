mod discover;
mod docker;

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

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    match command.as_str() {
        // Both entry points do the same detection today; herdr decides when to
        // call us. `hook --all` re-detects for the focused workspace, which is
        // still just "the cwd herdr gave us".
        "refresh" | "hook" => {}
        other => {
            eprintln!("usage: herdr-devcontainer-status <refresh|hook> [--all]");
            eprintln!("unknown command: {other:?}");
            std::process::exit(2);
        }
    }

    // herdr exports the pane's cwd; fall back to our own for manual runs.
    let cwd = std::env::var_os("HERDR_PANE_CWD")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let status = detect(&cwd);
    println!("{}", serde_json::to_string(&status).expect("status serializes"));
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
fn best_match<'a>(containers: &'a [docker::Container], root: &Path) -> Option<&'a docker::Container> {
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
    fn no_match_for_other_project() {
        let cs = vec![container("running", "/p/api", "api-1")];
        assert!(best_match(&cs, Path::new("/p/web")).is_none());
    }
}
