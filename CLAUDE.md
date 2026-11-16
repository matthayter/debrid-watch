# debrid-watch

A small personal web server in Rust that shows magnets from an AllDebrid account and, later, sends their files to aria2 to download. All state is in memory only; nothing is persisted.

- Synchronous code only, with no tokio or async: rouille (server), ureq (HTTP client), askama (templates in `templates/`).
- `src/alldebrid.rs`: AllDebrid API client. `src/web.rs`: routes and handlers. `src/main.rs`: startup and shared state.
- Env vars: `ALLDEBRID_API_KEY` (optional starting key), `DEBRID_WATCH_ADDR` (default `127.0.0.1:3000`).
- `.env` (gitignored) holds the real API key. Load it with `set -a; . ./.env; set +a` and never print it.

## Testing

- `cargo test` runs offline unit tests only. Run these freely.
- Do NOT run the live-API tests (`cargo test -- --ignored`). A human runs them manually; this keeps us under AllDebrid's rate limit.
- Manual checks against the live API are fine: run the server and hit it with curl, or call AllDebrid with curl. Keep the number of requests low.
