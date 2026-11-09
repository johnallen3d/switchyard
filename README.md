# Switchyard

Rex pickers for switching tabs, managing agent tasks, and starting sessions.
Rust binaries, Lua actions, and a Home Manager module.

| Key           | Opens                                |
| ------------- | ------------------------------------ |
| `cmd+k`       | Tab picker: All, Agents, Archive     |
| `cmd+i`       | The same picker, starting on Agents  |
| `cmd+shift+n` | New session                          |
| `ctrl+cmd+n`  | New agent task in a project worktree |

## Develop

Start `pi` or `claude` in `~/dev/src/playground/switchyard` and describe the
change. Ask for the checkout path and commands to try it locally.

### Try uncommitted changes

With Cargo/Rust installed, run from the checkout containing the change:

```bash
cargo build --locked --workspace
./target/debug/rex-tab-picker
./target/debug/rex-inbox                 # Agents view
./target/debug/rex-spawn --kind session
./target/debug/rex-spawn --kind agent
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

Development conventions and the source map are in [AGENTS.md](AGENTS.md).
