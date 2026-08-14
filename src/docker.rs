use std::path::Path;
use std::process::Command;

/// One devcontainer-labelled container as reported by `docker ps -a`.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub state: String,
    pub local_folder: String,
    pub config_file: String,
    pub name: String,
}

impl Container {
    pub fn is_running(&self) -> bool {
        self.state.eq_ignore_ascii_case("running")
    }
}

/// Pick the container labelled for `root`. Compose projects put several
/// containers on one local_folder; prefer a running one so callers don't flap
/// between it and a stopped sidecar.
pub fn best_match<'a>(containers: &'a [Container], root: &Path) -> Option<&'a Container> {
    containers
        .iter()
        .filter(|c| Path::new(&c.local_folder) == root)
        .max_by_key(|c| c.is_running())
}

/// `best_match`, with the dogfood fallback: when this binary runs *inside*
/// its own devcontainer against the host daemon (a manual `hook`/`refresh`/
/// `exec` invocation from a shell in there — see `.devcontainer/README.md`
/// "The path-mapping caveat"), discovery sees the `/workspaces` bind-mount
/// path while docker labels still carry the host path.
/// `HERDR_HOST_WORKSPACE` (set by our own `devcontainer.json`) bridges the
/// two. Shared by `main::detect` (status) and `exec::container_name` (which
/// container to `docker cp`/`docker exec` into), so the two can never
/// disagree about which container a project's pane means.
pub fn container_for<'a>(containers: &'a [Container], root: &Path) -> Option<&'a Container> {
    best_match(containers, root).or_else(|| {
        let host_root = std::path::PathBuf::from(std::env::var_os("HERDR_HOST_WORKSPACE")?);
        best_match(containers, &host_root)
    })
}

// Tab-separated on purpose: with `{{json .}}` docker flattens Labels into one
// comma-joined string that cannot be split safely when values contain commas.
const FORMAT: &str = "{{.State}}\t{{.Label \"devcontainer.local_folder\"}}\t{{.Label \"devcontainer.config_file\"}}\t{{.Names}}";

/// List all devcontainer-labelled containers (any state).
/// Err = the probe itself failed (docker missing, daemon down) — distinct from Ok(vec![]).
pub fn list() -> Result<Vec<Container>, String> {
    let output = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "label=devcontainer.local_folder",
            "--format",
            FORMAT,
        ])
        .output()
        .map_err(|e| format!("failed to run docker: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("docker ps failed: {}", stderr.trim()));
    }
    Ok(parse(&String::from_utf8_lossy(&output.stdout)))
}

/// Parse the tab-separated `docker ps` output. Malformed lines are skipped.
pub fn parse(output: &str) -> Vec<Container> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let state = parts.next()?.trim();
            let local_folder = parts.next()?.trim();
            let config_file = parts.next()?.trim();
            let name = parts.next().unwrap_or("").trim();
            if state.is_empty() || local_folder.is_empty() {
                return None;
            }
            Some(Container {
                state: state.to_string(),
                local_folder: local_folder.to_string(),
                config_file: config_file.to_string(),
                name: name.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_running_and_exited() {
        let out = "running\t/Users/x/proj\t/Users/x/proj/.devcontainer/devcontainer.json\tvibrant_cat\n\
                   exited\t/Users/x/other\t/Users/x/other/.devcontainer/devcontainer.json\tsad_dog\n";
        let cs = parse(out);
        assert_eq!(cs.len(), 2);
        assert!(cs[0].is_running());
        assert!(!cs[1].is_running());
        assert_eq!(cs[1].local_folder, "/Users/x/other");
    }

    #[test]
    fn compose_project_shares_local_folder() {
        let out = "running\t/Users/x/api\t/Users/x/api/.devcontainer/devcontainer.json\tapi-devcontainer-1\n\
                   running\t/Users/x/api\t/Users/x/api/.devcontainer/devcontainer.json\tapi-db-1\n";
        let cs = parse(out);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].local_folder, cs[1].local_folder);
    }

    #[test]
    fn skips_blank_and_malformed_lines() {
        let out = "\nrunning\n\
                   running\t/Users/x/proj\t\tname-only-no-config\n";
        let cs = parse(out);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].config_file, "");
        assert_eq!(cs[0].name, "name-only-no-config");
    }

    #[test]
    fn empty_output_is_empty_vec() {
        assert!(parse("").is_empty());
    }

    fn container(state: &str, folder: &str, name: &str) -> Container {
        Container {
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

    #[test]
    fn container_for_falls_back_to_host_workspace_when_set() {
        // HERDR_HOST_WORKSPACE is process-global and no other test touches it,
        // so this is safe under cargo test's default parallel threads; restore
        // it regardless of outcome so a later run never inherits it.
        let prior = std::env::var_os("HERDR_HOST_WORKSPACE");
        std::env::set_var("HERDR_HOST_WORKSPACE", "/Users/x/repo");
        let cs = vec![container("running", "/Users/x/repo", "repo-1")];
        // Discovery sees the /workspaces bind mount; the label carries the host path.
        let result = container_for(&cs, Path::new("/workspaces/repo"));
        match prior {
            Some(v) => std::env::set_var("HERDR_HOST_WORKSPACE", v),
            None => std::env::remove_var("HERDR_HOST_WORKSPACE"),
        }
        assert_eq!(result.map(|c| c.name.as_str()), Some("repo-1"));
    }

    #[test]
    fn container_for_matches_directly_without_needing_the_fallback() {
        let cs = vec![container("running", "/p/api", "api-1")];
        assert_eq!(
            container_for(&cs, Path::new("/p/api")).map(|c| c.name.as_str()),
            Some("api-1")
        );
    }
}
