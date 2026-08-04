use std::path::{Path, PathBuf};

/// Result of walking up from a pane's cwd looking for devcontainer configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Discovery {
    /// Nearest ancestor directory that contains devcontainer configuration.
    pub project_root: PathBuf,
    /// All config files found at that level.
    pub configs: Vec<PathBuf>,
}

/// Find the nearest ancestor of `cwd` (including itself) that carries
/// devcontainer configuration. Recognised forms, per the devcontainer spec:
///   <dir>/.devcontainer/devcontainer.json
///   <dir>/.devcontainer.json
///   <dir>/.devcontainer/<subfolder>/devcontainer.json
///
/// The walk does not ascend past the parent of $HOME.
pub fn find(cwd: &Path) -> Option<Discovery> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let stop = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|h| h.parent().map(Path::to_path_buf));

    for dir in cwd.ancestors() {
        let configs = configs_in(dir);
        if !configs.is_empty() {
            return Some(Discovery {
                project_root: dir.to_path_buf(),
                configs,
            });
        }
        if stop.as_deref() == Some(dir) {
            break;
        }
    }
    None
}

fn configs_in(dir: &Path) -> Vec<PathBuf> {
    let mut configs = Vec::new();
    let nested = dir.join(".devcontainer").join("devcontainer.json");
    if nested.is_file() {
        configs.push(nested);
    }
    let flat = dir.join(".devcontainer.json");
    if flat.is_file() {
        configs.push(flat);
    }
    if let Ok(entries) = std::fs::read_dir(dir.join(".devcontainer")) {
        let mut subs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path().join("devcontainer.json"))
            .filter(|p| p.is_file())
            .collect();
        subs.sort();
        configs.extend(subs);
    }
    configs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "{}").unwrap();
    }

    #[test]
    fn finds_nested_config_from_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        touch(&root.join(".devcontainer/devcontainer.json"));
        let deep = root.join("src/module/inner");
        fs::create_dir_all(&deep).unwrap();

        let d = find(&deep).unwrap();
        assert_eq!(d.project_root, root.canonicalize().unwrap());
        assert_eq!(d.configs.len(), 1);
    }

    #[test]
    fn finds_flat_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        touch(&root.join(".devcontainer.json"));

        let d = find(&root).unwrap();
        assert_eq!(d.configs, vec![root.canonicalize().unwrap().join(".devcontainer.json")]);
    }

    #[test]
    fn finds_subfolder_configs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        touch(&root.join(".devcontainer/api/devcontainer.json"));
        touch(&root.join(".devcontainer/web/devcontainer.json"));

        let d = find(&root).unwrap();
        assert_eq!(d.configs.len(), 2);
    }

    #[test]
    fn nearest_config_wins_in_monorepo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mono");
        touch(&root.join(".devcontainer/devcontainer.json"));
        let pkg = root.join("packages/svc");
        touch(&pkg.join(".devcontainer/devcontainer.json"));

        let d = find(&pkg.join("src")).unwrap();
        assert_eq!(d.project_root, pkg.canonicalize().unwrap());
    }

    #[test]
    fn none_when_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plain");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(find(&dir), None);
    }

    #[test]
    fn missing_cwd_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(find(&tmp.path().join("gone/away")), None);
    }
}
