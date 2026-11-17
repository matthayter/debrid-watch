# Handoff

Status as of 2026-11-17: the app lists magnets added since startup, fetches and caches their file lists, works out an intended destination for each file, and lets you override the show name or movie title per magnet. A "Send to aria2" button on each ready TV or movie card unlocks the included files' links on AllDebrid and adds them to aria2, and the card then shows download progress. See `README.md` for the architecture.

## Done: sending to aria2

All the steps planned in the previous handoff are in:

1. **Settings.** aria2 RPC URL and secret on the Settings page and as `DEBRID_WATCH_ARIA2_URL` / `DEBRID_WATCH_ARIA2_SECRET`. The Settings page checks the connection (`aria2.getVersion`) each time it loads.
2. **Unlocking.** `alldebrid::Client::unlock_link` (`/v4/link/unlock`, including the `link/delayed` flow). Links are unlocked only when the user clicks Send.
3. **aria2 client.** `src/aria2.rs`. A magnet's files go in one `system.multicall` of `aria2.addUri` with `dir` and `out`.
4. **Confirm UI.** `POST /magnets/{id}/send`. Only `Decision::Include` files are sent. `AppState.sends` records the result, so a magnet can't be sent twice, and its card shows "Sent".
5. **Progress.** One `system.multicall` of `aria2.tellStatus` per home page load, shown per file and summed per magnet.

Checked by hand against the live API and a local aria2 1.37.0: a one-file TV magnet went to `{prefix}/tv/{Show}/Season_01/` with the right name, progress showed while downloading, a second send was refused, and with aria2 down the card showed the error and kept the button without spending any unlock requests.

### Behaviour worth knowing

- **All or nothing before aria2.** If any link fails to unlock, nothing is added to aria2 and the card shows the error with the button still there. If aria2 refuses individual files, the rest are still sent; refused files show on their rows and the magnet counts as sent.
- **No re-send.** Once sent, there's no button to send again, even if the downloads were removed from aria2 or failed. Restarting the server clears the record (and with `DEBRID_WATCH_SINCE` set, re-shows the button).
- **Sent destinations are frozen.** A sent card shows the classification as sent, and the name form is hidden. Changing the prefix afterwards doesn't change what's shown for it.
- **Gone from aria2.** A GID aria2 no longer knows (after `forceRemove`, a restart, or `max-download-result` trimming) shows as "No longer in aria2".
- **Slow sends block the request.** Unlocks are spaced 100 ms apart, so a 50-file pack takes several seconds before the page redirects. A delayed link can add up to a minute.
- **Stuck "Sending".** If the handler panicked mid-send, the magnet would stay `Sending` until restart. No code path is known to panic there.
- **Only TV and movies** have the button, as planned. "Other" magnets can't be sent yet.

## Improvements to consider

- **Send "Other" magnets.** The destinations are already worked out; this needs only the button allowed for `Kind::Other`, but check the file count first, since "Other" keeps every file.
- **Re-send.** A way to send a magnet again on purpose, for example when every download failed or was removed.
- **Change the kind.** Let the user switch a magnet between TV, Movie and Other when detection gets it wrong. This needs the override plumbing like `name_overrides`, plus a way to give a movie's title when switching to Movie.
- **Include/exclude individual files.** For example, include a TV extra that was excluded as "not recognised as an episode", or a video with no episode number (like the first episode of the real Peppa Pig S08 pack, `Peppa Pig-2004-S08 1080p...mkv`).
- **Show names differing only in capitalisation.** "Colin from Accounts" and "Colin From Accounts" become two folders on Linux. Name overrides cover this by hand. Fixing it automatically would mean remembering the names already used and matching them case-insensitively.
- **Duplicate releases of the same movie** (e.g. three Frozen II magnets) all go to the same folder. Now that aria2 is involved, decide whether to warn, or skip existing file names. By default aria2 renames a clashing file (`--auto-file-renaming`) rather than overwriting it.
- **Updated magnets.** Early on, "new (or updated)" magnets were considered, but only *added since startup* was built. A magnet added before startup that finishes on AllDebrid after startup isn't shown.
- **Auto-refresh or background polling.** The home page only updates when reloaded, including download progress. A meta refresh, or a background thread polling `magnet/status` (perhaps using its `session`/`counter` live mode), would be needed for any automatic sending.
- **The cache is never cleared.** Entries in `AppState.files`, `name_overrides` and `sends` for magnets deleted from AllDebrid stay until restart. That's harmless at this scale.
- **Batch size limit.** `magnet/files` is sent up to 50 IDs per call (`FILES_BATCH_SIZE`). AllDebrid doesn't document a limit; 47 in one call worked.
- **API versions.** The AllDebrid docs disagree on `v4` vs `v4.1` for `magnet/files`. `v4` works today; `magnet/status` uses `v4.1` (the `v4` version is deprecated). `link/unlock` and `link/delayed` are `v4`.

## Housekeeping

- **rouille warning.** Building warns that rouille's dependencies (`buf_redux`, `multipart`) contain code a future Rust version will reject. If that becomes a hard error, swap rouille for another blocking server such as `tiny_http`; only `web.rs` would change.
- **No authentication.** Add some (even HTTP basic auth) before binding to anything other than localhost, since the page shows and changes the API key settings and can start downloads.
- **Clippy** is in the dev shell: `nix develop -c cargo clippy --all-targets`.
- **No git remote.** The repo is local only, on branch `master`.
- **Live API tests** (`cargo test -- --ignored`) are run by a human only, per `CLAUDE.md`. Manual checks with curl against the running server are fine.
