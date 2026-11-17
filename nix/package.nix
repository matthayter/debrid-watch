# Build recipe, for use with callPackage. Shared by the flake, default.nix
# and the NixOS module.
{ lib, rustPlatform }:

let
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;

  # Only what the build needs, so a local checkout's target/ isn't copied
  # into the store and doc edits don't cause rebuilds.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../templates
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  meta = {
    description = "Web UI that sends AllDebrid magnets to aria2";
    mainProgram = "debrid-watch";
    platforms = lib.platforms.linux;
  };
}
