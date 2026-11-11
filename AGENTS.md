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
- Build the affected binaries and give the user the exact checkout path and
  commands to try the change before committing or pushing. Hand off changed
  paths, implementation attribution, checks actually run, and deployment gaps.

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
- For authorized deployment, follow system-config's instructions, coordinator
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

Floats dispatch actions through Lua; standalone binaries act directly. Consider
both paths when changing a picker or protocol. `lua/switchyard/host_rows.lua`
is shared by both paths and embedded into Rust.
