# Home Manager module: the binaries on every host that enables it, the runtime
# config (~/.config/switchyard/config.toml, read by crates/core/src/config.rs)
# built from settings, and the Rex Lua linked to ~/.config/rex/switchyard
# (Mac by default, where the Rex config lives). Settings edits only rewrite
# the config file; they never rebuild the Rust package.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.switchyard;
  toml = pkgs.formats.toml {};
  inherit (lib) mkEnableOption mkIf mkOption types;
in {
  options.programs.switchyard = {
    enable = mkEnableOption "switchyard, the Rex float tools";

    package = mkOption {
      type = types.package;
      default = pkgs.callPackage ./package.nix {};
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix {}";
      description = "The switchyard package (built with the consumer's nixpkgs by default).";
    };

    rexLua = mkOption {
      type = types.bool;
      default = pkgs.stdenv.hostPlatform.isDarwin;
      defaultText = lib.literalExpression "pkgs.stdenv.hostPlatform.isDarwin";
      description = ''
        Link the Rex actions to ~/.config/rex/switchyard, where a Rex init.lua
        can `require("switchyard")`.
      '';
    };

    settings = mkOption {
      default = {};
      description = "Written to ~/.config/switchyard/config.toml.";
      type = types.submodule {
        freeformType = toml.type;
        options = {
          hosts = mkOption {
            type = types.listOf types.str;
            default = [];
            description = ''
              Rex host labels of the Linux dev machines the pickers offer. The
              config host's own label is added at runtime.
            '';
          };
          pools = mkOption {
            type = types.attrsOf (types.listOf types.str);
            default = {};
            example = {
              personal = ["omarchy"];
              work = ["miller" "drummer"];
            };
            description = ''
              Hosts `auto` chooses from per profile group (least busy wins); a
              project's own `hosts` overrides its group.
            '';
          };
          palette = mkOption {
            type = types.attrsOf types.str;
            default = {};
            description = ''
              Rosé Pine role names (base, surface, overlay, hl_med, hl_high,
              text, subtle, muted, love, gold, rose, pine, foam, iris) to
              `#rrggbb`. Missing roles fall back to Rosé Pine.
            '';
          };
          projects = mkOption {
            default = [];
            description = "The project catalog the new-session picker offers.";
            type = types.listOf (types.submodule {
              options = {
                profile = mkOption {
                  type = types.str;
                  description = "Profile group, matching a pool name.";
                };
                path = mkOption {
                  type = types.str;
                  description = "Checkout path, relative to the home directory.";
                };
                clone_url = mkOption {
                  type = types.str;
                  description = "Clone URL used when the checkout is missing.";
                };
                hosts = mkOption {
                  type = types.listOf types.str;
                  default = [];
                  description = "Hosts for this project, overriding its pool.";
                };
              };
            });
          };
        };
      };
    };
  };

  config = mkIf cfg.enable {
    home.packages = [cfg.package];
    xdg.configFile = {
      "switchyard/config.toml".source = toml.generate "switchyard-config.toml" cfg.settings;
      "rex/switchyard" = mkIf cfg.rexLua {source = ../lua/switchyard;};
    };
  };
}
