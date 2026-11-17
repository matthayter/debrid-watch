# debrid-watch

A small personal web server that watches an [AllDebrid](https://alldebrid.com) account for newly added magnets, lists the files in each one, and works out where each file should be downloaded to (TV / movies / other). Once you've checked the destinations, one click sends a TV or movie magnet's files to an [aria2](https://aria2.github.io/) instance to download, and the page then shows their progress.

Everything is held in memory. Restarting the server clears the settings, cached file lists, name overrides and the record of what was sent to aria2.

## Running

```sh
nix develop            # dev shell with rustc and cargo (see flake.nix)
cargo run              # serves http://127.0.0.1:3000
```

Set the AllDebrid API key and the aria2 RPC URL on the Settings page, or pass them at startup:

```sh
set -a; . ./.env; set +a   # .env (gitignored) contains ALLDEBRID_API_KEY=...
cargo run
```

### Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `ALLDEBRID_API_KEY` | unset | Starting API key. It's checked at startup but kept even if the check fails. |
| `DEBRID_WATCH_ADDR` | `127.0.0.1:3000` | Address to listen on. |
| `DEBRID_WATCH_DEST_PREFIX` | `/downloads` | Root folder for destinations. It can also be changed on the Settings page. |
| `DEBRID_WATCH_ARIA2_URL` | unset | aria2 JSON-RPC URL, e.g. `http://host:6800/jsonrpc`. It's checked at startup but kept even if aria2 can't be reached. |
| `DEBRID_WATCH_ARIA2_SECRET` | unset | aria2's `--rpc-secret`, if it has one. |
| `DEBRID_WATCH_SINCE` | startup time | Unix time from which magnets count as "new". `0` shows every magnet on the account, which is handy for testing. |
| `RUST_LOG` | `info` | Log level (`env_logger`). |

The server has no authentication. Only bind it to a non-local address on a network you trust.

### NixOS

The flake provides a package (`nix build`, `nix run`) and a NixOS module. Add the flake as an input, then:

```nix
# flake.nix:   inputs.debrid-watch.url = "github:<owner>/debrid-watch";
# in modules:  debrid-watch.nixosModules.default
services.debrid-watch = {
  enable = true;
  listenAddress = "127.0.0.1:3000";               # default
  destPrefix = "/srv/media";                      # optional, as aria2 sees it
  aria2Url = "http://127.0.0.1:6800/jsonrpc";     # optional
  since = null;                                   # optional unix time; null = service start
  environmentFile = "/run/secrets/debrid-watch.env";
};
```

Secrets go in `environmentFile`, never in Nix options, because anything in the Nix config ends up in the world-readable `/nix/store`. Its contents look like this:

```sh
ALLDEBRID_API_KEY=...
DEBRID_WATCH_ARIA2_SECRET=...
```

systemd reads it as root, so make it `root:root` with mode `0600`, or let sops-nix or agenix create it. Give the path as a string, not a Nix path literal, or Nix will copy the file into the store.

The service runs as a sandboxed dynamic user with no write access to the filesystem. That's fine because aria2 writes the downloads. Because state is in memory, every restart (including some `nixos-rebuild switch` runs) clears what was sent, and with `since = null` it also hides magnets added while the service was down.

## Architecture

The code is all synchronous Rust, with no async runtime. The app is for one user with very little traffic, so plain threads are simpler and fast enough.

| Crate | Role |
|---|---|
| `rouille` | HTTP server. Runs each request on a thread from a pool. |
| `ureq` | Blocking HTTP client (rustls) for the AllDebrid API and aria2's JSON-RPC. |
| `askama` | HTML templates in `templates/`, compiled into the binary and checked at build time. |
| `regex` | Name parsing in `media.rs`. |
| `serde`, `serde_json` | JSON (AllDebrid and aria2) and forms. |

### Modules

```
src/
  main.rs       startup, env vars, shared AppState
  alldebrid.rs  AllDebrid API client and response types
  aria2.rs      aria2 JSON-RPC client
  media.rs      file classification: works out each file's destination (pure, no I/O)
  web.rs        routes, handlers, and the view data handed to templates
templates/
  base.html     layout, nav and all CSS (light/dark)
  home.html     new magnets, their files and destinations
  settings.html API key, destination prefix and aria2
```

#### `main.rs`: state

`AppState` is created once and shared with every request through an `Arc`:

- `settings: Mutex<Settings>`: the API key, the AllDebrid username, the destination prefix, and the aria2 RPC URL and secret.
- `since: i64`: magnets with `uploadDate >= since` count as new.
- `files: Mutex<HashMap<magnet id, CachedFiles>>`: the file-list cache (the magnet's name and its `Vec<MagnetFile>`). Each ready magnet's list is fetched only once.
- `name_overrides: Mutex<HashMap<magnet id, String>>`: show names or movie titles set by the user.
- `sends: Mutex<HashMap<magnet id, SendState>>`: magnets sent to aria2. `Sending` while a request is unlocking and adding; `Failed { error }` if nothing was added (it can be retried); `Sent(SentMagnet)` once added, holding the classification as it was when sent and each file's aria2 GID (or why aria2 refused it).

The locks are held briefly, and never during an HTTP call to AllDebrid or aria2. A poisoned lock is recovered rather than unwrapped, since a half-finished update can't leave this state inconsistent.

#### `alldebrid.rs`: API client

The client uses `Authorization: Bearer <key>` auth. Every response is wrapped in `{"status": "success", "data": ...}` or `{"status": "error", "error": {code, message}}`, and the client turns that into `Result<T, Error>`. Because AllDebrid puts errors in the JSON body, the HTTP status code isn't treated as an error by itself.

| Method | Endpoint | Used for |
|---|---|---|
| `user()` | `GET /v4/user` | Checking an API key and getting the username. |
| `magnets()` | `POST /v4.1/magnet/status` | Listing every magnet with its status and dates. |
| `magnet_files()` | `POST /v4/magnet/files` with form `id[]=…` | File trees for several magnets in one call. |
| `unlock_link()` | `POST /v4/link/unlock` with form `link=…` | Turning a file link into a direct download URL. If the reply has a `delayed` ID, it polls `POST /v4/link/delayed` every 5 seconds, for up to a minute. |

`magnet_files` flattens AllDebrid's tree (`n` name, `s` size, `l` link, `e` children) into `MagnetFile { path: "dir/sub/file.ext", size, link }`. The `link` values (`https://alldebrid.com/f/...`) aren't direct downloads; `unlock_link` turns each into one just before it's sent to aria2, since direct URLs expire. The `magnet/files` endpoint returns IDs as strings, and the client accepts strings or numbers.

#### `aria2.rs`: aria2 client

Blocking JSON-RPC over HTTP POST. When a secret is set, `token:<secret>` is put first in each call's parameters. Errors come back in the body (`{"error": {code, message}}`), usually with a non-2xx status, so the body is always read.

| Method | RPC | Used for |
|---|---|---|
| `version()` | `aria2.getVersion` | Checking the URL and secret (5-second timeout). |
| `add_uris()` | `system.multicall` of `aria2.addUri` | Adding all of a magnet's files in one request, each with `{"dir": dest_dir, "out": file_name}`. Returns each file's GID or error. |
| `statuses()` | `system.multicall` of `aria2.tellStatus` | Progress of several downloads in one request. aria2 sends numbers as strings; they're parsed to `u64`. |

`system.multicall` returns one item per call: `[result]` on success, or an error object.

#### `media.rs`: classification

`classify(magnet_name, files, prefix, name_override) -> Classification { kind, detected_name, files: Vec<PlannedFile> }`. Each `PlannedFile` gets a `Decision`: either `Include { dest_dir, file_name }` or `Exclude { reason }`.

1. **File facts.** Each file's extension decides its type (video, subtitle or other). A video is a *sample* if its name contains the word "sample" or it sits in a `Sample/` folder. An episode is found by looking for `S01E02` or `1x02` in the file name, then in each parent folder from the innermost out.
2. **Kind.**
   - **TV:** at least one non-sample video has an episode number.
   - **Movie:** non-sample videos, but none with an episode number.
   - **Other:** everything else.
3. **Destinations.**
   - **TV:** `{prefix}/tv/{Show}/Season_{NN}/{file}`.
     - The show name is the text before the episode number. If that's empty, it comes from the folder names, then from the magnet name.
     - The season comes from each file, so multi-season packs are split.
     - A subtitle whose episode number came from its folder is renamed `{folder}.{file}`, so files don't overwrite each other.
     - A subtitle with no episode number goes to the season folder if the pack has one season, and is excluded otherwise.
     - Videos that aren't episodes (extras), samples and other files are excluded.
   - **Movie:** `{prefix}/movies/{Title (Year)}/{file}`.
     - The title comes from the magnet name, or else the largest video. It's cut at release info (`1080p`, `BluRay`, `x265` and so on), and the last year before that cut is kept.
     - Videos and subtitles are put in one folder, flattened. If two files have the same name, the second gets its parent folder name as a prefix.
     - Samples and other files are excluded.
   - **Other:** `{prefix}/other/{original path}`. Nothing is excluded.
4. **Name cleanup.** Leading `[Group]` tags and `www.site.org - ` prefixes are removed, and `.` and `_` become spaces. A trailing year becomes `(YYYY)`. User overrides are cleaned by `clean_name_override` so they're safe as one folder name.

Everything in this module is a pure function covered by unit tests, many of them based on real release names. When changing the rules, add a test case for the release that prompted it.

#### `web.rs`: request flow

Routes: `GET /`, `GET /settings`, `POST /settings` (with `action` set to `save_key`, `remove_key`, `save_prefix`, `save_aria2` or `remove_aria2`), `POST /magnets/{id}/name` (with `action` set to `save` or `reset`), and `POST /magnets/{id}/send`. Every POST redirects afterwards (303), so reloading a page never re-submits a form.

`GET /` works like this:

1. Copy the settings, then release the lock.
2. `magnets()`: keep those added since `since`, newest first.
3. `fetch_missing_files()`: for ready magnets (`statusCode == 4`) not in the cache, call `magnet_files()` in batches of 50 and cache the results. Errors for individual magnets are shown on their cards and retried on the next load.
4. `fetch_download_statuses()`: if any shown magnets have been sent, ask aria2 for the status of all their GIDs in one request. If aria2 can't be reached, the page says so and shows no progress.
5. For each magnet, run `media::classify` with the cached files, the prefix and any name override, then build the `MagnetCard` view data for `home.html`. A sent magnet uses its saved classification instead, and gets each file's download status and a summary line.

Classification runs again on every render (it's cheap), so a change to the prefix or a name takes effect straight away. Once file lists are cached, a page load makes one AllDebrid request, plus one more if new magnets have become ready. aria2 gets one request if anything has been sent.

`POST /magnets/{id}/send` works like this:

1. Mark the magnet `Sending`, unless it's already `Sending` or `Sent`, so a double click or a second tab can't send it twice.
2. Classify the cached files again with the current prefix and name, as the page showed them. Only TV and movies are sent, and only `Decision::Include` files.
3. Check aria2 with `aria2.getVersion`, so a wrong URL or secret doesn't use up unlock requests.
4. Unlock each file's link, 100 ms apart to stay under AllDebrid's rate limit. If any unlock fails, stop and send nothing.
5. Add them all to aria2 in one `system.multicall`.
6. Record `Sent` (with each file's GID or error) or `Failed { error }`, then redirect back to the card.

Once sent, the card hides the name form, since a new name couldn't change where the files are going.

The Settings page checks the aria2 connection each time it loads and shows the version or the error.

## Testing

```sh
cargo test                  # offline unit tests (parsing, classification, formatting, aria2 replies)
cargo test -- --ignored     # live AllDebrid tests; needs ALLDEBRID_API_KEY
```

The live tests are meant to be run by hand, to stay well under AllDebrid's rate limits (12 requests per second, 600 per minute). For checking the UI against real data, run with `DEBRID_WATCH_SINCE=0`.

To try sending without a real download machine, run a local aria2 and point `DEBRID_WATCH_ARIA2_URL` at it:

```sh
nix shell nixpkgs#aria2 -c aria2c --enable-rpc --rpc-listen-port=6800 --rpc-secret=test --dir=/tmp/aria2-test
```

Downloads start straight away (`--pause=true` didn't hold back downloads added over RPC when tested), so pick a small magnet, and stop it with `aria2.forceRemove` or by shutting aria2 down. Each send uses one AllDebrid request per file.
