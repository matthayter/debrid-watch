# debrid-watch

A small personal web server in Rust that shows magnets from an AllDebrid account and sends their files to aria2 to download. All state is in memory only; nothing is persisted.

- Synchronous code only, with no tokio or async: rouille (server), ureq (HTTP client), askama (templates in `templates/`).
- `src/alldebrid.rs`: AllDebrid API client. `src/aria2.rs`: aria2 JSON-RPC client. `src/web.rs`: routes and handlers. `src/main.rs`: startup and shared state.
- `flake.nix`: package and dev shell; `nix/module.nix`: NixOS module (`services.debrid-watch`). Flakes only see git-tracked files, so `git add` new files before `nix build`.
- `src/media.rs`: decides each file's destination (TV / movie / other) from file and folder names. Pure functions, unit-tested.
- The home page shows magnets added since startup. File lists are fetched once per ready magnet and cached in memory. Users can override the detected show name or movie title per magnet (also in memory). A Send button unlocks the included files' links and adds them to aria2; what was sent is recorded in memory so it isn't sent twice.
- Env vars:
  - `ALLDEBRID_API_KEY`: optional starting key.
  - `DEBRID_WATCH_ADDR`: default `127.0.0.1:3000`.
  - `DEBRID_WATCH_DEST_PREFIX`: default `/downloads`.
  - `DEBRID_WATCH_ARIA2_URL`, `DEBRID_WATCH_ARIA2_SECRET`: optional aria2 RPC URL and secret.
  - `DEBRID_WATCH_SINCE`: unix time to count magnets as new from; defaults to startup. `0` shows every existing magnet, which is useful for testing.
- `.env` (gitignored) holds the real API key. Load it with `set -a; . ./.env; set +a` and never print it.

## Testing

- `cargo test` runs offline unit tests only. Run these freely.
- Do NOT run the live-API tests (`cargo test -- --ignored`). A human runs them manually; this keeps us under AllDebrid's rate limit.
- Manual checks against the live API are fine: run the server and hit it with curl, or call AllDebrid with curl. Keep the number of requests low. Sending a magnet costs one unlock request per file, so test with small magnets.
