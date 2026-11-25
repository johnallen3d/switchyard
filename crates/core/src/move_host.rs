//! Move a live or ended task to another compatible pool host:
//! `switchyard task act move HOST SESSION WINDOW TARGET_HOST`.
//!
//! Runs on the config host, which holds the task registry and reaches every
//! Rex host.
//!
//! Safety invariants:
//! - Only personal tasks captured by Atuin (`claude` / `pi`) may move across
//!   hosts. Work tasks use the local-only `atuin-work` store (not synced) and
//!   are refused clearly rather than dropping conversation history or touching
//!   the personal store. `notes`, `agy`, orphans, primary checkouts, shared
//!   worktrees, detached HEADs, and non-catalog folders are also refused.
//! - Target branch updates are strictly fast-forward: a diverged or newer
//!   branch on the target host, or a dirty existing target worktree, refuses
//!   the move before changing anything.
//! - Committed history moves via `git bundle` between hosts (never pushing to
//!   a remote and never force-pushing).
//! - Uncommitted changes (staged, unstaged, binary, modes, symlinks, and
//!   untracked non-ignored files) are carried over and verified against
//!   `git status --porcelain` on the target before touching the source host.
//!   In-progress merges/rebases, conflicts, and dirty submodules are refused.
//! - Atuin syncs on source then target, restores the thread into the target
//!   worktree (handling cross-machine divergence with `--switch --branch`),
//!   and verifies the transcript exists before opening the target tab.
//! - If any step fails before the target tab opens, a newly created target
//!   worktree is rolled back and the source host stays untouched.

use std::fmt::Write as _;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use crate::actions::{
    agent_block, branch_head, find_switch_task, kill_if_empty, label_of, remove_worktree,
    worktree_user,
};
use crate::atuin;
use crate::folders::home_relative;
use crate::hosts::{AUTO, Project, catalog, choose_other_host, other_compatible, resolve};
use crate::launch::launch;
use crate::naming::{branch_and_slug, harness_spec};
use crate::rex::{host_bash, local_label, log, rex, sh_quote};
use crate::sessions::newest_session;
use crate::tasks::{in_window, load_tasks, update_tasks};

/// Maximum tar payload for untracked files carried across hosts (100 MiB).
const MAX_UNTRACKED_BYTES: usize = 100 * 1024 * 1024;

/// Move the live or ended task in `window` on `host` to `target_host`
/// (a host label, `"config"`, or `"auto"` for the least busy compatible host).
///
/// Transfers committed work on the task's branch via `git bundle`, carries any
/// uncommitted changes across and verifies them, syncs and restores the Atuin
/// thread on the target host, opens the resumed tab there, updates the task
/// registry (and any open child tasks' `parent_*` pointers), and only then
/// closes the old tab and removes the old worktree on the source host.
///
/// Prints `HOST<TAB>SESSION<TAB>WINDOW`, like `resume` and `switch`.
///
/// # Errors
///
/// Fails with a clear explanation when the task can't be moved (unsynced work
/// store, uncaptured profile/harness, orphan, primary checkout, shared
/// worktree, detached HEAD, non-catalog repo, incompatible or unreachable
/// host, in-progress merge/rebase/conflicts, dirty or diverged target, failed
/// Atuin sync/restore, or Rex launch failure).
pub fn move_host(
    host: &str,
    session: &str,
    window: &str,
    target_host: &str,
) -> Result<String, String> {
    let plan = validate_move(host, session, window, target_host)?;
    let source = inspect_source_git(
        &plan.source_host,
        &plan.source_repo,
        &plan.source_worktree,
        &plan.branch,
        &plan.label,
    )?;
    let target = prepare_target_repo(
        &plan.target_host,
        &plan.project.path,
        &plan.project.clone_url,
        &source.branch,
        &source.head,
    )?;
    if let Some(dirty_wt) = &target.dirty_worktree {
        return Err(format!(
            "{} already has uncommitted changes on {} in {dirty_wt}",
            plan.target_host, source.branch
        ));
    }

    if !source.head.is_empty() {
        transfer_branch_commits(&plan, &source, &target)?;
    }

    let (target_wt, created_target_wt) = ensure_target_worktree(
        &plan.target_host,
        &target.repo,
        &plan.name,
        &source.branch,
        target.worktree.as_deref(),
    )?;
    copy_branch_links(
        &plan.target_host,
        &target.repo,
        &source.branch,
        &source.links,
    );

    let mut target_dirty = false;
    let outcome = (|| -> Result<(String, String, String), String> {
        if source.dirty() {
            target_dirty = true;
            transfer_uncommitted(&plan, &source, &target_wt)?;
        }
        sync_and_restore_atuin(&plan, &target_wt)?;
        open_and_migrate(&plan, &source, &target.repo, &target_wt)
    })();

    let (new_host, new_session_id, new_window_id) = match outcome {
        Ok(tab) => tab,
        Err(err) => {
            if created_target_wt {
                let _ = remove_worktree(
                    &plan.target_host,
                    &target.repo,
                    &source.branch,
                    target_dirty,
                );
            }
            return Err(err);
        }
    };

    cleanup_source(&plan, &source);
    crate::server::refresh();
    Ok(format!("{new_host}\t{new_session_id}\t{new_window_id}"))
}

/// Validated inputs for moving one task across hosts.
struct MovePlan {
    task: serde_json::Value,
    label: String,
    project: Project,
    profile: String,
    harness: String,
    model: String,
    name: String,
    branch: String,
    ended: bool,
    source_host: String,
    source_session_id: String,
    source_window: String,
    source_repo: String,
    source_worktree: String,
    agent_session: String,
    target_host: String,
}

fn validate_move(
    host: &str,
    session: &str,
    window: &str,
    target_host: &str,
) -> Result<MovePlan, String> {
    let target_arg = target_host.trim();
    if target_arg.is_empty() {
        return Err("move takes a HOST or auto".into());
    }
    let tasks = load_tasks()?;
    let task = find_switch_task(&tasks, window)?;
    let field = |k: &str| task[k].as_str().unwrap_or("").to_string();
    let label = label_of(&task);
    if field("ended_reason") == "orphaned" {
        return Err(format!(
            "{label} is an orphan worktree with no agent session to move"
        ));
    }
    let profile = field("profile");
    let harness = match field("harness").as_str() {
        "" => "claude".to_string(),
        h => h.to_string(),
    };
    atuin::move_plan(&profile, &harness).map_err(|e| format!("{label}: {e}"))?;

    let source_host = match field("host") {
        h if !h.is_empty() => h,
        _ => host.to_string(),
    };
    let source_repo = field("repo");
    let source_worktree = field("worktree");
    if !source_worktree.is_empty()
        && source_worktree.trim_end_matches('/') == source_repo.trim_end_matches('/')
    {
        return Err(format!(
            "{label} is in the primary checkout {source_worktree}, not a task worktree"
        ));
    }
    if let Some(i) = tasks
        .iter()
        .position(|t| t["ended_at"].is_null() == task["ended_at"].is_null() && in_window(t, window))
        && let Some(other) = worktree_user(&tasks, i)
    {
        return Err(format!(
            "{label}: {other} still uses {source_worktree} on {source_host}"
        ));
    }

    let name = field("name");
    let branch = field("branch");
    if branch.is_empty() {
        return Err(format!(
            "{label} is on a detached HEAD with no branch to move"
        ));
    }
    let project = catalog()?
        .into_iter()
        .find(|p| home_relative(&p.path) == home_relative(&source_repo))
        .ok_or_else(|| {
            format!("{label} isn't a catalog project, so it can't be moved to another host")
        })?;
    let target_host = resolve_target_host(&project, &source_host, &label, target_arg)?;

    let ended = !task["ended_at"].is_null();
    let source_session_id = match field("session_id") {
        s if !s.is_empty() => s,
        _ => session.to_string(),
    };
    let source_window = match field("window_id") {
        w if !w.is_empty() => w,
        _ => window.to_string(),
    };
    if !ended {
        agent_block(&source_host, &source_session_id, &source_window)?;
    }

    let agent_session = if ended {
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
    .ok_or_else(|| format!("{label}: no {harness} session found on {source_host}"))?;

    Ok(MovePlan {
        model: field("model"),
        task,
        label,
        project,
        profile,
        harness,
        name,
        branch,
        ended,
        source_host,
        source_session_id,
        source_window,
        source_repo,
        source_worktree,
        agent_session,
        target_host,
    })
}

fn resolve_target_host(
    project: &Project,
    source_host: &str,
    label: &str,
    target_arg: &str,
) -> Result<String, String> {
    if target_arg == AUTO {
        let (chosen, why) = choose_other_host(project, source_host)?;
        log(&format!("move auto chose {chosen} for {label}: {why}"));
        return Ok(chosen);
    }
    let resolved = resolve(target_arg);
    if resolved == source_host {
        return Err(format!("{label} is already on {resolved}"));
    }
    let allowed = other_compatible(project, source_host);
    if !allowed.contains(&resolved) {
        let list = if allowed.is_empty() {
            "none".to_string()
        } else {
            allowed.join(", ")
        };
        return Err(format!(
            "{resolved} isn't a compatible host for {label} (compatible: {list})"
        ));
    }
    Ok(resolved)
}

/// Which categories of uncommitted changes exist in a source worktree.
#[derive(Debug, Default)]
struct DirtyKinds {
    staged: bool,
    unstaged: bool,
    untracked: bool,
}

/// Snapshot of the task's Git state on the source host (read-only).
#[derive(Debug, Default)]
struct SourceGit {
    branch: String,
    head: String,
    wt_exists: bool,
    changes: DirtyKinds,
    status_lines: Vec<String>,
    links: Vec<String>,
}

impl SourceGit {
    fn dirty(&self) -> bool {
        !self.status_lines.is_empty()
            || self.changes.staged
            || self.changes.unstaged
            || self.changes.untracked
    }
}

fn inspect_source_git(
    host: &str,
    repo: &str,
    worktree: &str,
    branch: &str,
    label: &str,
) -> Result<SourceGit, String> {
    let script = format!(
        "repo={}\nwt={}\nbranch={}\n\
         if [ ! -d \"$repo\" ]; then echo '@no-repo'; exit 0; fi\n\
         if [ -n \"$wt\" ] && [ -d \"$wt\" ]; then\n\
           gd=$(git -C \"$wt\" rev-parse --git-dir 2>/dev/null) || {{ echo '@bad-wt'; exit 0; }}\n\
           case \"$gd\" in /*) ;; *) gd=\"$wt/$gd\" ;; esac\n\
           for m in MERGE_HEAD REBASE_HEAD CHERRY_PICK_HEAD REVERT_HEAD rebase-merge rebase-apply; do\n\
             if [ -e \"$gd/$m\" ]; then printf '@in-progress\\t%s\\n' \"$m\"; exit 0; fi\n\
           done\n\
           actual=$(git -C \"$wt\" symbolic-ref --quiet --short HEAD 2>/dev/null)\n\
           if [ -z \"$actual\" ]; then echo '@detached'; exit 0; fi\n\
           head=$(git -C \"$wt\" rev-parse --verify --quiet HEAD 2>/dev/null)\n\
           status=$(git -C \"$wt\" status --porcelain 2>/dev/null) || {{ echo '@bad-status'; exit 0; }}\n\
           while IFS= read -r line; do\n\
             case \"${{line:0:2}}\" in\n\
               DD|AU|UD|UA|DU|AA|UU) echo '@unmerged'; exit 0 ;;\n\
             esac\n\
           done <<<\"$status\"\n\
           while IFS= read -r path; do\n\
             [ -n \"$path\" ] || continue\n\
             mode=$(git -C \"$wt\" ls-files --stage -- \"$path\" 2>/dev/null | head -n 1 | awk '{{print $1}}')\n\
             if [ \"$mode\" = 160000 ]; then printf '@submodule\\t%s\\n' \"$path\"; exit 0; fi\n\
           done < <({{ git -C \"$wt\" diff --name-only; git -C \"$wt\" diff --cached --name-only; }} | sort -u)\n\
           staged=0; git -C \"$wt\" diff --no-ext-diff --cached --quiet 2>/dev/null || staged=1\n\
           unstaged=0; git -C \"$wt\" diff --no-ext-diff --quiet 2>/dev/null || unstaged=1\n\
           untracked=0; [ -n \"$(git -C \"$wt\" ls-files --others --exclude-standard 2>/dev/null)\" ] && untracked=1\n\
           printf '@ok\\t1\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"$actual\" \"$head\" \"$staged\" \"$unstaged\" \"$untracked\"\n\
           git -C \"$repo\" config --get-all \"branch.$actual.switchyard-link\" 2>/dev/null | while IFS= read -r l; do\n\
             [ -n \"$l\" ] && printf '@link\\t%s\\n' \"$l\"\n\
           done\n\
           if [ -n \"$status\" ]; then\n\
             echo '@status'\n\
             printf '%s\\n' \"$status\"\n\
           fi\n\
         else\n\
           head=$(git -C \"$repo\" rev-parse --verify --quiet \"refs/heads/$branch\" 2>/dev/null)\n\
           printf '@ok\\t0\\t%s\\t%s\\t0\\t0\\t0\\n' \"$branch\" \"$head\"\n\
           git -C \"$repo\" config --get-all \"branch.$branch.switchyard-link\" 2>/dev/null | while IFS= read -r l; do\n\
             [ -n \"$l\" ] && printf '@link\\t%s\\n' \"$l\"\n\
           done\n\
         fi\n",
        sh_quote(repo),
        sh_quote(worktree),
        sh_quote(branch),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_source_git(&text, host, label)
}

fn parse_source_git(text: &str, host: &str, label: &str) -> Result<SourceGit, String> {
    let mut info = SourceGit::default();
    let mut in_status = false;
    let mut saw_ok = false;
    for line in text.lines() {
        if in_status {
            if !line.is_empty() {
                info.status_lines.push(line.to_string());
            }
            continue;
        }
        if line == "@status" {
            in_status = true;
            continue;
        }
        if line == "@no-repo" {
            return Err(format!("{label}: repository is missing on {host}"));
        }
        if line == "@bad-wt" || line == "@bad-status" {
            return Err(format!("{label}: can't inspect worktree on {host}"));
        }
        if let Some(marker) = line.strip_prefix("@in-progress\t") {
            return Err(format!(
                "{label} has an in-progress git operation ({marker}) on {host}; finish or abort it before moving"
            ));
        }
        if line == "@detached" {
            return Err(format!(
                "{label} is on a detached HEAD on {host}; check out a branch before moving"
            ));
        }
        if line == "@unmerged" {
            return Err(format!(
                "{label} has unresolved merge conflicts on {host}; resolve them before moving"
            ));
        }
        if let Some(sub) = line.strip_prefix("@submodule\t") {
            return Err(format!(
                "{label} has uncommitted changes in submodule {sub} on {host}; commit or clean it before moving"
            ));
        }
        if let Some(link) = line.strip_prefix("@link\t") {
            info.links.push(link.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("@ok\t") {
            let mut f = rest.split('\t');
            info.wt_exists = f.next() == Some("1");
            info.branch = f.next().unwrap_or("").to_string();
            info.head = f.next().unwrap_or("").to_string();
            info.changes.staged = f.next() == Some("1");
            info.changes.unstaged = f.next() == Some("1");
            info.changes.untracked = f.next() == Some("1");
            saw_ok = true;
        }
    }
    if !saw_ok {
        return Err(format!("{label}: unexpected git status from {host}"));
    }
    info.status_lines.sort();
    Ok(info)
}

/// Snapshot of the target host's repository before modifying refs or worktrees.
#[derive(Debug, Default)]
struct TargetRepo {
    repo: String,
    worktree: Option<String>,
    dirty_worktree: Option<String>,
    head: String,
    has_source_head: bool,
    have_shas: Vec<String>,
}

fn prepare_target_repo(
    host: &str,
    project_path: &str,
    clone_url: &str,
    branch: &str,
    source_head: &str,
) -> Result<TargetRepo, String> {
    let script = format!(
        "expand() {{ case \"$1\" in '~/'*) printf '%s' \"$HOME/${{1#~/}}\" ;; '~') printf '%s' \"$HOME\" ;; *) printf '%s' \"$1\" ;; esac; }}\n\
         repo=$(expand {})\n\
         clone_url=$(expand {})\n\
         branch={}\n\
         src_head={}\n\
         if [ ! -d \"$repo\" ]; then\n\
           if [ -z \"$clone_url\" ]; then echo \"Git folder $repo does not exist on target\"; exit 1; fi\n\
           mkdir -p \"$(dirname \"$repo\")\"\n\
           git clone --quiet \"$clone_url\" \"$repo\" 2>&1 || exit 1\n\
         fi\n\
         wt=\n\
         cur=\n\
         while IFS= read -r line; do\n\
           case $line in\n\
             'worktree '*) cur=${{line#worktree }} ;;\n\
             \"branch refs/heads/$branch\") wt=$cur; break ;;\n\
           esac\n\
         done < <(git -C \"$repo\" worktree list --porcelain 2>/dev/null)\n\
         if [ -n \"$wt\" ] && [ -d \"$wt\" ]; then\n\
           st=$(git -C \"$wt\" status --porcelain 2>/dev/null) || exit 1\n\
           if [ -n \"$st\" ]; then\n\
             printf '@dirty\\t%s\\n' \"$wt\"\n\
             exit 0\n\
           fi\n\
         fi\n\
         tgt_head=$(git -C \"$repo\" rev-parse --verify --quiet \"refs/heads/$branch\" 2>/dev/null)\n\
         has_src=0\n\
         if [ -n \"$src_head\" ] && git -C \"$repo\" cat-file -e \"${{src_head}}^{{commit}}\" 2>/dev/null; then\n\
           has_src=1\n\
         fi\n\
         printf '@ready\\t%s\\t%s\\t%s\\t%s\\n' \"$repo\" \"$wt\" \"$tgt_head\" \"$has_src\"\n\
         for ref in \"refs/heads/$branch\" refs/remotes/origin/HEAD refs/heads/main refs/heads/master HEAD; do\n\
           sha=$(git -C \"$repo\" rev-parse --verify --quiet \"${{ref}}^{{commit}}\" 2>/dev/null) && printf '@have\\t%s\\n' \"$sha\"\n\
         done | sort -u\n",
        sh_quote(project_path),
        sh_quote(clone_url),
        sh_quote(branch),
        sh_quote(source_head),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let msg = last_non_empty(&text).unwrap_or("could not prepare repository");
        return Err(format!("{host}: {msg}"));
    }
    let mut info = TargetRepo::default();
    let mut saw_ready = false;
    for line in text.lines() {
        if let Some(wt) = line.strip_prefix("@dirty\t") {
            info.dirty_worktree = Some(wt.to_string());
            return Ok(info);
        }
        if let Some(rest) = line.strip_prefix("@ready\t") {
            let mut f = rest.split('\t');
            info.repo = f.next().unwrap_or("").to_string();
            info.worktree = f.next().filter(|s| !s.is_empty()).map(String::from);
            info.head = f.next().unwrap_or("").to_string();
            info.has_source_head = f.next() == Some("1");
            saw_ready = true;
        } else if let Some(sha) = line.strip_prefix("@have\t")
            && !sha.is_empty()
        {
            info.have_shas.push(sha.to_string());
        }
    }
    if !saw_ready {
        return Err(format!("{host}: unexpected repository response"));
    }
    Ok(info)
}

/// Transfer `source.branch` commits from source to target using `git bundle`
/// (when target doesn't already have `source.head`), then fast-forward or
/// create `refs/heads/<branch>` on target. Refuses diverged or newer target
/// branches.
fn transfer_branch_commits(
    plan: &MovePlan,
    source: &SourceGit,
    target: &TargetRepo,
) -> Result<(), String> {
    if target.head == source.head {
        return Ok(());
    }
    if !target.has_source_head {
        let bundle = create_branch_bundle(
            &plan.source_host,
            &plan.source_repo,
            &source.branch,
            &target.have_shas,
        )?;
        fetch_branch_bundle(&plan.target_host, &target.repo, &source.branch, &bundle)?;
    }
    update_target_branch(
        &plan.target_host,
        &target.repo,
        target.worktree.as_deref(),
        &source.branch,
        &target.head,
        &source.head,
    )
}

fn create_branch_bundle(
    host: &str,
    repo: &str,
    branch: &str,
    have_shas: &[String],
) -> Result<Vec<u8>, String> {
    let mut script = format!(
        "repo={}\nbranch={}\n\
         excludes=()\n",
        sh_quote(repo),
        sh_quote(branch),
    );
    for sha in have_shas {
        let _ = writeln!(
            script,
            "if git -C \"$repo\" cat-file -e {} 2>/dev/null; then excludes+=({}); fi",
            sh_quote(&format!("{sha}^{{commit}}")),
            sh_quote(&format!("^{sha}")),
        );
    }
    script.push_str(
        "tmp=$(mktemp)\n\
         trap 'rm -f \"$tmp\"' EXIT\n\
         git -C \"$repo\" bundle create \"$tmp\" \"refs/heads/$branch\" \"${excludes[@]}\" >/dev/null 2>&1 || exit 1\n\
         cat \"$tmp\"\n",
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(format!("{host}: failed to bundle branch {branch}"));
    }
    Ok(out.stdout)
}

fn fetch_branch_bundle(host: &str, repo: &str, branch: &str, bundle: &[u8]) -> Result<(), String> {
    let payload = encode_b64_lines(bundle);
    let script = format!(
        "repo={}\nbranch={}\n\
         tmp=$(mktemp)\n\
         trap 'rm -f \"$tmp\"' EXIT\n\
         base64 -d <<'SW_PAYLOAD' > \"$tmp\" || exit 1\n\
         {payload}\
         SW_PAYLOAD\n\
         git -C \"$repo\" fetch --quiet \"$tmp\" \"refs/heads/$branch:refs/switchyard/move-incoming\" 2>&1\n",
        sh_quote(repo),
        sh_quote(branch),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let msg = last_non_empty(&text).unwrap_or("failed to fetch branch bundle");
        return Err(format!("{host}: {msg}"));
    }
    Ok(())
}

fn update_target_branch(
    host: &str,
    repo: &str,
    target_wt: Option<&str>,
    branch: &str,
    target_head: &str,
    source_head: &str,
) -> Result<(), String> {
    let script = format!(
        "repo={}\nwt={}\nbranch={}\ntgt_head={}\nsrc_head={}\n\
         cleanup() {{ git -C \"$repo\" update-ref -d refs/switchyard/move-incoming >/dev/null 2>&1 || true; }}\n\
         trap cleanup EXIT\n\
         if ! git -C \"$repo\" cat-file -e \"${{src_head}}^{{commit}}\" 2>/dev/null; then\n\
           echo \"commit $src_head missing after bundle fetch\"; exit 1\n\
         fi\n\
         if [ -n \"$tgt_head\" ] && [ \"$tgt_head\" != \"$src_head\" ]; then\n\
           if ! git -C \"$repo\" merge-base --is-ancestor \"$tgt_head\" \"$src_head\" 2>/dev/null; then\n\
             echo '@diverged'; exit 2\n\
           fi\n\
         fi\n\
         if [ -n \"$wt\" ] && [ -d \"$wt\" ]; then\n\
           if [ \"$tgt_head\" != \"$src_head\" ]; then\n\
             git -C \"$wt\" merge --ff-only \"$src_head\" 2>&1 || exit 1\n\
           fi\n\
         elif [ -n \"$tgt_head\" ]; then\n\
           if [ \"$tgt_head\" != \"$src_head\" ]; then\n\
             git -C \"$repo\" update-ref \"refs/heads/$branch\" \"$src_head\" \"$tgt_head\" 2>&1 || exit 1\n\
           fi\n\
         else\n\
           git -C \"$repo\" update-ref \"refs/heads/$branch\" \"$src_head\" 2>&1 || exit 1\n\
         fi\n",
        sh_quote(repo),
        sh_quote(target_wt.unwrap_or("")),
        sh_quote(branch),
        sh_quote(target_head),
        sh_quote(source_head),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if out.status.code() == Some(2) {
        let short_t: String = target_head.chars().take(8).collect();
        let short_s: String = source_head.chars().take(8).collect();
        return Err(format!(
            "{host} already has a diverged or newer {branch} ({short_t} vs {short_s}); won't overwrite it"
        ));
    }
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let msg = last_non_empty(&text).unwrap_or("could not update branch");
        return Err(format!("{host}: {msg}"));
    }
    Ok(())
}

/// Ensure the target host has a worktree checked out on `branch`. Returns
/// `(worktree_path, created)` so failures before launch can clean up a newly
/// created worktree.
fn ensure_target_worktree(
    host: &str,
    repo: &str,
    name: &str,
    branch: &str,
    existing_wt: Option<&str>,
) -> Result<(String, bool), String> {
    if let Some(wt) = existing_wt.filter(|s| !s.is_empty()) {
        return Ok((wt.to_string(), false));
    }
    let slug = branch_and_slug(name).1;
    let template = format!("worktree-path = \"{{{{ repo_path }}}}/.worktrees/{slug}\"");
    let script = format!(
        "repo={}\nbranch={}\ntemplate={}\n\
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
         if ! git -C \"$repo\" rev-parse --verify --quiet \"refs/heads/$branch\" >/dev/null 2>&1; then\n\
           git -C \"$repo\" fetch --quiet origin \"$branch\" >/dev/null 2>&1 || true\n\
         fi\n\
         if git -C \"$repo\" rev-parse --verify --quiet \"refs/heads/$branch\" >/dev/null 2>&1; then\n\
           wt -C \"$repo\" --config-set \"$template\" switch \"$branch\" --no-cd --yes 2>&1 || exit 1\n\
         else\n\
           wt -C \"$repo\" --config-set \"$template\" switch --create \"$branch\" --no-cd --yes 2>&1 || exit 1\n\
         fi\n\
         if made=$(find_wt); then printf 'created\\t%s\\n' \"$made\"; exit 0; fi\n\
         echo \"wt made no worktree for $branch\"; exit 1\n",
        sh_quote(repo),
        sh_quote(branch),
        sh_quote(&template),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let msg = last_non_empty(&text).unwrap_or("could not create worktree");
        return Err(format!("{host}: {msg}"));
    }
    let last = last_non_empty(&text).unwrap_or("");
    if let Some(wt) = last.strip_prefix("reused\t") {
        return Ok((wt.to_string(), false));
    }
    if let Some(wt) = last.strip_prefix("created\t") {
        return Ok((wt.to_string(), true));
    }
    Err(format!("{host}: unexpected worktree result: {last}"))
}

/// Copy `branch.<branch>.switchyard-link` config values to the target repo,
/// skipping any already recorded there.
fn copy_branch_links(host: &str, repo: &str, branch: &str, links: &[String]) {
    if links.is_empty() {
        return;
    }
    let key = format!("branch.{branch}.switchyard-link");
    let mut script = format!(
        "repo={}\nkey={}\n\
         existing=$(git -C \"$repo\" config --get-all \"$key\" 2>/dev/null || true)\n",
        sh_quote(repo),
        sh_quote(&key),
    );
    for link in links {
        let _ = writeln!(
            script,
            "printf '%s\\n' \"$existing\" | grep -Fqx -- {q} || git -C \"$repo\" config --add \"$key\" {q}",
            q = sh_quote(link),
        );
    }
    let _ = host_bash(host, &script);
}

/// Carry staged, unstaged, and untracked changes from the source worktree to
/// the target worktree, then verify `git status --porcelain` matches.
fn transfer_uncommitted(
    plan: &MovePlan,
    source: &SourceGit,
    target_wt: &str,
) -> Result<(), String> {
    if source.changes.staged {
        let patch = read_git_diff(&plan.source_host, &plan.source_worktree, true)?;
        apply_git_patch(&plan.target_host, target_wt, &patch, true)?;
    }
    if source.changes.unstaged {
        let patch = read_git_diff(&plan.source_host, &plan.source_worktree, false)?;
        apply_git_patch(&plan.target_host, target_wt, &patch, false)?;
    }
    if source.changes.untracked {
        let archive = read_untracked_tar(&plan.source_host, &plan.source_worktree)?;
        extract_untracked_tar(&plan.target_host, target_wt, &archive)?;
    }
    let actual = read_porcelain_status(&plan.target_host, target_wt)?;
    if actual != source.status_lines {
        return Err(format!(
            "uncommitted changes didn't match after transfer to {}; kept {} on {}",
            plan.target_host, plan.label, plan.source_host
        ));
    }
    Ok(())
}

fn read_git_diff(host: &str, worktree: &str, cached: bool) -> Result<Vec<u8>, String> {
    let flag = if cached { "--cached " } else { "" };
    let script = format!(
        "git -C {} -c diff.noprefix=false -c diff.mnemonicprefix=false diff {flag}--no-ext-diff --no-color --binary\n",
        sh_quote(worktree)
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        return Err(format!("{host}: failed to read uncommitted diff"));
    }
    Ok(out.stdout)
}

fn apply_git_patch(host: &str, worktree: &str, patch: &[u8], index: bool) -> Result<(), String> {
    if patch.is_empty() {
        return Ok(());
    }
    let payload = encode_b64_lines(patch);
    let flag = if index { "--index " } else { "" };
    let script = format!(
        "base64 -d <<'SW_PAYLOAD' | git -C {} apply {flag}--binary 2>&1\n{payload}SW_PAYLOAD\n",
        sh_quote(worktree),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let msg = last_non_empty(&text).unwrap_or("git apply failed");
        return Err(format!(
            "{host}: failed to apply uncommitted changes: {msg}"
        ));
    }
    Ok(())
}

fn read_untracked_tar(host: &str, worktree: &str) -> Result<Vec<u8>, String> {
    let script = format!(
        "cd {} || exit 1\n\
         git ls-files --others --exclude-standard -z | COPYFILE_DISABLE=1 tar --null -T - -cf -\n",
        sh_quote(worktree),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        return Err(format!("{host}: failed to archive untracked files"));
    }
    if out.stdout.len() > MAX_UNTRACKED_BYTES {
        return Err(format!(
            "untracked files on {host} exceed {} MiB; commit or ignore them before moving",
            MAX_UNTRACKED_BYTES / (1024 * 1024)
        ));
    }
    Ok(out.stdout)
}

fn extract_untracked_tar(host: &str, worktree: &str, archive: &[u8]) -> Result<(), String> {
    if archive.is_empty() {
        return Ok(());
    }
    let payload = encode_b64_lines(archive);
    let script = format!(
        "base64 -d <<'SW_PAYLOAD' | tar -C {} -xf - 2>&1\n{payload}SW_PAYLOAD\n",
        sh_quote(worktree),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let msg = last_non_empty(&text).unwrap_or("tar extract failed");
        return Err(format!("{host}: failed to unpack untracked files: {msg}"));
    }
    Ok(())
}

fn read_porcelain_status(host: &str, worktree: &str) -> Result<Vec<String>, String> {
    let script = format!("git -C {} status --porcelain\n", sh_quote(worktree));
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    if !out.status.success() {
        return Err(format!("{host}: failed to verify target worktree status"));
    }
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    lines.sort();
    Ok(lines)
}

/// Sync Atuin on the source host (pushing latest turns) and target host
/// (pulling them), then restore the thread into `target_wt` on the target host.
fn sync_and_restore_atuin(plan: &MovePlan, target_wt: &str) -> Result<(), String> {
    let (program, atuin_harness) = atuin::move_plan(&plan.profile, &plan.harness)?;
    let source_atuin_id = sync_source_atuin(&plan.source_host, &plan.profile, program)?;
    sync_and_restore_on_target(
        plan,
        program,
        atuin_harness,
        target_wt,
        source_atuin_id.as_deref(),
    )
}

fn sync_source_atuin(host: &str, profile: &str, program: &str) -> Result<Option<String>, String> {
    if host == local_label() {
        atuin::sync_here(profile).map_err(|e| format!("{host}: {e}"))?;
        return Ok(atuin::host_id_here(profile));
    }
    let script = format!(
        "prog={}\n\
         out=$(\"$prog\" sync 2>&1) || {{ printf '%s\\n' \"$out\"; exit 1; }}\n\
         \"$prog\" info 2>/dev/null || true\n",
        sh_quote(program),
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let msg = first_non_empty(&text).unwrap_or("atuin sync failed");
        return Err(format!("{host}: {msg}"));
    }
    Ok(atuin::parse_host_id(&text))
}

fn sync_and_restore_on_target(
    plan: &MovePlan,
    program: &str,
    atuin_harness: &str,
    worktree: &str,
    source_host_id: Option<&str>,
) -> Result<(), String> {
    let host = &plan.target_host;
    if host == &local_label() {
        atuin::sync_here(&plan.profile).map_err(|e| format!("{host}: {e}"))?;
        return atuin::restore_for_move_here(
            &plan.profile,
            &plan.harness,
            Path::new(worktree),
            &plan.agent_session,
            source_host_id,
        )
        .map_err(|e| format!("{host}: {e}"));
    }
    let script = remote_restore_script(
        program,
        &plan.harness,
        atuin_harness,
        worktree,
        &plan.agent_session,
        source_host_id,
    );
    let out = host_bash(host, &script).ok_or_else(|| format!("{host} can't be reached"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if out.status.success() {
        return Ok(());
    }
    let msg = first_non_empty(&text).unwrap_or("atuin restore failed");
    Err(format!("{host}: {msg}"))
}

/// Bash script that runs on a remote target host to sync Atuin, restore
/// `session_id` into `worktree` (retrying with `--switch --branch` on
/// divergence), and verify the harness session file exists for `worktree`.
fn remote_restore_script(
    program: &str,
    harness: &str,
    atuin_harness: &str,
    worktree: &str,
    session_id: &str,
    source_host_id: Option<&str>,
) -> String {
    let branch_arg = source_host_id
        .filter(|s| !s.is_empty())
        .map(|s| format!("@{s}"))
        .unwrap_or_default();
    let verify_session = if harness == "pi" {
        "root=${PI_CODING_AGENT_DIR:-$HOME/.config/pi}\n\
         flat=${wt#/}\n\
         flat=$(printf '%s' \"$flat\" | tr '/\\\\:' '---')\n\
         dir=\"$root/sessions/--$flat--\"\n\
         found=0\n\
         if [ -f \"$dir/$id.jsonl\" ]; then found=1; fi\n\
         for f in \"$dir\"/*_\"$id\".jsonl; do [ -f \"$f\" ] && found=1 && break; done\n"
    } else {
        "root=${CLAUDE_CONFIG_DIR:-$HOME/.config/claude-personal}\n\
         flat=$(printf '%s' \"$wt\" | LC_ALL=C sed 's/[^A-Za-z0-9]/-/g')\n\
         dir=\"$root/projects/$flat\"\n\
         found=0\n\
         if [ -f \"$dir/$id.jsonl\" ]; then found=1; fi\n"
    };
    format!(
        "prog={}\nwt={}\nid={}\nsrc_branch={}\natuin_h={}\n\
         out=$(\"$prog\" sync 2>&1) || {{ printf '%s\\n' \"$out\"; exit 1; }}\n\
         cd \"$wt\" || {{ echo \"worktree $wt is missing\"; exit 1; }}\n\
         run_resume() {{\n\
           \"$prog\" threads resume --print \"$@\" 2>&1\n\
         }}\n\
         if ! err=$(run_resume \"$id\"); then\n\
           case \"$err\" in\n\
             *--switch*|*--branch*|*--as-is*|*diverged*)\n\
               if [ -n \"$src_branch\" ]; then\n\
                 err2=$(run_resume --switch --branch \"$src_branch\" \"$id\") || {{ printf '%s\\n' \"${{err2:-$err}}\"; exit 1; }}\n\
               else\n\
                 err2=$(run_resume --switch \"$id\") || {{ printf '%s\\n' \"${{err2:-$err}}\"; exit 1; }}\n\
               fi\n\
               ;;\n\
             *\"picker needs a terminal\"*)\n\
               echo \"no $atuin_h thread $id in $prog\"; exit 1 ;;\n\
             *)\n\
               printf '%s\\n' \"$err\"; exit 1 ;;\n\
           esac\n\
         fi\n\
         {verify_session}\
         if [ \"$found\" != 1 ]; then\n\
           echo \"$prog restored {harness} session $id, but not for $wt\"; exit 1\n\
         fi\n",
        sh_quote(program),
        sh_quote(worktree),
        sh_quote(session_id),
        sh_quote(&branch_arg),
        sh_quote(atuin_harness),
    )
}

/// Open the resumed tab on the target host and migrate the registry record
/// (and any open child tasks' `parent_*` fields) atomically.
fn open_and_migrate(
    plan: &MovePlan,
    source: &SourceGit,
    target_repo: &str,
    target_wt: &str,
) -> Result<(String, String, String), String> {
    let spec = harness_spec(&plan.harness, Some(&plan.model));
    let launch_name = if source.branch == branch_and_slug(&plan.name).0 {
        plan.name.as_str()
    } else {
        source.branch.as_str()
    };
    let resume_head = if plan.ended {
        branch_head(&plan.target_host, target_repo, &source.branch)
    } else {
        plan.task["resume_head"].as_str().map(String::from)
    };
    let (new_host, new_session_id, new_window_id) = launch(
        &plan.target_host,
        &plan.project.path,
        launch_name,
        &spec,
        None,
        None,
        Some((&plan.agent_session, &plan.profile)),
    )?;
    update_tasks(|tasks| {
        migrate_registry(
            tasks,
            &plan.task,
            plan.ended,
            &plan.source_window,
            &MovedRecord {
                host: &new_host,
                session: &new_session_id,
                window: &new_window_id,
                name: &plan.name,
                branch: &source.branch,
                worktree: target_wt,
                resume_head: resume_head.as_deref(),
            },
        );
        Ok(())
    })?;
    Ok((new_host, new_session_id, new_window_id))
}

struct MovedRecord<'a> {
    host: &'a str,
    session: &'a str,
    window: &'a str,
    name: &'a str,
    branch: &'a str,
    worktree: &'a str,
    resume_head: Option<&'a str>,
}

/// Remove the old task record, carry metadata onto the newly launched record,
/// and repoint any open child tasks whose parent was `old_window`.
fn migrate_registry(
    tasks: &mut Vec<serde_json::Value>,
    old_task: &serde_json::Value,
    was_ended: bool,
    old_window: &str,
    dest: &MovedRecord<'_>,
) {
    tasks.retain(|t| t["ended_at"].is_null() == was_ended || !in_window(t, old_window));
    if let Some(new) = tasks
        .iter_mut()
        .rev()
        .find(|t| t["ended_at"].is_null() && in_window(t, dest.window))
    {
        new["name"] = dest.name.into();
        new["branch"] = dest.branch.into();
        new["worktree"] = dest.worktree.into();
        if old_task["links"].as_array().is_some_and(|l| !l.is_empty()) {
            new["links"] = old_task["links"].clone();
        }
        if old_task["pinned"].as_bool().unwrap_or(false) {
            new["pinned"] = true.into();
        }
        if !old_task["snoozed_until"].is_null() {
            new["snoozed_until"] = old_task["snoozed_until"].clone();
        }
        for key in [
            "parent_window",
            "parent_host",
            "parent_session",
            "notify_parent",
        ] {
            if !old_task[key].is_null() {
                new[key] = old_task[key].clone();
            }
        }
        if let Some(head) = dest.resume_head {
            new["resume_head"] = head.into();
        }
    }
    for child in tasks
        .iter_mut()
        .filter(|t| t["ended_at"].is_null() && t["parent_window"].as_str() == Some(old_window))
    {
        child["parent_host"] = dest.host.into();
        child["parent_session"] = dest.session.into();
        child["parent_window"] = dest.window.into();
    }
}

/// Clean up the source host after the moved task is live on the target host
/// and recorded in the registry.
fn cleanup_source(plan: &MovePlan, source: &SourceGit) {
    if !plan.ended {
        let _ = rex(
            &plan.source_host,
            &[
                "window",
                "close",
                "-s",
                &plan.source_session_id,
                &plan.source_window,
            ],
        );
        kill_if_empty(&plan.source_host, &plan.source_session_id);
    }
    if source.wt_exists {
        match remove_worktree(
            &plan.source_host,
            &plan.source_repo,
            &source.branch,
            source.dirty(),
        ) {
            Ok((note, true)) => log(&format!(
                "moved {} to {}; source cleanup warning on {}{note}",
                plan.label, plan.target_host, plan.source_host
            )),
            Err(e) => log(&format!(
                "moved {} to {}; could not remove old worktree on {}: {e}",
                plan.label, plan.target_host, plan.source_host
            )),
            Ok((_, false)) => {}
        }
    }
}

fn encode_b64_lines(bytes: &[u8]) -> String {
    let encoded = B64.encode(bytes);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 76 + 1);
    for chunk in encoded.as_bytes().chunks(76) {
        if let Ok(s) = std::str::from_utf8(chunk) {
            out.push_str(s);
            out.push('\n');
        }
    }
    out
}

fn first_non_empty(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.strip_prefix("Error: ").unwrap_or(l))
}

fn last_non_empty(text: &str) -> Option<&str> {
    text.lines().rev().map(str::trim).find(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    struct GitFixture {
        root: PathBuf,
        origin: PathBuf,
        src_repo: PathBuf,
        tgt_repo: PathBuf,
    }

    impl GitFixture {
        fn new(tag: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "switchyard-move-{tag}-{}-{unique}-{seq}",
                    std::process::id()
                ));
            let origin = root.join("origin.git");
            let src_repo = root.join("src/repo");
            let tgt_repo = root.join("tgt/repo");
            std::fs::create_dir_all(&origin).unwrap();
            std::fs::create_dir_all(src_repo.parent().unwrap()).unwrap();
            std::fs::create_dir_all(tgt_repo.parent().unwrap()).unwrap();
            git(&origin, &["init", "--quiet", "--bare", "-b", "main"]);
            assert!(
                Command::new("git")
                    .args(["clone", "--quiet"])
                    .arg(&origin)
                    .arg(&src_repo)
                    .status()
                    .unwrap()
                    .success()
            );
            std::fs::write(src_repo.join("base.txt"), "hello\n").unwrap();
            std::fs::write(src_repo.join("remove-me.txt"), "bye\n").unwrap();
            git(&src_repo, &["add", "."]);
            git(&src_repo, &["commit", "--quiet", "-m", "initial"]);
            git(&src_repo, &["push", "--quiet", "-u", "origin", "main"]);
            assert!(
                Command::new("git")
                    .args(["clone", "--quiet"])
                    .arg(&origin)
                    .arg(&tgt_repo)
                    .status()
                    .unwrap()
                    .success()
            );
            Self {
                root,
                origin,
                src_repo,
                tgt_repo,
            }
        }
    }

    impl Drop for GitFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn seed_dirty_worktree(src_repo: &Path, src_wt: &Path) {
        git(
            src_repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/task-a",
                src_wt.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::write(src_wt.join("committed.txt"), "commit-1\n").unwrap();
        git(src_wt, &["add", "committed.txt"]);
        git(src_wt, &["commit", "--quiet", "-m", "branch work"]);
        git(
            src_repo,
            &[
                "config",
                "--add",
                "branch.feature/task-a.switchyard-link",
                "1472",
            ],
        );
        std::fs::write(src_wt.join("staged.txt"), "staged\n").unwrap();
        git(src_wt, &["add", "staged.txt"]);
        std::fs::write(src_wt.join("base.txt"), "staged-line\n").unwrap();
        git(src_wt, &["add", "base.txt"]);
        std::fs::write(src_wt.join("base.txt"), "staged-line\nunstaged-line\n").unwrap();
        std::fs::remove_file(src_wt.join("remove-me.txt")).unwrap();
        std::fs::write(src_wt.join("bin.dat"), [0u8, 1, 2, 255, 42]).unwrap();
        git(src_wt, &["add", "bin.dat"]);
        std::fs::create_dir_all(src_wt.join("nested")).unwrap();
        std::fs::write(src_wt.join("nested/untracked file.txt"), "fresh\n").unwrap();
    }

    fn assert_transferred_worktree(tgt_wt: &Path) {
        assert_eq!(
            std::fs::read_to_string(tgt_wt.join("committed.txt")).unwrap(),
            "commit-1\n"
        );
        assert_eq!(
            std::fs::read_to_string(tgt_wt.join("staged.txt")).unwrap(),
            "staged\n"
        );
        assert_eq!(
            std::fs::read_to_string(tgt_wt.join("base.txt")).unwrap(),
            "staged-line\nunstaged-line\n"
        );
        assert_eq!(git_out(tgt_wt, &["show", ":base.txt"]), "staged-line");
        assert!(!tgt_wt.join("remove-me.txt").exists());
        assert_eq!(
            std::fs::read(tgt_wt.join("bin.dat")).unwrap(),
            [0u8, 1, 2, 255, 42]
        );
        assert_eq!(
            std::fs::read_to_string(tgt_wt.join("nested/untracked file.txt")).unwrap(),
            "fresh\n"
        );
    }

    #[test]
    fn bundles_committed_branch_and_carries_uncommitted_changes_faithfully() {
        let fx = GitFixture::new("full");
        let host = local_label();
        let src_wt = fx.src_repo.join(".worktrees/task-a");
        seed_dirty_worktree(&fx.src_repo, &src_wt);

        let source = inspect_source_git(
            &host,
            fx.src_repo.to_str().unwrap(),
            src_wt.to_str().unwrap(),
            "feature/task-a",
            "repo/task-a",
        )
        .unwrap();
        assert!(source.wt_exists);
        assert!(source.changes.staged && source.changes.unstaged && source.changes.untracked);
        assert_eq!(source.links, ["1472"]);

        let target = prepare_target_repo(
            &host,
            fx.tgt_repo.to_str().unwrap(),
            fx.origin.to_str().unwrap(),
            &source.branch,
            &source.head,
        )
        .unwrap();
        assert!(!target.has_source_head && target.dirty_worktree.is_none());

        let bundle = create_branch_bundle(
            &host,
            fx.src_repo.to_str().unwrap(),
            &source.branch,
            &target.have_shas,
        )
        .unwrap();
        fetch_branch_bundle(&host, &target.repo, &source.branch, &bundle).unwrap();
        update_target_branch(
            &host,
            &target.repo,
            None,
            &source.branch,
            &target.head,
            &source.head,
        )
        .unwrap();
        assert_eq!(
            git_out(&fx.tgt_repo, &["rev-parse", "refs/heads/feature/task-a"]),
            source.head
        );

        let tgt_wt = fx.tgt_repo.join(".worktrees/task-a");
        git(
            &fx.tgt_repo,
            &[
                "worktree",
                "add",
                "--quiet",
                tgt_wt.to_str().unwrap(),
                "feature/task-a",
            ],
        );
        copy_branch_links(&host, &target.repo, &source.branch, &source.links);
        assert_eq!(
            git_out(
                &fx.tgt_repo,
                &[
                    "config",
                    "--get-all",
                    "branch.feature/task-a.switchyard-link"
                ]
            ),
            "1472"
        );

        let dummy_plan = MovePlan {
            task: serde_json::json!({}),
            label: "repo/task-a".into(),
            project: Project {
                profile: "personal".into(),
                path: fx.tgt_repo.to_str().unwrap().into(),
                clone_url: fx.origin.to_str().unwrap().into(),
                name: "repo".into(),
                hosts: vec![],
                tracker: String::new(),
            },
            profile: "personal".into(),
            harness: "claude".into(),
            model: String::new(),
            name: "task-a".into(),
            branch: "feature/task-a".into(),
            ended: false,
            source_host: host.clone(),
            source_session_id: "s".into(),
            source_window: "w".into(),
            source_repo: fx.src_repo.to_str().unwrap().into(),
            source_worktree: src_wt.to_str().unwrap().into(),
            agent_session: "id".into(),
            target_host: host,
        };
        transfer_uncommitted(&dummy_plan, &source, tgt_wt.to_str().unwrap()).unwrap();
        assert_transferred_worktree(&tgt_wt);
    }

    #[test]
    fn refuses_diverged_target_branch_and_dirty_target_worktree() {
        let fx = GitFixture::new("safety");
        let host = local_label();
        let src_wt = fx.src_repo.join(".worktrees/div");
        git(
            &fx.src_repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/div",
                src_wt.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::write(src_wt.join("a.txt"), "src\n").unwrap();
        git(&src_wt, &["add", "a.txt"]);
        git(&src_wt, &["commit", "--quiet", "-m", "src commit"]);

        let tgt_wt = fx.tgt_repo.join(".worktrees/div");
        git(
            &fx.tgt_repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/div",
                tgt_wt.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::write(tgt_wt.join("b.txt"), "tgt\n").unwrap();
        git(&tgt_wt, &["add", "b.txt"]);
        git(&tgt_wt, &["commit", "--quiet", "-m", "tgt commit"]);

        let source = inspect_source_git(
            &host,
            fx.src_repo.to_str().unwrap(),
            src_wt.to_str().unwrap(),
            "feature/div",
            "repo/div",
        )
        .unwrap();
        let target = prepare_target_repo(
            &host,
            fx.tgt_repo.to_str().unwrap(),
            fx.origin.to_str().unwrap(),
            &source.branch,
            &source.head,
        )
        .unwrap();
        let bundle = create_branch_bundle(
            &host,
            fx.src_repo.to_str().unwrap(),
            &source.branch,
            &target.have_shas,
        )
        .unwrap();
        fetch_branch_bundle(&host, &target.repo, &source.branch, &bundle).unwrap();
        let err = update_target_branch(
            &host,
            &target.repo,
            target.worktree.as_deref(),
            &source.branch,
            &target.head,
            &source.head,
        )
        .unwrap_err();
        assert!(err.contains("diverged or newer"), "{err}");
        // Target branch head is untouched.
        assert_eq!(
            git_out(&fx.tgt_repo, &["rev-parse", "refs/heads/feature/div"]),
            target.head
        );

        // Dirty target worktree is detected before touching refs.
        std::fs::write(tgt_wt.join("dirty.txt"), "uncommitted\n").unwrap();
        let dirty_target = prepare_target_repo(
            &host,
            fx.tgt_repo.to_str().unwrap(),
            fx.origin.to_str().unwrap(),
            &source.branch,
            &source.head,
        )
        .unwrap();
        assert!(dirty_target.dirty_worktree.is_some());
    }

    #[test]
    fn refuses_detached_head_and_in_progress_merge() {
        let fx = GitFixture::new("guards");
        let host = local_label();
        let wt = fx.src_repo.join(".worktrees/g");
        git(
            &fx.src_repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/g",
                wt.to_str().unwrap(),
                "main",
            ],
        );
        // Simulate MERGE_HEAD in worktree gitdir.
        let gd = PathBuf::from(git_out(&wt, &["rev-parse", "--git-dir"]));
        let merge_head = gd.join("MERGE_HEAD");
        std::fs::write(&merge_head, "0000000000000000000000000000000000000000\n").unwrap();
        let err = inspect_source_git(
            &host,
            fx.src_repo.to_str().unwrap(),
            wt.to_str().unwrap(),
            "feature/g",
            "repo/g",
        )
        .unwrap_err();
        assert!(err.contains("in-progress git operation"), "{err}");
        std::fs::remove_file(&merge_head).unwrap();

        // Detach HEAD.
        git(&wt, &["checkout", "--quiet", "--detach", "HEAD"]);
        let err = inspect_source_git(
            &host,
            fx.src_repo.to_str().unwrap(),
            wt.to_str().unwrap(),
            "feature/g",
            "repo/g",
        )
        .unwrap_err();
        assert!(err.contains("detached HEAD"), "{err}");
    }

    #[test]
    fn migrate_registry_preserves_metadata_and_repoints_open_children() {
        let old = serde_json::json!({
            "host": "mac",
            "session_id": "s-old",
            "window_id": "w-old",
            "project": "switchyard",
            "name": "move-me",
            "branch": "fix/move-me",
            "worktree": "/Users/j/dev/switchyard/.worktrees/move-me",
            "links": [{"kind": "card", "ref": "1472"}],
            "pinned": true,
            "snoozed_until": 9999,
            "parent_host": "mac",
            "parent_session": "s-lead",
            "parent_window": "w-lead",
            "notify_parent": false,
            "ended_at": null
        });
        let new = serde_json::json!({
            "host": "omarchy",
            "session_id": "s-new",
            "window_id": "w-new",
            "project": "switchyard",
            "name": "fix/move-me",
            "branch": "fix/move-me",
            "worktree": "/home/j/dev/switchyard/.worktrees/fix-move-me",
            "links": [],
            "ended_at": null
        });
        let child = serde_json::json!({
            "host": "mac",
            "session_id": "s-c",
            "window_id": "w-c",
            "parent_host": "mac",
            "parent_session": "s-old",
            "parent_window": "w-old",
            "ended_at": null
        });
        let mut tasks = vec![old.clone(), new, child];
        migrate_registry(
            &mut tasks,
            &old,
            false,
            "w-old",
            &MovedRecord {
                host: "omarchy",
                session: "s-new",
                window: "w-new",
                name: "move-me",
                branch: "fix/move-me",
                worktree: "/home/j/dev/switchyard/.worktrees/move-me",
                resume_head: Some("deadbeef"),
            },
        );
        assert_eq!(tasks.len(), 2);
        let moved = tasks.iter().find(|t| t["window_id"] == "w-new").unwrap();
        assert_eq!(moved["host"], "omarchy");
        assert_eq!(moved["name"], "move-me");
        assert_eq!(moved["branch"], "fix/move-me");
        assert_eq!(
            moved["worktree"],
            "/home/j/dev/switchyard/.worktrees/move-me"
        );
        assert_eq!(moved["pinned"], true);
        assert_eq!(moved["snoozed_until"], 9999);
        assert_eq!(moved["parent_window"], "w-lead");
        assert_eq!(moved["notify_parent"], false);
        assert_eq!(moved["resume_head"], "deadbeef");
        assert_eq!(moved["links"][0]["ref"], "1472");

        let kid = tasks.iter().find(|t| t["window_id"] == "w-c").unwrap();
        assert_eq!(kid["parent_host"], "omarchy");
        assert_eq!(kid["parent_session"], "s-new");
        assert_eq!(kid["parent_window"], "w-new");
    }
}
