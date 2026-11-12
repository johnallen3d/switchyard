//! The task registry. It lives on the config host, the only one that
//! reaches every Rex host: a JSON array in $XDG_STATE_HOME/rex/spawn-tasks.json,
//! one object per spawned task (see `launch` for the fields). Rex has no
//! custom metadata for sessions or tabs, so this is what maps a tab back to
//! its project, branch and worktree. Ended tasks are kept a while so the
//! inbox can show recent finishes, then pruned.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::links::{
    Lookup, Pr, Tracker, add_link, add_live, branch_number, find_prs, github_repo, parse_ref, pr_key, set_agent_links,
    set_derived_items, set_derived_pr, tracker_or_fizzy, FIZZY_BOARD_SH,
};
use crate::rex::{home, host_bash, local_label, now};
use crate::sessions::newest_session;

/// How long ended tasks stay in the registry, and so can be resumed from the
/// inbox: Claude's default transcript cleanup (`cleanupPeriodDays`).
const ENDED_KEEP_SECS: u64 = 30 * 24 * 60 * 60;

/// How long a new task may go without its worktree: `task launch` records
/// it before `task run`, in the new tab, fetches (or clones) and creates
/// the worktree, so a missing one only ends the task after this.
const LAUNCH_GRACE_SECS: u64 = 10 * 60;

/// Whether an open task is still within its launch grace at `now`, when a
/// missing worktree may not have been created yet.
fn launching(task: &serde_json::Value, now: u64) -> bool {
    task["created_at"].as_u64().is_some_and(|created| now.saturating_sub(created) < LAUNCH_GRACE_SECS)
}

pub fn tasks_path() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local/state"))
        .join("rex/spawn-tasks.json")
}

pub fn load_tasks() -> Result<Vec<serde_json::Value>, String> {
    let path = tasks_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Replace the registry atomically, so a reader never sees half a file.
pub fn save_tasks(tasks: &[serde_json::Value]) -> Result<(), String> {
    let path = tasks_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    let mut text = serde_json::to_string_pretty(tasks).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether `task` was spawned in `window`. Matched on the window alone:
/// Rex window IDs are unique across servers, but a host's label
/// (`hostname -s`) can change under a task when macOS takes its name from
/// the network, which would leave the task unreachable by host.
pub fn in_window(task: &serde_json::Value, window: &str) -> bool {
    task["window_id"].as_str() == Some(window)
}

pub fn record_task(task: serde_json::Value) -> Result<(), String> {
    let mut tasks = load_tasks()?;
    tasks.push(task);
    save_tasks(&tasks)
}

/// Every window on a host's Rex server with its most urgent agent status, as
/// `WINDOW_ID<TAB>STATE<TAB>UPDATED_AT<TAB>MSG` lines (the last three empty
/// for a tab with no status). A tab shows its most urgent pane, as in the
/// cmd+k tab picker.
const WINDOWS_SCRIPT: &str = r#"
local T = "com.superlogical.terminal"
local urgency = { idle = 1, done = 2, working = 3, error = 4, blocked = 5 }
local lines = {}
local function clean(s) return (tostring(s or ""):gsub("[\t\r\n]", " ")) end
local list = rex.call("session.list")
for _, s in ipairs((list and list.sessions) or {}) do
  rex.session.attach{ session_id = s.session_id }
  local view = rex.session.view{ session_id = s.session_id }
  for _, w in ipairs((view and view.windows) or {}) do
    local best
    for _, l in ipairs(w.layers or {}) do
      if l.kind == "tiled" then
        for _, b in ipairs(l.blocks or {}) do
          local st = rex.block.call(T, "program_status", { session_id = s.session_id, block_id = b.block_id })
          for _, r in ipairs((st and st.records) or {}) do
            if best == nil or (urgency[r.state] or 0) > (urgency[best.state] or 0) then best = r end
          end
        end
      end
    end
    best = best or {}
    lines[#lines + 1] = table.concat({ w.window_id, clean(best.state), clean(best.updated_at), clean(best.msg) }, "\t")
  end
end
return table.concat(lines, "\n")
"#;

/// A tab's live agent status: state, when it last changed (Unix seconds,
/// 0 when unknown) and the agent's message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabStatus {
    pub state: String,
    pub at: u64,
    pub msg: String,
}

/// A `WINDOW_ID<TAB>STATE<TAB>UPDATED_AT<TAB>MSG` line, as WINDOWS_SCRIPT
/// prints them and host_rows.lua's W rows carry them after the W.
pub fn parse_window_status(line: &str) -> (String, TabStatus) {
    let mut f = line.split('\t');
    let id = f.next().unwrap_or("").to_string();
    let state = f.next().unwrap_or("").to_string();
    let at = rfc3339_secs(f.next().unwrap_or("")).unwrap_or(0);
    let msg = f.next().unwrap_or("").to_string();
    (id, TabStatus { state, at, msg })
}

/// Give each open task its tab's live agent status (`status`, `status_at`,
/// `status_msg`) from `live`, by window ID; a task whose tab isn't there
/// keeps what it had.
pub fn apply_live(tasks: &mut [serde_json::Value], live: &HashMap<String, TabStatus>) {
    for task in tasks.iter_mut().filter(|t| t["ended_at"].is_null()) {
        if let Some(status) = task["window_id"].as_str().and_then(|id| live.get(id)) {
            task["status"] = status.state.clone().into();
            task["status_at"] = status.at.into();
            task["status_msg"] = status.msg.clone().into();
        }
    }
}

/// Seconds since the Unix epoch for an RFC 3339 timestamp such as
/// `2026-11-07T15:40:18.294391-05:00`, as Rex reports status changes.
pub fn rfc3339_secs(s: &str) -> Option<u64> {
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest.as_bytes().first()? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let oh: i64 = rest.get(1..3)?.parse().ok()?;
            let om: i64 = rest.get(4..6)?.parse().ok()?;
            (oh * 3600 + om * 60) * if *sign == b'-' { -1 } else { 1 }
        }
        _ => return None,
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    u64::try_from(days * 86400 + h * 3600 + mi * 60 + sec - offset).ok()
}

fn host_windows(host: &str) -> Option<HashMap<String, TabStatus>> {
    let mut cmd = Command::new("rex");
    if host != local_label() {
        cmd.args(["-S", host]);
    }
    let out = cmd
        .args(["do", "-e", WINDOWS_SCRIPT])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let reply: String = serde_json::from_slice(&out.stdout).ok()?;
    Some(reply.lines().map(parse_window_status).collect())
}

/// Bash that sets `head` (the full branch head), `baseref` (the default
/// branch's first ref found), `baserefs` (all of them, space separated:
/// the local default branch and origin's, which may be ahead of it) and
/// `merged` for `$branch` in the checkout at `$path`: `merged` when the
/// head is in the default branch (local or origin's), `new` when it is but
/// the branch's reflog shows no commits made on it (it was only created or
/// fast-forwarded, as when it pulls the default branch), `unmerged`, or
/// empty when it can't tell. A caller that already knows them can set
/// `wt_head` (the head) and `wt_base` (origin/HEAD's short name, possibly
/// empty) to skip those lookups.
pub(crate) const MERGED_SH: &str = r#"
  head=${wt_head:-$(git -C "$path" rev-parse --verify --quiet "refs/heads/$branch" 2>/dev/null)}
  base=${wt_base-$(git -C "$path" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)}
  base=${base#origin/}
  merged=
  baseref=
  baserefs=
  for ref in "refs/heads/${base:-main}" "refs/remotes/origin/${base:-main}"; do
    [ -n "$head" ] || break
    git -C "$path" rev-parse --verify --quiet "$ref" >/dev/null 2>&1 || continue
    baseref=${baseref:-$ref}
    baserefs="$baserefs $ref"
    [ "$merged" = merged ] && continue
    if git -C "$path" merge-base --is-ancestor "$head" "$ref" 2>/dev/null; then merged=merged; else merged=unmerged; fi
  done
  if [ "$merged" = merged ]; then
    # Without a reflog it can't tell, and stays merged.
    reflog= own=
    while IFS= read -r line; do
      reflog=1
      # Only the branch's creation and fast-forwards to a default branch
      # are not its own work; anything else (commit, rebase, reset) is.
      case $line in branch:*|merge\ *:\ Fast-forward) ;; *) own=1; break ;; esac
    done < <(git -C "$path" reflog show --format=%gs "refs/heads/$branch" 2>/dev/null)
    [ -n "$reflog" ] && [ -z "$own" ] && merged=new
  fi
"#;

/// For each (repo, branch), the branch's worktree on `host` with its
/// uncommitted files (count, and the first few paths), its branch head,
/// whether that is merged into the default branch, and the repo's origin
/// URL, or an empty path when
/// the repo is there but the branch has no worktree; None for a repo that
/// can't be read. None overall when the host can't be reached. One script
/// per host on stdin: `bash -s` over SSH by Rex label (as `probe` reaches
/// hosts; their login shell is Fish), or locally for the config host.
fn host_worktrees(host: &str, pairs: &[(String, String, String)]) -> Option<Vec<Option<Worktree>>> {
    // Pairs are REPO, BRANCH and the task's recorded worktree, which still
    // counts while detached from BRANCH, as a rebase leaves it until it
    // finishes: a check in that moment would otherwise end the task as
    // "worktree removed". One line per pair:
    // PATH<TAB>COUNT<TAB>HEAD<TAB>MERGED<TAB>ORIGIN<TAB>LINKS<TAB>FIXES<TAB>FIZZY<TAB>FILE...,
    // "-<TAB>FIZZY" for no worktree, "?" for no repo. MERGED is as MERGED_SH
    // sets it; LINKS are the branch's `switchyard-link` git config values,
    // FIXES its commit message lines that may close issues, space separated;
    // FIZZY the repo's `.fizzy.yaml` board (see `links::FIZZY_BOARD_SH`).
    // Each repo's worktree list, origin URL, default branch and link config
    // are read once, the branch head comes from the worktree list, and the
    // rest is shell builtins where it can be: starting processes is most of
    // the cost, on macOS especially. Bash 3.2 (macOS's /bin/bash) runs it too.
    let mut script = String::from(
        r#"merged_state() {
"#,
    );
    script.push_str(MERGED_SH);
    script.push_str("}\n");
    script.push_str(FIZZY_BOARD_SH);
    script.push_str(
        r#"repos=() lists=() origins=() bases=() configs=() boards=()
while IFS=$'\t' read -r repo branch want; do
  list= n=
  for i in "${!repos[@]}"; do
    if [ "${repos[$i]}" = "$repo" ]; then list=${lists[$i]} n=$i; break; fi
  done
  if [ -z "$n" ]; then
    n=${#repos[@]}
    list=$(git -C "$repo" worktree list --porcelain 2>/dev/null) || list="?"
    repos[$n]=$repo lists[$n]=$list origins[$n]= bases[$n]= configs[$n]= boards[$n]=
    if [ "$list" != "?" ]; then
      origins[$n]=$(git -C "$repo" remote get-url origin 2>/dev/null)
      bases[$n]=$(git -C "$repo" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
      configs[$n]=$(git -C "$repo" config --get-regexp '^branch\..*\.switchyard-link$' 2>/dev/null)
      boards[$n]=$(fizzy_board "$repo")
      boards[$n]=${boards[$n]//$'\t'/}
    fi
  fi
  if [ "$list" = "?" ]; then echo "?"; continue; fi
  path=- wt= wt_head= sha=
  while IFS= read -r line; do
    case $line in
      "worktree "*) wt=${line#worktree } sha= ;;
      "HEAD "*) sha=${line#HEAD } ;;
      "branch refs/heads/$branch") path=$wt wt_head=$sha; break ;;
      detached) if [ -n "$want" ] && [ "$wt" -ef "$want" ]; then path=$wt wt_head=$sha; fi ;;
    esac
  done <<<"$list"
  if [ "$path" = "-" ]; then printf -- '-\t%s\n' "${boards[$n]}"; continue; fi
  count=0 files=
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    count=$((count + 1))
    if [ "$count" -le 5 ]; then files=${files:+$files$'\t'}${line:3}; fi
  done < <(git -C "$path" status --porcelain 2>/dev/null)
  wt_base=${bases[$n]}
  merged_state
  origin=${origins[$n]//$'\t'/}
  links=
  while IFS= read -r line; do
    case $line in
      "branch.$branch.switchyard-link "*) links="$links ${line#"branch.$branch.switchyard-link "}" ;;
    esac
  done <<<"${configs[$n]}"
  links=${links//$'\t'/ }
  fixes=
  if [ -n "$baseref" ]; then
    shopt -s nocasematch
    # Commits in neither default branch ref, not just since the local one:
    # a branch that pulled origin's newer one would claim what they close.
    while IFS= read -r line; do
      case $line in *'#'*) ;; *) continue ;; esac
      case $line in *fix*|*clos*|*resolv*) fixes="$fixes ${line//$'\t'/ }" ;; esac
    done < <(git -C "$path" log --format=%B "$head" --not $baserefs 2>/dev/null)
    shopt -u nocasematch
    fixes=${fixes:0:4000}
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$path" "$count" "${head:0:12}" "$merged" "$origin" "$links" "$fixes" "${boards[$n]}" "$files"
done <<'RSP_PAIRS'
"#,
    );
    for (repo, branch, worktree) in pairs {
        script.push_str(&format!("{repo}\t{branch}\t{worktree}\n"));
    }
    script.push_str("RSP_PAIRS\n");
    let out = host_bash(host, &script).filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() != pairs.len() {
        return None;
    }
    Some(
        lines
            .into_iter()
            .map(|l| match l {
                "?" => None,
                line if line == "-" || line.starts_with("-\t") => {
                    let fizzy = line.split('\t').nth(1).unwrap_or("").to_string();
                    Some(Worktree { fizzy, ..Worktree::default() })
                }
                line => {
                    let mut f = line.split('\t');
                    let path = f.next().unwrap_or("").to_string();
                    let dirty = f.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                    let head = f.next().unwrap_or("").to_string();
                    let merged = f.next().unwrap_or("").to_string();
                    let origin = f.next().unwrap_or("").to_string();
                    let links = f.next().unwrap_or("").split_whitespace().map(String::from).collect();
                    let fixes = crate::links::fixed_refs(f.next().unwrap_or(""));
                    let fizzy = f.next().unwrap_or("").to_string();
                    let files = f.filter(|s| !s.is_empty()).map(String::from).collect();
                    Some(Worktree { path, dirty, files, head, merged, origin, links, fixes, fizzy })
                }
            })
            .collect(),
    )
}

/// A task's worktree on its host; an empty path means it has none.
#[derive(Clone, Default)]
struct Worktree {
    path: String,
    /// Uncommitted files, and the first few of their paths.
    dirty: u64,
    files: Vec<String>,
    /// The branch head (abbreviated), whether it is merged (see
    /// `host_worktrees`), and the repo's origin URL.
    head: String,
    merged: String,
    origin: String,
    /// The branch's `switchyard-link` refs, and the issues its commits fix.
    links: Vec<String>,
    fixes: Vec<String>,
    /// The repo's `.fizzy.yaml` board, `ACCOUNT/BOARD`, or empty.
    fizzy: String,
}


/// The registry with each open task's live agent status from its host's Rex
/// server, as `reconcile_tasks` gives it but without checking worktrees over
/// SSH or ending anything, so the inbox can show tasks before that check.
pub fn live_tasks() -> Result<Vec<serde_json::Value>, String> {
    let mut tasks = load_tasks()?;
    let mut hosts: Vec<String> = tasks
        .iter()
        .filter(|t| t["ended_at"].is_null())
        .filter_map(|t| t["host"].as_str().map(String::from))
        .collect();
    hosts.sort();
    hosts.dedup();
    let checks: Vec<_> = hosts.into_iter().map(|host| std::thread::spawn(move || host_windows(&host))).collect();
    let mut live: HashMap<String, TabStatus> = HashMap::new();
    for handle in checks {
        if let Ok(Some(windows)) = handle.join() {
            live.extend(windows);
        }
    }
    apply_live(&mut tasks, &live);
    Ok(tasks)
}

/// Check the open tasks against their hosts, end the ones whose tab or
/// worktree is gone, prune old ended ones, and return the registry. Open
/// tasks also get their tab's live agent status (`status`, `status_at`,
/// `status_msg`) and their worktree's uncommitted files (`dirty_count`,
/// `dirty_files`), which are not saved. Tasks that ended with their tab
/// closed are checked too, since closing the tab leaves the worktree: one
/// still there gets `worktree_left` and its uncommitted files (not saved),
/// so the archive can clean it up, and one gone becomes "worktree removed".
/// Checked tasks also get their branch head and merge state
/// (`branch_head`, `branch_merged`, not saved) and their work items as
/// links (saved): the branch's git config links, and derived ones such as
/// their PR, with its live `state` and `checks` (not saved); see `links`.
/// A host that can't be reached leaves its tasks as they were, without
/// status. A new task's missing worktree ends it only after
/// `LAUNCH_GRACE_SECS`, since `task run` may still be creating it.
pub fn reconcile_tasks() -> Result<Vec<serde_json::Value>, String> {
    let mut tasks = load_tasks()?;
    let before = serde_json::to_string(&tasks).map_err(|e| e.to_string())?;
    let field = |t: &serde_json::Value, k: &str| t[k].as_str().unwrap_or("").to_string();
    let checked = |t: &serde_json::Value| t["ended_at"].is_null() || t["ended_reason"].as_str() == Some("tab closed");
    let mut hosts: Vec<String> = tasks.iter().filter(|t| checked(t)).map(|t| field(t, "host")).collect();
    hosts.sort();
    hosts.dedup();
    let checks: Vec<_> = hosts
        .into_iter()
        .map(|host| {
            let pairs: Vec<(String, String, String)> = tasks
                .iter()
                .filter(|t| checked(t) && field(t, "host") == host)
                .map(|t| (field(t, "repo"), field(t, "branch"), field(t, "worktree")))
                .collect();
            // The tab listing (Rex) and the worktree check (SSH) don't depend
            // on each other, so a host costs the slower of the two.
            std::thread::spawn(move || {
                let (windows, worktrees) = std::thread::scope(|s| {
                    let windows = s.spawn(|| host_windows(&host));
                    let worktrees = host_worktrees(&host, &pairs);
                    (windows.join().ok().flatten(), worktrees)
                });
                (host, windows, worktrees)
            })
        })
        .collect();
    let stamp = now();
    let mut live: HashMap<String, TabStatus> = HashMap::new();
    // Worktrees by HOST/WINDOW_ID.
    let mut dirty: HashMap<String, Worktree> = HashMap::new();
    for handle in checks {
        let Ok((host, windows, worktrees)) = handle.join() else { continue };
        let mine = tasks.iter_mut().filter(|t| checked(t) && t["host"].as_str() == Some(host.as_str()));
        for (i, task) in mine.enumerate() {
            let window_id = task["window_id"].as_str().unwrap_or("").to_string();
            let window_id = window_id.as_str();
            let key = format!("{host}/{window_id}");
            let worktree = worktrees.as_ref().and_then(|w| w.get(i).cloned().flatten());
            if !task["ended_at"].is_null() {
                match worktree {
                    Some(w) if w.path.is_empty() => task["ended_reason"] = "worktree removed".into(),
                    Some(w) => {
                        task["worktree"] = w.path.clone().into();
                        dirty.insert(key, w);
                    }
                    None => {}
                }
                continue;
            }
            let tab = windows.as_ref().map(|w| w.get(window_id));
            let tab_gone = matches!(tab, Some(None));
            if let Some(Some(status)) = tab {
                live.insert(window_id.to_string(), status.clone());
            }
            match worktree {
                Some(w) if w.path.is_empty() && launching(task, stamp) => {}
                Some(w) if w.path.is_empty() => {
                    task["ended_at"] = stamp.into();
                    task["ended_reason"] = "worktree removed".into();
                }
                Some(w) => {
                    task["worktree"] = w.path.clone().into();
                    dirty.insert(key, w);
                }
                None => {}
            }
            if tab_gone && task["ended_at"].is_null() {
                task["ended_at"] = stamp.into();
                task["ended_reason"] = "tab closed".into();
            }
        }
    }
    // Tasks that ended on their own just now keep their newest agent session
    // (after a /clear it is no longer the one spawn chose), for a resume.
    let ended_now: Vec<usize> = (0..tasks.len()).filter(|&i| tasks[i]["ended_at"].as_u64() == Some(stamp)).collect();
    let found: Vec<_> = ended_now
        .iter()
        .map(|&i| {
            let task = tasks[i].clone();
            std::thread::spawn(move || newest_session(&task))
        })
        .collect();
    for (&i, handle) in ended_now.iter().zip(found) {
        if let Ok(Some(id)) = handle.join() {
            tasks[i]["agent_session"] = id.into();
        }
    }
    // Each checked task's work items (see `links`): its branch's git config
    // links, issues its branch name and commits name, and its PR from
    // GitHub, which is saved with its live state added below.
    let projects = crate::hosts::catalog().unwrap_or_default();
    let mut plans: Vec<(usize, Tracker, Option<Worktree>)> = Vec::new();
    for (i, task) in tasks.iter().enumerate().filter(|(_, t)| checked(t)) {
        let key = format!("{}/{}", field(task, "host"), field(task, "window_id"));
        let worktree = dirty.get(&key).cloned();
        let repo = crate::folders::home_relative(&field(task, "repo"));
        let project = projects.iter().find(|p| crate::folders::home_relative(p.path.trim_end_matches('/')) == repo);
        let remote = worktree
            .as_ref()
            .map(|w| w.origin.clone())
            .filter(|o| github_repo(o).is_some())
            .or_else(|| project.map(|p| p.clone_url.clone()))
            .unwrap_or_default();
        let board = worktree.as_ref().map(|w| w.fizzy.as_str()).unwrap_or("");
        let catalog = project.map(|p| p.tracker.as_str()).unwrap_or("");
        let mut tracker = Tracker::new(&tracker_or_fizzy(catalog, board), &remote);
        if tracker.github.is_none() {
            tracker.github = task["links"].as_array().and_then(|links| {
                links.iter().filter(|l| l["kind"] == "pr" && l["system"] == "github").find_map(|l| {
                    l["ref"].as_str()?.split_once('#').map(|(repo, _)| repo.to_string())
                })
            });
        }
        plans.push((i, tracker, worktree));
    }
    let mut lookups: Vec<(usize, Lookup)> = Vec::new();
    // The derived issue and card links of tasks whose worktree was read,
    // rebuilt rather than added to, so one the branch no longer implies
    // goes. Without a worktree (or a PR answer) they are only added to.
    let mut rebuilt: HashMap<usize, Vec<serde_json::Value>> = HashMap::new();
    for (i, tracker, worktree) in &plans {
        let task = &mut tasks[*i];
        let branch = field(task, "branch");
        let mut items = Vec::new();
        if let Some(w) = worktree {
            set_agent_links(task, &w.links, tracker);
            items.extend(w.fixes.iter().filter_map(|r| parse_ref(r, tracker)));
        }
        items.extend(branch_number(&branch).and_then(|n| parse_ref(n, tracker)));
        if worktree.as_ref().is_some_and(|w| !w.path.is_empty()) {
            rebuilt.insert(*i, items);
        } else {
            for link in items {
                add_link(task, link, "derived");
            }
        }
        if let (Some(repo), false) = (&tracker.github, branch.is_empty()) {
            lookups.push((*i, Lookup { profile: field(task, "profile"), repo: repo.clone(), branch }));
        }
    }
    let prs = find_prs(&lookups.iter().map(|(_, l)| l.clone()).collect::<Vec<_>>());
    let mut live_prs: Vec<(usize, String, Pr)> = Vec::new();
    for (i, l) in &lookups {
        let Some(pr) = prs.get(&pr_key(&l.repo, &l.branch)) else {
            // GitHub couldn't be asked, so the PR body's links stay.
            for link in rebuilt.remove(i).unwrap_or_default() {
                add_link(&mut tasks[*i], link, "derived");
            }
            continue;
        };
        set_derived_pr(&mut tasks[*i], &l.repo, pr);
        if let Some(pr) = pr {
            let tracker = &plans.iter().find(|(k, _, _)| k == i).expect("planned").1;
            let items = pr.fixes.iter().filter_map(|r| parse_ref(r, tracker));
            match rebuilt.get_mut(i) {
                Some(rebuilt) => rebuilt.extend(items),
                None => {
                    for link in items {
                        add_link(&mut tasks[*i], link, "derived");
                    }
                }
            }
            live_prs.push((*i, l.repo.clone(), pr.clone()));
        }
    }
    for (i, items) in rebuilt {
        set_derived_items(&mut tasks[i], items);
    }
    let live_prs: Vec<(String, String, Pr)> = live_prs
        .into_iter()
        .map(|(i, repo, pr)| (format!("{}/{}", field(&tasks[i], "host"), field(&tasks[i], "window_id")), repo, pr))
        .collect();
    tasks.retain(|t| t["ended_at"].as_u64().is_none_or(|ended| stamp.saturating_sub(ended) < ENDED_KEEP_SECS));
    // A pin or snooze saved while the hosts were checked wins over the
    // registry as it was read.
    if let Ok(fresh) = load_tasks() {
        carry_inbox_state(&mut tasks, &fresh);
    }
    if serde_json::to_string(&tasks).map_err(|e| e.to_string())? != before {
        save_tasks(&tasks)?;
    }
    apply_live(&mut tasks, &live);
    for task in tasks.iter_mut() {
        let open = task["ended_at"].is_null();
        let key = format!("{}/{}", field(task, "host"), field(task, "window_id"));
        if let Some(w) = dirty.get(&key) {
            task["dirty_count"] = w.dirty.into();
            task["dirty_files"] = w.files.clone().into();
            if !w.head.is_empty() {
                task["branch_head"] = w.head.clone().into();
            }
            if !w.merged.is_empty() {
                task["branch_merged"] = w.merged.clone().into();
            }
            if !open {
                task["worktree_left"] = true.into();
            }
        }
        for (_, repo, pr) in live_prs.iter().filter(|(k, _, _)| *k == key) {
            add_live(task, repo, pr);
        }
    }
    Ok(tasks)
}

/// The inbox state John sets on a task (see actions' pin and snooze).
const INBOX_KEYS: [&str; 2] = ["pinned", "snoozed_until"];

/// Copy each task's inbox state from `fresh`, a later read of the registry,
/// matching tasks by host, window and start.
fn carry_inbox_state(tasks: &mut [serde_json::Value], fresh: &[serde_json::Value]) {
    let same = |a: &serde_json::Value, b: &serde_json::Value| {
        ["host", "window_id", "created_at"].iter().all(|k| a[*k] == b[*k])
    };
    for task in tasks.iter_mut() {
        let Some(newer) = fresh.iter().find(|f| same(f, task)) else { continue };
        let Some(obj) = task.as_object_mut() else { continue };
        for key in INBOX_KEYS {
            match newer.get(key) {
                Some(v) => obj.insert(key.into(), v.clone()),
                None => obj.remove(key),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn inbox_state_saved_during_a_check_survives_it() {
        let mut tasks = vec![
            serde_json::json!({"host":"h", "window_id":"a", "created_at":1, "status":"x", "pinned":true}),
            serde_json::json!({"host":"h", "window_id":"b", "created_at":2}),
            serde_json::json!({"host":"h", "window_id":"c", "created_at":3, "pinned":true}),
        ];
        let fresh = vec![
            serde_json::json!({"host":"h", "window_id":"a", "created_at":1}),
            serde_json::json!({"host":"h", "window_id":"b", "created_at":2, "pinned":true, "snoozed_until":9}),
        ];
        carry_inbox_state(&mut tasks, &fresh);
        assert_eq!(tasks[0], serde_json::json!({"host":"h", "window_id":"a", "created_at":1, "status":"x"}));
        assert_eq!(tasks[1]["snoozed_until"], 9);
        assert_eq!(tasks[1]["pinned"], true);
        // Gone from the later read (pruned or replaced): left alone.
        assert_eq!(tasks[2]["pinned"], true);
    }

    #[test]
    fn a_new_task_waits_for_its_worktree() {
        let task = serde_json::json!({"created_at": 1000});
        assert!(launching(&task, 1000));
        assert!(launching(&task, 1000 + LAUNCH_GRACE_SECS - 1));
        assert!(!launching(&task, 1000 + LAUNCH_GRACE_SECS));
        // An entry without a creation time gets no grace.
        assert!(!launching(&serde_json::json!({}), 1000));
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn worktree_script_reports_head_merge_state_and_origin() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("switchyard-wt-{}-{unique}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", "git@github.com:o/r.git"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        for b in ["fresh", "done", "open", "pulled", "rebasing"] {
            git(&repo, &["worktree", "add", "--quiet", "-b", b, root.join(b).to_str().unwrap(), "main"]);
        }
        git(&root.join("done"), &["commit", "--quiet", "--allow-empty", "-m", "done"]);
        git(&repo, &["merge", "--quiet", "--ff-only", "done"]);
        git(&root.join("open"), &["commit", "--quiet", "--allow-empty", "-m", "open\n\nFixes #9, closes o/r#10"]);
        git(&repo, &["config", "--add", "branch.open.switchyard-link", "fizzy:5"]);
        git(&repo, &["config", "--add", "branch.open.switchyard-link", "https://github.com/o/r/issues/3"]);
        // Origin's default branch moves on with someone else's fix, and a
        // branch with no commits of its own pulls it; local main lags.
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "other (#8)\n\nResolves #7"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&repo, &["reset", "--quiet", "--hard", "HEAD~1"]);
        git(&root.join("pulled"), &["merge", "--quiet", "--ff-only", "origin/main"]);
        // Mid-rebase, a worktree is detached from its branch.
        git(&root.join("rebasing"), &["checkout", "--quiet", "--detach"]);
        std::fs::write(root.join("open/new.txt"), "x").unwrap();
        std::fs::write(repo.join(".fizzy.yaml"), "account: \"6284043\"\napi_url: https://app.fizzy.do\nboard: b1\n").unwrap();
        let r = repo.to_str().unwrap().to_string();
        let mut pairs: Vec<(String, String, String)> =
            ["fresh", "done", "open", "gone", "pulled"].iter().map(|b| (r.clone(), b.to_string(), String::new())).collect();
        // Detached: found at the task's recorded worktree, and not without it.
        let rebasing = root.join("rebasing").to_str().unwrap().to_string();
        pairs.push((r.clone(), "rebasing".into(), rebasing.clone()));
        pairs.push((r.clone(), "rebasing".into(), String::new()));
        let got = host_worktrees(&local_label(), &pairs).unwrap();
        let w: Vec<Worktree> = got.into_iter().map(Option::unwrap).collect();
        assert_eq!(w[0].merged, "new");
        assert_eq!(w[1].merged, "merged");
        assert_eq!(w[2].merged, "unmerged");
        assert_eq!((w[2].dirty, w[2].files.clone()), (1, vec!["new.txt".to_string()]));
        assert_eq!(w[2].links, ["fizzy:5", "https://github.com/o/r/issues/3"]);
        assert_eq!(w[2].fixes, ["#9", "o/r#10"]);
        assert!(w[0].links.is_empty() && w[0].fixes.is_empty());
        assert!(w[3].path.is_empty());
        assert_eq!(w[4].merged, "new");
        assert!(w[4].fixes.is_empty(), "{:?}", w[4].fixes);
        assert!(std::path::Path::new(&w[5].path) == std::fs::canonicalize(&rebasing).unwrap() || w[5].path == rebasing, "{}", w[5].path);
        assert!(w[6].path.is_empty());
        // The repo's `.fizzy.yaml` board, with or without a worktree.
        assert!(w.iter().all(|w| w.fizzy == "6284043/b1"));
        assert!(w[..3].iter().chain(&w[4..6]).all(|w| w.head.len() == 12 && w.origin == "git@github.com:o/r.git"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
