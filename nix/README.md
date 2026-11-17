# Installing debrid-watch on NixOS

This directory has the Nix packaging for debrid-watch:

- `package.nix`: the build recipe, for `callPackage`.
- `module.nix`: a NixOS module that runs debrid-watch as a systemd service, configured through `services.debrid-watch`.

Both work with or without flakes. The source is at <https://github.com/matthayter/debrid-watch>.

To try it without installing anything:

```sh
nix run github:matthayter/debrid-watch     # flakes
```

## With flakes

Add the flake as an input to your system flake and include its module:

```nix
# /etc/nixos/flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    debrid-watch.url = "github:matthayter/debrid-watch";
  };

  outputs = { nixpkgs, debrid-watch, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        debrid-watch.nixosModules.default
      ];
    };
  };
}
```

Then enable it in `configuration.nix`. [Configuration](#configuration) has the full set of options.

```nix
services.debrid-watch = {
  enable = true;
  aria2Url = "http://127.0.0.1:6800/jsonrpc";
  environmentFile = "/etc/secrets/debrid-watch.env";
};
```

By default, the package builds with the nixpkgs pinned in debrid-watch's own `flake.lock`. Add `debrid-watch.inputs.nixpkgs.follows = "nixpkgs";` to the inputs to build it with your nixpkgs instead. That avoids downloading a second copy of nixpkgs.

To update to the latest version, run `nix flake update debrid-watch`, then `nixos-rebuild switch`.

## Without flakes

Fetch the source pinned to a commit, then import the module from it:

```nix
# /etc/nixos/configuration.nix
{ ... }:
let
  debrid-watch = builtins.fetchTarball {
    url = "https://github.com/matthayter/debrid-watch/archive/<commit>.tar.gz";
    sha256 = "<hash>";
  };
in
{
  imports = [ "${debrid-watch}/nix/module.nix" ];

  services.debrid-watch = {
    enable = true;
    aria2Url = "http://127.0.0.1:6800/jsonrpc";
    environmentFile = "/etc/secrets/debrid-watch.env";
  };
}
```

`<commit>` is a full commit hash from the repository. Get `<hash>` for it with:

```sh
nix-prefetch-url --unpack https://github.com/matthayter/debrid-watch/archive/<commit>.tar.gz
```

To update, change both values. This setup builds the package with your system's nixpkgs, which needs Rust 1.85 or newer (NixOS 25.05 or later).

You can leave out `sha256` and use `.../archive/master.tar.gz` to always get the latest version. Nix then only checks for updates once its download cache expires, after an hour by default, so builds aren't reproducible. Tools such as [npins](https://github.com/andir/npins) or [niv](https://github.com/nmattia/niv) can manage the pin for you.

## Just the package

To install the program without the service, for example to run it by hand:

```nix
# flakes
environment.systemPackages = [ debrid-watch.packages.${pkgs.stdenv.hostPlatform.system}.default ];

# without flakes, using the fetchTarball above
environment.systemPackages = [ (import debrid-watch { inherit pkgs; }) ];
```

You can also run `nix-build` or `nix-env -f . -i` in a checkout of the repository.

## Configuration

| Option | Default | Description |
|---|---|---|
| `enable` | `false` | Run debrid-watch as a systemd service. |
| `package` | this repo's build | The package to run. |
| `listenAddress` | `"127.0.0.1:3000"` | Address and port to listen on. There is no authentication, so only bind to a non-local address on a network you trust. The module doesn't open the firewall. |
| `destPrefix` | `null` (`/downloads`) | Root folder for downloads, as aria2 sees it. |
| `aria2Url` | `null` | aria2 JSON-RPC URL, e.g. `http://127.0.0.1:6800/jsonrpc`. You can also set it on the Settings page. |
| `since` | `null` (service start) | Unix time from which magnets count as new. |
| `logLevel` | `"info"` | `RUST_LOG` value. |
| `environmentFile` | `null` | File of secrets. See below. |

### Secrets

Don't put the AllDebrid API key or the aria2 RPC secret in Nix options. Everything in your Nix configuration is copied to `/nix/store`, which every user on the machine can read. Put them in a file that `environmentFile` points to instead:

```sh
ALLDEBRID_API_KEY=...
DEBRID_WATCH_ARIA2_SECRET=...
```

systemd reads this file as root before starting the service, so it can be owned by root with mode `0600`:

```sh
sudo install -m 600 -o root -g root /dev/null /etc/secrets/debrid-watch.env
sudoedit /etc/secrets/debrid-watch.env
```

Write the path as a string (`"/etc/secrets/..."`), not as a bare Nix path (`/etc/secrets/...`), or Nix will copy the file into the store. Secret managers such as [sops-nix](https://github.com/Mic92/sops-nix) and [agenix](https://github.com/ryantm/agenix) can create the file for you. Point `environmentFile` at the path they give.

Both values are optional at startup. You can also enter them on the Settings page, but they're lost when the service restarts.

## Things to know

- **aria2 is separate.** debrid-watch only sends links to aria2. It never writes downloads itself, so its service has no write access to the filesystem. You can run aria2 with NixOS's `services.aria2` module or anywhere else you can reach. `destPrefix` is a path on the machine running aria2.
- **State is in memory.** Restarting the service clears what was sent, name overrides and Settings page changes. Some `nixos-rebuild switch` runs restart it. With `since = null`, magnets added while the service was down won't be listed afterwards. Set `since` to a fixed time to keep them.
- **Logs:** `journalctl -u debrid-watch`.
