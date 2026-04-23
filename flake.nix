{
  description = "Tsunagu (繋ぐ) — service/daemon IPC framework: Unix sockets, health checks, process management";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-25.11";
    substrate = {
      url = "github:pleme-io/substrate";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crate2nix.url = "github:nix-community/crate2nix";
  };

  outputs =
    {
      self,
      nixpkgs,
      substrate,
      crate2nix,
      ...
    }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" "aarch64-linux" ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          rustLibrary = import "${substrate}/lib/rust-library.nix" {
            inherit system nixpkgs;
            nixLib = substrate;
            inherit crate2nix;
          };
          result = rustLibrary {
            name = "tsunagu";
            src = ./.;
          };
        in
        f { inherit pkgs result; }
      );
    in
    {
      packages = forEachSystem ({ result, ... }: result.packages);
      devShells = forEachSystem ({ result, ... }: result.devShells);
      apps = forEachSystem ({ result, ... }: result.apps);
      formatter = forEachSystem ({ pkgs, ... }: pkgs.nixfmt-tree);

      overlays.default = final: prev: {
        tsunagu = self.packages.${final.system}.default;
      };
    };
}
