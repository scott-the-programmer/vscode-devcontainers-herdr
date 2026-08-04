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
}
