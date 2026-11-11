//! Rex's ctrl+cmd+n and cmd+shift+n float, and the tools behind it.
//!
//! Its parts, behind the `switchyard` binary on every host (crates/cli
//! parses the arguments):
//!
//! - The picker (`switchyard new`, `switchyard float new`): runs in the float
//!   on the focused session's host. It lists the catalog's projects and the
//!   zoxide folders of one host (ctrl-o chooses the host), and takes either a
//!   plain session in the highlighted folder or project, or agent work in a
//!   catalog project or Git folder: a name, a harness (ctrl-t, Claude by
//!   default), an optional work item and an optional prompt. ctrl-s switches
//!   the kind; `--kind` starts it (cmd+shift+n: session, ctrl+cmd+n: agent).
//!   The picker reports the choice in the terminal title as
//!   `rex-new:session/HOST/PATH` or
//!   `rex-new:agent/HOST/PATH/NAME/HARNESS[/PROMPT_REF[/WORK_ITEM]]` (fields
//!   hex encoded); the config's `new` mode turns Enter into the action, as
//!   with the tab picker. Escape reaches the picker, which exits (closing the
//!   float) but asks first when the prompt has text. Titles are capped at
//!   about 1 KiB, too little for a prompt, so the optional prompt lives in a
//!   file on the picker's host (see core's `prompts`), rewritten as it is
//!   typed because the picker never sees the Enter that launches, and the
//!   title carries only its `LABEL:ID` reference. The float passes `--home
//!   LABEL FOLDERS`, the config host's label and zoxide folders; as
//!   `switchyard new` from a shell on the config host, the picker is
//!   standalone instead: Enter reaches it, and it acts itself and shows the
//!   new tab.
//! - `switchyard task open HOST DIR`: runs on the config host. Finds or
//!   creates the session named after DIR on HOST (a ~/ path is expanded for
//!   HOST) and prints its ID, as a session pick's action needs (see core's
//!   `folders`).
//! - `switchyard task launch HOST PATH NAME HARNESS [PROMPT_REF
//!   [WORK_ITEM]]`: runs on the config host (the Mac), which can reach every
//!   Rex host. HOST `auto` picks from the project's host pool (see core's
//!   `hosts::pool`), least busy first. A Git folder outside the catalog uses
//!   its explicit host and primary checkout. Reads the prompt from the
//!   picker's host (PROMPT_REF may be empty), leads it with the optional work
//!   item (see core's `launch::work_item_prompt`), finds or creates the
//!   project's session on the host, opens a tab running `task run` there,
//!   deletes the prompt file, records the task (see core's `tasks`), and
//!   prints `HOST<TAB>SESSION_ID<TAB>WINDOW_ID`.
//! - `switchyard task list`: runs on the config host. Checks every recorded
//!   task against its host, marks the ones whose tab or worktree is gone as
//!   ended, and prints the registry as JSON with each open task's live agent
//!   status, for the tab picker's tabs and archive views (cmd+k).
//! - `switchyard task act ACTION HOST SESSION WINDOW`: runs on the config
//!   host. Reviews or archives the task in that tab, resumes an ended one, or
//!   prints its main work item's URL (see core's `actions` and `links`).
//! - `switchyard task run PROFILE PATH NAME HARNESS CLONE_URL [AGENT_SESSION
//!   MODE [PROMPT]]`: runs in that tab. Clones a missing project, creates or
//!   reuses the worktree with Worktrunk, starts (MODE `new`, with the prompt
//!   as its first message) or resumes (`resume`) the harness's session as the
//!   tab's process, so quitting it closes the tab.
//! - `switchyard task session-of PROFILE HARNESS WORKTREE`: runs on a task's
//!   host. Prints the newest agent session for the worktree (see core's
//!   `sessions`).
//!
//! The work itself (catalog, work names, launches, the task registry and
//! actions) is in switchyard-core; this crate holds the picker and `task run`.

mod picker;
mod run;

use std::io;

pub use crate::picker::Kind;
pub use crate::run::run;

/// The picker in a float: `home` and `folders` are the config host's label
/// and zoxide folders, one per line (possibly none).
pub fn float(kind: Kind, home: String, folders: String) -> io::Result<()> {
    picker::run_picker(kind, home, folders, false)
}

/// The picker from a shell on the config host: it opens the pick itself.
pub fn standalone(kind: Kind) -> io::Result<()> {
    picker::run_standalone(kind)
}
