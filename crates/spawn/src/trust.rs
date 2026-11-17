//! Mark a task's folders trusted in Claude's config before the agent starts,
//! so a new task doesn't stop on Claude's workspace trust dialog. That dialog
//! comes before Claude runs hooks, so the task would show no status while it
//! waits there. Spawned tasks already run with `--dangerously-skip-permissions`,
//! so trusting their folder grants nothing more.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use switchyard_core::rex::home;

/// Claude's global config for a profile: `.claude.json` in its
/// `CLAUDE_CONFIG_DIR`, or in home when that is unset.
pub fn claude_config(config_dir: Option<&str>) -> PathBuf {
    config_dir
        .filter(|d| !d.is_empty())
        .map_or_else(home, PathBuf::from)
        .join(".claude.json")
}

/// Whether Claude trusts `dir`: `projects[PATH].hasTrustDialogAccepted` is
/// true for it or a folder above it, as Claude checks.
fn trusted(config: &serde_json::Value, dir: &Path) -> bool {
    dir.ancestors().any(|d| {
        d.to_str()
            .and_then(|d| config["projects"][d]["hasTrustDialogAccepted"].as_bool())
            .unwrap_or(false)
    })
}

/// Mark each of `dirs` trusted in the Claude config at `path` unless it or a
/// folder above it already is. Earlier `dirs` come first, so a repository
/// trusted here covers its worktrees after it. The config is left alone
/// when it is missing (Claude hasn't run under that profile yet) or isn't
/// JSON, and replaced atomically, keeping its permissions, so a running
/// Claude never reads it half written. Returns the folders it marked.
///
/// # Errors
///
/// Fails when the config can't be read, or the new one can't be written.
pub fn trust_at(path: &Path, dirs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Err(format!("{}: not JSON; left alone", path.display()));
    };
    if !config.is_object() {
        return Err(format!("{}: not a JSON object; left alone", path.display()));
    }
    let mut marked = Vec::new();
    for dir in dirs {
        // Claude keys projects by real path (/private/tmp, not /tmp).
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
        let Some(key) = dir.to_str() else { continue };
        if trusted(&config, &dir) {
            continue;
        }
        if !config["projects"].is_object() {
            config["projects"] = serde_json::json!({});
        }
        let project = &mut config["projects"][key];
        if !project.is_object() {
            *project = serde_json::json!({});
        }
        project["hasTrustDialogAccepted"] = true.into();
        marked.push(dir);
    }
    if marked.is_empty() {
        return Ok(marked);
    }
    let out = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    let tmp = path.with_file_name(format!(".claude.json.switchyard-{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        std::fs::write(&tmp, out)?;
        let mode = std::fs::metadata(path)?.permissions().mode();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(marked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("switchyard-trust-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn trusts_the_repo_and_skips_a_worktree_inside_it() {
        let dir = scratch("repo");
        let repo = dir.join("repo");
        let wt = repo.join(".worktrees/x");
        std::fs::create_dir_all(&wt).unwrap();
        let config = dir.join(".claude.json");
        std::fs::write(&config, r#"{"numStartups":3,"projects":{"/a":{"x":1}}}"#).unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();

        let marked = trust_at(&config, &[repo.clone(), wt]).unwrap();

        assert_eq!(marked, vec![repo.clone()]);
        let v = read(&config);
        assert_eq!(v["numStartups"], 3);
        assert_eq!(v["projects"]["/a"]["x"], 1);
        assert_eq!(
            v["projects"][repo.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
        let mode = std::fs::metadata(&config).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn leaves_a_config_alone_when_a_parent_is_trusted() {
        let dir = scratch("parent");
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let config = dir.join(".claude.json");
        let text = format!(
            r#"{{"projects":{{"{}":{{"hasTrustDialogAccepted":true}}}}}}"#,
            dir.display()
        );
        std::fs::write(&config, &text).unwrap();

        assert_eq!(trust_at(&config, &[repo]).unwrap(), Vec::<PathBuf>::new());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
    }

    #[test]
    fn keeps_an_untrusted_projects_other_settings() {
        let dir = scratch("keep");
        let config = dir.join(".claude.json");
        let text = format!(
            r#"{{"projects":{{"{}":{{"hasTrustDialogAccepted":false,"allowedTools":[]}}}}}}"#,
            dir.display()
        );
        std::fs::write(&config, text).unwrap();

        assert_eq!(
            trust_at(&config, std::slice::from_ref(&dir)).unwrap(),
            vec![dir.clone()]
        );
        let v = read(&config);
        let p = &v["projects"][dir.to_str().unwrap()];
        assert_eq!(p["hasTrustDialogAccepted"], true);
        assert!(p["allowedTools"].is_array());
    }

    #[test]
    fn skips_a_missing_or_broken_config() {
        let dir = scratch("missing");
        let config = dir.join(".claude.json");
        assert_eq!(
            trust_at(&config, std::slice::from_ref(&dir)).unwrap(),
            Vec::<PathBuf>::new()
        );
        assert!(!config.exists());

        std::fs::write(&config, "{not json").unwrap();
        assert!(trust_at(&config, std::slice::from_ref(&dir)).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{not json");
    }

    #[test]
    fn finds_the_config_in_the_profile_dir_or_home() {
        assert_eq!(claude_config(Some("/c")), PathBuf::from("/c/.claude.json"));
        assert_eq!(claude_config(None), home().join(".claude.json"));
        assert_eq!(claude_config(Some("")), home().join(".claude.json"));
    }
}
