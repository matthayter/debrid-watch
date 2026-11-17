{
  description = "debrid-watch: send AllDebrid magnets to aria2";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = cargoToml.package.name;
          version = cargoToml.package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          meta = {
            description = "Web UI that sends AllDebrid magnets to aria2";
            mainProgram = "debrid-watch";
            platforms = pkgs.lib.platforms.linux;
          };
        };
      });

      # Defaults `package` to this flake's build for the host's platform.
      nixosModules.default = import ./nix/module.nix (system: self.packages.${system}.default);

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
