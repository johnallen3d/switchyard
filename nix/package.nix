# The Rex float tools, one Cargo workspace:
#
# - rex-tab-picker: the cmd+k picker for tabs and agents, grouped by session
#   and status, and ended tasks (lua/switchyard/tab-picker.lua).
# - rex-spawn: the new-session picker (lua/switchyard/new.lua), which opens a
#   plain session in a zoxide folder or starts agent work in a new worktree of
#   a catalog project on a chosen host. Its --launch and --run modes do the
#   work, and it holds the task registry behind the tab picker's tasks.
# - switchyard-server: keeps the pickers' rows and tasks current from Rex
#   events on the config host and serves them on a Unix socket; the tools
#   fall back to direct queries without it (the Home Manager module runs it
#   as a service).
# - switchyard: `switchyard link REF`, which links the current branch to a
#   work item (issue, card, PR) for the inbox.
#
# The Lua ships separately (the Home Manager module links it on the Mac).
{
  git,
  lib,
  makeWrapper,
  rustPlatform,
  zoxide,
}:
rustPlatform.buildRustPackage {
  pname = "switchyard";
  version = "0.1.0";
  src = lib.cleanSourceWith {
    src = ../.;
    # The Rex Lua ships separately (the Home Manager module), except
    # host_rows.lua, which rex-tab-picker and switchyard-server build in.
    filter = path: type: let
      rel = lib.removePrefix "${toString ../.}/" (toString path);
    in
      (lib.cleanSourceFilter path type)
      && !(builtins.elem (baseNameOf path) ["target" "nix" "flake.nix" "flake.lock" "README.md"])
      && (!lib.hasPrefix "lua" rel || builtins.elem rel ["lua" "lua/switchyard" "lua/switchyard/host_rows.lua"]);
  };
  cargoLock.lockFile = ../Cargo.lock;
  nativeBuildInputs = [makeWrapper];
  # core's launch tests make throwaway repos with git.
  nativeCheckInputs = [git];
  postInstall = ''
    # The picker lists this host's folders with zoxide; rex, ssh and git stay
    # the caller's (the server's service sets its PATH).
    wrapProgram $out/bin/rex-spawn --prefix PATH : ${lib.makeBinPath [zoxide]}
  '';
  meta = {
    description = "Rex float tools: tab picker, new-session picker and data server";
    mainProgram = "rex-spawn";
    platforms = lib.platforms.unix;
  };
}
