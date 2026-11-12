# Switchyard

Rex pickers for switching tabs, managing agent tasks, and starting sessions.
One Rust binary (`switchyard`), Lua actions, and a Home Manager module.

| Key           | Opens                                    |
| ------------- | ---------------------------------------- |
| `cmd+k`       | Tab picker tab: tabs and agents, Archive |
| `cmd+shift+n` | New session                              |
| `ctrl+cmd+n`  | New agent task in a project worktree     |

cmd+k shows the tab picker's own tab, the bare `switchyard` picker in the
Mac's `agents` session. Without one it opens the tab there, creating the
session if needed; on that tab already, it does nothing. Quitting the picker
(Escape or ctrl-c) closes the tab.

From a terminal on the Mac:

```bash
switchyard                  # tab picker: tabs, agents, archive
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
action checks. Escape closes the picker; force-archive can discard changes.

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

Development conventions and the source map are in [AGENTS.md](AGENTS.md).
