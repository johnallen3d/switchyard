# Home Manager module: the binaries on every host that enables it, the runtime
# config (~/.config/switchyard/config.toml, read by crates/core/src/config.rs)
# built from settings, and the Rex Lua linked to ~/.config/rex/switchyard
# (Mac by default, where the Rex config lives), plus a user service running
# switchyard-server there (launchd on the Mac; optional systemd on Linux).
# Settings edits only rewrite the config file; they never rebuild the Rust
# package.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.switchyard;
  toml = pkgs.formats.toml {};
  inherit (lib) mkEnableOption mkIf mkMerge mkOption types;
  inherit (pkgs.stdenv.hostPlatform) isDarwin isLinux;
  home = config.home.homeDirectory;
  # Services start with a bare PATH. The server runs rex (often outside Nix,
  # in ~/.local/bin), ssh, git, bash and hostname, so give it the same
  # lookup as the Rex Lua's util.user_path, then the system's.
  servicePath = lib.concatStringsSep ":" (lib.unique [
    "${home}/.local/bin"
    "${config.home.profileDirectory}/bin"
    "/etc/profiles/per-user/${config.home.username}/bin"
    "/run/current-system/sw/bin"
    "/nix/var/nix/profiles/default/bin"
    "/usr/local/bin"
    "/usr/bin"
    "/bin"
    "/usr/sbin"
    "/sbin"
  ]);
  # The task registry and config.toml, where the shells would find them.
  serviceEnv = {
    PATH = servicePath;
    XDG_CONFIG_HOME = config.xdg.configHome;
    XDG_STATE_HOME = config.xdg.stateHome;
  };
  serverExe = lib.getExe' cfg.package "switchyard";
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

    server = {
      enable = mkOption {
        type = types.bool;
        default = isDarwin;
        defaultText = lib.literalExpression "pkgs.stdenv.hostPlatform.isDarwin";
        description = ''
          Keep switchyard-server running as a user service: a launchd agent on
          macOS, a systemd user unit on Linux. The pickers and floats ask it
          for rows and tasks and fall back to querying the hosts directly when
          it isn't running, so this only makes them faster. Meant for the
          config host (the Mac), which owns the task registry.
        '';
      };
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
          usage = mkOption {
            type = toml.type;
            default = {};
            example = {
              quotas = [{
                path = "~/.cache/llm-usage/work.json";
                account = "work";
                profile = "work";
              }];
              sessions = [{
                harness = "pi";
                path = "~/.config/pi-work/sessions";
                account = "work";
                profile = "work";
              }];
            };
            description = ''
              Read-only Usage tab inputs, configured separately on each host.
              `quotas` is a list of llm-usage schema-v2 JSON paths with account
              and optional profile labels. `sessions` lists projects/sessions
              directories with harness (claude, pi, codex), path, account and
              optional profile. Paths may start with `~/`. Use the same account
              label on hosts sharing a login; different logins need different
              labels. Quota snapshots and copied session events are deduplicated,
              not added twice. Omit either list to use the current process's
              active profile/cache, explicitly marked unmapped; an empty list
              disables that source. No other profile or credential is discovered.

              Optional `rates` entries match an exact model and optional provider,
              and require input, output, cache_read and cache_write USD prices per
              million tokens. Missing prices stay unknown, not zero. Recorded
              costs take precedence. Estimates are API-equivalent, not the bill
              for a subscription. The tab covers the last seven days and polls
              every minute; configured hosts need the matching Switchyard binary.
              This option does not install or configure quota collectors.
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
                tracker = mkOption {
                  type = types.str;
                  default = "";
                  example = "fizzy:6284043/03h7f11emprus7nifd286mh8n";
                  description = ''
                    Where the project's work items live, for linking tasks to
                    them: `github:OWNER/REPO` and/or `fizzy:ACCOUNT/BOARD`,
                    comma separated. The first decides what a bare `#42`
                    means; the GitHub repo defaults to the clone URL's.
                  '';
                };
              };
            });
          };
        };
      };
    };
  };

  config = mkIf cfg.enable (mkMerge [
    {
      home.packages = [cfg.package];
      xdg.configFile = {
        "switchyard/config.toml".source = toml.generate "switchyard-config.toml" cfg.settings;
        "rex/switchyard" = mkIf cfg.rexLua {source = ../lua/switchyard;};
      };
    }

    (mkIf (cfg.server.enable && isDarwin) {
      launchd.agents.switchyard-server = {
        enable = true;
        config = {
          ProgramArguments = [serverExe "server"];
          EnvironmentVariables = serviceEnv;
          RunAtLoad = true;
          KeepAlive = true;
          StandardOutPath = "${home}/Library/Logs/switchyard-server.log";
          StandardErrorPath = "${home}/Library/Logs/switchyard-server.log";
        };
      };
    })

    (mkIf (cfg.server.enable && isLinux) {
      systemd.user.services.switchyard-server = {
        Unit.Description = "switchyard-server: Rex picker data kept current";
        Service = {
          ExecStart = "${serverExe} server";
          Environment = lib.mapAttrsToList (k: v: "${k}=${v}") serviceEnv;
          Restart = "always";
          RestartSec = 5;
        };
        Install.WantedBy = ["default.target"];
      };
    })
  ]);
}
