//! What the highlighted task has changed, in Inbox's preview: ctrl-d turns
//! the transcript into its diff against the base branch, then its local
//! commits, then back (see `Lens`).
//!
//! A thread fetches them from core's `changes` for the tab the picker asks
//! for, only while a lens other than the transcript is on, and sends them
//! as a `changes` paste: `TARGET/SESSION/WINDOW`, a newline, then the JSON.
//! The diff starts at its top, a list of the changed files over the patch,
//! and stays where John scrolled it as fetches arrive.

use std::fmt::Write as _;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use switchyard_ui::Palette;
use switchyard_ui::draw::str_width;

use crate::model::age;
use crate::preview::{self, Pane};

/// How often the shown changes are fetched again: Git over SSH is slower
/// than a log read, and a diff changes less often than a transcript.
pub const REFRESH: Duration = Duration::from_secs(5);

/// What Inbox's preview shows of the highlighted agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lens {
    #[default]
    Transcript,
    Diff,
    Commits,
}

impl Lens {
    pub const ALL: [Lens; 3] = [Lens::Transcript, Lens::Diff, Lens::Commits];

    /// The one ctrl-d moves to.
    pub fn next(self) -> Lens {
        match self {
            Lens::Transcript => Lens::Diff,
            Lens::Diff => Lens::Commits,
            Lens::Commits => Lens::Transcript,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lens::Transcript => "Transcript",
            Lens::Diff => "Diff",
            Lens::Commits => "Commits",
        }
    }
}

struct File {
    path: String,
    added: u64,
    removed: u64,
    binary: bool,
    untracked: bool,
}

struct Commit {
    sha: String,
    subject: String,
    at: u64,
}

struct Data {
    base: String,
    branch: String,
    dirty: u64,
    commits: Vec<Commit>,
    files: Vec<File>,
    patch: String,
    truncated: bool,
}

/// A task's changes, or why there are none, and its lines as last drawn.
pub struct Changes {
    pub pane: Pane,
    data: Result<Data, String>,
    drawn: Option<(Lens, u16, Vec<Line<'static>>)>,
}

/// A `changes` paste's body: the tab's header line, then core's JSON.
pub fn parse(body: &str) -> Option<Changes> {
    let (header, json) = body.split_once('\n')?;
    let mut f = header.splitn(3, '/');
    let pane = Pane {
        target: f.next()?.into(),
        session: f.next()?.into(),
        window: f.next()?.trim_end().into(),
    };
    let data = match serde_json::from_str::<Value>(json) {
        Ok(v) => match v["error"].as_str() {
            Some(e) => Err(e.to_string()),
            None => Ok(data(&v)),
        },
        Err(_) => Err("unreadable changes".to_string()),
    };
    Some(Changes {
        pane,
        data,
        drawn: None,
    })
}

fn data(v: &Value) -> Data {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    Data {
        base: s(v, "base"),
        branch: s(v, "branch"),
        dirty: v["dirty"].as_u64().unwrap_or(0),
        commits: list("commits")
            .iter()
            .map(|c| Commit {
                sha: s(c, "sha"),
                subject: s(c, "subject"),
                at: c["at"].as_u64().unwrap_or(0),
            })
            .collect(),
        files: list("files")
            .iter()
            .map(|f| File {
                path: s(f, "path"),
                added: f["added"].as_u64().unwrap_or(0),
                removed: f["removed"].as_u64().unwrap_or(0),
                binary: f["binary"].as_bool().unwrap_or(false),
                untracked: f["untracked"].as_bool().unwrap_or(false),
            })
            .collect(),
        patch: s(v, "patch"),
        truncated: v["truncated"].as_bool().unwrap_or(false),
    }
}

impl Changes {
    /// `lens`'s lines wrapped to `width`, or why there are none.
    pub fn lines(
        &mut self,
        lens: Lens,
        width: u16,
        now: u64,
        p: &Palette,
    ) -> Result<&[Line<'static>], &str> {
        let data = self.data.as_ref().map_err(String::as_str)?;
        if self
            .drawn
            .as_ref()
            .is_none_or(|(l, w, _)| *l != lens || *w != width)
        {
            let w = usize::from(width).max(1);
            let lines = match lens {
                Lens::Commits => commit_lines(data, w, now, p),
                _ => diff_lines(data, w, p),
            };
            self.drawn = Some((lens, width, lines));
        }
        Ok(self
            .drawn
            .as_ref()
            .map(|(_, _, l)| l.as_slice())
            .unwrap_or_default())
    }

    /// What the lens shows, in brief, for the line over it: the branch
    /// against its base, then the diff's size or the commits' count.
    pub fn summary(&self, lens: Lens) -> Option<String> {
        let d = self.data.as_ref().ok()?;
        let range = if d.branch.is_empty() {
            format!("{}…HEAD", d.base)
        } else {
            format!("{}…{}", d.base, d.branch)
        };
        if lens == Lens::Commits {
            let mut s = format!("{range} · {}", plural(d.commits.len() as u64, "commit"));
            if d.dirty > 0 {
                let _ = write!(s, " · {} uncommitted", d.dirty);
            }
            return Some(s);
        }
        let added: u64 = d.files.iter().map(|f| f.added).sum();
        let removed: u64 = d.files.iter().map(|f| f.removed).sum();
        Some(format!(
            "{range} · {} +{added} −{removed}",
            plural(d.files.len() as u64, "file")
        ))
    }
}

fn plural(n: u64, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// The changed files with their line counts, then the patch: a band per
/// file, hunk headers, and added and removed lines in their colors.
fn diff_lines(d: &Data, width: usize, p: &Palette) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    if d.files.is_empty() {
        let what = if d.branch.is_empty() {
            "HEAD".to_string()
        } else {
            d.branch.clone()
        };
        out.push(Line::styled(
            format!("No changes on {what} since {}", d.base),
            Style::new().fg(p.muted),
        ));
        return out;
    }
    let num = |n: u64, sign: char| format!("{sign}{n}");
    let wa = d
        .files
        .iter()
        .map(|f| num(f.added, '+').len())
        .max()
        .unwrap_or(2);
    let wr = d
        .files
        .iter()
        .map(|f| num(f.removed, '-').len())
        .max()
        .unwrap_or(2);
    for f in &d.files {
        let mut spans = if f.binary {
            vec![Span::styled(
                format!("{:>w$} ", "bin", w = wa + wr + 1),
                Style::new().fg(p.muted),
            )]
        } else {
            vec![
                Span::styled(
                    format!("{:>wa$} ", num(f.added, '+')),
                    Style::new().fg(p.foam),
                ),
                Span::styled(
                    format!("{:>wr$} ", num(f.removed, '−')),
                    Style::new().fg(p.love),
                ),
            ]
        };
        spans.push(Span::styled(f.path.clone(), Style::new().fg(p.text)));
        if f.untracked {
            spans.push(Span::styled(" untracked", Style::new().fg(p.muted)));
        }
        out.push(Line::from(spans));
    }
    let band = Style::new()
        .fg(p.text)
        .bg(p.surface)
        .add_modifier(Modifier::BOLD);
    for line in d.patch.lines() {
        let line = line.replace('\t', "    ");
        if let Some(rest) = line.strip_prefix("diff --git ") {
            out.push(Line::default());
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b);
            let pad = width.saturating_sub(str_width(path) + 1);
            out.push(Line::styled(format!(" {path}{}", " ".repeat(pad)), band));
            continue;
        }
        if ["index ", "--- ", "+++ "]
            .iter()
            .any(|pre| line.starts_with(pre))
        {
            continue;
        }
        let style = match line.chars().next() {
            Some('+') => Style::new().fg(p.foam),
            Some('-') => Style::new().fg(p.love),
            Some('@') => Style::new().fg(p.iris),
            Some(' ') => Style::new().fg(p.subtle),
            _ => Style::new().fg(p.muted),
        };
        out.extend(
            wrap(&line, width)
                .into_iter()
                .map(|l| Line::styled(l, style)),
        );
    }
    if d.truncated {
        out.push(Line::default());
        out.push(Line::styled(
            "… the diff goes on; open the task to see the rest",
            Style::new().fg(p.muted),
        ));
    }
    out
}

/// The uncommitted changes, when there are any, then the commits on the
/// branch, newest first, each with its age.
fn commit_lines(d: &Data, width: usize, now: u64, p: &Palette) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    if d.dirty > 0 {
        out.push(Line::from(vec![
            Span::styled("●       ", Style::new().fg(p.gold)),
            Span::styled(
                plural(d.dirty, "uncommitted change"),
                Style::new().fg(p.gold),
            ),
        ]));
    }
    if d.commits.is_empty() {
        out.push(Line::styled(
            format!("No commits beyond {}", d.base),
            Style::new().fg(p.muted),
        ));
    }
    for c in &d.commits {
        let when = age(now.saturating_sub(c.at));
        let sha = format!("{:<8}", c.sha);
        let room = width.saturating_sub(str_width(&sha) + str_width(&when) + 2);
        let subject = preview::clip_width(&c.subject, room);
        let pad = room.saturating_sub(str_width(&subject));
        out.push(Line::from(vec![
            Span::styled(sha, Style::new().fg(p.gold)),
            Span::styled(subject, Style::new().fg(p.text)),
            Span::raw(" ".repeat(pad + 2)),
            Span::styled(when, Style::new().fg(p.muted)),
        ]));
    }
    out
}

/// `text` broken into rows of at most `width` columns, by character, as a
/// diff's lines have no words to wrap at.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let w = str_width(c.encode_utf8(&mut [0; 4]));
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().unwrap_or(&mut String::new()).push(c);
        used += w;
    }
    rows
}

/// Fetch the changes the standalone picker wants (sent on `asks`), once
/// the highlight settles and every REFRESH after; see `preview::follow`.
pub fn follow(asks: &Receiver<Option<Pane>>, tx: &Sender<String>) {
    preview::follow_with(asks, tx, "changes", REFRESH, |p| {
        switchyard_core::changes::fetch(&p.target, &p.session, &p.window)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    const BODY: &str = r#"-/s/w
{"base":"main","branch":"feature/x","dirty":2,
 "commits":[{"sha":"abc1234","at":1000,"subject":"add two"}],
 "files":[{"path":"a.txt","added":2,"removed":1,"binary":false,"untracked":false},
          {"path":"new.txt","added":1,"removed":0,"binary":false,"untracked":true}],
 "patch":"diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1,2 @@\n-one\n+uno\n+two\n",
 "truncated":false}"#;

    #[test]
    fn the_diff_lists_files_then_draws_the_patch_without_its_plumbing() {
        let p = Palette::load();
        let mut c = parse(BODY).unwrap();
        assert_eq!(c.pane.window, "w");
        let lines: Vec<String> = c
            .lines(Lens::Diff, 20, 0, &p)
            .unwrap()
            .iter()
            .map(text)
            .collect();
        assert_eq!(
            lines,
            [
                "+2 −1 a.txt",
                "+1 −0 new.txt untracked",
                "",
                " a.txt              ",
                "@@ -1 +1,2 @@",
                "-one",
                "+uno",
                "+two",
            ]
        );
        assert_eq!(
            c.summary(Lens::Diff).unwrap(),
            "main…feature/x · 2 files +3 −1"
        );
    }

    #[test]
    fn commits_list_uncommitted_work_first_then_newest_first_with_ages() {
        let p = Palette::load();
        let mut c = parse(BODY).unwrap();
        let lines: Vec<String> = c
            .lines(Lens::Commits, 30, 1000 + 7200, &p)
            .unwrap()
            .iter()
            .map(text)
            .collect();
        assert_eq!(lines[0], "●       2 uncommitted changes");
        assert_eq!(lines[1], "abc1234 add two             2h");
        assert_eq!(
            c.summary(Lens::Commits).unwrap(),
            "main…feature/x · 1 commit · 2 uncommitted"
        );
    }

    #[test]
    fn long_diff_lines_wrap_by_character() {
        assert_eq!(wrap("+abcdef", 3), ["+ab", "cde", "f"]);
    }

    #[test]
    fn a_failed_read_says_why() {
        let p = Palette::load();
        let mut c = parse("-/s/w\n{\"error\":\"not a Git checkout\"}").unwrap();
        assert_eq!(
            c.lines(Lens::Diff, 20, 0, &p).err(),
            Some("not a Git checkout")
        );
    }

    #[test]
    fn ctrl_g_cycles_back_to_the_transcript() {
        assert_eq!(Lens::Transcript.next().next().next(), Lens::Transcript);
    }
}
