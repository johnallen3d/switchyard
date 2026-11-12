# Agent instructions

## Workflow

- Follow the personal worktree workflow: inspect canonical Git metadata and
  reuse this task's linked worktree, or create one from local `main`. Do not
  adopt another task's checkout.
- Track work on the Switchyard Fizzy board; the primary checkout's local
  `.fizzy.yaml` selects it. Use its explicit board context if the task worktree
  has no local config.
- Preserve `PI_CODING_AGENT_DIR` and `CLAUDE_CONFIG_DIR`. Do not change profiles
  or copy credentials to make commands work.
- Keep changes scoped. Do not commit, push, deploy, close cards, or remove
  worktrees without authorization.
- Build the affected binaries for code changes and give the user the exact
  checkout path and commands to try the change before committing or pushing.
  Hand off changed paths, implementation attribution, checks actually run, and
  any local validation gaps.

## Completion is not deployment

**Switchyard does not own deployment. Do not keep completed Switchyard cards
open waiting for deployment or installation.** Host rollout belongs to
system-config and is a separate lifecycle, not this repository's definition of
completion.

- Complete repository work with the requested changes and task-relevant local
  validation. Documentation-only work needs no binary build or activation.
- Do not add deployment, host installation, live-float checks, or a real spawn
  as completion gates unless John explicitly requested them as acceptance
  criteria. Report unperformed runtime checks as handoff gaps, not automatic
  reasons to keep a completed card open.
- Close completed, in-scope cards when John explicitly authorizes closure or
  actually invokes `/wrap`; do not wait for a later host rollout. Completion
  alone still does not authorize closure.
- Never start deployment as part of ordinary completion or `/wrap`. If John
  separately requests it, follow system-config's deployment workflow and report
  its outcome separately from Switchyard repository completion.

## Boundaries and validation

- Tool behavior belongs here. Project catalogs, host pools, and theme settings
  belong in system-config, not compiled-in defaults or generated config files.
- Run only task-relevant checks. Documentation-only changes need no runtime
  build or activation.
- Local binaries use real Rex servers and task state. Use disposable tasks for
  action checks; do not archive, force-archive, resume, or switch unrelated work
  just to validate a change.
- `switchyard-dev` is build-only. Pass the task worktree's absolute path; the
  default primary checkout will miss unmerged changes. Do not report a build
  as installation or a standalone run as verification of the live float.
- Pre-push activation is not implemented. Do not bypass system-config's
  deployment coordinator or replace Home Manager-managed symlinks by hand.
- For separately authorized deployment, follow system-config's instructions, coordinator
  status checks, locks, and provenance gates. Review the lock diff for unrelated
  input changes. Shared changes require installation verification on Mac,
  Omarchy, Miller, and Drummer unless fewer hosts were requested. Report failed
  or unreachable hosts as partial deployment; distinguish interactive checks
  from installed-artifact verification.

## Source map

- `crates/tab-picker`: tabs, agent tasks, archive, and standalone mode
- `crates/spawn`: session/agent picker and launch commands
- `crates/cli`: the `switchyard` binary: argument parsing, subcommands, and
  `link`
- `crates/core`: config, Rex/SSH, task registry, and worktree actions
- `crates/ui`: shared drawing, filtering, and float protocol
- `lua/switchyard`: Rex bindings and float actions
- `nix`: package and Home Manager module

The new-session float dispatches actions through Lua; standalone binaries,
including the tab picker cmd+k shows in its own tab, act directly. Consider
both paths when changing the new-session picker or its protocol.
`lua/switchyard/host_rows.lua` is embedded into Rust for the tab picker and
switchyard-server.
