//! `switchyard task launch`: open a work's tab on its host and record the task.
//! Runs on the config host (the Mac), which can reach every Rex host.

use crate::hosts::{AUTO, Project, catalog, choose_host};
use crate::links::{FIZZY_BOARD_SH, Tracker, parse_ref, refs_in_text, tracker_or_fizzy};
use crate::naming::{HARNESSES, branch_and_slug, name_problem};
use crate::prompts::{discard_prompt, fetch_prompt};
use crate::rex::{host_bash, host_home, log, now, rex_json, sh_quote};
use crate::sessions::new_session_id;
use crate::tasks::record_task;

/// Resolve an existing folder on its own host, including a subdirectory or
/// linked worktree, to the primary checkout. Never clone or move an arbitrary
/// host-local path to a catalog pool.
fn folder_project(host: &str, path: &str, projects: &[Project]) -> Result<Project, String> {
    if host == AUTO {
        return Err("a folder needs its own host, not auto".into());
    }
    let script = folder_script(path);
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        return Err(format!("{path} is not an accessible Git working folder on {host}"));
    }
    let repo = String::from_utf8(out.stdout).map_err(|e| format!("Git folder path: {e}"))?;
    if !repo.starts_with('/') {
        return Err(format!("Git returned no repository path for {path} on {host}"));
    }
    let display = crate::folders::home_relative(&repo);
    // A catalog repository selected through one of its subfolders keeps its
    // configured profile. Otherwise use the host's normal login environment,
    // just like personal catalog work (no profile or credential switching).
    let configured = projects.iter().find(|p| {
        crate::folders::home_relative(p.path.trim_end_matches('/')) == display
    });
    Ok(Project {
        profile: configured.map(|p| p.profile.clone()).unwrap_or_else(|| "personal".into()),
        name: repo.rsplit('/').next().unwrap_or(&repo).to_string(),
        path: repo,
        clone_url: String::new(),
        hosts: vec![host.to_string()],
        tracker: configured.map(|p| p.tracker.clone()).unwrap_or_default(),
    })
}

fn folder_script(path: &str) -> String {
    let path = match path.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"/{}", sh_quote(rest)),
        None if path == "~" => "\"$HOME\"".to_string(),
        None => sh_quote(path),
    };
    format!(
        "cd -- {path} || exit 1\n\
         [ \"$(git rev-parse --is-inside-work-tree)\" = true ] || exit 1\n\
         IFS= read -r -d '' first < <(git worktree list --porcelain -z) || exit 1\n\
         case \"$first\" in 'worktree '*) printf '%s' \"${{first#worktree }}\";; *) exit 1;; esac"
    )
}

/// The slash command a work project's agent starts a work item with; the
/// item and these flags follow it.
pub const WORK_ITEM_COMMAND: &str = "/agent-kit:issue-implement";
const WORK_ITEM_FLAGS: &str = "--create-pr --agent-author";

/// The prompt that starts work on `item` ahead of the typed `prompt`: a
/// work project hands it to `WORK_ITEM_COMMAND`, which knows its tracker;
/// otherwise the agent is told the item, as its URL when the tracker can
/// place it.
pub fn work_item_prompt(profile: &str, item: &str, tracker: &Tracker, prompt: &str) -> String {
    let lead = if profile == "work" {
        format!("{WORK_ITEM_COMMAND} {item} {WORK_ITEM_FLAGS}")
    } else {
        let url = parse_ref(item, tracker).and_then(|l| l["url"].as_str().map(String::from)).unwrap_or_default();
        format!("Let's work on {}", if url.is_empty() { item } else { &url })
    };
    if prompt.is_empty() { lead } else { format!("{lead}\n\n{prompt}") }
}

/// A work item as typed: one ref or key, without spaces.
fn work_item_problem(item: &str) -> Option<&'static str> {
    item.chars().any(|c| c.is_whitespace() || c.is_control()).then_some("a work item is one ref, without spaces")
}

/// Open the work's tab on `host` and record it, with the work items its
/// prompt names as `spawn` links. An optional work item leads the prompt
/// (see `work_item_prompt`). A path outside the catalog must be an
/// existing Git folder on that explicit host. With `resume`, the tab
/// resumes that agent session with its recorded profile instead of starting
/// a new one, and `host` is used as is (never `auto`: the session lives there).
pub fn launch(
    host: &str,
    path: &str,
    name: &str,
    harness: &str,
    prompt_ref: Option<&str>,
    work_item: Option<&str>,
    resume: Option<(&str, &str)>,
) -> Result<(String, String, String), String> {
    launch_with(host, path, name, harness, Prompt::Ref(prompt_ref), work_item, resume)
}

/// `launch` with the prompt's text itself rather than a prompt file, for
/// callers that have it in hand (switchyard-server's MCP tools).
pub fn launch_text(
    host: &str,
    path: &str,
    name: &str,
    harness: &str,
    prompt: &str,
    work_item: Option<&str>,
) -> Result<(String, String, String), String> {
    launch_with(host, path, name, harness, Prompt::Text(prompt), work_item, None)
}

/// Where a launch's prompt comes from.
enum Prompt<'a> {
    /// A prompt file (see `prompts`), or none; the file is removed once the
    /// tab holds it.
    Ref(Option<&'a str>),
    Text(&'a str),
}

fn launch_with(
    host: &str,
    path: &str,
    name: &str,
    harness: &str,
    prompt: Prompt,
    work_item: Option<&str>,
    resume: Option<(&str, &str)>,
) -> Result<(String, String, String), String> {
    let projects = catalog()?;
    let mut project = match projects.iter().find(|p| p.path == path) {
        Some(project) => project.clone(),
        None => folder_project(host, path, &projects)?,
    };
    if let Some((_, profile)) = resume.filter(|(_, profile)| !profile.is_empty()) {
        project.profile = profile.to_string();
    }
    if let Some(problem) = name_problem(name) {
        return Err(format!("{name}: {problem}"));
    }
    if !HARNESSES.contains(&harness) {
        return Err(format!("unknown harness {harness}"));
    }
    let work_item = work_item.map(str::trim).filter(|i| !i.is_empty());
    if let Some(problem) = work_item.and_then(work_item_problem) {
        return Err(format!("{}: {problem}", work_item.unwrap_or_default()));
    }
    // Fetch the prompt first: without it the agent would start on nothing.
    let (typed, prompt_ref) = match prompt {
        Prompt::Ref(r) => (r.map(fetch_prompt).transpose()?.unwrap_or_default(), r),
        Prompt::Text(text) => (text.to_string(), None),
    };
    let (branch, slug) = branch_and_slug(name);
    let host = if host == AUTO {
        let (chosen, why) = choose_host(&project)?;
        log(&format!("auto chose {chosen} for {}/{name}: {why}", project.name));
        chosen
    } else {
        host.to_string()
    };
    let host = host.as_str();

    // Start the session and tab in the project so their processes, and
    // splits taken from them, sit there rather than in ~. Rex doesn't expand
    // ~ in --cwd and quietly falls back to home for a missing directory (a
    // project not cloned yet), so give it the host's absolute path. The
    // remote dev hosts are Linux, so home is /home/ACCOUNT for the account
    // its Rex server runs as.
    let remote_home = host_home(host)?;
    let cwd = match project.path.strip_prefix("~/") {
        Some(rest) if !remote_home.is_empty() => format!("{remote_home}/{rest}"),
        _ => project.path.clone(),
    };

    // One session per project, shared with the project picker's sessions.
    let sessions = rex_json(host, &["ls", "--json"])?;
    let existing = sessions["sessions"].as_array().and_then(|list| {
        list.iter()
            .find(|s| s["label"].as_str() == Some(project.name.as_str()))
            .and_then(|s| s["session_id"].as_str())
            .map(String::from)
    });
    // The agent's own session ID, chosen here so the registry knows it from
    // the start; an archive later records the newest one (after /clear).
    let agent_session = resume.map(|(id, _)| id.to_string()).unwrap_or_else(new_session_id);
    let mode = if resume.is_some() { "resume" } else { "new" };
    // Work items the prompt names (`#42`, `fizzy:123`, issue and card
    // URLs), under the project's tracker, or the checkout's `.fizzy.yaml`
    // board when the catalog sets none.
    let board = if project.tracker.trim().is_empty() && (!typed.trim().is_empty() || work_item.is_some()) {
        let script = format!("{FIZZY_BOARD_SH}fizzy_board {}\n", sh_quote(&cwd));
        host_bash(host, &script).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    } else {
        String::new()
    };
    let tracker = Tracker::new(&tracker_or_fizzy(&project.tracker, &board), &project.clone_url);
    let prompt = match work_item {
        Some(item) => work_item_prompt(&project.profile, item, &tracker, typed.trim()),
        None => typed,
    };
    let mut run = vec![
        "switchyard", "task", "run", &project.profile, &project.path, name, harness, &project.clone_url,
        &agent_session, mode,
    ];
    if !prompt.is_empty() {
        run.push(&prompt);
    }
    // A new session gets the work as its first tab, not beside a default
    // shell tab.
    let (session, window_id) = match existing {
        Some(session) => {
            let mut args = vec!["window", "new", "-s", &session, &slug, "-c", &cwd, "--json", "--"];
            args.extend(&run);
            let window = rex_json(host, &args)?;
            let window_id = window["window_id"].as_str().ok_or("rex window new returned no window_id")?;
            (session.clone(), window_id.to_string())
        }
        None => {
            let mut args = vec!["new", &project.name, "--window", &slug, "-c", &cwd, "--json", "--"];
            args.extend(&run);
            let created = rex_json(host, &args)?;
            let session = created["session_id"].as_str().ok_or("rex new returned no session_id")?;
            let window_id = created["initial_windows"][0]["window_id"]
                .as_str()
                .ok_or("rex new returned no initial window")?;
            (session.to_string(), window_id.to_string())
        }
    };
    // The tab holds the prompt now.
    if let Some(r) = prompt_ref {
        discard_prompt(r);
    }
    let links: Vec<serde_json::Value> = refs_in_text(&prompt, &tracker)
        .into_iter()
        .map(|mut l| {
            l["source"] = "spawn".into();
            l
        })
        .collect();
    let task = serde_json::json!({
        "host": host,
        "session_id": session,
        "window_id": window_id,
        "project": project.name,
        "profile": project.profile,
        "repo": cwd,
        "name": name,
        "branch": branch,
        // Where `task run` puts a new worktree; `task list` corrects it when the
        // branch already had one elsewhere.
        "worktree": format!("{cwd}/.worktrees/{slug}"),
        "harness": harness,
        "agent_session": agent_session,
        // Work items (issues, cards, PRs); see `links`.
        "links": links,
        "created_at": now(),
        "ended_at": null,
        "ended_reason": null,
    });
    // The tab is already open: a registry failure only costs the inbox entry.
    if let Err(e) = record_task(task) {
        log(&format!("could not record {}/{name}: {e}", project.name));
    }
    // List the new task now rather than at the server's next registry poll;
    // a resume does it once it has updated the registry (see `actions`).
    if resume.is_none() {
        crate::server::refresh();
    }
    Ok((host.to_string(), session, window_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            // macOS clocks tick in microseconds, so parallel tests can read the
            // same time; the counter keeps their directories distinct.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("switchyard-folder-{}-{unique}-{seq}-it's a repo", std::process::id()));
            std::fs::create_dir_all(path.join("nested folder")).unwrap();
            assert!(Command::new("git").args(["init", "--quiet"]).arg(&path).status().unwrap().success());
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn folder_and_subdirectory_resolve_to_primary_checkout() {
        let repo = Fixture::new();
        let host = crate::rex::local_label();
        for path in [&repo.0, &repo.0.join("nested folder")] {
            let project = folder_project(&host, path.to_str().unwrap(), &[]).unwrap();
            assert_eq!(project.path, repo.0.to_str().unwrap());
            assert_eq!(project.profile, "personal");
            assert!(project.clone_url.is_empty());
            assert_eq!(project.hosts, [host.clone()]);
        }
    }

    #[test]
    fn catalog_subdirectory_preserves_work_profile() {
        let repo = Fixture::new();
        let configured = Project {
            profile: "work".into(), path: repo.0.to_str().unwrap().into(),
            clone_url: "not used".into(), name: "configured".into(), hosts: vec![], tracker: String::new(),
        };
        let project = folder_project(
            &crate::rex::local_label(), repo.0.join("nested folder").to_str().unwrap(), &[configured],
        ).unwrap();
        assert_eq!(project.profile, "work");
    }

    #[test]
    fn work_item_leads_the_prompt() {
        let fizzy = Tracker::new("fizzy:6284043/board", "");
        assert_eq!(
            work_item_prompt("work", "ENG-12", &fizzy, ""),
            "/agent-kit:issue-implement ENG-12 --create-pr --agent-author",
        );
        assert_eq!(
            work_item_prompt("work", "#7", &fizzy, "keep it small"),
            "/agent-kit:issue-implement #7 --create-pr --agent-author\n\nkeep it small",
        );
        let personal = work_item_prompt("personal", "#1045", &fizzy, "");
        assert_eq!(personal, "Let's work on https://app.fizzy.do/6284043/cards/1045");
        // The prompt's links pick the item up as a spawn link.
        assert_eq!(refs_in_text(&personal, &fizzy)[0]["ref"], "1045");
        assert_eq!(work_item_prompt("personal", "ENG-12", &Tracker::default(), "go"), "Let's work on ENG-12\n\ngo");
        assert!(work_item_problem("ENG-12").is_none());
        assert!(work_item_problem("two refs").is_some());
    }

    #[test]
    fn non_git_folder_and_auto_are_rejected() {
        let repo = Fixture::new();
        assert!(folder_project(AUTO, repo.0.to_str().unwrap(), &[]).err().unwrap().contains("own host"));
        let outside = repo.0.parent().unwrap().to_str().unwrap();
        assert!(folder_project(&crate::rex::local_label(), outside, &[]).err().unwrap().contains("Git working folder"));
    }

    #[test]
    fn linked_worktree_uses_first_checkout_not_nested_worktree() {
        // Git's first worktree record is the primary checkout. Simulate its
        // NUL-delimited output without creating or removing a live worktree.
        let script = format!(
            "git() {{ if [ \"$1\" = rev-parse ]; then printf true; else printf 'worktree /primary repo\\0HEAD abc\\0\\0worktree /linked\\0'; fi; }}\n{}",
            folder_script("/"),
        );
        let out = Command::new("bash").args(["-c", &script]).output().unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"/primary repo");
    }
}
