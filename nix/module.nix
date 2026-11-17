# NixOS module: services.debrid-watch
#
# Works without flakes: import this file directly. The flake's
# nixosModules.default wraps it to use the flake's pinned build instead.
{ config, lib, pkgs, ... }:

let
  cfg = config.services.debrid-watch;
in
{
  options.services.debrid-watch = {
    enable = lib.mkEnableOption "debrid-watch, a web UI that sends AllDebrid magnets to aria2";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalMD ''
        debrid-watch built with the system's nixpkgs, or with the flake's
        pinned nixpkgs when the module comes from the flake
      '';
      description = "The debrid-watch package to run.";
    };

    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:3000";
      example = "0.0.0.0:3000";
      description = ''
        Address and port to listen on. The server has no authentication, so
        only bind to a non-local address on a network you trust.
      '';
    };

    destPrefix = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/srv/media";
      description = ''
        Root folder for download destinations, as aria2 sees it. When null,
        the program's default (`/downloads`) is used. debrid-watch never
        writes there itself; aria2 does.
      '';
    };

    aria2Url = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "http://127.0.0.1:6800/jsonrpc";
      description = "aria2 JSON-RPC URL. Can also be set on the Settings page.";
    };

    since = lib.mkOption {
      type = lib.types.nullOr lib.types.int;
      default = null;
      example = 1767225600;
      description = ''
        Unix time from which magnets count as new. When null, this is the
        time the service started, so magnets added while it was stopped
        are not shown after a restart.
      '';
    };

    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "info";
      example = "debug";
      description = "Value for `RUST_LOG`.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      example = "/run/secrets/debrid-watch.env";
      description = ''
        File of `NAME=value` lines loaded into the service's environment,
        for secrets that must not go in the Nix store:

        ```
        ALLDEBRID_API_KEY=...
        DEBRID_WATCH_ARIA2_SECRET=...
        ```

        It is read by systemd as root, so it can be owned by root with mode
        0600. Use a string path (e.g. from sops-nix or agenix), not a Nix
        path literal, or the file will be copied into the store.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.debrid-watch = {
      description = "debrid-watch";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];

      environment = {
        DEBRID_WATCH_ADDR = cfg.listenAddress;
        RUST_LOG = cfg.logLevel;
      }
      // lib.optionalAttrs (cfg.destPrefix != null) { DEBRID_WATCH_DEST_PREFIX = cfg.destPrefix; }
      // lib.optionalAttrs (cfg.aria2Url != null) { DEBRID_WATCH_ARIA2_URL = cfg.aria2Url; }
      // lib.optionalAttrs (cfg.since != null) { DEBRID_WATCH_SINCE = toString cfg.since; };

      serviceConfig = {
        ExecStart = lib.getExe cfg.package;
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        Restart = "on-failure";
        RestartSec = 5;

        # The service only talks to the network; aria2 writes the files.
        DynamicUser = true;
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        CapabilityBoundingSet = "";
        UMask = "0077";
      };
    };
  };
}
