//! Reviewing in tuicr, for the tab picker's review float (`switchyard
//! review`, see the CLI): a task's changes in its checkout, to send the
//! review to its agent, or a pull request on GitHub.
//!
//! A task's review runs on the agent's host, here or there over `ssh -t`:
//! `prepare` writes a small script into a fresh temporary folder on that
//! host, `run` runs it in the terminal, and `collect` reads back what it
//! left and removes the folder. The script runs tuicr in the checkout on
//! the branch's commits since its base (`main`, else `master`, else
//! `origin/HEAD`'s branch, as core's `changes` finds it) and the working
//! tree, with `--stdout`: tuicr draws on the terminal, and its `y` writes
//! the review as Markdown to stdout and quits, which the script keeps in a
//! file (a terminal over SSH carries stdout and the screen together).
//! Quitting with `:q` leaves no review.
//!
//! tuicr keeps a review's comments in a session between runs; `clear`
//! drops them (keeping the files marked reviewed) once the review has
//! reached the agent, so the next review starts afresh.

use std::process::{Command, ExitStatus};

use crate::rex::{host_bash, local_label, sh_quote, ssh_tty};
use crate::transcript::unhex;

/// The review float's pane label.
pub const LABEL: &str = "switchyard-review";

/// Puts the user's own tools (tuicr, Git) on `PATH`, for a pane or SSH
/// command that may start with a bare one.
pub const PATH_SETUP: &str = "PATH=\"$HOME/.local/bin:$HOME/.nix-profile/bin:\
/etc/profiles/per-user/$(id -un)/bin:/run/current-system/sw/bin:/opt/homebrew/bin:$PATH\"; \
export PATH";

/// The script `run` runs, with the checkout as `$1`; it leaves tuicr's
/// review, stderr and exit status beside itself.
fn script() -> String {
    format!(
        r#"{PATH_SETUP}
here=$(dirname "$0")
cd "$1" || {{ echo "no checkout at $1" >"$here/stderr"; echo 1 >"$here/status"; exit 1; }}
base=
for b in main master; do
  if git rev-parse -q --verify "refs/heads/$b" >/dev/null; then base=$b; break; fi
done
[ -n "$base" ] || base=$(git symbolic-ref -q --short refs/remotes/origin/HEAD)
if [ -n "$base" ] && [ -n "$(git rev-list -n1 "$base..HEAD" 2>/dev/null)" ]; then
  set -- -r "$base..HEAD"
else
  set --
fi
tuicr -w "$@" --stdout --no-update-check >"$here/review.md" 2>"$here/stderr"
echo $? >"$here/status"
"#
    )
}

/// The agent's host for a picker target: `-` for this host, else the
/// hex-encoded Rex label.
#[must_use]
pub fn target_host(target: &str) -> String {
    if target == "-" {
        local_label()
    } else {
        unhex(target)
    }
}

/// Write the review script into a new temporary folder on `host`, and
/// give the folder.
///
/// # Errors
///
/// Fails when the host can't be reached or the folder can't be made.
pub fn prepare(host: &str) -> Result<String, String> {
    let out = host_bash(
        host,
        &format!(
            "d=$(mktemp -d \"${{TMPDIR:-/tmp}}/switchyard-review.XXXXXX\") || exit 1\n\
             cat >\"$d/run.sh\" <<'SWITCHYARD_REVIEW'\n{}SWITCHYARD_REVIEW\n\
             printf %s \"$d\"\n",
            script()
        ),
    )
    .ok_or_else(|| format!("can't reach {host}"))?;
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || dir.is_empty() {
        return Err(format!("can't make a temporary folder on {host}"));
    }
    Ok(dir)
}

/// Run the script `prepare` wrote in `folder` on `host`, on `checkout`, in
/// this terminal.
///
/// # Errors
///
/// Fails when the shell or ssh can't be started.
pub fn run(host: &str, folder: &str, checkout: &str) -> Result<ExitStatus, String> {
    let script = format!("{folder}/run.sh");
    let mut cmd = if host == local_label() {
        let mut c = Command::new("sh");
        c.arg(&script).arg(checkout);
        c
    } else {
        let mut c = ssh_tty(host);
        c.arg(format!("sh {} {}", sh_quote(&script), sh_quote(checkout)));
        c
    };
    cmd.status()
        .map_err(|e| format!("can't run the review: {e}"))
}

/// What a review run left.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// tuicr's exit status, when it got as far as having one.
    pub status: Option<i32>,
    /// What tuicr wrote to stderr: its session and summary lines, or why
    /// it failed.
    pub stderr: String,
    /// The review, Markdown; empty when John quit without one.
    pub review: String,
}

impl Outcome {
    /// tuicr's session slug, for `clear`.
    #[must_use]
    pub fn session(&self) -> Option<&str> {
        self.line("tuicr-session:")
    }

    /// tuicr's summary: files reviewed and comments added.
    #[must_use]
    pub fn summary(&self) -> Option<&str> {
        self.line("tuicr-summary:")
    }

    fn line(&self, prefix: &str) -> Option<&str> {
        self.stderr
            .lines()
            .find_map(|l| l.strip_prefix(prefix))
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

/// Read what the review in `folder` on `host` left, then remove the
/// folder.
///
/// # Errors
///
/// Fails when the host can't be reached.
pub fn collect(host: &str, folder: &str) -> Result<Outcome, String> {
    let d = sh_quote(folder);
    let out = host_bash(
        host,
        &format!(
            "cat {d}/status 2>/dev/null; printf '\\0'; cat {d}/stderr 2>/dev/null; \
             printf '\\0'; cat {d}/review.md 2>/dev/null; rm -rf {d}"
        ),
    )
    .ok_or_else(|| format!("can't reach {host}"))?;
    Ok(parse(&String::from_utf8_lossy(&out.stdout)))
}

fn parse(text: &str) -> Outcome {
    let mut parts = text.splitn(3, '\0');
    let status = parts.next().and_then(|s| s.trim().parse().ok());
    let stderr = parts.next().unwrap_or_default().trim().to_string();
    let review = parts.next().unwrap_or_default().trim().to_string();
    Outcome {
        status,
        stderr,
        review,
    }
}

/// Drop the comments of tuicr's review session `session` on `host`,
/// keeping its reviewed marks.
///
/// # Errors
///
/// Fails when the host can't be reached or tuicr refuses.
pub fn clear(host: &str, session: &str) -> Result<(), String> {
    let out = host_bash(
        host,
        &format!(
            "{PATH_SETUP}; tuicr review clearc --session {} >/dev/null",
            sh_quote(session)
        ),
    )
    .ok_or_else(|| format!("can't reach {host}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("tuicr couldn't clear review session {session}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_status_stderr_and_review() {
        let o = parse(
            "0\n\0tuicr-session: repo@feat/x\ntuicr-summary: reviewed 0/4 files, 1 comment added\n\
             \0## Session: repo@feat/x\n\n1. **[ISSUE]** `f.txt:3` - fix\n",
        );
        assert_eq!(o.status, Some(0));
        assert_eq!(o.session(), Some("repo@feat/x"));
        assert_eq!(o.summary(), Some("reviewed 0/4 files, 1 comment added"));
        assert!(o.review.starts_with("## Session"));
        assert!(o.review.ends_with("- fix"));
    }

    #[test]
    fn parse_a_run_that_never_started() {
        let o = parse("\0\0");
        assert_eq!(o, Outcome::default());
        assert_eq!(o.session(), None);
    }

    #[test]
    fn the_script_reviews_the_branch_and_working_tree() {
        let s = script();
        assert!(s.contains("tuicr -w \"$@\" --stdout"));
        assert!(s.contains("set -- -r \"$base..HEAD\""));
        // It ends before the heredoc's terminator, on a line of its own.
        assert!(s.ends_with('\n'));
        assert!(!s.contains("SWITCHYARD_REVIEW"));
    }
}
