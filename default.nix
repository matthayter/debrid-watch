# For non-flake use: `nix-build`, or `nix-env -f . -i`.
# The NixOS module is at nix/module.nix.
{ pkgs ? import <nixpkgs> { } }:

pkgs.callPackage ./nix/package.nix { }
