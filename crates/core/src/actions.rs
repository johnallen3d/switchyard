//! Actions on a spawned task: `switchyard task act ACTION HOST SESSION WINDOW`.
//!
//! Runs on the config host, which holds the task registry and reaches every
//! Rex host; the config's `tab_picker` mode calls it.
//!
//! - archive: refuse a worktree with uncommitted changes; otherwise record
//!   the newest agent session, close the tab, remove the worktree with
//!   Worktrunk, close the project session if that left it empty, and mark
//!   the task ended.
//! - force-archive: the same, but throw away uncommitted changes
//!   (`wt remove --force`).
//!
//!   A clean worktree whose work is merged (its PR, as last seen on
//!   GitHub, or its branch into the default branch) or whose linked cards
//!   Fizzy last reported all closed is safe to archive; the task ends as "merged"
//!   rather than "archived".
//!
//!   switchyard-server archives a task on its own once its work lands
//!   upstream (its PR merged, or its commits pushed to the default branch)
//!   and its agent has gone quiet, or its tab was closed; see `auto_archive`.
//!
//!   Both also work on an ended task whose tab was closed without an
//!   archive, which leaves its worktree behind: they remove the worktree
//!   and mark the task archived.
//!
//!   On a tab that is still open with no open task, they first reopen its
//!   latest ended task (one ended by mistake), or else adopt the agent in
//!   it as a task (its checkout, branch and harness from the agent's
//!   pane), so any agent tab can be archived. A primary checkout is never
//!   removed: its archive only closes the tab.
//! - link: print the URL of the task's main work item (its PR, else an
//!   issue, card or commit; see `links`), open or ended, for the caller to
//!   open on the client machine.
//! - pin: pin an open task to the top of the inbox, or unpin a pinned one
//!   (`pinned` in the registry).
//! - snooze (`task act snooze HOST SESSION WINDOW UNTIL`): hide an
//!   open task in the inbox's Snoozed group until UNTIL, Unix seconds
//!   (`snoozed_until`); 0 wakes it now.
//! - message (`task act message HOST SESSION WINDOW PROMPT_REF`): send
//!   the prompt file's text to the agent in the tab (see `message`), then
//!   remove the file.
//! - resume: for an ended task (archived, or its tab or worktree gone), open
//!   a new tab on the task's host that recreates the worktree on its branch
//!   and resumes the agent session, replace the ended entry with the new
//!   task, and print the tab as `task launch` does. SESSION is unused here.
//! - switch (`task act switch HOST SESSION WINDOW HARNESS`): continue a live
//!   or ended task in another harness (`HARNESS` or `HARNESS:MODEL`) in the
//!   same worktree through Atuin Threads (`atuin threads resume --print --in`),
//!   updating `harness`, `model`, and `agent_session` in the registry and
//!   relaunching in the same tab (for a live task) or opening a new tab (for
//!   an ended task), printing `HOST<TAB>SESSION<TAB>WINDOW`.
//! - move (`task act move HOST SESSION WINDOW TARGET_HOST`): move a live or
//!   ended personal task to another compatible pool host (`HOST`, `"config"`,
//!   or `"auto"` for the least busy), carrying committed and uncommitted
//!   worktree state and syncing/restoring its Atuin thread (see `move_host`),
//!   printing `HOST<TAB>SESSION<TAB>WINDOW`.
//!
//! Archiving doesn't delete an unmerged branch: only uncommitted work can be
//! lost, and a resume finds its commits again.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::hosts::catalog;
use crate::images::carry_from;
use crate::launch::launch;
use crate::naming::{HARNESSES, branch_and_slug, harness_spec, models, split_harness};
use crate::prompts::{discard_prompt, fetch_prompt, parse_prompt_ref};
use crate::rex::{host_bash, local_label, now, rex, rex_json, sh_quote};
use crate::sessions::newest_session;
use crate::tasks::{MERGED_SH, in_window, is_in_place, load_tasks, save_record, update_tasks};

/// The tab's agent pane: the first pane of its tiled layer, which is where
/// `--run` started the agent.
pub(crate) fn agent_block(host: &str, session: &str, window: &str) -> Result<String, String> {
    let list = rex_json(host, &["window", "ls", "-s", session, "--json"])?;
    list["windows"]
        .as_array()
        .and_then(|ws| ws.iter().find(|w| w["window_id"].as_str() == Some(window)))
        .and_then(|w| w["layers"].as_array())
        .and_then(|ls| ls.iter().find(|l| l["kind"].as_str() == Some("tiled")))
        .and_then(|l| l["blocks"][0]["block_id"].as_str())
        .map(String::from)
        .ok_or_else(|| "the task's tab is gone".to_string())
}

/// Where the agent in a tab works, from its foreground process's folder on
/// `host`: PATH (the checkout's top), REPO (the primary checkout) and
/// BRANCH, tab separated; exit 3 when that isn't a Git checkout.
fn checkout_script(cwd: &str) -> String {
    format!(
        "cd {} 2>/dev/null || exit 3\n\
         top=$(git rev-parse --show-toplevel 2>/dev/null) || exit 3\n\
         main=$(git worktree list --porcelain | sed -n '1s/^worktree //p')\n\
         printf '%s\\t%s\\t%s\\n' \"$top\" \"${{main:-$top}}\" \"$(git symbolic-ref --quiet --short HEAD)\"\n",
        sh_quote(cwd)
    )
}

/// A task record for an agent tab Switchyard has none for (started by hand,
/// say), so it can be archived like a spawned one: its checkout, branch
/// and harness from the agent's pane. In a primary checkout, the archive
/// only closes the tab. `Adopted::Folder` when the agent isn't in a Git
/// checkout at all: there is nothing to archive but the tab.
fn adopt(host: &str, session: &str, window: &str) -> Result<Adopted, String> {
    let pane = crate::transcript::agent_pane(host, session, window)?;
    let cwd = pane["cwd"].as_str().unwrap_or("");
    if cwd.is_empty() {
        return Err("no agent in that tab to archive".into());
    }
    let out =
        host_bash(host, &checkout_script(cwd)).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        return Ok(Adopted::Folder(cwd.to_string()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut f = text.trim_end_matches('\n').split('\t');
    let (top, repo, branch) = (
        f.next().unwrap_or(""),
        f.next().unwrap_or(""),
        f.next().unwrap_or(""),
    );
    let base = |p: &str| {
        p.trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string()
    };
    // Rex names Claude Code `claude-code` and Antigravity's CLI
    // `antigravity`; spawned tasks record `claude` and `agy`.
    let app = pane["app"].as_str().unwrap_or("");
    let harness = match app {
        "antigravity" => "agy",
        app => app.strip_suffix("-code").unwrap_or(app),
    };
    Ok(Adopted::Task(serde_json::json!({
        "host": host,
        "session_id": session,
        "window_id": window,
        "project": base(repo),
        "repo": repo,
        "name": base(top),
        "branch": branch,
        "worktree": top,
        "harness": harness,
        "adopted": true,
        "created_at": now(),
        "ended_at": null,
        "ended_reason": null,
    })))
}

/// What `adopt` made of an agent tab Switchyard has no task for.
enum Adopted {
    /// A task record for its checkout.
    Task(serde_json::Value),
    /// The folder it works in, which isn't a Git checkout.
    Folder(String),
}

/// A session spawn created for the project goes when its last tab does.
pub(crate) fn kill_if_empty(host: &str, session: &str) {
    // An orphan worktree has no session.
    if session.is_empty() {
        return;
    }
    if let Ok(list) = rex_json(host, &["window", "ls", "-s", session, "--json"])
        && list["windows"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty)
    {
        let _ = rex(host, &["kill", session]);
    }
}

/// Resume the ended task that had `window` on `host`.
fn resume(host: &str, window: &str) -> Result<String, String> {
    let tasks = load_tasks()?;
    let task = tasks
        .iter()
        .rev()
        .find(|t| !t["ended_at"].is_null() && in_window(t, window))
        .ok_or("no ended task for that tab")?
        .clone();
    let field = |k: &str| task[k].as_str().unwrap_or("").to_string();
    let label = format!("{}/{}", field("project"), field("name"));
    if field("ended_reason") == "orphaned" {
        return Err(format!(
            "{label} is an orphan worktree with no agent session to resume; ctrl-x removes it"
        ));
    }
    // Tasks from before sessions were recorded: ask the host for the newest.
    let agent_session = match field("agent_session") {
        id if !id.is_empty() => id,
        _ => newest_session(&task)
            .ok_or_else(|| format!("{label}: no agent session found on {host}"))?,
    };
    // Folder tasks have no catalog entry. Their recorded repository is the
    // primary checkout, which remains after the task worktree is archived.
    let path = catalog()?
        .into_iter()
        .find(|p| {
            crate::folders::home_relative(&p.path) == crate::folders::home_relative(&field("repo"))
        })
        .map_or_else(|| field("repo"), |p| p.path);
    // The model it was started on, so the resumed agent stays on it.
    let harness = match field("harness").as_str() {
        "" => "claude".to_string(),
        h => crate::naming::harness_spec(h, Some(&field("model"))),
    };
    // The branch head it resumes at: auto-archive waits for work landed
    // after it, so a merged task resumed for a follow-up isn't archived again
    // before the follow-up lands.
    let resume_head = branch_head(host, &path, &field("branch"));
    let (host, session, window_id) = launch(
        host,
        &path,
        &field("name"),
        &harness,
        None,
        None,
        Some((&agent_session, &field("profile"))),
    )?;
    // The new task takes the ended one's place in the archive, keeping its
    // work items.
    update_tasks(|tasks| {
        tasks.retain(|t| t["ended_at"].is_null() || !in_window(t, window));
        if let Some(new) = tasks
            .iter_mut()
            .rev()
            .find(|t| t["ended_at"].is_null() && in_window(t, &window_id))
        {
            if task["links"].as_array().is_some_and(|l| !l.is_empty()) {
                new["links"] = task["links"].clone();
            }
            if let Some(head) = &resume_head {
                new["resume_head"] = head.clone().into();
            }
        }
        Ok(())
    })?;
    Ok(format!("{host}\t{session}\t{window_id}"))
}

/// Continue the live or ended task in `window` on `host` in `target_spec`
/// (`HARNESS` or `HARNESS:MODEL`) through Atuin Threads, keeping the same
/// worktree (and the same Rex tab for a live task) and recording the new
/// `harness`, `model`, and `agent_session` in the registry.
///
/// Prints `HOST<TAB>SESSION<TAB>WINDOW`, like `resume`.
///
/// # Errors
///
/// Fails when the target spec is unknown, no matching task exists, the task
/// is an orphan worktree, Atuin doesn't support the profile or source/target
/// harness, no source session exists, conversion fails, or Rex can't launch
/// the new pane/tab.
pub fn switch_harness(
    host: &str,
    session: &str,
    window: &str,
    target_spec: &str,
) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_switch(host, session, window, target_spec);
    }
    let (target_harness, target_model) = validate_target_spec(target_spec)?;
    let spec = harness_spec(target_harness, target_model);
    let tasks = load_tasks()?;
    let task = find_switch_task(&tasks, window)?;
    let field = |k: &str| task[k].as_str().unwrap_or("").to_string();
    let label = format!("{}/{}", field("project"), field("name"));
    if field("ended_reason") == "orphaned" {
        return Err(format!(
            "{label} is an orphan worktree with no agent session to continue"
        ));
    }
    let profile = field("profile");
    let source_harness = match field("harness").as_str() {
        "" => "claude".to_string(),
        h => h.to_string(),
    };
    crate::atuin::switch_plan(&profile, &source_harness, target_harness)
        .map_err(|e| format!("{label}: {e}"))?;

    let task_host = match field("host") {
        h if !h.is_empty() => h,
        _ => host.to_string(),
    };
    let ended = !task["ended_at"].is_null();
    let source_session = if ended {
        match field("agent_session") {
            id if !id.is_empty() => Some(id),
            _ => newest_session(&task),
        }
    } else {
        newest_session(&task).or_else(|| {
            let id = field("agent_session");
            (!id.is_empty()).then_some(id)
        })
    }
    .ok_or_else(|| format!("{label}: no {source_harness} session found on {task_host}"))?;

    let (path, clone_url) = project_path_and_clone(&field("repo"))?;
    let name = field("name");
    let branch = if is_in_place(&task) {
        String::new()
    } else {
        match field("branch") {
            b if !b.is_empty() => b,
            _ => branch_and_slug(&name).0,
        }
    };
    let result = if ended {
        switch_ended(
            &task,
            window,
            &task_host,
            &profile,
            &source_harness,
            target_harness,
            &spec,
            &path,
            &name,
            &branch,
            &clone_url,
            &source_session,
        )
    } else {
        let task_session = match field("session_id") {
            s if !s.is_empty() => s,
            _ => session.to_string(),
        };
        let task_window = match field("window_id") {
            w if !w.is_empty() => w,
            _ => window.to_string(),
        };
        switch_live(
            &task,
            &task_host,
            &task_session,
            &task_window,
            &profile,
            &source_harness,
            target_harness,
            target_model,
            &spec,
            &path,
            &name,
            &clone_url,
            &source_session,
        )
    };
    if result.is_ok() {
        crate::server::refresh();
    }
    result
}

fn validate_target_spec(spec: &str) -> Result<(&str, Option<&str>), String> {
    let (harness, model) = split_harness(spec.trim());
    if !HARNESSES.contains(&harness) {
        return Err(format!("unknown harness {harness}"));
    }
    if let Some(model) = model.filter(|m| !models(harness).contains(m)) {
        return Err(format!("unknown {harness} model {model}"));
    }
    Ok((harness, model))
}

/// Move the live or ended task in `window` on `host` to `target_host` (a
/// compatible host label, `"config"`, or `"auto"` for the least busy).
///
/// See `crate::move_host::move_host` for details and safety guarantees.
///
/// # Errors
///
/// Fails as `crate::move_host::move_host` does.
pub fn move_host(
    host: &str,
    session: &str,
    window: &str,
    target_host: &str,
) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_move(host, session, window, target_host);
    }
    crate::move_host::move_host(host, session, window, target_host)
}

pub(crate) fn find_switch_task(
    tasks: &[serde_json::Value],
    window: &str,
) -> Result<serde_json::Value, String> {
    tasks
        .iter()
        .find(|t| t["ended_at"].is_null() && in_window(t, window))
        .or_else(|| tasks.iter().rev().find(|t| in_window(t, window)))
        .cloned()
        .ok_or_else(|| "no task for that tab".to_string())
}

fn project_path_and_clone(repo: &str) -> Result<(String, String), String> {
    Ok(catalog()?
        .into_iter()
        .find(|p| crate::folders::home_relative(&p.path) == crate::folders::home_relative(repo))
        .map_or_else(
            || (repo.to_string(), String::new()),
            |p| (p.path, p.clone_url),
        ))
}

#[allow(clippy::too_many_arguments)]
fn switch_live(
    task: &serde_json::Value,
    host: &str,
    session: &str,
    window: &str,
    profile: &str,
    source_harness: &str,
    target_harness: &str,
    target_model: Option<&str>,
    spec: &str,
    path: &str,
    name: &str,
    clone_url: &str,
    source_session: &str,
) -> Result<String, String> {
    // Check the tab is still open and locate its agent pane before converting.
    let old_block = crate::transcript::agent_pane(host, session, window)
        .ok()
        .and_then(|p| {
            p["block"]
                .as_str()
                .filter(|b| !b.is_empty())
                .map(String::from)
        })
        .map_or_else(|| agent_block(host, session, window), Ok)?;
    let worktree = task["worktree"].as_str().unwrap_or("");
    if worktree.is_empty() {
        return Err(format!("{}: task has no worktree recorded", label_of(task)));
    }
    let new_session = convert_on_host(
        host,
        profile,
        source_harness,
        target_harness,
        worktree,
        source_session,
    )?;
    // Launch the new harness in a sibling split of the same tab first, then
    // close the old agent block so the Rex window (and any other splits) stay
    // intact with the same window_id and label.
    let mut run = vec!["switchyard", "task", "run"];
    if is_in_place(task) {
        run.push("--in-place");
    }
    run.extend([profile, path, name, spec, clone_url, &new_session, "resume"]);
    let mut args = vec![
        "split",
        "-s",
        session,
        "-b",
        &old_block,
        "--split=right",
        "--focus=false",
        "-c",
        worktree,
        "--json",
        "--",
    ];
    args.extend(run);
    rex_json(host, &args)?;
    let _ = rex(host, &["block", "close", "-s", session, &old_block]);
    update_tasks(|tasks| {
        if let Some(record) = tasks
            .iter_mut()
            .find(|t| t["ended_at"].is_null() && in_window(t, window))
        {
            apply_switched_harness(record, target_harness, target_model, &new_session);
        }
        Ok(())
    })?;
    Ok(format!("{host}\t{session}\t{window}"))
}

#[allow(clippy::too_many_arguments)]
fn switch_ended(
    task: &serde_json::Value,
    old_window: &str,
    host: &str,
    profile: &str,
    source_harness: &str,
    target_harness: &str,
    spec: &str,
    path: &str,
    name: &str,
    branch: &str,
    clone_url: &str,
    source_session: &str,
) -> Result<String, String> {
    let recorded_wt = task["worktree"].as_str().unwrap_or("");
    let in_place = is_in_place(task);
    let (worktree, created) = if in_place {
        let dir = if recorded_wt.is_empty() {
            path
        } else {
            recorded_wt
        };
        (dir.to_string(), false)
    } else {
        ensure_worktree(host, path, name, branch, recorded_wt, clone_url)?
    };
    let new_session = match convert_on_host(
        host,
        profile,
        source_harness,
        target_harness,
        &worktree,
        source_session,
    ) {
        Ok(id) => id,
        Err(e) => {
            if created {
                let repo = task["repo"]
                    .as_str()
                    .filter(|r| !r.is_empty())
                    .unwrap_or(path);
                let _ = remove_worktree(host, repo, branch, false);
            }
            return Err(e);
        }
    };
    let resume_head = if in_place {
        None
    } else {
        branch_head(host, path, branch)
    };
    let (new_host, new_session_id, new_window_id) = launch(
        host,
        path,
        name,
        spec,
        None,
        None,
        Some((&new_session, profile)),
    )?;
    update_tasks(|tasks| {
        tasks.retain(|t| t["ended_at"].is_null() || !in_window(t, old_window));
        if let Some(new) = tasks
            .iter_mut()
            .rev()
            .find(|t| t["ended_at"].is_null() && in_window(t, &new_window_id))
        {
            if task["links"].as_array().is_some_and(|l| !l.is_empty()) {
                new["links"] = task["links"].clone();
            }
            if let Some(head) = &resume_head {
                new["resume_head"] = head.clone().into();
            }
        }
        Ok(())
    })?;
    Ok(format!("{new_host}\t{new_session_id}\t{new_window_id}"))
}

fn apply_switched_harness(
    task: &mut serde_json::Value,
    harness: &str,
    model: Option<&str>,
    agent_session: &str,
) {
    task["harness"] = harness.into();
    task["model"] = model.map_or(serde_json::Value::Null, Into::into);
    task["agent_session"] = agent_session.into();
}

/// Convert `source_session` on `host` via Atuin Threads, returning the new
/// `target_harness` session ID written for `worktree`.
fn convert_on_host(
    host: &str,
    profile: &str,
    source_harness: &str,
    target_harness: &str,
    worktree: &str,
    source_session: &str,
) -> Result<String, String> {
    if host == local_label() {
        return crate::atuin::convert_here(
            profile,
            source_harness,
            target_harness,
            Path::new(worktree),
            source_session,
        );
    }
    let script = format!(
        "switchyard task convert {} {} {} {} {} 2>&1\n",
        sh_quote(profile),
        sh_quote(source_harness),
        sh_quote(target_harness),
        sh_quote(worktree),
        sh_quote(source_session),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !text.is_empty() {
        return Ok(text);
    }
    let msg = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map_or("conversion failed", |l| {
            l.trim().strip_prefix("switchyard: ").unwrap_or(l.trim())
        });
    Err(format!("{host}: {msg}"))
}

/// Ensure the ended task's worktree exists on `host` before Atuin converts
/// its session (Atuin falls back to the current directory if the session's
/// original working directory is missing). Returns `(worktree_path, created)`
/// where `created` is true only when this call created it, so a failed
/// conversion can remove it again.
fn ensure_worktree(
    host: &str,
    path: &str,
    name: &str,
    branch: &str,
    recorded_wt: &str,
    clone_url: &str,
) -> Result<(String, bool), String> {
    let slug = branch_and_slug(name).1;
    let template = format!("worktree-path = \"{{{{ repo_path }}}}/.worktrees/{slug}\"");
    let script = ensure_worktree_script(path, branch, recorded_wt, clone_url, &template);
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let msg = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map_or("could not prepare worktree", str::trim);
        return Err(format!("{host}: {msg}"));
    }
    let last = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    if let Some(wt) = last.strip_prefix("reused\t") {
        return Ok((wt.to_string(), false));
    }
    if let Some(wt) = last.strip_prefix("created\t") {
        return Ok((wt.to_string(), true));
    }
    Err(format!("{host}: unexpected worktree result: {last}"))
}

fn ensure_worktree_script(
    path: &str,
    branch: &str,
    recorded_wt: &str,
    clone_url: &str,
    template: &str,
) -> String {
    format!(
        "expand() {{ case \"$1\" in '~/'*) printf '%s' \"$HOME/${{1#~/}}\" ;; '~') printf '%s' \"$HOME\" ;; *) printf '%s' \"$1\" ;; esac; }}\n\
         repo=$(expand {})\n\
         branch={}\n\
         recorded=$(expand {})\n\
         clone_url=$(expand {})\n\
         template={}\n\
         if [ -n \"$recorded\" ] && [ -d \"$recorded\" ]; then printf 'reused\\t%s\\n' \"$recorded\"; exit 0; fi\n\
         if [ ! -d \"$repo\" ]; then\n\
           if [ -z \"$clone_url\" ]; then echo \"Git folder $repo no longer exists\"; exit 1; fi\n\
           mkdir -p \"$(dirname \"$repo\")\"\n\
           git clone \"$clone_url\" \"$repo\" 2>&1 || exit 1\n\
         fi\n\
         find_wt() {{\n\
           wt=\n\
           while IFS= read -r line; do\n\
             case $line in\n\
               'worktree '*) wt=${{line#worktree }} ;;\n\
               \"branch refs/heads/$branch\") printf '%s' \"$wt\"; return 0 ;;\n\
             esac\n\
           done < <(git -C \"$repo\" worktree list --porcelain 2>/dev/null)\n\
           return 1\n\
         }}\n\
         if existing=$(find_wt); then printf 'reused\\t%s\\n' \"$existing\"; exit 0; fi\n\
         if git -C \"$repo\" rev-parse --verify --quiet \"refs/heads/$branch\" >/dev/null 2>&1; then\n\
           wt -C \"$repo\" --config-set \"$template\" switch \"$branch\" --no-cd --yes 2>&1 || exit 1\n\
         else\n\
           wt -C \"$repo\" --config-set \"$template\" switch --create \"$branch\" --no-cd --yes 2>&1 || exit 1\n\
         fi\n\
         if made=$(find_wt); then printf 'created\\t%s\\n' \"$made\"; exit 0; fi\n\
         echo \"wt made no worktree for $branch\"; exit 1\n",
        sh_quote(path),
        sh_quote(branch),
        sh_quote(recorded_wt),
        sh_quote(clone_url),
        sh_quote(template),
    )
}

/// `branch`'s head commit in the checkout at `repo` on `host`, in full; None
/// when it can't be read.
pub(crate) fn branch_head(host: &str, repo: &str, branch: &str) -> Option<String> {
    if branch.is_empty() {
        return None;
    }
    let script = format!(
        "git -C {} rev-parse --verify --quiet {}\n",
        sh_quote(repo),
        sh_quote(&format!("refs/heads/{branch}"))
    );
    let out = host_bash(host, &script).filter(|o| o.status.success())?;
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!head.is_empty()).then_some(head)
}

/// The archive's check of a worktree: exit 3 when it is gone, else its
/// `git status --porcelain` lines and a last `@STATE` line from `MERGED_SH`.
/// With no `branch` (a detached HEAD), `MERGED_SH` checks HEAD itself, and
/// an `@stray` line before the last says no branch or remote-tracking ref
/// contains HEAD, so removing the worktree would lose its commits.
fn archive_check(worktree: &str, branch: &str) -> String {
    format!(
        "cd {} 2>/dev/null || exit 3\n\
         status=$(git status --porcelain) || exit 1\n\
         [ -n \"$status\" ] && printf '%s\\n' \"$status\"\n\
         path=$PWD\nbranch={}\n\
         if [ -z \"$branch\" ]; then\n\
         wt_head=$(git rev-parse --verify --quiet HEAD) || exit 1\n\
         [ -n \"$(git for-each-ref --count=1 --contains \"$wt_head\" refs/heads refs/remotes)\" ] || echo @stray\n\
         fi\n\
         {MERGED_SH}\necho \"@$merged\"\n",
        sh_quote(worktree),
        sh_quote(branch),
    )
}

/// Pin or unpin the open task in `window`, as `pinned`; the outcome says
/// which.
fn pin(window: &str) -> Result<String, String> {
    update_tasks(|tasks| {
        let task = open_task(tasks, window)?;
        let pinned = !task["pinned"].as_bool().unwrap_or(false);
        set_or_remove(task, "pinned", pinned.then(|| true.into()));
        Ok(format!(
            "{} {}",
            if pinned { "pinned" } else { "unpinned" },
            label_of(task)
        ))
    })
}

/// Snooze the open task in `window` until `until` (Unix seconds), or wake
/// it for 0.
///
/// # Errors
///
/// Fails when `window` has no open task or the registry can't be updated.
pub fn snooze(window: &str, until: u64) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_snooze(window, until);
    }
    let label = update_tasks(|tasks| {
        let task = open_task(tasks, window)?;
        set_or_remove(task, "snoozed_until", (until > 0).then(|| until.into()));
        Ok(label_of(task))
    })?;
    crate::server::refresh();
    Ok(if until > 0 {
        format!("snoozed {label}")
    } else {
        format!("woke {label}")
    })
}

/// How long to let the agent take the typed text before pressing Enter.
const TYPE_SETTLE: Duration = Duration::from_millis(150);

/// The most text typed in one write. Claude takes a larger burst of input
/// (over about 800 bytes) for a paste, so it is typed in pieces this size.
const TYPE_CHUNK: usize = 256;

/// One step of typing a message into an agent's prompt.
#[derive(Debug, PartialEq, Eq)]
enum Typing {
    /// Text typed as-is, never more than `TYPE_CHUNK` bytes.
    Text(String),
    /// This many line breaks, each a shift-Enter: it starts a new line in
    /// Claude's and Pi's prompts, where a plain Enter would submit.
    Breaks(usize),
}

/// How to type `text`: its lines in chunks of at most `TYPE_CHUNK` bytes,
/// split on character boundaries, with the breaks between them. Tabs become
/// spaces (a typed tab would complete) and other control characters are
/// dropped, so nothing typed can act as a key.
fn typing(text: &str) -> Vec<Typing> {
    let mut steps = Vec::new();
    for (i, line) in text.replace("\r\n", "\n").split(['\n', '\r']).enumerate() {
        if i > 0 {
            match steps.last_mut() {
                Some(Typing::Breaks(n)) => *n += 1,
                _ => steps.push(Typing::Breaks(1)),
            }
        }
        let mut chunk = String::new();
        for c in line.chars() {
            let c = if c == '\t' { ' ' } else { c };
            if c.is_control() {
                continue;
            }
            if chunk.len() + c.len_utf8() > TYPE_CHUNK {
                steps.push(Typing::Text(std::mem::take(&mut chunk)));
            }
            chunk.push(c);
        }
        if !chunk.is_empty() {
            steps.push(Typing::Text(chunk));
        }
    }
    steps
}

/// Send `text` to the agent in `window` on `host`, as if John typed it in
/// the tab: typed text with shift-Enter between its lines (so newlines stay
/// in the message), then Enter. It is typed rather than pasted because an
/// agent takes a pasted message as quoted material rather than John's own
/// words, and may ask him to confirm it before acting on it. An
/// idle agent starts a turn on it; a working one takes it into the current
/// turn at its next chance (Claude after the running tool call, Pi as a
/// steering message). An agent waiting on a prompt (`blocked`) is refused,
/// since the keys would answer the prompt instead.
///
/// Files pasted into the text (see `images`) are taken from this machine.
///
/// # Errors
///
/// Fails when the text is empty, the tab or its agent pane can't be found,
/// the agent is waiting on a prompt, a pasted file can't be carried over,
/// or Rex refuses the typing or the Enter.
pub fn message(host: &str, session: &str, window: &str, text: &str) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        let local = local_label();
        return crate::server::remote_message(host, session, window, text, Some(&local));
    }
    message_from(&local_label(), host, session, window, text)
}

/// `message` with pasted files taken from host `from`.
///
/// # Errors
///
/// Fails as `message` does.
pub fn message_from(
    from: &str,
    host: &str,
    session: &str,
    window: &str,
    text: &str,
) -> Result<String, String> {
    message_from_hosts(&[from], host, session, window, text)
}

fn message_from_hosts(
    sources: &[&str],
    host: &str,
    session: &str,
    window: &str,
    text: &str,
) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to send".into());
    }
    let pane = crate::transcript::agent_pane(host, session, window)?;
    let block = pane["block"]
        .as_str()
        .filter(|b| !b.is_empty())
        .ok_or("no agent in that tab")?;
    let label = load_tasks()
        .ok()
        .and_then(|tasks| {
            tasks
                .iter()
                .find(|t| t["ended_at"].is_null() && in_window(t, window))
                .map(label_of)
        })
        .unwrap_or_else(|| "the agent".into());
    if pane["state"].as_str() == Some("blocked") {
        return Err(format!(
            "{label} is waiting on a prompt; answer it in its tab"
        ));
    }
    let text = carry_from(sources, host, text)?;
    for step in typing(&text) {
        match step {
            Typing::Text(chunk) => rex(host, &["send", "-s", session, "-b", block, "--", &chunk])?,
            Typing::Breaks(n) => {
                let mut args = vec!["send-key", "-s", session, "-b", block];
                args.extend(std::iter::repeat_n("shift+enter", n));
                rex(host, &args)?;
            }
        }
    }
    std::thread::sleep(TYPE_SETTLE);
    rex(host, &["send-key", "-s", session, "-b", block, "enter"])?;
    Ok(format!("sent to {label}"))
}

/// Interrupt the agent in `window` on `host`: press Escape in its pane, as
/// John would in the tab, which stops Claude's or Pi's current turn. Only a
/// working agent is interrupted, going by its pane's status at the moment:
/// Escape would dismiss a blocked agent's prompt, and twice at an idle
/// Claude's prompt opens its rewind menu.
///
/// # Errors
///
/// Fails when the tab or its agent pane can't be found, the agent isn't
/// working, or Rex refuses the key.
pub fn interrupt(host: &str, session: &str, window: &str) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_interrupt(host, session, window);
    }
    let pane = crate::transcript::agent_pane(host, session, window)?;
    let block = pane["block"]
        .as_str()
        .filter(|b| !b.is_empty())
        .ok_or("no agent in that tab")?;
    let label = load_tasks()
        .ok()
        .and_then(|tasks| {
            tasks
                .iter()
                .find(|t| t["ended_at"].is_null() && in_window(t, window))
                .map(label_of)
        })
        .unwrap_or_else(|| "the agent".into());
    match pane["state"].as_str() {
        Some("working") => {}
        Some("blocked") => {
            return Err(format!(
                "{label} is waiting on a prompt; answer it in its tab"
            ));
        }
        _ => return Err(format!("{label} isn't working: nothing to interrupt")),
    }
    rex(host, &["send-key", "-s", session, "-b", block, "escape"])?;
    Ok(format!("interrupted {label}"))
}

/// One answer to a question in an interactive `agy` `ask_question` prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// 0-based preset option indices to select (at most 1 for single-select;
    /// empty when answering single-select purely via `write_in`).
    #[serde(default)]
    pub selected: Vec<usize>,
    /// Optional custom write-in text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_in: Option<String>,
    /// Whether this question uses multi-select checkboxes (`space` toggles).
    #[serde(default)]
    pub multi_select: bool,
    /// Number of preset options (`Write-in...` sits at index `option_count`).
    pub option_count: usize,
}

/// What to submit to an `agy` `ask_question` prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionResponse {
    Answers { answers: Vec<QuestionAnswer> },
    Skip,
}

/// Short settle between key bursts while driving `agy`'s question UI.
const QUESTION_KEY_SETTLE: Duration = Duration::from_millis(80);
/// Short settle between questions so `agy` renders the next screen before
/// we inspect it.
const QUESTION_STEP_SETTLE: Duration = Duration::from_millis(120);

/// Parsed state of `agy`'s live `Question` prompt from a plain screen capture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AgyQuestionScreen {
    question_num: Option<usize>,
    question_total: Option<usize>,
    cursor: Option<usize>,
    checked: Vec<usize>,
    last_index: Option<usize>,
    write_in_open: bool,
}

/// Parse the last `Question` widget visible in a plain terminal capture of
/// `agy`.
fn parse_agy_question_screen(screen: &str) -> AgyQuestionScreen {
    let lines: Vec<&str> = screen.lines().collect();
    let start = lines
        .iter()
        .rposition(|l| l.trim() == "Question")
        .unwrap_or_else(|| lines.len().saturating_sub(40));
    let mut state = AgyQuestionScreen::default();
    for raw in &lines[start..] {
        let trimmed = raw.trim();
        if trimmed == "Your answer:" {
            state.write_in_open = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Question ")
            && let Some((nums, _)) = rest.split_once(':')
            && let Some((cur, total)) = nums.split_once('/')
            && let (Ok(c), Ok(t)) = (cur.trim().parse::<usize>(), total.trim().parse::<usize>())
        {
            state.question_num = Some(c);
            state.question_total = Some(t);
            continue;
        }
        let (is_cursor, rest) = match trimmed.strip_prefix('>') {
            Some(r) => (true, r.trim_start()),
            None => (false, trimmed),
        };
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0
            && rest[digits..].starts_with(". ")
            && let Ok(num) = rest[..digits].parse::<usize>()
            && num > 0
        {
            let idx = num - 1;
            if is_cursor {
                state.cursor = Some(idx);
            }
            let body = &rest[digits + 2..];
            if body.starts_with("[x]") || body.starts_with("[X]") {
                state.checked.push(idx);
            }
            state.last_index = Some(state.last_index.map_or(idx, |m| m.max(idx)));
        }
    }
    state
}

/// Sanitize write-in text into single-line chunks safe to type into `agy`'s
/// `Your answer:` input.
fn sanitize_write_in(text: &str) -> Vec<String> {
    let collapsed = text.replace("\r\n", " ").replace(['\n', '\r', '\t'], " ");
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for c in collapsed.trim().chars() {
        if c.is_control() {
            continue;
        }
        if chunk.len() + c.len_utf8() > TYPE_CHUNK {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

fn send_keys(host: &str, session: &str, block: &str, keys: &[&str]) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut args = vec!["send-key", "-s", session, "-b", block];
    args.extend_from_slice(keys);
    rex(host, &args)?;
    Ok(())
}

fn move_cursor(
    host: &str,
    session: &str,
    block: &str,
    from: usize,
    to: usize,
) -> Result<(), String> {
    if to == from {
        return Ok(());
    }
    let (key, count) = if to > from {
        ("down", to - from)
    } else {
        ("up", from - to)
    };
    let keys = vec![key; count];
    send_keys(host, session, block, &keys)
}

fn send_write_in(host: &str, session: &str, block: &str, chunks: &[String]) -> Result<(), String> {
    send_keys(host, session, block, &["enter"])?;
    std::thread::sleep(QUESTION_KEY_SETTLE);
    for chunk in chunks {
        rex(host, &["send", "-s", session, "-b", block, "--", chunk])?;
    }
    std::thread::sleep(TYPE_SETTLE);
    send_keys(host, session, block, &["enter"])
}

fn verify_agy_question_pane(
    host: &str,
    session: &str,
    window: &str,
    label: &str,
) -> Result<(String, AgyQuestionScreen), String> {
    let pane = crate::transcript::pane_with_screen(host, session, window)?;
    let block = pane["block"]
        .as_str()
        .filter(|b| !b.is_empty())
        .ok_or("no agent in that tab")?;
    let app = pane["app"].as_str().unwrap_or("");
    if !matches!(app, "antigravity" | "agy") {
        return Err(format!(
            "{label} is not an Antigravity agent; answer it in its tab"
        ));
    }
    if pane["state"].as_str() != Some("blocked") || pane["kind"].as_str() != Some("question") {
        return Err(format!("{label} is no longer waiting on a question"));
    }
    let screen = parse_agy_question_screen(pane["screen"].as_str().unwrap_or(""));
    Ok((block.to_string(), screen))
}

/// Answer (or skip) an `agy` agent's pending `ask_question` prompt in `window`
/// on `host` by driving its live question TUI through Rex.
///
/// # Errors
///
/// Fails when the tab or agent pane can't be found, the agent is no longer
/// blocked on an `agy` `ask_question` prompt, an answer is invalid, or Rex
/// refuses the keys.
pub fn answer_question(
    host: &str,
    session: &str,
    window: &str,
    response: &QuestionResponse,
) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_answer(host, session, window, response);
    }
    let label = load_tasks()
        .ok()
        .and_then(|tasks| {
            tasks
                .iter()
                .find(|t| t["ended_at"].is_null() && in_window(t, window))
                .map(label_of)
        })
        .unwrap_or_else(|| "the agent".into());
    let (block, mut screen) = verify_agy_question_pane(host, session, window, &label)?;
    if screen.write_in_open {
        send_keys(host, session, &block, &["escape"])?;
        std::thread::sleep(QUESTION_KEY_SETTLE);
        screen = verify_agy_question_pane(host, session, window, &label)?.1;
    }
    match response {
        QuestionResponse::Skip => {
            send_keys(host, session, &block, &["escape"])?;
            Ok(format!("skipped question for {label}"))
        }
        QuestionResponse::Answers { answers } => {
            if answers.is_empty() {
                return Err("no answers to submit".into());
            }
            if let Some(cur_q) = screen.question_num
                && cur_q > 1
            {
                let backs = vec!["left"; cur_q - 1];
                send_keys(host, session, &block, &backs)?;
                std::thread::sleep(QUESTION_STEP_SETTLE);
            }
            for (i, ans) in answers.iter().enumerate() {
                let (_, cur_screen) = if i == 0 && screen.question_num.unwrap_or(1) == 1 {
                    (block.clone(), screen.clone())
                } else {
                    verify_agy_question_pane(host, session, window, &label)?
                };
                apply_agy_answer(host, session, &block, ans, &cur_screen)?;
                if i + 1 < answers.len() {
                    std::thread::sleep(QUESTION_STEP_SETTLE);
                }
            }
            Ok(format!("answered {label}"))
        }
    }
}

fn apply_agy_answer(
    host: &str,
    session: &str,
    block: &str,
    ans: &QuestionAnswer,
    screen: &AgyQuestionScreen,
) -> Result<(), String> {
    let write_in_idx = screen.last_index.unwrap_or(ans.option_count);
    let mut cur = screen.cursor.unwrap_or(0);
    let write_in = ans
        .write_in
        .as_deref()
        .map(sanitize_write_in)
        .filter(|c| !c.is_empty());

    if ans.multi_select {
        for idx in 0..ans.option_count {
            let want = ans.selected.contains(&idx);
            let have = screen.checked.contains(&idx);
            if want != have {
                move_cursor(host, session, block, cur, idx)?;
                cur = idx;
                send_keys(host, session, block, &["space"])?;
                std::thread::sleep(Duration::from_millis(40));
            }
        }
        if let Some(chunks) = write_in {
            move_cursor(host, session, block, cur, write_in_idx)?;
            send_write_in(host, session, block, &chunks)?;
        } else {
            if cur >= write_in_idx {
                move_cursor(host, session, block, cur, 0)?;
            }
            send_keys(host, session, block, &["enter"])?;
        }
        return Ok(());
    }

    if let Some(chunks) = write_in {
        move_cursor(host, session, block, cur, write_in_idx)?;
        send_write_in(host, session, block, &chunks)
    } else if let Some(&opt) = ans.selected.first() {
        if opt >= write_in_idx {
            return Err(format!("option {} out of range", opt + 1));
        }
        move_cursor(host, session, block, cur, opt)?;
        send_keys(host, session, block, &["enter"])
    } else {
        Err("no option selected for question".into())
    }
}

/// `message` with the text of a prompt file (`LABEL:ID`, see `prompts`),
/// which is removed once sent.
///
/// # Errors
///
/// Fails when the prompt reference is malformed or its file can't be
/// fetched, or as `message` does.
pub fn message_ref(
    host: &str,
    session: &str,
    window: &str,
    prompt_ref: &str,
) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        let local = local_label();
        let (from, _) = parse_prompt_ref(prompt_ref)?;
        if from == local {
            let text = fetch_prompt(prompt_ref)?;
            let sent = crate::server::remote_message(host, session, window, &text, Some(from))?;
            discard_prompt(prompt_ref);
            return Ok(sent);
        }
        return crate::server::remote_message_ref(host, session, window, prompt_ref, Some(&local));
    }
    message_ref_from(None, host, session, window, prompt_ref)
}

/// `message_ref` with an optional caller host (`caller`) used as a fallback
/// source for pasted local image paths when the prompt file lives on another
/// host.
///
/// # Errors
///
/// Fails as `message_ref` does.
pub fn message_ref_from(
    caller: Option<&str>,
    host: &str,
    session: &str,
    window: &str,
    prompt_ref: &str,
) -> Result<String, String> {
    let text = fetch_prompt(prompt_ref)?;
    let (from, _) = parse_prompt_ref(prompt_ref)?;
    let local = local_label();
    let fallback = caller.unwrap_or(&local);
    let sent = message_from_hosts(&[from, fallback], host, session, window, &text)?;
    discard_prompt(prompt_ref);
    Ok(sent)
}

fn open_task<'a>(
    tasks: &'a mut [serde_json::Value],
    window: &str,
) -> Result<&'a mut serde_json::Value, String> {
    tasks
        .iter_mut()
        .find(|t| t["ended_at"].is_null() && in_window(t, window))
        .ok_or_else(|| "no open task for that tab".to_string())
}

pub(crate) fn label_of(task: &serde_json::Value) -> String {
    format!(
        "{}/{}",
        task["project"].as_str().unwrap_or(""),
        task["name"].as_str().unwrap_or("")
    )
}

fn set_or_remove(task: &mut serde_json::Value, key: &str, value: Option<serde_json::Value>) {
    match (value, task.as_object_mut()) {
        (Some(v), Some(obj)) => {
            obj.insert(key.into(), v);
        }
        (None, Some(obj)) => {
            obj.remove(key);
        }
        _ => {}
    }
}

/// Run tab-picker `action` (`pin`, `archive`, `link`, ...) on the task in
/// `window`, then have switchyard-server refresh unless it only opened a
/// work item.
///
/// # Errors
///
/// Fails with the action's message when it is unknown or doesn't succeed.
pub fn act(action: &str, host: &str, session: &str, window: &str) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_act(action, host, session, window);
    }
    let result = match action {
        "pin" => pin(window),
        _ => run_action(action, host, session, window),
    };
    // Even a failed archive may have changed the registry or closed the
    // tab: have switchyard-server check now rather than at its next poll.
    // Opening a work item changes nothing.
    if action != "link" {
        crate::server::refresh();
    }
    result
}

/// Archive (or force-archive) the task in `window` as `act` does, then each
/// open task it spawned (`parent_window`), and theirs in turn, so archiving a
/// lead agent doesn't leave its helpers running. Children always get a plain
/// archive: one with uncommitted changes is kept and named in the outcome,
/// since forcing the parent only answered for the parent's own changes.
///
/// # Errors
///
/// Fails as `act` does when the parent's archive fails, leaving its children
/// alone; a child that can't be archived only adds to the message.
pub fn archive_family(
    action: &str,
    host: &str,
    session: &str,
    window: &str,
) -> Result<String, String> {
    if crate::server::remote_url().is_some() {
        return crate::server::remote_archive_family(action, host, session, window);
    }
    let mut outcome = run_action(action, host, session, window);
    if outcome.is_ok() {
        let mut seen = std::collections::HashSet::from([window.to_string()]);
        let mut queue = vec![window.to_string()];
        let (mut archived, mut kept) = (0, Vec::new());
        while let Some(parent) = queue.pop() {
            let Ok(tasks) = load_tasks() else { break };
            for (h, s, w) in open_children(&tasks, &parent) {
                if !seen.insert(w.clone()) {
                    continue;
                }
                match run_action("archive", &h, &s, &w) {
                    Ok(_) => {
                        archived += 1;
                        queue.push(w);
                    }
                    Err(e) => kept.push(e),
                }
            }
        }
        if let Ok(msg) = &mut outcome {
            if archived > 0 {
                let s = if archived == 1 { "" } else { "s" };
                let _ = write!(msg, " and {archived} child task{s}");
            }
            for e in &kept {
                let _ = write!(msg, "; kept {e}");
            }
        }
    }
    crate::server::refresh();
    outcome
}

/// Each open task in `tasks` spawned from the tab `window`, as host,
/// session and window.
fn open_children(tasks: &[serde_json::Value], window: &str) -> Vec<(String, String, String)> {
    let s = |t: &serde_json::Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    tasks
        .iter()
        .filter(|t| t["ended_at"].is_null() && s(t, "parent_window") == window)
        .map(|t| (s(t, "host"), s(t, "session_id"), s(t, "window_id")))
        .filter(|(_, _, w)| !w.is_empty() && w != window)
        .collect()
}

/// Another open task on task `i`'s host whose worktree is task `i`'s, as
/// PROJECT/NAME.
pub(crate) fn worktree_user(tasks: &[serde_json::Value], i: usize) -> Option<String> {
    let s = |t: &serde_json::Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    let path = |t: &serde_json::Value| s(t, "worktree").trim_end_matches('/').to_string();
    let me = &tasks[i];
    if path(me).is_empty() {
        return None;
    }
    tasks
        .iter()
        .enumerate()
        .find(|&(j, t)| {
            j != i
                && t["ended_at"].is_null()
                && s(t, "host") == s(me, "host")
                && path(t) == path(me)
        })
        .map(|(_, t)| format!("{}/{}", s(t, "project"), s(t, "name")))
}

fn run_action(action: &str, host: &str, session: &str, window: &str) -> Result<String, String> {
    if action == "resume" {
        return resume(host, window);
    }
    let mut tasks = load_tasks()?;
    let archiving = matches!(action, "archive" | "force-archive");
    let i = match action_task(&mut tasks, archiving, host, session, window)? {
        Target::Task(i) => i,
        Target::Folder(cwd) => {
            let _ = rex(host, &["window", "close", "-s", session, window]);
            kill_if_empty(host, session);
            return Ok(format!("closed the tab; {cwd} isn't a Git checkout"));
        }
    };
    if archiving && tasks[i]["ended_at"].is_null() {
        save_record(&tasks[i])?;
    }
    let ended = !tasks[i]["ended_at"].is_null();
    let field = |k: &str| tasks[i][k].as_str().unwrap_or("").to_string();
    let (label, worktree, repo) = (
        format!("{}/{}", field("project"), field("name")),
        field("worktree"),
        field("repo"),
    );

    match action {
        // The task's main work item (see `links::primary`), for the caller
        // to open where John is.
        "link" => crate::links::primary(&tasks[i])
            .and_then(|l| l["url"].as_str())
            .map(String::from)
            .ok_or_else(|| format!("{label} has no linked work item")),
        // An in-place project (`worktree = false`) has no worktree to check or
        // remove: capture its session, close the tab, and mark it archived.
        "archive" | "force-archive" if is_in_place(&tasks[i]) => {
            end_keeping_worktree(&mut tasks[i], host, session, window)?;
            Ok(format!("archived {label}"))
        }
        // A primary checkout is never removed: close the tab and end the task.
        "archive" | "force-archive"
            if !worktree.is_empty()
                && worktree.trim_end_matches('/') == repo.trim_end_matches('/') =>
        {
            if !ended {
                let _ = rex(host, &["window", "close", "-s", session, window]);
                tasks[i]["ended_at"] = now().into();
            }
            kill_if_empty(host, session);
            tasks[i]["ended_reason"] = "archived".into();
            save_record(&tasks[i])?;
            Ok(format!(
                "archived {label}; kept the primary checkout {worktree}"
            ))
        }
        // Another open task works in the same worktree (spawned on the
        // same branch): keep it for that task, as for a primary checkout.
        "archive" | "force-archive" => match worktree_user(&tasks, i) {
            Some(other) => {
                end_keeping_worktree(&mut tasks[i], host, session, window)?;
                Ok(format!(
                    "archived {label}; kept {worktree}, which {other} still uses"
                ))
            }
            None => archive_worktree(
                &mut tasks[i],
                action == "force-archive",
                host,
                session,
                window,
            ),
        },
        _ => Err(format!("unknown action {action}")),
    }
}

/// Archive `task` but leave its worktree: close its tab and end it.
fn end_keeping_worktree(
    task: &mut serde_json::Value,
    host: &str,
    session: &str,
    window: &str,
) -> Result<(), String> {
    end_keeping_worktree_with(
        task,
        newest_session,
        || {
            let _ = rex(host, &["window", "close", "-s", session, window]);
        },
        || kill_if_empty(host, session),
        save_record,
    )
}

fn end_keeping_worktree_with(
    task: &mut serde_json::Value,
    find_session: impl FnOnce(&serde_json::Value) -> Option<String>,
    close_tab: impl FnOnce(),
    cleanup_session: impl FnOnce(),
    save: impl FnOnce(&serde_json::Value) -> Result<(), String>,
) -> Result<(), String> {
    if task["ended_at"].is_null() {
        if let Some(id) = find_session(task) {
            task["agent_session"] = id.into();
        }
        close_tab();
        task["ended_at"] = now().into();
    }
    cleanup_session();
    task["ended_reason"] = "archived".into();
    save(task)
}

/// Archive `task`, whose worktree is its own (not the primary checkout):
/// refuse uncommitted changes unless `force`, close its tab, remove the
/// worktree, and end the task.
fn archive_worktree(
    task: &mut serde_json::Value,
    force: bool,
    host: &str,
    session: &str,
    window: &str,
) -> Result<String, String> {
    let ended = !task["ended_at"].is_null();
    let field = |k: &str| task[k].as_str().unwrap_or("").to_string();
    let (label, worktree, repo, branch) = (
        format!("{}/{}", field("project"), field("name")),
        field("worktree"),
        field("repo"),
        field("branch"),
    );
    // Exit 3: the worktree is already gone. Any status line:
    // uncommitted changes. The last line, `@STATE`, says whether the
    // branch is merged (see MERGED_SH).
    let out = host_bash(host, &archive_check(&worktree, &branch))
        .ok_or_else(|| format!("{host} can't be reached"))?;
    let exists = out.status.code() != Some(3);
    if exists && !force && !out.status.success() {
        return Err(format!(
            "{label}: can't read the worktree's status on {host}"
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let status: Vec<&str> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('@'))
        .collect();
    let branch_merged = text.lines().any(|l| l == "@merged");
    let stray = text.lines().any(|l| l == "@stray");
    // Safe to archive: the work is merged (its PR, or the branch
    // itself) or all its cards closed, and nothing is left uncommitted.
    let merged = exists
        && out.status.success()
        && status.is_empty()
        && (branch_merged || crate::links::pr_merged(task) || crate::cards::cards_closed(task));
    if exists && !force && !status.is_empty() {
        return Err(uncommitted_refusal(&label, &status));
    }
    if exists && !force && stray {
        return Err(format!(
            "Archive refused: {label} is on a detached HEAD with commits no branch has. \
             Press ⌃X, Enter, Enter in the tab picker (⌘K) to remove it anyway."
        ));
    }
    if ended && !exists {
        task["ended_reason"] = "worktree removed".into();
        save_record(task)?;
        return Err(format!("{label}: its worktree is already gone"));
    }
    // The newest session, not spawn's choice: a /clear starts
    // another. An ended task recorded it when it ended.
    if !ended && let Some(id) = newest_session(task) {
        task["agent_session"] = id.into();
    }
    // Close the tab first so the agent isn't left in a deleted
    // directory; a tab that is already gone is fine.
    if !ended {
        let _ = rex(host, &["window", "close", "-s", session, window]);
    }
    let (note, failed) = if exists {
        // A detached HEAD has no branch to name it by: Worktrunk takes
        // the worktree's path instead.
        let target = if branch.is_empty() {
            &worktree
        } else {
            &branch
        };
        remove_worktree(host, &repo, target, force)?
    } else {
        (String::new(), false)
    };
    kill_if_empty(host, session);
    if !ended {
        task["ended_at"] = now().into();
    }
    task["ended_reason"] = if merged {
        "merged"
    } else if force {
        "force-archived"
    } else {
        "archived"
    }
    .into();
    save_record(task)?;
    if failed {
        return Err(format!("{label}: tab closed{note}"));
    }
    Ok(format!(
        "archived {label}{}{note}",
        if merged { " (merged)" } else { "" }
    ))
}

/// How long an agent must have been idle on landed work before
/// `auto_archive` archives its task, so its last message can still be read.
pub const AUTO_ARCHIVE_IDLE_SECS: u64 = 300;

/// Whether reconciled task `t` (as `reconcile_tasks` returns it, with its
/// unsaved fields) is due to be archived on its own at `now`: it is open,
/// or ended with only its tab closed, and not pinned, has its own clean
/// worktree, its work has landed (a PR GitHub reported merged with the
/// branch head as its head, or the branch head in the default branch, local
/// or origin's, as after a direct push), the branch head has moved since a
/// resume (`resume_head`), and, when open, its agent is neither working,
/// blocked nor scheduled, and hasn't changed state for
/// `AUTO_ARCHIVE_IDLE_SECS`. A closed tab has no agent left to wait for.
/// Closed cards alone don't count: only the work itself landing does.
#[must_use]
pub fn auto_archivable(t: &serde_json::Value, now: u64) -> bool {
    let s = |k: &str| t[k].as_str().unwrap_or("");
    let own_worktree = !s("worktree").is_empty()
        && s("worktree").trim_end_matches('/') != s("repo").trim_end_matches('/');
    let head = s("branch_head");
    let landed = s("branch_merged") == "merged" || crate::links::pr_merged_at(t, head);
    // A resumed task: only work landed since the resume counts.
    let moved =
        s("resume_head").is_empty() || (!head.is_empty() && !s("resume_head").starts_with(head));
    let open = t["ended_at"].is_null();
    let quiet = !open
        || !matches!(s("status"), "working" | "blocked" | "scheduled")
            && now.saturating_sub(t["status_at"].as_u64().unwrap_or(0)) >= AUTO_ARCHIVE_IDLE_SECS;
    (open || s("ended_reason") == "tab closed")
        && !t["pinned"].as_bool().unwrap_or(false)
        && own_worktree
        && t["dirty_count"].as_u64() == Some(0)
        && landed
        && moved
        && quiet
}

/// Archive each task in reconciled `tasks` that `auto_archivable` picks, as
/// the tab picker's archive does (it still refuses uncommitted changes, and
/// the task ends as "merged"), and remove each orphan worktree that
/// `orphans::auto_removable` picks and no Rex pane on its host sits in, the
/// same way (`wt remove` without `--force`). Returns each outcome, for the
/// caller to log.
#[must_use]
pub fn auto_archive(tasks: &[serde_json::Value]) -> Vec<Result<String, String>> {
    let now = now();
    let s = |t: &serde_json::Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    let mut outcomes: Vec<Result<String, String>> = tasks
        .iter()
        .filter(|t| auto_archivable(t, now))
        .map(|t| {
            run_action(
                "archive",
                &s(t, "host"),
                &s(t, "session_id"),
                &s(t, "window_id"),
            )
        })
        .collect();
    let orphans: Vec<&serde_json::Value> = tasks
        .iter()
        .filter(|t| crate::orphans::auto_removable(t))
        .collect();
    let mut cwds: std::collections::HashMap<String, Option<Vec<String>>> =
        std::collections::HashMap::new();
    for t in orphans {
        let host = s(t, "host");
        let worktree = s(t, "worktree");
        let here = cwds
            .entry(host.clone())
            .or_insert_with(|| crate::orphans::pane_cwds(&host));
        match here {
            None => outcomes.push(Err(format!(
                "kept orphan {host}:{worktree}: can't list {host}'s panes"
            ))),
            Some(c) if crate::orphans::occupied(c, &worktree) => {}
            Some(_) => outcomes.push(
                run_action("archive", &host, "", &s(t, "window_id"))
                    .map(|m| format!("orphan {host}:{worktree}: {m}")),
            ),
        }
    }
    outcomes
}

/// The index in `tasks` of the task an action on `window` acts on: an open
/// task. For an archive of a tab that is still open, the latest ended task
/// reopened (it ended by mistake), or else one adopted for the agent in it,
/// so any agent tab can be archived. Otherwise the latest ended task
/// (closing its tab left the worktree). An agent outside any Git checkout
/// has no task: its archive only closes the tab.
fn action_task(
    tasks: &mut Vec<serde_json::Value>,
    archiving: bool,
    host: &str,
    session: &str,
    window: &str,
) -> Result<Target, String> {
    let here = |t: &serde_json::Value| in_window(t, window);
    if let Some(i) = tasks
        .iter()
        .position(|t| t["ended_at"].is_null() && here(t))
    {
        return Ok(Target::Task(i));
    }
    if archiving && agent_block(host, session, window).is_ok() {
        // The caller saves it now, so a refusal (uncommitted changes) still
        // leaves the picker a task to show them on.
        if let Some(i) = tasks.iter().rposition(here) {
            let task = tasks[i].as_object_mut().ok_or("bad task record")?;
            task.insert("ended_at".into(), serde_json::Value::Null);
            task.insert("ended_reason".into(), serde_json::Value::Null);
            return Ok(Target::Task(i));
        }
        return match adopt(host, session, window)? {
            Adopted::Task(task) => {
                tasks.push(task);
                Ok(Target::Task(tasks.len() - 1))
            }
            Adopted::Folder(cwd) => Ok(Target::Folder(cwd)),
        };
    }
    tasks
        .iter()
        .rposition(here)
        .map(Target::Task)
        .ok_or_else(|| "no task for that tab".into())
}

/// What an action on a tab acts on (see `action_task`).
enum Target {
    /// The index of its task in the registry.
    Task(usize),
    /// The folder of an agent outside any Git checkout.
    Folder(String),
}

/// Why an archive of `label` is refused, naming the first few files of its
/// uncommitted `status` lines (`git status --porcelain`).
fn uncommitted_refusal(label: &str, status: &[&str]) -> String {
    let files: Vec<&str> = status.iter().map(|l| l.get(3..).unwrap_or(l)).collect();
    let shown = files.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    let more = if files.len() > 3 {
        format!(" and {} more", files.len() - 3)
    } else {
        String::new()
    };
    format!(
        "Archive refused: {label} has uncommitted changes in {shown}{more}. \
         Press ⌃X, Enter, Enter in the tab picker (⌘K) to throw them away."
    )
}

/// Remove the worktree of `repo` on `host` that `target` (its branch, or
/// its path for a detached HEAD) names with Worktrunk, returning
/// a note for the outcome (`; ...`) and whether the removal failed.
pub(crate) fn remove_worktree(
    host: &str,
    repo: &str,
    target: &str,
    force: bool,
) -> Result<(String, bool), String> {
    let remove = format!(
        // Worktrunk deletes the files in the background (its log is under
        // .git/wt/logs), so a big worktree doesn't hold up the picker; it
        // still refuses an unsafe removal here.
        "wt -C {} remove {}{} 2>&1",
        sh_quote(repo),
        if force { "--force " } else { "" },
        sh_quote(target)
    );
    let out = host_bash(host, &remove).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let last = text
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        return Ok((format!("; wt remove failed: {last}"), true));
    }
    // Worktrunk's own lines, not the indented files it lists.
    let summary: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace))
        .map(|l| l.trim().trim_end_matches(':'))
        .collect();
    let note = if summary.is_empty() {
        String::new()
    } else {
        format!("; {}", summary.join(" · "))
    };
    Ok((note, false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn open_children_are_open_tasks_spawned_from_the_window() {
        let tasks = vec![
            serde_json::json!({"host":"h","session_id":"s","window_id":"lead","ended_at":null}),
            serde_json::json!({"host":"h","session_id":"s","window_id":"kid","parent_window":"lead","ended_at":null}),
            serde_json::json!({"host":"m","session_id":"t","window_id":"far","parent_window":"lead","ended_at":null}),
            serde_json::json!({"host":"h","session_id":"s","window_id":"old","parent_window":"lead","ended_at":5}),
            serde_json::json!({"host":"h","session_id":"s","window_id":"grand","parent_window":"kid","ended_at":null}),
        ];
        assert_eq!(
            open_children(&tasks, "lead"),
            [
                ("h".into(), "s".into(), "kid".into()),
                ("m".into(), "t".into(), "far".into())
            ]
        );
        assert_eq!(open_children(&tasks, "grand"), []);
    }

    #[test]
    fn worktree_user_is_another_open_task_in_the_same_worktree() {
        let task = |win: &str, host: &str, wt: &str, ended: Option<u64>| {
            serde_json::json!({"host":host,"window_id":win,"worktree":wt,
                "project":"p","name":win,"ended_at":ended})
        };
        let tasks = vec![
            task("claude", "h", "/r/.worktrees/x", None),
            task("agy", "h", "/r/.worktrees/x/", None),
            task("done", "h", "/r/.worktrees/x", Some(5)),
            task("far", "m", "/r/.worktrees/y", None),
            task("none", "h", "", None),
        ];
        assert_eq!(worktree_user(&tasks, 0).as_deref(), Some("p/agy"));
        assert_eq!(worktree_user(&tasks, 1).as_deref(), Some("p/claude"));
        assert_eq!(worktree_user(&tasks, 2).as_deref(), Some("p/claude"));
        assert_eq!(worktree_user(&tasks, 3), None);
        assert_eq!(worktree_user(&tasks, 4), None);
    }

    #[test]
    fn typing_breaks_lines_with_shift_enter() {
        assert_eq!(
            typing("one\r\ntwo\n\n\tthree"),
            [
                Typing::Text("one".into()),
                Typing::Breaks(1),
                Typing::Text("two".into()),
                Typing::Breaks(2),
                Typing::Text(" three".into()),
            ]
        );
    }

    #[test]
    fn typing_chunks_long_lines_on_character_boundaries() {
        let line = format!("{}é{}", "a".repeat(TYPE_CHUNK - 1), "b".repeat(10));
        assert_eq!(
            typing(&line),
            [
                Typing::Text("a".repeat(TYPE_CHUNK - 1)),
                Typing::Text(format!("é{}", "b".repeat(10))),
            ]
        );
    }

    #[test]
    fn typing_drops_control_characters() {
        assert_eq!(typing("a\u{1b}[2Jb"), [Typing::Text("a[2Jb".into())]);
    }

    #[test]
    fn checkout_script_finds_the_checkout_repo_and_branch() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("switchyard-adopt-{}-{unique}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args([
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@t",
                        "-c",
                        "commit.gpgsign=false"
                    ])
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?}"
            );
        };
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        let wt = root.join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/x",
                wt.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::create_dir_all(wt.join("sub")).unwrap();
        let run = |dir: &std::path::Path| {
            let out = Command::new("bash")
                .args(["-c", &checkout_script(dir.to_str().unwrap())])
                .output()
                .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let (r, w) = (repo.to_str().unwrap(), wt.to_str().unwrap());
        // From a subfolder of a linked worktree.
        assert_eq!(
            run(&wt.join("sub")),
            (Some(0), format!("{w}\t{r}\tfeature/x\n"))
        );
        // The primary checkout is its own repo, which the archive keeps.
        assert_eq!(run(&repo), (Some(0), format!("{r}\t{r}\tmain\n")));
        assert_eq!(run(&root).0, Some(3));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn archive_check_reports_status_and_merge_state() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "switchyard-archive-{}-{unique}",
            std::process::id()
        ));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |dir: &std::path::Path, args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args([
                        "-c",
                        "user.name=t",
                        "-c",
                        "user.email=t@t",
                        "-c",
                        "commit.gpgsign=false"
                    ])
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?}"
            );
        };
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        let wt = root.join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/x",
                wt.to_str().unwrap(),
                "main",
            ],
        );
        git(&wt, &["commit", "--quiet", "--allow-empty", "-m", "work"]);
        let run_on = |branch: &str| {
            let out = Command::new("bash")
                .args(["-c", &archive_check(wt.to_str().unwrap(), branch)])
                .output()
                .unwrap();
            (
                out.status.code(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let run = || run_on("feature/x");
        assert_eq!(run(), (Some(0), "@unmerged\n".into()));
        git(&repo, &["merge", "--quiet", "--ff-only", "feature/x"]);
        assert_eq!(run(), (Some(0), "@merged\n".into()));
        // A detached HEAD whose branch is gone: HEAD itself is checked,
        // and it is in main.
        git(&wt, &["checkout", "--quiet", "--detach"]);
        git(&repo, &["branch", "--quiet", "-D", "feature/x"]);
        assert_eq!(run_on(""), (Some(0), "@merged\n".into()));
        // A commit made on the detached HEAD is in no branch.
        git(&wt, &["commit", "--quiet", "--allow-empty", "-m", "stray"]);
        assert_eq!(run_on(""), (Some(0), "@stray\n@unmerged\n".into()));
        std::fs::write(wt.join("f"), "x").unwrap();
        assert_eq!(run_on(""), (Some(0), "?? f\n@stray\n@unmerged\n".into()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(run().0, Some(3));
    }

    #[test]
    fn auto_archive_needs_landed_clean_quiet_work() {
        let now = 10_000;
        let base = serde_json::json!({
            "ended_at": null,
            "worktree": "/r/.worktrees/x",
            "repo": "/r",
            "dirty_count": 0,
            "branch_merged": "merged",
            "branch_head": "abc123def456",
            "status": "idle",
            "status_at": now - AUTO_ARCHIVE_IDLE_SECS,
        });
        assert!(auto_archivable(&base, now));
        let with = |k: &str, v: serde_json::Value| {
            let mut t = base.clone();
            t[k] = v;
            t
        };
        // A PR GitHub reported merged counts as landed too, when the branch
        // head is the PR's: later commits haven't landed (a squash merge).
        let mut pr = with("branch_merged", "unmerged".into());
        pr["links"] =
            serde_json::json!([{"kind": "pr", "state": "merged", "head": "abc123def4560000"}]);
        assert!(auto_archivable(&pr, now));
        pr["links"][0]["head"] = "fff".into();
        assert!(!auto_archivable(&pr, now));
        pr["links"][0]["head"] = serde_json::Value::Null;
        assert!(!auto_archivable(&pr, now));
        // A task whose tab was closed has no agent to wait for: its stale
        // status doesn't matter, but the rest still does.
        let closed = |reason: &str, status: &str| {
            let mut t = with("ended_at", 1.into());
            t["ended_reason"] = reason.into();
            t["status"] = status.into();
            t["status_at"] = now.into();
            t
        };
        assert!(auto_archivable(&closed("tab closed", "working"), now));
        let mut dirty = closed("tab closed", "idle");
        dirty["dirty_count"] = 1.into();
        assert!(!auto_archivable(&dirty, now));
        let mut unmerged = closed("tab closed", "idle");
        unmerged["branch_merged"] = "unmerged".into();
        assert!(!auto_archivable(&unmerged, now));
        // A resumed task waits for its head to move from where it resumed.
        assert!(auto_archivable(
            &with("resume_head", "0000aaaa".into()),
            now
        ));
        let mut resumed = with("resume_head", "abc123def4560000".into());
        assert!(!auto_archivable(&resumed, now));
        resumed["branch_head"] = "".into();
        assert!(!auto_archivable(&resumed, now));
        for t in [
            with("branch_merged", "unmerged".into()),
            with("branch_merged", "new".into()),
            with("dirty_count", 1.into()),
            with("dirty_count", serde_json::Value::Null),
            with("worktree", "/r".into()),
            with("pinned", true.into()),
            with("ended_at", 1.into()),
            closed("worktree removed", "idle"),
            closed("archived", "idle"),
            with("status", "working".into()),
            with("status", "blocked".into()),
            with("status", "scheduled".into()),
            with("status_at", (now - 10).into()),
        ] {
            assert!(!auto_archivable(&t, now), "{t}");
        }
    }

    #[test]
    fn auto_archive_removes_only_clean_landed_orphans_no_pane_is_in() {
        use crate::orphans::{auto_removable, occupied};
        let base = serde_json::json!({
            "ended_at": 1,
            "ended_reason": "orphaned",
            "worktree_left": true,
            "worktree": "/r/.worktrees/x",
            "repo": "/r",
            "branch": "feature/x",
            "dirty_count": 0,
            "branch_merged": "merged",
            "branch_head": "abc123def456",
        });
        assert!(auto_removable(&base));
        let with = |k: &str, v: serde_json::Value| {
            let mut t = base.clone();
            t[k] = v;
            t
        };
        // A merged PR counts only when its head is the branch head.
        let mut pr = with("branch_merged", "unmerged".into());
        pr["links"] =
            serde_json::json!([{"kind": "pr", "state": "merged", "head": "abc123def4560000"}]);
        assert!(auto_removable(&pr));
        pr["links"][0]["head"] = "fff".into();
        assert!(!auto_removable(&pr));
        // A branch with no commits of its own isn't removed, even with a
        // merged PR at its head.
        let mut fresh = with("branch_merged", "new".into());
        fresh["links"] =
            serde_json::json!([{"kind": "pr", "state": "merged", "head": "abc123def4560000"}]);
        assert!(!auto_removable(&fresh));
        for t in [
            with("branch_merged", "unmerged".into()),
            with("branch_merged", "".into()),
            with("dirty_count", 1.into()),
            with("dirty_count", serde_json::Value::Null),
            with("branch", "".into()),
            with("worktree", "/r".into()),
            with("worktree", "/r/".into()),
            with("worktree_left", serde_json::Value::Null),
            with("ended_reason", "tab closed".into()),
            with("ended_reason", "worktree removed".into()),
        ] {
            assert!(!auto_removable(&t), "{t}");
        }
        // A pane in the worktree, or under it, keeps it; a sibling doesn't.
        let cwds = |c: &[&str]| c.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert!(occupied(&cwds(&["/r/.worktrees/x"]), "/r/.worktrees/x"));
        assert!(occupied(
            &cwds(&["/r/.worktrees/x/src/"]),
            "/r/.worktrees/x/"
        ));
        assert!(!occupied(
            &cwds(&["/r/.worktrees/xy", "/r"]),
            "/r/.worktrees/x"
        ));
    }

    #[test]
    fn switch_harness_validates_target_and_updates_registry_fields() {
        assert_eq!(validate_target_spec("pi"), Ok(("pi", None)));
        assert_eq!(
            validate_target_spec("claude:opus"),
            Ok(("claude", Some("opus")))
        );
        assert!(validate_target_spec("nope").is_err());
        assert!(validate_target_spec("claude:no-such-model").is_err());

        let mut task = serde_json::json!({
            "window_id": "w1",
            "harness": "claude",
            "model": "opus",
            "agent_session": "old-id",
            "ended_at": null,
        });
        apply_switched_harness(&mut task, "pi", None, "new-id");
        assert_eq!(task["harness"], "pi");
        assert!(task["model"].is_null());
        assert_eq!(task["agent_session"], "new-id");

        apply_switched_harness(&mut task, "claude", Some("sonnet"), "third-id");
        assert_eq!(task["harness"], "claude");
        assert_eq!(task["model"], "sonnet");
        assert_eq!(task["agent_session"], "third-id");

        let tasks = vec![
            serde_json::json!({"window_id": "w1", "ended_at": 1, "agent_session": "old"}),
            serde_json::json!({"window_id": "w1", "ended_at": null, "agent_session": "live"}),
            serde_json::json!({"window_id": "w2", "ended_at": 2, "agent_session": "arch"}),
        ];
        assert_eq!(
            find_switch_task(&tasks, "w1").unwrap()["agent_session"],
            "live"
        );
        assert_eq!(
            find_switch_task(&tasks, "w2").unwrap()["agent_session"],
            "arch"
        );
        assert!(find_switch_task(&tasks, "w3").is_err());
    }

    #[test]
    fn in_place_archive_closes_tab_and_keeps_directory_without_git_checks() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let notes = std::env::temp_dir().join(format!(
            "switchyard-notes-archive-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&notes).unwrap();
        let note_file = notes.join("todo.md");
        std::fs::write(&note_file, "keep me").unwrap();

        let mut task = serde_json::json!({
            "host": "omarchy",
            "session_id": "s1",
            "window_id": "w1",
            "project": "notes",
            "name": "chat-1",
            "repo": notes.to_str().unwrap(),
            "worktree": notes.to_str().unwrap(),
            "branch": "",
            "in_place": true,
            "harness": "agy",
            "ended_at": null,
            "ended_reason": null,
        });
        assert!(is_in_place(&task));
        assert!(!auto_archivable(&task, 10_000));

        let mut closed = false;
        let mut cleaned = false;
        let mut saved = false;
        end_keeping_worktree_with(
            &mut task,
            |_| Some("conv-123".into()),
            || closed = true,
            || cleaned = true,
            |t| {
                assert_eq!(t["ended_reason"], "archived");
                assert_eq!(t["agent_session"], "conv-123");
                assert!(t["ended_at"].as_u64().is_some());
                saved = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(closed && cleaned && saved);
        assert!(note_file.is_file());
        std::fs::remove_dir_all(notes).unwrap();
    }

    #[test]
    fn parses_agy_question_screen_states() {
        let single = "\
? Pick one
Question
────────────────────────────────────────
Question 1/2: Pick one
> 1. (Recommended) Alpha
  2. Beta
  3. Write-in...
  ↑/↓ Navigate · → Next · enter Select · esc Skip";
        assert_eq!(
            parse_agy_question_screen(single),
            AgyQuestionScreen {
                question_num: Some(1),
                question_total: Some(2),
                cursor: Some(0),
                checked: Vec::new(),
                last_index: Some(2),
                write_in_open: false,
            }
        );

        let multi_write_in = "\
Question
────────────────────────────────────────
Question 2/2: Pick many
  1. [x] One
  2. [ ] Two
> 3. Write-in...
  Your answer:
  extra
  enter Submit All · esc Back";
        assert_eq!(
            parse_agy_question_screen(multi_write_in),
            AgyQuestionScreen {
                question_num: Some(2),
                question_total: Some(2),
                cursor: Some(2),
                checked: vec![0],
                last_index: Some(2),
                write_in_open: true,
            }
        );
    }

    #[test]
    fn sanitize_write_in_collapses_newlines_and_strips_controls() {
        assert_eq!(
            sanitize_write_in("  line one\nline\ttwo\x1b[31m!  "),
            vec!["line one line two[31m!".to_string()]
        );
        assert_eq!(sanitize_write_in("  \n\t  "), Vec::<String>::new());
    }
}
