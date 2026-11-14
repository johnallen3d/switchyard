{
  description = "Switchyard: the Rex float tools (tab picker, inbox, new-session picker)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = {
    self,
    nixpkgs,
    ...
  }: let
    systems = ["aarch64-darwin" "aarch64-linux" "x86_64-linux"];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    packages = forAllSystems (system: let
      switchyard = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix {};
    in {
      default = switchyard;
      inherit switchyard;
    });

    # `nix flake check` (and CI) fails on unformatted Rust or any clippy
    # warning (pedantic included, see Cargo.toml).
    checks = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      clippy = self.packages.${system}.default.overrideAttrs (old: {
        pname = "switchyard-clippy";
        nativeBuildInputs = old.nativeBuildInputs ++ [pkgs.clippy];
        buildPhase = ''
          cargo clippy --workspace --all-targets --offline -- -D warnings
        '';
        doCheck = false;
        installPhase = "touch $out";
      });
      fmt = pkgs.runCommand "switchyard-fmt" {nativeBuildInputs = [pkgs.cargo pkgs.rustfmt];} ''
        cd ${./.}
        cargo fmt --all --check
        touch $out
      '';
    });

    homeManagerModules.default = ./nix/module.nix;
    homeModules.default = self.homeManagerModules.default;

    formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.alejandra);
  };
}
