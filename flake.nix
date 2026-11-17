{
  description = "debrid-watch: send AllDebrid magnets to aria2";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.callPackage ./nix/package.nix { };
      });

      # The plain module, but defaulting `package` to this flake's build
      # (pinned nixpkgs) for the host's platform.
      nixosModules.default = { lib, pkgs, ... }: {
        imports = [ ./nix/module.nix ];
        services.debrid-watch.package =
          lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.rustc
            pkgs.cargo
            pkgs.clippy
            pkgs.pkg-config
          ];
        };
      });
    };
}
