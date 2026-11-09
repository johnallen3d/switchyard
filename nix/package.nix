# The Rex float tools, one Cargo workspace:
#
# - rex-tab-picker: the cmd+k picker for tabs, agents (also cmd+i) and ended
#   tasks (lua/switchyard/tab-picker.lua); rex-inbox is its standalone Agents
#   view.
# - rex-spawn: the new-session picker (lua/switchyard/new.lua), which opens a
#   plain session in a zoxide folder or starts agent work in a new worktree of
#   a catalog project on a chosen host. Its --launch and --run modes do the
#   work, and it holds the task registry behind the tab picker's tasks.
#
# The Lua ships separately (the Home Manager module links it on the Mac).
{
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
    # host_rows.lua, which rex-tab-picker builds in.
    filter = path: type: let
      rel = lib.removePrefix "${toString ../.}/" (toString path);
    in
      (lib.cleanSourceFilter path type)
      && !(builtins.elem (baseNameOf path) ["target" "nix" "flake.nix" "flake.lock" "README.md"])
      && (!lib.hasPrefix "lua" rel || builtins.elem rel ["lua" "lua/switchyard" "lua/switchyard/host_rows.lua"]);
  };
  cargoLock.lockFile = ../Cargo.lock;
  nativeBuildInputs = [makeWrapper];
  postInstall = ''
    # The picker lists this host's folders with zoxide; rex and ssh stay the
    # caller's.
    wrapProgram $out/bin/rex-spawn --prefix PATH : ${lib.makeBinPath [zoxide]}
  '';
  meta = {
    description = "Rex float tools: tab picker, inbox and new-session picker";
    mainProgram = "rex-spawn";
    platforms = lib.platforms.unix;
  };
}
