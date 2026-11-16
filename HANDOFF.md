# Handoff

Status as of 2026-11-16 (commit `6fc87c3`): the app lists magnets added since startup, fetches and caches their file lists, works out an intended destination for each file, and lets you override the show name or movie title per magnet. Nothing is downloaded yet. See `README.md` for the architecture.

## Next: send files to aria2 (the main remaining feature)

The original goal: after you have checked the destinations shown, send the included files to an aria2 instance to download.

1. **Settings.** Add the aria2 RPC URL (e.g. `http://host:6800/jsonrpc`) and the RPC secret, on the Settings page and as env vars, following the pattern of `DEBRID_WATCH_DEST_PREFIX`. Consider a "test connection" check using `aria2.getVersion`, like the API key check.
2. **Unlock the links.** `MagnetFile.link` holds `https://alldebrid.com/f/...` links, which aren't direct downloads. Each needs to be unlocked through AllDebrid's link-unlock endpoint (`/v4/link/unlock`; check the current version and parameters in the docs) to get a direct URL.
   - Do this when the user confirms, not when the page loads, because unlocked links can expire and each unlock costs a request against the rate limit.
3. **aria2 client.** Add `src/aria2.rs`: blocking JSON-RPC over `ureq`, matching the style of `alldebrid.rs`.
   - Call `aria2.addUri` with `["token:<secret>", [direct_url], {"dir": dest_dir, "out": file_name}]` for each included file. `out` handles the renamed files (subtitles from folders, name clashes).
   - `system.multicall` could send a whole magnet in one request.
4. **Confirm UI.** Add a "Send to aria2" button on each ready TV or movie card, posting to e.g. `POST /magnets/{id}/send`.
   - Send only `Decision::Include` files.
   - Record in memory that the magnet was sent (with the aria2 GIDs) so it can't be sent twice by accident, and show "Sent" on the card.
5. **Optional extra.** Show aria2 download progress for sent magnets (`aria2.tellStatus` per GID).

## Improvements to consider

- **Change the kind.** Let the user switch a magnet between TV, Movie and Other when detection gets it wrong. This needs the override plumbing like `name_overrides`, plus a way to give a movie's title when switching to Movie.
- **Include/exclude individual files.** For example, include a TV extra that was excluded as "not recognised as an episode", or a video with no episode number (like the first episode of the real Peppa Pig S08 pack, `Peppa Pig-2004-S08 1080p...mkv`).
- **Show names differing only in capitalisation.** "Colin from Accounts" and "Colin From Accounts" become two folders on Linux. Name overrides cover this by hand. Fixing it automatically would mean remembering the names already used and matching them case-insensitively.
- **Duplicate releases of the same movie** (e.g. three Frozen II magnets) all go to the same folder. That's fine until aria2 is involved; then decide whether to warn, or skip existing file names.
- **Updated magnets.** Early on, "new (or updated)" magnets were considered, but only *added since startup* was built. A magnet added before startup that finishes on AllDebrid after startup isn't shown.
- **Auto-refresh or background polling.** The home page only updates when reloaded. A meta refresh, or a background thread polling `magnet/status` (perhaps using its `session`/`counter` live mode), would be needed for any automatic sending.
- **The cache is never cleared.** Entries in `AppState.files` and `name_overrides` for magnets deleted from AllDebrid stay until restart. That's harmless at this scale.
- **Batch size limit.** `magnet/files` is sent up to 50 IDs per call (`FILES_BATCH_SIZE`). AllDebrid doesn't document a limit; 47 in one call worked.
- **API versions.** The AllDebrid docs disagree on `v4` vs `v4.1` for `magnet/files`. `v4` works today; `magnet/status` uses `v4.1` (the `v4` version is deprecated).

## Housekeeping

- **rouille warning.** Building warns that rouille's dependencies (`buf_redux`, `multipart`) contain code a future Rust version will reject. If that becomes a hard error, swap rouille for another blocking server such as `tiny_http`; only `web.rs` would change.
- **No authentication.** Add some (even HTTP basic auth) before binding to anything other than localhost, since the page shows and changes the API key settings.
- **No git remote.** The repo is local only, on branch `master`.
- **Live API tests** (`cargo test -- --ignored`) are run by a human only, per `CLAUDE.md`. Manual checks with curl against the running server are fine.
