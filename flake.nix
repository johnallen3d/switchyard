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

    homeManagerModules.default = ./nix/module.nix;
    homeModules.default = self.homeManagerModules.default;

    formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.alejandra);
  };
}
