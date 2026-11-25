# Switchyard

Rex pickers for switching tabs, managing agent tasks, and starting sessions.
One Rust binary (`switchyard`), Lua actions, and a Home Manager module.

| Key           | Opens                                       |
| ------------- | ------------------------------------------- |
| `cmd+i`       | Tab picker tab: Inbox, Work, Usage, Archive |
| `cmd+k`       | Quick jump float: tabs and agents           |
| `cmd+shift+n` | New session                                 |
| `ctrl+cmd+n`  | New agent task in a project worktree        |

cmd+i shows the tab picker's own tab, the bare `switchyard` picker in the
Mac's `agents` session. Without one it opens the tab there, creating the
session if needed; on that tab already, it does nothing. No key quits the
picker; close its tab like any other (cmd+w).

cmd+k opens a quick jump float over the current tab: Inbox's list alone,
searchable, with that tab highlighted. Enter jumps to the highlighted tab
and Escape closes the float; cmd+k again closes it too. Over a session on
another host, the Mac feeds the float its tabs and tasks and makes the jump.

[docs/keys.md](docs/keys.md) tables every tab picker key by view and pane.

From a terminal on the Mac:

```bash
switchyard                  # tab picker: agents, archive
switchyard --quick          # quick jump: Enter jumps and quits
switchyard new              # new agent task (--session: plain session)
switchyard link [REF...]    # link the current branch to work items
switchyard usage            # usage view (--json, --local: print JSON)
switchyard server           # data server (normally the service)
```

## Develop

Start `pi` or `claude` in `~/dev/src/playground/switchyard` and describe the
change. Ask for the checkout path and commands to try it locally.

### Try uncommitted changes

With Cargo/Rust installed, run from the checkout containing the change:

```bash
cargo build --locked --workspace
./target/debug/switchyard
./target/debug/switchyard new --session
./target/debug/switchyard new
./target/debug/switchyard server         # foreground; Ctrl-C stops it
```

Run these in an interactive terminal. They use your existing Rex setup, runtime
config, and PATH tools (`rex`, SSH, `wt`, zoxide, and the agent harness).
Paths assume Cargo's default `target/` directory.

**These are real actions, not a sandbox.** Task loading can update registry
state; selections act on live tabs and worktrees. Use a disposable task for
action checks. Close the picker's tab to quit it; force-archive can discard changes.

### Build against your Mac config

Pass the task worktree, not the unchanged main checkout:

```bash
cd ~/dev/src/system-config
mise run switchyard-dev \
  "$HOME/dev/src/playground/switchyard/.worktrees/<task>"
```

**Build only:** this does not install anything, reload Rex, or change what the
keybindings launch. A supported pre-push activation workflow is still missing.
For now, use the standalone binaries to try changes before pushing.

## Deploy

1. Commit the Switchyard change, integrate it into `main`, and push to GitHub.
2. In the system-config checkout for the deployment, update the pin:
   `nix flake lock --update-input switchyard`.
3. Follow system-config's deployment instructions to apply it to Mac, Omarchy,
   Miller, and Drummer. The deployment command is
   `mise exec -- system-config-deploy "$PWD" --switch-only`.
4. For Lua changes, check and reload Rex on the Mac:
   `rex config check` and `rex config reload`.

A push alone does not update the installed tools. Settings-only changes need
only a system-config deployment; README changes need no activation.

## Changelog

The changelog is the repository's
[GitHub Releases](https://github.com/johnallen3d/switchyard/releases). After CI
passes on a push to `main`, the Release workflow publishes a release named like
`switchyard --version` names the build, such as `2026-11-14 17:22 UTC
(4e909b4)`, and tagged `2026.11.14-1722-4e909b4`. Its notes are
[git-cliff](https://git-cliff.org)'s list of changes since the previous
release, generated from the conventional commit subjects (see `cliff.toml`):
`feat` is Added, `fix` is Fixed, and `refactor` and `perf` are Changed. Other
types are omitted. Nothing is generated or committed locally; write a clear
conventional subject and the release notes follow.

## Configuration

Home Manager's `programs.switchyard` installs the binaries, writes
`~/.config/switchyard/config.toml` (or under `$XDG_CONFIG_HOME`), and links the
Rex Lua on macOS. Rex loads it with `require("switchyard")`.

Settings live in system-config's
`modules/home-manager/packages/switchyard-config.nix`; the project catalog is
`modules/home-manager/agent-projects/projects.toml`. Edit those sources, not the
generated config. The Mac owns the task registry; standalone Linux pickers show
tabs only.

## Server

`switchyard server` keeps the pickers' data current on the Mac: one
`rex do --all` watcher per Rex server refreshes tab rows on Rex events, and the
task registry is checked against the hosts in the background. The pickers
ask it over a Unix socket and fall back to querying the hosts directly
when it doesn't answer, so it is optional: it only makes them faster.

`programs.switchyard.server.enable` (default on macOS) runs it as the launchd
agent `org.nix-community.home.switchyard-server`, restarted if it exits. On
Linux the same option adds a `switchyard-server` systemd user unit (off by
default). The service gets a PATH with `~/.local/bin`, the Home Manager
profile, and the system directories, so it finds `rex`, `ssh`, `git`, and
`bash`.

```bash
switchyard server             # run in the foreground (stop the service first)
switchyard server snapshot    # the running server's rows, tasks, errors (JSON)
switchyard server subscribe   # a snapshot per change, until interrupted
launchctl kickstart -k "gui/$(id -u)/org.nix-community.home.switchyard-server"
```

- Socket: `/tmp/switchyard-$UID/server.sock`, or
  `$XDG_RUNTIME_DIR/switchyard/server.sock` when that is set (Linux).
- Logs: `~/Library/Logs/switchyard-server.log` on macOS;
  `journalctl --user -u switchyard-server` on Linux.

### MCP endpoint

Only the Mac can spawn tasks, and nothing else can reach it, so the server
also gives agents on the tailnet an MCP endpoint (Streamable HTTP, plain
JSON replies). It listens on loopback and the Mac's Tailscale address, port
7391, and answers only loopback and tailnet peers. There is no token: the
tailnet is the boundary. Requests from browsers (with an `Origin` header)
are refused.

| Tool             | Does                                                     |
| ---------------- | -------------------------------------------------------- |
| `list_tasks`     | Open tasks with live agent status                        |
| `list_projects`  | Catalog projects that `spawn_task` accepts               |
| `spawn_task`     | Start a task: catalog project, name, prompt, harness     |
| `read_task`      | Status and the last transcript entries                   |
| `message_task`   | Type text into the agent's tab (not while it's blocked)  |
| `message_parent` | From a spawned task: send text to its spawner (Rex tab or callback URL) |
| `stop_task`      | Archive (refused with uncommitted changes)               |

Tasks are named `PROJECT/NAME`. `spawn_task` records the calling agent as
the new task's parent when it knows how to reach it:

- **Rex callers**: from `parent_session` and `parent_block` arguments, else
  from the `X-Rex-Session` and `X-Rex-Block` request headers. The registry
  records `parent_window`, `parent_host`, and `parent_session`, and the tab
  picker's Inbox nests the task under the agent's row. A client config sends
  the headers from the agent's environment, so agents needn't pass anything.
  Empty values and unexpanded placeholders (such as a literal `${REX_BLOCK}`)
  are ignored.
- **Non-Rex callers** (such as services or chat bots): from `parent_url`
  (else `X-Parent-Url`) and optional `parent_label` (else `X-Parent-Label`).
  `parent_url` must be `http://` targeting loopback or the tailnet (or a DNS
  hostname resolving exclusively to loopback/tailnet IPs). Passing both an
  explicit `parent_block` and `parent_url` is rejected; when `parent_url` is
  set, Rex headers are ignored and `parent_window` stays unset so the task
  stays top-level in the Inbox. When set, `parent_label` is included in
  `list_tasks` and `read_task` summaries.

The server tells the parent when a child settles, so it needn't poll or set
a timer: each time the child's agent stops working (idle, done, or waiting
on a prompt), it either types a short notice into a Rex parent's tab or
`POST`s a JSON payload (`Content-Type: application/json`) to `parent_url`:

- Settle/blocked notice: `{ "event": "settled", "task", "project", "name",
  "host", "state", "status_msg", "last_message", "summary" }`
  (`last_message` capped at 2000 chars; retried with short backoff on
  connect/network/5xx errors, never on 4xx).
- Mid-work message (`message_parent`): `{ "event": "message", "task",
  "project", "name", "host", "message", "summary" }`.

A child with a timer or background job pending will resume by itself and
gets no notice until it settles for good. Pass `notify_parent: false` to
`spawn_task` to turn settle notices off. A child can also write to its
parent mid-work with `message_parent`, which finds the child's task from
its Rex block and delivers transparently to either a Rex parent tab or
`parent_url`.

To add it to Claude Code on another host:

```bash
claude mcp add --transport http switchyard \
  http://johns-macbook-pro.taila14c2.ts.net:7391/mcp \
  --header 'X-Rex-Session: ${REX_SESSION}' \
  --header 'X-Rex-Block: ${REX_BLOCK}'
```

To add it to the Antigravity CLI (`agy`):

```bash
agy mcp add switchyard http://johns-macbook-pro.taila14c2.ts.net:7391/mcp
```

Antigravity sends header values as written, without expanding variables, so
it can't send its Rex location. Spawned `agy` tasks are told their session
and block in their first message, and pass them to `message_parent` and
`spawn_task` themselves.

`[mcp]` in the config sets `enabled` (default: on macOS only) and `port`.

Development conventions and the source map are in [AGENTS.md](AGENTS.md).
